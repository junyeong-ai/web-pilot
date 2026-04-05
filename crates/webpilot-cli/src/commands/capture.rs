use anyhow::{Context, Result};
use clap::Args;
use webpilot::ipc;
use webpilot::protocol::{Command, ResponseData};

use crate::output::CommandOutput;

#[derive(Args)]
pub struct CaptureArgs {
    /// Extract DOM (interactive elements)
    #[arg(long)]
    pub dom: bool,

    /// Capture screenshot (saved to file, path returned)
    #[arg(long)]
    pub screenshot: bool,

    /// Extract visible page text content
    #[arg(long)]
    pub text: bool,

    /// Include bounding box coordinates for each element
    #[arg(long)]
    pub bounds: bool,

    /// Full-page screenshot (captures entire scrollable area)
    #[arg(long)]
    pub full_page: bool,

    /// Accessibility tree via CDP (shows debugger banner)
    #[arg(long)]
    pub accessibility: bool,

    /// Detect occluded elements (center-point coverage check)
    #[arg(long)]
    pub occlusion: bool,

    /// Annotated screenshot: numbered labels drawn on interactive elements
    #[arg(long)]
    pub annotate: bool,

    /// Generate PDF of the page
    #[arg(long)]
    pub pdf: bool,

    /// Navigate to URL before capturing
    #[arg(long)]
    pub url: Option<String>,
}

pub async fn run(args: CaptureArgs) -> Result<CommandOutput> {
    // --annotate implies --dom --screenshot --bounds, conflicts with --fullpage
    let annotate = args.annotate;
    if annotate && args.full_page {
        anyhow::bail!(
            "--annotate and --full-page cannot be combined. Annotations are viewport-only."
        );
    }
    let dom = args.dom || annotate || (!args.screenshot && !args.text && !args.accessibility);
    let screenshot = args.screenshot || annotate;
    let bounds = args.bounds || annotate;

    let request = serde_json::to_value(webpilot::protocol::Request::new(
        1,
        Command::Capture {
            dom,
            screenshot,
            text: args.text,
            url: args.url,
            bounds,
            full_page: args.full_page,
            accessibility: args.accessibility,
            occlusion: args.occlusion,
            annotate,
            pdf: args.pdf,
        },
    ))?;

    let response = ipc::send_request(&request).await.context(
        "WebPilot host not running. Run `webpilot install` and reload the Chrome extension.",
    )?;

    // Check for fullpage tiles (raw JSON, before typed deserialization)
    if let Some(tiles) = response
        .pointer("/result/screenshot_tiles")
        .and_then(|v| v.as_array())
        && !tiles.is_empty()
    {
        let output_dir = std::path::Path::new(webpilot::OUTPUT_DIR);
        match crate::stitch::stitch_tiles(tiles, output_dir) {
            Ok(path) => {
                let path_str = path.to_string_lossy().to_string();
                return Ok(CommandOutput::Data {
                    json: serde_json::json!({"screenshot_path": path_str}),
                    human: format!("Full-page screenshot: {}", path.display()),
                });
            }
            Err(e) => anyhow::bail!("Stitch error: {e:#}"),
        }
    }

    let resp: webpilot::protocol::Response =
        serde_json::from_value(response).context("Invalid response from host")?;

    match resp.result {
        ResponseData::Capture {
            dom: dom_snapshot,
            screenshot_path,
            screenshot_error,
            ..
        } => {
            // Save accessibility tree to file if present
            let mut ax_path: Option<String> = None;
            if let Some(ref snapshot) = dom_snapshot
                && let Some(ref ax_tree) = snapshot.accessibility_tree
            {
                let output_dir = std::path::Path::new(webpilot::OUTPUT_DIR);
                let _ = std::fs::create_dir_all(output_dir);
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis();
                let path = output_dir.join(format!("accessibility_{ts}.json"));
                std::fs::write(&path, ax_tree).context("Cannot save accessibility tree")?;
                ax_path = Some(path.to_string_lossy().to_string());
            }

            // Build the Dom output with extra fields
            let mut extra = serde_json::Map::new();
            if let Some(ref path) = ax_path {
                extra.insert("accessibility_path".into(), serde_json::json!(path));
            }
            if let Some(ref path) = screenshot_path {
                extra.insert("screenshot_path".into(), serde_json::json!(path));
            }
            if let Some(ref err) = screenshot_error {
                extra.insert("screenshot_error".into(), serde_json::json!(err));
            }

            if let Some(mut snapshot) = dom_snapshot {
                // Strip accessibility_tree from snapshot (saved to file separately)
                snapshot.accessibility_tree = None;
                Ok(CommandOutput::Dom { snapshot, extra })
            } else {
                // No DOM data — return Data with just screenshot/error info
                if extra.is_empty() {
                    Ok(CommandOutput::Ok("OK".into()))
                } else {
                    let json = serde_json::Value::Object(extra.clone());
                    let mut human_parts = Vec::new();
                    if let Some(path) = extra.get("accessibility_path").and_then(|v| v.as_str()) {
                        human_parts.push(format!("Accessibility tree: {path}"));
                    }
                    if let Some(path) = extra.get("screenshot_path").and_then(|v| v.as_str()) {
                        human_parts.push(format!("Screenshot: {path}"));
                    }
                    if let Some(err) = extra.get("screenshot_error").and_then(|v| v.as_str()) {
                        human_parts.push(crate::output::format_error_str(err));
                    }
                    Ok(CommandOutput::Data {
                        json,
                        human: human_parts.join("\n"),
                    })
                }
            }
        }
        ResponseData::Error { message, .. } => {
            anyhow::bail!("{message}");
        }
        _ => {
            anyhow::bail!("Unexpected response type");
        }
    }
}
