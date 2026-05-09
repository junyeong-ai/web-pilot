use anyhow::Result;
use clap::{Args, Subcommand};
use webpilot::protocol::{Command, FrameSelector, ResponseData};

use crate::output::CommandOutput;
use crate::transport::{Transport, lift_error};

#[derive(Args)]
pub struct FrameArgs {
    #[command(subcommand)]
    pub command: Option<FrameCommand>,
}

#[derive(Subcommand)]
pub enum FrameCommand {
    /// Switch to a frame by name.
    Switch { name: String },
    /// Switch to a frame by URL pattern.
    Url { pattern: String },
    /// Switch to a frame matching a JS predicate.
    Find { predicate: String },
    /// Switch back to main frame.
    Main,
}

impl FrameCommand {
    pub(crate) fn into_selector(self) -> FrameSelector {
        match self {
            Self::Switch { name } => FrameSelector::Name { value: name },
            Self::Url { pattern } => FrameSelector::Url { pattern },
            Self::Find { predicate } => FrameSelector::Predicate { js: predicate },
            Self::Main => FrameSelector::Main,
        }
    }
}

pub async fn run<T: Transport>(transport: &mut T, args: FrameArgs) -> Result<CommandOutput> {
    match args.command {
        None => list_frames(transport).await,
        Some(cmd) => switch_frame(transport, cmd.into_selector()).await,
    }
}

async fn list_frames<T: Transport>(transport: &mut T) -> Result<CommandOutput> {
    let result = transport.send(Command::FrameList).await?;
    match result {
        ResponseData::Frames {
            frames,
            active_frame_id,
        } => {
            let human_lines: Vec<String> = frames
                .iter()
                .map(|f| {
                    let marker = if f.frame_id == active_frame_id { "*" } else { " " };
                    let main = if f.is_main { " [main]" } else { "" };
                    let url_short: String = f.url.chars().take(60).collect();
                    format!("{marker} [{:>3}] {url_short}{main}", f.frame_id)
                })
                .collect();
            Ok(CommandOutput::List {
                items: serde_json::json!({
                    "frames": frames,
                    "active_frame_id": active_frame_id,
                }),
                human_lines,
                summary: format!("({} frames, active={})", frames.len(), active_frame_id),
            })
        }
        ResponseData::Error { error } => Err(error.into()),
        _ => anyhow::bail!("Unexpected response shape"),
    }
}

async fn switch_frame<T: Transport>(
    transport: &mut T,
    selector: FrameSelector,
) -> Result<CommandOutput> {
    let result = transport.send(Command::FrameSwitch { selector }).await?;
    match result {
        ResponseData::FrameSwitched {
            success,
            frame_id,
            url,
            error,
            ..
        } => {
            lift_error(success, error, ())?;
            Ok(CommandOutput::Data {
                json: serde_json::json!({"success": true, "frame_id": frame_id, "url": url}),
                human: format!(
                    "Switched to frame {frame_id} ({})",
                    url.unwrap_or_default()
                ),
            })
        }
        ResponseData::Error { error } => Err(error.into()),
        _ => anyhow::bail!("Unexpected response shape"),
    }
}
