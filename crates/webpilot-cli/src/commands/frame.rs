use anyhow::Result;
use clap::{Args, Subcommand};
use webpilot::protocol::{Command, FrameSelector, ResponseData};

use webpilot::types::line_safe_clip;

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
        Some(cmd) => {
            // A `frame url` pattern that is empty or only wildcards matches every
            // frame — reject it rather than silently switch into the first one.
            if let FrameCommand::Url { pattern } = &cmd
                && webpilot::url_glob::is_blank(pattern)
            {
                return Err(webpilot::WebPilotError::InvalidArgument {
                    detail: "frame url pattern must contain a non-wildcard character".into(),
                }
                .into());
            }
            switch_frame(transport, cmd.into_selector()).await
        }
    }
}

async fn list_frames<T: Transport>(transport: &mut T) -> Result<CommandOutput> {
    let result = transport.send(Command::FrameList).await?;
    match result {
        ResponseData::Frames {
            frames,
            active_frame_id,
        } => {
            let active_id = active_frame_id.as_deref();
            let human_lines: Vec<String> = frames
                .iter()
                .map(|f| {
                    let is_active = match (active_id, f.is_main) {
                        (Some(id), _) => id == f.frame_id,
                        (None, true) => true,
                        (None, false) => false,
                    };
                    let marker = if is_active { "*" } else { " " };
                    let main = if f.is_main { " [main]" } else { "" };
                    let id_short: String = f.frame_id.chars().take(8).collect();
                    // A generous 200-char cap, not the old 60: a frame URL is a
                    // single-line token whose point here is to identify which
                    // frame to `frame url <pattern>`, and clipping iframes that
                    // share a long common prefix to 60 made distinct frames look
                    // identical. 200 keeps real URLs whole while still bounding a
                    // multi-megabyte `data:` iframe `src` from flooding the
                    // terminal/MCP text; the full URL is always in the JSON.
                    let url_full = line_safe_clip(&f.url, 200);
                    // Show the name when the frame has one: it is the argument to
                    // `frame switch <name>`, so surfacing it is how that addressing
                    // mode becomes discoverable rather than guess-only.
                    let name = match f.name.as_deref() {
                        Some(n) if !n.is_empty() => {
                            format!(" name={}", line_safe_clip(n, 200))
                        }
                        _ => String::new(),
                    };
                    format!("{marker} [{id_short}] {url_full}{name}{main}")
                })
                .collect();
            let summary = match &active_frame_id {
                Some(id) => {
                    let id_short: String = id.chars().take(8).collect();
                    format!("({} frames, active={id_short})", frames.len())
                }
                None => format!("({} frames, active=main)", frames.len()),
            };
            Ok(CommandOutput::List {
                items: serde_json::json!({
                    "frames": frames,
                    "active_frame_id": active_frame_id,
                }),
                human_lines,
                summary,
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
            name,
            url,
            error,
        } => {
            lift_error(success, error, ())?;
            let target = frame_id.as_deref().unwrap_or("main");
            // Surface the name so the agent learns the handle it can re-`switch` by
            // (and so the JSON carries the same field both modes now populate).
            let name_suffix = match name.as_deref() {
                Some(n) if !n.is_empty() => format!(" name={}", line_safe_clip(n, 200)),
                _ => String::new(),
            };
            Ok(CommandOutput::Data {
                json: serde_json::json!({"success": true, "frame_id": frame_id, "name": name, "url": url}),
                human: format!(
                    "Switched to frame {target} ({}){name_suffix}",
                    line_safe_clip(&url.unwrap_or_default(), 200)
                ),
            })
        }
        ResponseData::Error { error } => Err(error.into()),
        _ => anyhow::bail!("Unexpected response shape"),
    }
}
