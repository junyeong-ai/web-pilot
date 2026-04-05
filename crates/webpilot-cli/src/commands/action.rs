use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use webpilot::ipc;
use webpilot::protocol::{BrowserAction, Command, ResponseData};

use crate::output::CommandOutput;

#[derive(Args)]
pub struct ActionArgs {
    #[command(subcommand)]
    pub command: ActionCommand,

    /// Auto-capture DOM after action (returns DOM in response)
    #[arg(long, global = true)]
    pub capture: bool,
}

#[derive(Subcommand)]
pub enum ActionCommand {
    /// Click an element by index
    Click { index: u32 },
    /// Type text into an element
    Type {
        index: u32,
        text: String,
        #[arg(long)]
        clear: bool,
    },
    /// Press a key
    #[command(name = "keypress")]
    KeyPress {
        key: String,
        #[arg(long)]
        ctrl: bool,
        #[arg(long)]
        shift: bool,
        #[arg(long)]
        alt: bool,
        #[arg(long)]
        meta: bool,
    },
    /// Navigate to a URL
    Navigate { url: String },
    /// Scroll the page
    Scroll {
        direction: String,
        #[arg(default_value = "600")]
        amount: u32,
    },
    /// Go back
    Back,
    /// Go forward
    Forward,
    /// Reload
    Reload,
    /// Hover over an element
    Hover { index: u32 },
    /// Focus an element
    Focus { index: u32 },
    /// Select an option
    Select { index: u32, value: String },
    /// Upload a file to a file input (uses CDP)
    Upload { index: u32, path: String },
    /// Scroll to bring an element into view
    ScrollTo { index: u32 },
    /// Drag element to another element position
    Drag {
        source: u32,
        target: u32,
        #[arg(long, default_value = "5")]
        steps: u32,
    },
}

impl ActionCommand {
    pub fn to_browser_action(&self) -> Result<BrowserAction> {
        Ok(match self {
            Self::Click { index } => BrowserAction::Click { index: *index },
            Self::Type { index, text, clear } => BrowserAction::Type {
                index: *index,
                text: text.clone(),
                clear: *clear,
            },
            Self::KeyPress {
                key,
                ctrl,
                shift,
                alt,
                meta,
            } => {
                let mut mods = Vec::new();
                if *ctrl {
                    mods.push("ctrl".into());
                }
                if *shift {
                    mods.push("shift".into());
                }
                if *alt {
                    mods.push("alt".into());
                }
                if *meta {
                    mods.push("meta".into());
                }
                BrowserAction::KeyPress {
                    key: key.clone(),
                    modifiers: mods,
                }
            }
            Self::Navigate { url } => BrowserAction::Navigate { url: url.clone() },
            Self::Scroll { direction, amount } => match direction.as_str() {
                "up" => BrowserAction::ScrollUp { amount: *amount },
                "down" => BrowserAction::ScrollDown { amount: *amount },
                other => anyhow::bail!("Invalid direction '{other}'. Use 'up' or 'down'."),
            },
            Self::Back => BrowserAction::Back,
            Self::Forward => BrowserAction::Forward,
            Self::Reload => BrowserAction::Reload,
            Self::Hover { index } => BrowserAction::Hover { index: *index },
            Self::Focus { index } => BrowserAction::Focus { index: *index },
            Self::Select { index, value } => BrowserAction::Select {
                index: *index,
                value: value.clone(),
            },
            Self::ScrollTo { index } => BrowserAction::ScrollToElement { index: *index },
            Self::Upload { index, path } => BrowserAction::Upload {
                index: *index,
                path: path.clone(),
            },
            Self::Drag {
                source,
                target,
                steps,
            } => BrowserAction::Drag {
                source: *source,
                target: *target,
                steps: *steps,
            },
        })
    }
}

pub async fn run(args: ActionArgs) -> Result<CommandOutput> {
    let request = serde_json::to_value(webpilot::protocol::Request::new(
        1,
        Command::Action {
            action: args.command.to_browser_action()?,
            capture: args.capture,
        },
    ))?;

    let response = ipc::send_request(&request)
        .await
        .context("Failed to connect to WebPilot host")?;

    let resp: webpilot::protocol::Response =
        serde_json::from_value(response).context("Invalid response")?;

    match resp.result {
        ResponseData::Action {
            success,
            error,
            dom,
            url_changed,
            new_tab,
            ..
        } => {
            if !success {
                if let Some(ref err) = error {
                    anyhow::bail!("{}", crate::output::format_error(err));
                } else {
                    anyhow::bail!("Unknown error");
                }
            }

            // If DOM was captured (--capture flag), return Dom variant
            if let Some(snapshot) = dom {
                let mut extra = serde_json::Map::new();
                if let Some(ref url) = url_changed {
                    extra.insert("url_changed".into(), serde_json::json!(url));
                }
                if let Some(ref tab) = new_tab {
                    extra.insert(
                        "new_tab".into(),
                        serde_json::to_value(tab).unwrap_or_default(),
                    );
                }
                return Ok(CommandOutput::Dom { snapshot, extra });
            }

            // Simple OK with optional extra info
            let mut msg = "OK".to_string();
            if let Some(ref url) = url_changed {
                msg.push_str(&format!("\nURL changed: {url}"));
            }
            if let Some(ref tab) = new_tab {
                msg.push_str(&format!(
                    "\nNew tab opened: {} (switched automatically)",
                    tab.url
                ));
            }

            if url_changed.is_some() || new_tab.is_some() {
                let mut json = serde_json::json!({"success": true});
                if let Some(ref url) = url_changed {
                    json["url_changed"] = serde_json::json!(url);
                }
                if let Some(ref tab) = new_tab {
                    json["new_tab"] = serde_json::to_value(tab).unwrap_or_default();
                }
                Ok(CommandOutput::Data { json, human: msg })
            } else {
                Ok(CommandOutput::Ok(msg))
            }
        }
        ResponseData::Error { message, .. } => {
            anyhow::bail!("Extension error: {message}");
        }
        _ => anyhow::bail!("Unexpected response type"),
    }
}
