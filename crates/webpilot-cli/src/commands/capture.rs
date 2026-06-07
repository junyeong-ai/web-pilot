use anyhow::{Context, Result};
use clap::Args;
use webpilot::capture::{CaptureField, CaptureOpts};
use webpilot::protocol::{Command, ResponseData};

use crate::output::CommandOutput;
use crate::transport::Transport;

#[derive(Args)]
pub struct CaptureArgs {
    /// What to extract. Repeatable. Default: dom.
    #[arg(long, value_enum, num_args = 1.., default_values_t = vec![CaptureField::Dom])]
    pub include: Vec<CaptureField>,

    #[arg(long)]
    pub url: Option<String>,

    #[command(flatten)]
    pub opts: CaptureOpts,
}

pub async fn run<T: Transport>(transport: &mut T, args: CaptureArgs) -> Result<CommandOutput> {
    args.opts
        .validate()
        .map_err(|m| webpilot::WebPilotError::InvalidArgument {
            detail: m.to_owned(),
        })?;

    let result = transport
        .send(Command::Capture {
            include: args.include,
            opts: args.opts,
            url: args.url,
        })
        .await?;

    // The CLI is the single file writer. Headless transports return paths
    // directly; browser mode returns bytes inline (`pdf_b64`,
    // `screenshot_tiles`, the accessibility JSON) for the CLI to persist.
    match result {
        ResponseData::Capture {
            dom,
            screenshot_path,
            screenshot_error,
            pdf_path,
            pdf_b64,
            screenshot_tiles,
            tile_viewport_height,
            tile_total_height,
            ..
        } => {
            let dir = webpilot::dirs::artifacts_dir();

            // Persist accessibility tree to a file when present.
            let mut ax_path: Option<String> = None;
            if let Some(ref snapshot) = dom
                && let Some(ref ax_tree) = snapshot.accessibility_tree
            {
                let path = dir.join(format!("accessibility_{}.json", epoch_ms()));
                std::fs::write(&path, ax_tree).context("Cannot save accessibility tree")?;
                ax_path = Some(path.to_string_lossy().into_owned());
            }

            // Browser-mode full-page screenshot arrives as tiles; stitch them.
            let stitched = if !screenshot_tiles.is_empty() {
                Some(
                    crate::stitch::stitch_tiles(
                        &screenshot_tiles,
                        tile_viewport_height,
                        tile_total_height,
                        &dir,
                    )?
                    .to_string_lossy()
                    .into_owned(),
                )
            } else {
                None
            };
            let screenshot_path = screenshot_path.or(stitched);

            // Browser-mode PDF arrives base64-encoded; decode and write it.
            let pdf_written = if let Some(b64) = pdf_b64 {
                let bytes = base64::Engine::decode(
                    &base64::engine::general_purpose::STANDARD,
                    b64.as_bytes(),
                )
                .context("Cannot decode PDF bytes")?;
                let path = dir.join(format!("capture_{}.pdf", epoch_ms()));
                std::fs::write(&path, bytes).context("Cannot save PDF")?;
                Some(path.to_string_lossy().into_owned())
            } else {
                None
            };
            let pdf_path = pdf_path.or(pdf_written);

            let mut extra = serde_json::Map::new();
            for (key, value) in [
                ("accessibility_path", ax_path.as_deref()),
                ("screenshot_path", screenshot_path.as_deref()),
                ("pdf_path", pdf_path.as_deref()),
                ("screenshot_error", screenshot_error.as_deref()),
            ] {
                if let Some(v) = value {
                    extra.insert(key.into(), serde_json::json!(v));
                }
            }

            if let Some(mut snapshot) = dom {
                snapshot.accessibility_tree = None;
                Ok(CommandOutput::Dom { snapshot, extra })
            } else if extra.is_empty() {
                Ok(CommandOutput::Ok("OK".into()))
            } else {
                let json = serde_json::Value::Object(extra.clone());
                let human = crate::output::dom_extra_lines(&extra).join("\n");
                Ok(CommandOutput::Data { json, human })
            }
        }
        ResponseData::Error { error } => Err(error.into()),
        _ => anyhow::bail!("Unexpected response shape"),
    }
}

fn epoch_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}
