use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use webpilot::ipc;
use webpilot::protocol::{Command, ResponseData};

use crate::output::OutputMode;

#[derive(Args)]
pub struct FramesArgs {
    #[command(subcommand)]
    pub command: Option<FramesCommand>,
}

#[derive(Subcommand)]
pub enum FramesCommand {
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
    match args.command {
        None => list_frames(output_mode).await,
        Some(FramesCommand::Switch { name }) => {
            switch_frame(Some(name), None, None, false, output_mode).await
        }
        Some(FramesCommand::Url { pattern }) => {
            switch_frame(None, Some(pattern), None, false, output_mode).await
        }
        Some(FramesCommand::Find { predicate }) => {
            switch_frame(None, None, Some(predicate), false, output_mode).await
        }
        Some(FramesCommand::Main) => switch_frame(None, None, None, true, output_mode).await,
    }
}

async fn list_frames(output_mode: OutputMode) -> Result<()> {
    let request = serde_json::to_value(webpilot::protocol::Request::new(1, Command::ListFrames))?;

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
    let request = serde_json::to_value(webpilot::protocol::Request::new(
        1,
        Command::SwitchFrame {
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
            match output_mode {
                OutputMode::Human => {
                    if success {
                        eprintln!(
                            "Switched to frame {} ({})",
                            frame_id,
                            url.unwrap_or_default()
                        );
                    } else if let Some(ref err) = error {
                        eprintln!("{}", crate::output::format_error(err));
                    } else {
                        eprintln!("Unknown error");
                    }
                }
                OutputMode::Json => {
                    println!(
                        "{}",
                        serde_json::json!({"success": success, "frame_id": frame_id, "url": url, "error": error})
                    );
                }
            }
            if !success {
                if let Some(ref err) = error {
                    anyhow::bail!("{}", crate::output::format_error(err));
                } else {
                    anyhow::bail!("Unknown error");
                }
            }
        }
        ResponseData::Error { message, .. } => anyhow::bail!("{message}"),
        _ => anyhow::bail!("Unexpected response"),
    }
    Ok(())
}
