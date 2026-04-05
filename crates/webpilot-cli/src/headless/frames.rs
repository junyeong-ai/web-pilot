use crate::cdp::CdpClient;
use crate::commands;
use crate::output::CommandOutput;
use anyhow::Result;

use super::{invoke_bridge, parse_bridge_response};

pub(crate) async fn run(
    cdp: &CdpClient,
    args: commands::frames::FramesArgs,
) -> Result<CommandOutput> {
    match args.command {
        None => list_frames(cdp).await,
        Some(commands::frames::FrameCommand::Switch { name }) => {
            switch_frame(cdp, Some(&name), None, None, false).await
        }
        Some(commands::frames::FrameCommand::Url { pattern }) => {
            switch_frame(cdp, None, Some(&pattern), None, false).await
        }
        Some(commands::frames::FrameCommand::Find { predicate }) => {
            switch_frame(cdp, None, None, Some(&predicate), false).await
        }
        Some(commands::frames::FrameCommand::Main) => {
            switch_frame(cdp, None, None, None, true).await
        }
    }
}

async fn list_frames(cdp: &CdpClient) -> Result<CommandOutput> {
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

    let human_lines: Vec<String> = frames
        .iter()
        .map(|f| {
            let id = f.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            let url = f.get("url").and_then(|v| v.as_str()).unwrap_or("");
            let name = f.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let url_short = if url.len() > 60 { &url[..60] } else { url };
            if name.is_empty() {
                format!("  [{id}] {url_short}")
            } else {
                format!("  [{id}] \"{name}\" {url_short}")
            }
        })
        .collect();
    let summary = format!("({} frames)", frames.len());

    Ok(CommandOutput::List {
        items: serde_json::json!({"frames": frames}),
        human_lines,
        summary,
    })
}

async fn switch_frame(
    cdp: &CdpClient,
    name: Option<&str>,
    url_pattern: Option<&str>,
    predicate: Option<&str>,
    main: bool,
) -> Result<CommandOutput> {
    let msg = serde_json::json!({
        "type": "switchFrame",
        "name": name,
        "url_pattern": url_pattern,
        "predicate": predicate,
        "main": main,
    });

    let raw = invoke_bridge(cdp, &msg.to_string()).await?;
    let resp = parse_bridge_response(raw)?;

    let frame_id = resp
        .data
        .get("frame_id")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let url = resp
        .data
        .get("url")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    Ok(CommandOutput::Ok(format!(
        "Switched to frame {frame_id} ({url})"
    )))
}
