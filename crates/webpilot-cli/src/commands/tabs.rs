use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use webpilot::ipc;
use webpilot::protocol::{Command, ResponseData};

use crate::output::OutputMode;

#[derive(Args)]
pub struct TabsArgs {
    #[command(subcommand)]
    pub command: Option<TabsCommand>,
}

#[derive(Subcommand)]
pub enum TabsCommand {
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

pub async fn run(args: TabsArgs, output_mode: OutputMode) -> Result<()> {
    match args.command {
        None => list_tabs(output_mode).await,
        Some(TabsCommand::Switch { tab_id }) => switch_tab(tab_id, output_mode).await,
        Some(TabsCommand::New { url }) => new_tab(&url, output_mode).await,
        Some(TabsCommand::Close { tab_id }) => close_tab(tab_id, output_mode).await,
        Some(TabsCommand::Find { url }) => find_tab(&url, output_mode).await,
    }
}

async fn list_tabs(output_mode: OutputMode) -> Result<()> {
    let request = serde_json::to_value(webpilot::protocol::Request::new(1, Command::ListTabs))?;

    let response = ipc::send_request(&request)
        .await
        .context("Failed to connect")?;
    let resp: webpilot::protocol::Response = serde_json::from_value(response)?;

    match resp.result {
        ResponseData::Tabs { tabs } => match output_mode {
            OutputMode::Human => {
                for t in &tabs {
                    let marker = if t.active { "*" } else { " " };
                    println!("{marker} [{}] {} — {}", t.id, t.title, t.url);
                }
            }
            OutputMode::Json => {
                println!("{}", serde_json::to_string_pretty(&tabs)?);
            }
        },
        _ => anyhow::bail!("Unexpected response"),
    }
    Ok(())
}

async fn switch_tab(tab_id: String, output_mode: OutputMode) -> Result<()> {
    let request = serde_json::to_value(webpilot::protocol::Request::new(
        1,
        Command::SwitchTab {
            tab_id: tab_id.clone(),
        },
    ))?;
    let response = ipc::send_request(&request)
        .await
        .context("Failed to connect")?;
    let resp: webpilot::protocol::Response = serde_json::from_value(response)?;
    match resp.result {
        ResponseData::Action { success, error, .. } => match output_mode {
            OutputMode::Human => {
                if success {
                    eprintln!("Switched to tab {tab_id}");
                } else if let Some(ref err) = error {
                    eprintln!("{}", crate::output::format_error(err));
                } else {
                    eprintln!("Unknown error");
                }
            }
            OutputMode::Json => println!("{}", serde_json::json!({"success": success})),
        },
        _ => anyhow::bail!("Unexpected response"),
    }
    Ok(())
}

async fn new_tab(url: &str, output_mode: OutputMode) -> Result<()> {
    let request = serde_json::to_value(webpilot::protocol::Request::new(
        1,
        Command::NewTab {
            url: url.to_string(),
        },
    ))?;
    let response = ipc::send_request(&request)
        .await
        .context("Failed to connect")?;
    let resp: webpilot::protocol::Response = serde_json::from_value(response)?;
    match resp.result {
        ResponseData::Action { success, .. } => match output_mode {
            OutputMode::Human => {
                if success {
                    eprintln!("New tab opened: {url}");
                }
            }
            OutputMode::Json => println!("{}", serde_json::json!({"success": success, "url": url})),
        },
        _ => anyhow::bail!("Unexpected response"),
    }
    Ok(())
}

async fn close_tab(tab_id: String, output_mode: OutputMode) -> Result<()> {
    let request = serde_json::to_value(webpilot::protocol::Request::new(
        1,
        Command::CloseTab {
            tab_id: tab_id.clone(),
        },
    ))?;
    let response = ipc::send_request(&request)
        .await
        .context("Failed to connect")?;
    let resp: webpilot::protocol::Response = serde_json::from_value(response)?;
    match resp.result {
        ResponseData::Action { success, .. } => match output_mode {
            OutputMode::Human => {
                if success {
                    eprintln!("Tab {tab_id} closed");
                }
            }
            OutputMode::Json => println!("{}", serde_json::json!({"success": success})),
        },
        _ => anyhow::bail!("Unexpected response"),
    }
    Ok(())
}

async fn find_tab(url_pattern: &str, output_mode: OutputMode) -> Result<()> {
    let request = serde_json::to_value(webpilot::protocol::Request::new(1, Command::ListTabs))?;
    let response = ipc::send_request(&request)
        .await
        .context("Failed to connect")?;
    let resp: webpilot::protocol::Response = serde_json::from_value(response)?;

    match resp.result {
        ResponseData::Tabs { tabs } => {
            let pattern = url_pattern.replace('*', "");
            if let Some(tab) = tabs.iter().find(|t| t.url.contains(&pattern)) {
                switch_tab(tab.id.clone(), output_mode).await?;
            } else {
                match output_mode {
                    OutputMode::Human => eprintln!("No tab matching '{url_pattern}'"),
                    OutputMode::Json => println!(
                        "{}",
                        serde_json::json!({"success": false, "error": "No matching tab"})
                    ),
                }
                anyhow::bail!("No tab matching '{url_pattern}'");
            }
        }
        _ => anyhow::bail!("Unexpected response"),
    }
    Ok(())
}
