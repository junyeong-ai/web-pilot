use anyhow::{Context, Result};
use clap::Args;
use webpilot::ipc;
use webpilot::protocol::{Command, ResponseData};
use webpilot::types::serialize_dom;

use crate::output::OutputMode;

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

pub async fn run(args: CaptureArgs, output_mode: OutputMode) -> Result<()> {
    // --annotate implies --dom --screenshot --bounds, conflicts with --fullpage
    let annotate = args.annotate;
    if annotate && args.full_page {
        anyhow::bail!(
            "--annotate and --fullpage cannot be combined. Annotations are viewport-only."
        );
    }
    let dom = args.dom || annotate || (!args.screenshot && !args.text && !args.accessibility);
    let screenshot = args.screenshot || annotate;
    let bounds = args.bounds || annotate;

    let request = serde_json::to_value(webpilot::protocol::Request {
        id: 1,
        command: Command::Capture {
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
    })?;

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
            Ok(path) => match output_mode {
                OutputMode::Human => eprintln!("Full-page screenshot: {}", path.display()),
                OutputMode::Json => println!(
                    "{}",
                    serde_json::json!({"screenshot_path": path.to_string_lossy()})
                ),
            },
            Err(e) => eprintln!("Stitch error: {e:#}"),
        }
        return Ok(());
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

            match output_mode {
                OutputMode::Human => {
                    if let Some(ref snapshot) = dom_snapshot {
                        if let Some(ref text) = snapshot.text_content {
                            println!("{text}");
                        }
                        if !snapshot.elements.is_empty() {
                            print!("{}", serialize_dom(snapshot));
                        }
                    }
                    if let Some(ref path) = ax_path {
                        eprintln!("Accessibility tree: {path}");
                    }
                    if let Some(ref path) = screenshot_path {
                        eprintln!("Screenshot: {path}");
                    }
                    if let Some(ref err) = screenshot_error {
                        eprintln!("{}", crate::output::format_error(err, None));
                    }
                }
                OutputMode::Json => {
                    // Single unified JSON object — never output multiple objects
                    let mut out = serde_json::Map::new();
                    if let Some(ref snapshot) = dom_snapshot
                        && (!snapshot.elements.is_empty() || snapshot.text_content.is_some())
                    {
                        // Exclude accessibility_tree from JSON (saved to file separately)
                        let mut snap = snapshot.clone();
                        snap.accessibility_tree = None;
                        out = serde_json::to_value(&snap)?
                            .as_object()
                            .cloned()
                            .unwrap_or_default();
                    }
                    if let Some(ref path) = ax_path {
                        out.insert("accessibility_path".into(), serde_json::json!(path));
                    }
                    if let Some(ref path) = screenshot_path {
                        out.insert("screenshot_path".into(), serde_json::json!(path));
                    }
                    if let Some(ref err) = screenshot_error {
                        out.insert("screenshot_error".into(), serde_json::json!(err));
                    }
                    println!("{}", serde_json::Value::Object(out));
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

    Ok(())
}
