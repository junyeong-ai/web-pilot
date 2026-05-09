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

    // Tile-stitched full-page screenshot is delivered as `screenshot_tiles` on
    // the wire. The current `IpcTransport` deserializes into `ResponseData`,
    // which does not surface that field — full-page mode in browser path is
    // a future deliverable. For now, we handle the typed Capture response.
    match result {
        ResponseData::Capture {
            dom,
            screenshot_path,
            screenshot_error,
            pdf_path,
            ..
        } => {
            // Persist accessibility tree to a file when present. Transport
            // delivers the JSON inline; the CLI is the single writer.
            let mut ax_path: Option<String> = None;
            if let Some(ref snapshot) = dom
                && let Some(ref ax_tree) = snapshot.accessibility_tree
            {
                let dir = webpilot::dirs::artifacts_dir();
                let path = dir.join(format!("accessibility_{}.json", epoch_ms()));
                std::fs::write(&path, ax_tree).context("Cannot save accessibility tree")?;
                ax_path = Some(path.to_string_lossy().into_owned());
            }

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
                let human = render_capture_extras(&extra);
                Ok(CommandOutput::Data { json, human })
            }
        }
        ResponseData::Error { error } => Err(error.into()),
        _ => anyhow::bail!("Unexpected response shape"),
    }
}

fn render_capture_extras(extra: &serde_json::Map<String, serde_json::Value>) -> String {
    let labels = [
        ("accessibility_path", "Accessibility tree"),
        ("screenshot_path", "Screenshot"),
        ("pdf_path", "PDF"),
        ("screenshot_error", "Screenshot error"),
    ];
    labels
        .iter()
        .filter_map(|(key, label)| {
            extra
                .get(*key)
                .and_then(|v| v.as_str())
                .map(|v| format!("{label}: {v}"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn epoch_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}
