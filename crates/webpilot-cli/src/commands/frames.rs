use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use webpilot::ipc;
use webpilot::protocol::{Command, ResponseData};

use crate::output::CommandOutput;

#[derive(Args)]
pub struct FramesArgs {
    #[command(subcommand)]
    pub command: Option<FrameCommand>,
}

#[derive(Subcommand)]
pub enum FrameCommand {
    /// Switch to a frame by name
    Switch { name: String },
    /// Switch to a frame by URL pattern
    Url { pattern: String },
    /// Switch to a frame matching a JS predicate
    Find { predicate: String },
    /// Switch back to main frame
    Main,
}

pub async fn run(args: FramesArgs) -> Result<CommandOutput> {
    match args.command {
        None => list_frames().await,
        Some(FrameCommand::Switch { name }) => {
            switch_frame(Some(name), None, None, false).await
        }
        Some(FrameCommand::Url { pattern }) => {
            switch_frame(None, Some(pattern), None, false).await
        }
        Some(FrameCommand::Find { predicate }) => {
            switch_frame(None, None, Some(predicate), false).await
        }
        Some(FrameCommand::Main) => switch_frame(None, None, None, true).await,
    }
}

async fn list_frames() -> Result<CommandOutput> {
    let request = serde_json::to_value(webpilot::protocol::Request::new(1, Command::FrameList))?;

    let response = ipc::send_request(&request)
        .await
        .context("Host not running")?;
    let resp: webpilot::protocol::Response = serde_json::from_value(response)?;

    match resp.result {
        ResponseData::Frames {
            frames,
            active_frame_id,
        } => {
            let human_lines: Vec<String> = frames
                .iter()
                .map(|f| {
                    let marker = if f.frame_id == active_frame_id {
                        "*"
                    } else {
                        " "
                    };
                    let main = if f.is_main { " [main]" } else { "" };
                    let url_short = if f.url.len() > 60 {
                        &f.url[..60]
                    } else {
                        &f.url
                    };
                    format!("{marker} [{:>3}] {}{}", f.frame_id, url_short, main)
                })
                .collect();
            let summary = format!("({} frames, active={})", frames.len(), active_frame_id);
            Ok(CommandOutput::List {
                items: serde_json::json!({
                    "frames": frames,
                    "active_frame_id": active_frame_id,
                }),
                human_lines,
                summary,
            })
        }
        _ => anyhow::bail!("Unexpected response"),
    }
}

async fn switch_frame(
    name: Option<String>,
    url_pattern: Option<String>,
    predicate: Option<String>,
    main: bool,
) -> Result<CommandOutput> {
    let request = serde_json::to_value(webpilot::protocol::Request::new(
        1,
        Command::FrameSwitch {
            name,
            url_pattern,
            predicate,
            main,
        },
    ))?;

    let response = ipc::send_request(&request)
        .await
        .context("Host not running")?;
    let resp: webpilot::protocol::Response = serde_json::from_value(response)?;

    match resp.result {
        ResponseData::FrameSwitched {
            success,
            frame_id,
            url,
            error,
            ..
        } => {
            if !success {
                if let Some(ref err) = error {
                    anyhow::bail!("{}", crate::output::format_error(err));
                } else {
                    anyhow::bail!("Unknown error");
                }
            }
            Ok(CommandOutput::Data {
                json: serde_json::json!({"success": true, "frame_id": frame_id, "url": url}),
                human: format!(
                    "Switched to frame {} ({})",
                    frame_id,
                    url.unwrap_or_default()
                ),
            })
        }
        ResponseData::Error { message, .. } => anyhow::bail!("{message}"),
        _ => anyhow::bail!("Unexpected response"),
    }
}
