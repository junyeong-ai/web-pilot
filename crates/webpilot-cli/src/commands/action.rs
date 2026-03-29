use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use webpilot::ipc;
use webpilot::protocol::{BrowserAction, Command, ResponseData};

use crate::output::OutputMode;

#[derive(Args)]
pub struct ActionArgs {
    #[command(subcommand)]
    pub action: ActionCommand,

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
            Self::Upload { index, path } => BrowserAction::Upload {
                index: *index,
                path: path.clone(),
            },
        })
    }
}

pub async fn run(args: ActionArgs, output_mode: OutputMode) -> Result<()> {
    let request = serde_json::to_value(webpilot::protocol::Request {
        id: 1,
        command: Command::Action {
            action: args.action.to_browser_action()?,
            capture: args.capture,
        },
    })?;

    let response = ipc::send_request(&request)
        .await
        .context("Failed to connect to WebPilot host")?;

    let resp: webpilot::protocol::Response =
        serde_json::from_value(response).context("Invalid response")?;

    match resp.result {
        ResponseData::Action {
            success,
            error,
            code,
            dom,
            url_changed,
            new_tab,
            ..
        } => {
            if let Some(ref snapshot) = dom {
                match output_mode {
                    OutputMode::Human => print!("{}", webpilot::types::serialize_dom(snapshot)),
                    OutputMode::Json => println!(
                        "{}",
                        serde_json::to_string_pretty(snapshot).unwrap_or_default()
                    ),
                }
            }
            match output_mode {
                OutputMode::Human => {
                    if success {
                        eprintln!("OK");
                        if let Some(ref url) = url_changed {
                            eprintln!("URL changed: {url}");
                        }
                        if let Some(ref tab) = new_tab {
                            eprintln!(
                                "New tab opened: {} (switched automatically)",
                                tab.get("url").and_then(|u| u.as_str()).unwrap_or("")
                            );
                        }
                    } else {
                        eprintln!(
                            "{}",
                            crate::output::format_error(
                                error.as_deref().unwrap_or("unknown"),
                                code.as_deref(),
                            )
                        );
                    }
                }
                OutputMode::Json => {
                    if dom.is_none() {
                        let mut out = serde_json::json!({"success": success, "error": error});
                        if let Some(ref url) = url_changed {
                            out["url_changed"] = serde_json::json!(url);
                        }
                        if let Some(ref tab) = new_tab {
                            out["new_tab"] = tab.clone();
                        }
                        println!("{}", out);
                    }
                }
            }
            if !success {
                anyhow::bail!(
                    "{}",
                    crate::output::format_error(
                        error.as_deref().unwrap_or("unknown"),
                        code.as_deref(),
                    )
                );
            }
        }
        ResponseData::Error { message, .. } => {
            anyhow::bail!("Extension error: {message}");
        }
        _ => anyhow::bail!("Unexpected response type"),
    }

    Ok(())
}
