use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use webpilot::ipc;
use webpilot::protocol::{Command, ResponseData};

use crate::output::OutputMode;

#[derive(Args)]
pub struct FramesArgs {
    #[command(subcommand)]
    pub action: Option<FrameAction>,
}

#[derive(Subcommand)]
pub enum FrameAction {
    /// Switch to a frame by name
    Switch { name: String },
    /// Switch to a frame by URL pattern
    Url { pattern: String },
    /// Switch to a frame matching a JS predicate
    Find { predicate: String },
    /// Switch back to main frame
    Main,
}

pub async fn run(args: FramesArgs, output_mode: OutputMode) -> Result<()> {
    match args.action {
        None => list_frames(output_mode).await,
        Some(FrameAction::Switch { name }) => {
            switch_frame(Some(name), None, None, false, output_mode).await
        }
        Some(FrameAction::Url { pattern }) => {
            switch_frame(None, Some(pattern), None, false, output_mode).await
        }
        Some(FrameAction::Find { predicate }) => {
            switch_frame(None, None, Some(predicate), false, output_mode).await
        }
        Some(FrameAction::Main) => switch_frame(None, None, None, true, output_mode).await,
    }
}

async fn list_frames(output_mode: OutputMode) -> Result<()> {
    let request = serde_json::to_value(webpilot::protocol::Request {
        id: 1,
        command: Command::ListFrames,
    })?;

    let response = ipc::send_request(&request)
        .await
        .context("Host not running")?;
    let resp: webpilot::protocol::Response = serde_json::from_value(response)?;

    match resp.result {
        ResponseData::Frames {
            frames,
            active_frame_id,
        } => match output_mode {
            OutputMode::Human => {
                for f in &frames {
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
                    println!("{marker} [{:>3}] {}{}", f.frame_id, url_short, main);
                }
                eprintln!("({} frames, active={})", frames.len(), active_frame_id);
            }
            OutputMode::Json => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "frames": frames,
                        "active_frame_id": active_frame_id,
                    }))?
                );
            }
        },
        _ => anyhow::bail!("Unexpected response"),
    }
    Ok(())
}

async fn switch_frame(
    name: Option<String>,
    url_pattern: Option<String>,
    predicate: Option<String>,
    main: bool,
    output_mode: OutputMode,
) -> Result<()> {
    let request = serde_json::to_value(webpilot::protocol::Request {
        id: 1,
        command: Command::SwitchFrame {
            name,
            url_pattern,
            predicate,
            main,
        },
    })?;

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
            let err_str = error.unwrap_or_default();
            match output_mode {
                OutputMode::Human => {
                    if success {
                        eprintln!(
                            "Switched to frame {} ({})",
                            frame_id,
                            url.unwrap_or_default()
                        );
                    } else {
                        eprintln!("{}", crate::output::format_error(&err_str, None));
                    }
                }
                OutputMode::Json => {
                    println!(
                        "{}",
                        serde_json::json!({"success": success, "frame_id": frame_id, "url": url, "error": err_str})
                    );
                }
            }
            if !success {
                anyhow::bail!("{}", crate::output::format_error(&err_str, None));
            }
        }
        ResponseData::Error { message, .. } => anyhow::bail!("{message}"),
        _ => anyhow::bail!("Unexpected response"),
    }
    Ok(())
}
