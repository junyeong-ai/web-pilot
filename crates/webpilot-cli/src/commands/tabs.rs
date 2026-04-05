use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use webpilot::ipc;
use webpilot::protocol::{Command, ResponseData};

use crate::output::CommandOutput;

#[derive(Args)]
pub struct TabsArgs {
    #[command(subcommand)]
    pub command: Option<TabCommand>,
}

#[derive(Subcommand)]
pub enum TabCommand {
    /// Switch to a tab by ID
    Switch { tab_id: String },
    /// Open a new tab
    New { url: String },
    /// Close a tab
    Close { tab_id: String },
    /// Find and switch to a tab by URL pattern
    Find {
        #[arg(long)]
        url: String,
    },
}

pub async fn run(args: TabsArgs) -> Result<CommandOutput> {
    match args.command {
        None => list_tabs().await,
        Some(TabCommand::Switch { tab_id }) => switch_tab(tab_id).await,
        Some(TabCommand::New { url }) => new_tab(&url).await,
        Some(TabCommand::Close { tab_id }) => close_tab(tab_id).await,
        Some(TabCommand::Find { url }) => find_tab(&url).await,
    }
}

async fn list_tabs() -> Result<CommandOutput> {
    let request = serde_json::to_value(webpilot::protocol::Request::new(1, Command::TabList))?;

    let response = ipc::send_request(&request)
        .await
        .context("Failed to connect")?;
    let resp: webpilot::protocol::Response = serde_json::from_value(response)?;

    match resp.result {
        ResponseData::Tabs { tabs } => {
            let human_lines: Vec<String> = tabs
                .iter()
                .map(|t| {
                    let marker = if t.active { "*" } else { " " };
                    format!("{marker} [{}] {} — {}", t.id, t.title, t.url)
                })
                .collect();
            Ok(CommandOutput::List {
                items: serde_json::to_value(&tabs)?,
                human_lines,
                summary: String::new(),
            })
        }
        _ => anyhow::bail!("Unexpected response"),
    }
}

async fn switch_tab(tab_id: String) -> Result<CommandOutput> {
    let request = serde_json::to_value(webpilot::protocol::Request::new(
        1,
        Command::TabSwitch {
            tab_id: tab_id.clone(),
        },
    ))?;
    let response = ipc::send_request(&request)
        .await
        .context("Failed to connect")?;
    let resp: webpilot::protocol::Response = serde_json::from_value(response)?;
    match resp.result {
        ResponseData::Action { success, error, .. } => {
            if !success {
                if let Some(ref err) = error {
                    anyhow::bail!("{}", crate::output::format_error(err));
                } else {
                    anyhow::bail!("Unknown error");
                }
            }
            Ok(CommandOutput::Ok(format!("Switched to tab {tab_id}")))
        }
        _ => anyhow::bail!("Unexpected response"),
    }
}

async fn new_tab(url: &str) -> Result<CommandOutput> {
    let request = serde_json::to_value(webpilot::protocol::Request::new(
        1,
        Command::TabNew {
            url: url.to_string(),
        },
    ))?;
    let response = ipc::send_request(&request)
        .await
        .context("Failed to connect")?;
    let resp: webpilot::protocol::Response = serde_json::from_value(response)?;
    match resp.result {
        ResponseData::Action { success, .. } => {
            if !success {
                anyhow::bail!("Failed to open new tab");
            }
            Ok(CommandOutput::Data {
                json: serde_json::json!({"success": true, "url": url}),
                human: format!("New tab opened: {url}"),
            })
        }
        _ => anyhow::bail!("Unexpected response"),
    }
}

async fn close_tab(tab_id: String) -> Result<CommandOutput> {
    let request = serde_json::to_value(webpilot::protocol::Request::new(
        1,
        Command::TabClose {
            tab_id: tab_id.clone(),
        },
    ))?;
    let response = ipc::send_request(&request)
        .await
        .context("Failed to connect")?;
    let resp: webpilot::protocol::Response = serde_json::from_value(response)?;
    match resp.result {
        ResponseData::Action { success, .. } => {
            if !success {
                anyhow::bail!("Failed to close tab");
            }
            Ok(CommandOutput::Ok(format!("Tab {tab_id} closed")))
        }
        _ => anyhow::bail!("Unexpected response"),
    }
}

async fn find_tab(url_pattern: &str) -> Result<CommandOutput> {
    let request = serde_json::to_value(webpilot::protocol::Request::new(1, Command::TabList))?;
    let response = ipc::send_request(&request)
        .await
        .context("Failed to connect")?;
    let resp: webpilot::protocol::Response = serde_json::from_value(response)?;

    match resp.result {
        ResponseData::Tabs { tabs } => {
            let pattern = url_pattern.replace('*', "");
            if let Some(tab) = tabs.iter().find(|t| t.url.contains(&pattern)) {
                switch_tab(tab.id.clone()).await
            } else {
                anyhow::bail!("No tab matching '{url_pattern}'");
            }
        }
        _ => anyhow::bail!("Unexpected response"),
    }
}
