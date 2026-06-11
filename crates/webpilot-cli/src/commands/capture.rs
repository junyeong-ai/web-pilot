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
    // directly; browser mode returns bytes inline (`pdf_b64`, the
    // accessibility JSON) for the CLI to persist.
    match result {
        ResponseData::Capture {
            dom,
            screenshot_path,
            screenshot_width,
            screenshot_height,
            screenshot_scale,
            screenshot_error,
            pdf_path,
            pdf_b64,
            page_url,
            page_title,
        } => {
            // Persist accessibility tree to a file when present.
            let mut ax_path: Option<String> = None;
            if let Some(ref snapshot) = dom
                && let Some(ref ax_tree) = snapshot.accessibility_tree
            {
                let path = webpilot::dirs::artifact_path("accessibility", "json");
                std::fs::write(&path, ax_tree).context("Cannot save accessibility tree")?;
                ax_path = Some(path.to_string_lossy().into_owned());
            }

            // Browser-mode PDF arrives base64-encoded; decode and write it.
            let pdf_written = if let Some(b64) = pdf_b64 {
                let bytes = base64::Engine::decode(
                    &base64::engine::general_purpose::STANDARD,
                    b64.as_bytes(),
                )
                .context("Cannot decode PDF bytes")?;
                let path = webpilot::dirs::artifact_path("capture", "pdf");
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
            // The saved image's dimensions — and the downscale ratio when the
            // capture exceeded the long-edge cap — so coordinate math on the
            // image is possible (`coord / scale` = page pixels). Withholding
            // them made a downscaled full-page shot silently unmappable.
            if let Some(w) = screenshot_width {
                extra.insert("screenshot_width".into(), serde_json::json!(w));
            }
            if let Some(h) = screenshot_height {
                extra.insert("screenshot_height".into(), serde_json::json!(h));
            }
            if let Some(s) = screenshot_scale {
                extra.insert("screenshot_scale".into(), serde_json::json!(s));
            }

            if let Some(mut snapshot) = dom {
                // The DOM snapshot already carries the page URL/title in its
                // footer, so it identifies the captured page on its own.
                snapshot.accessibility_tree = None;
                Ok(CommandOutput::Dom { snapshot, extra })
            } else {
                // No DOM footer here, so a screenshot/PDF/AX-only capture must
                // still say WHAT page it captured — otherwise a redirected `--url`
                // or a switched iframe leaves the agent holding an artifact path
                // with no idea what navigation state it reflects.
                extra.insert("page_url".into(), serde_json::json!(page_url));
                if !page_title.is_empty() {
                    extra.insert("page_title".into(), serde_json::json!(page_title));
                }
                let json = serde_json::Value::Object(extra.clone());
                let human = crate::output::dom_extra_lines(&extra).join("\n");
                Ok(CommandOutput::Data { json, human })
            }
        }
        ResponseData::Error { error } => Err(error.into()),
        _ => anyhow::bail!("Unexpected response shape"),
    }
}
