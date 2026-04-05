use crate::cdp::CdpClient;
use crate::commands;
use crate::output::OutputMode;
use anyhow::Result;

use super::call_bridge;

pub(crate) async fn run(
    cdp: &CdpClient,
    args: commands::frames::FramesArgs,
    output_mode: OutputMode,
) -> Result<()> {
    match args.command {
        None => list_frames(cdp, output_mode).await,
        Some(commands::frames::FramesCommand::Switch { name }) => {
            switch_frame(cdp, Some(&name), None, None, false, output_mode).await
        }
        Some(commands::frames::FramesCommand::Url { pattern }) => {
            switch_frame(cdp, None, Some(&pattern), None, false, output_mode).await
        }
        Some(commands::frames::FramesCommand::Find { predicate }) => {
            switch_frame(cdp, None, None, Some(&predicate), false, output_mode).await
        }
        Some(commands::frames::FramesCommand::Main) => {
            switch_frame(cdp, None, None, None, true, output_mode).await
        }
    }
}

async fn list_frames(cdp: &CdpClient, output_mode: OutputMode) -> Result<()> {
    let result = cdp.send("Page.getFrameTree", None).await?;

    // Flatten frame tree into a list
    fn collect_frames(node: &serde_json::Value, out: &mut Vec<serde_json::Value>) {
        if let Some(frame) = node.get("frame") {
            out.push(frame.clone());
        }
        if let Some(children) = node.get("childFrames").and_then(|v| v.as_array()) {
            for child in children {
                collect_frames(child, out);
            }
        }
    }

    let mut frames = Vec::new();
    if let Some(tree) = result.get("frameTree") {
        collect_frames(tree, &mut frames);
    }

    match output_mode {
        OutputMode::Human => {
            for f in &frames {
                let id = f.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                let url = f.get("url").and_then(|v| v.as_str()).unwrap_or("");
                let name = f.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let url_short = if url.len() > 60 { &url[..60] } else { url };
                if name.is_empty() {
                    println!("  [{id}] {url_short}");
                } else {
                    println!("  [{id}] \"{name}\" {url_short}");
                }
            }
            eprintln!("({} frames)", frames.len());
        }
        OutputMode::Json => println!("{}", serde_json::json!({"frames": frames})),
    }
    Ok(())
}

async fn switch_frame(
    cdp: &CdpClient,
    name: Option<&str>,
    url_pattern: Option<&str>,
    predicate: Option<&str>,
    main: bool,
    output_mode: OutputMode,
) -> Result<()> {
    let msg = serde_json::json!({
        "type": "switchFrame",
        "name": name,
        "url_pattern": url_pattern,
        "predicate": predicate,
        "main": main,
    });

    let result = call_bridge(cdp, &msg.to_string()).await?;
    let success = result
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    match output_mode {
        OutputMode::Human => {
            if success {
                let frame_id = result.get("frame_id").and_then(|v| v.as_i64()).unwrap_or(0);
                let url = result.get("url").and_then(|v| v.as_str()).unwrap_or("");
                eprintln!("Switched to frame {frame_id} ({url})");
            } else {
                let err = result
                    .pointer("/error/message")
                    .or(result.get("error"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("Frame switch failed");
                eprintln!("{err}");
            }
        }
        OutputMode::Json => println!("{}", result),
    }

    if !success {
        anyhow::bail!("Frame switch failed");
    }
    Ok(())
}
