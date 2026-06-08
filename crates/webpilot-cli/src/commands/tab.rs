use anyhow::Result;
use clap::{Args, Subcommand};
use webpilot::protocol::{Command, ResponseData};

use webpilot::types::line_safe;

use crate::output::CommandOutput;
use crate::transport::{Transport, lift_error};

#[derive(Args)]
pub struct TabArgs {
    #[command(subcommand)]
    pub command: Option<TabCommand>,
}

#[derive(Subcommand)]
pub enum TabCommand {
    Switch {
        tab_id: String,
    },
    New {
        url: String,
    },
    Close {
        tab_id: String,
    },
    Find {
        #[arg(long)]
        url: String,
    },
}

pub async fn run<T: Transport>(transport: &mut T, args: TabArgs) -> Result<CommandOutput> {
    match args.command {
        None => list_tabs(transport).await,
        Some(TabCommand::Switch { tab_id }) => switch_tab(transport, tab_id).await,
        Some(TabCommand::New { url }) => new_tab(transport, &url).await,
        Some(TabCommand::Close { tab_id }) => close_tab(transport, tab_id).await,
        Some(TabCommand::Find { url }) => find_tab(transport, &url).await,
    }
}

async fn list_tabs<T: Transport>(transport: &mut T) -> Result<CommandOutput> {
    let result = transport.send(Command::TabList).await?;
    match result {
        ResponseData::Tabs { tabs } => {
            let human_lines: Vec<String> = tabs
                .iter()
                .map(|t| {
                    let marker = if t.active { "*" } else { " " };
                    format!(
                        "{marker} [{}] {} — {}",
                        t.id,
                        line_safe(&t.title),
                        line_safe(&t.url)
                    )
                })
                .collect();
            Ok(CommandOutput::List {
                items: serde_json::to_value(&tabs)?,
                human_lines,
                summary: String::new(),
            })
        }
        ResponseData::Error { error } => Err(error.into()),
        _ => anyhow::bail!("Unexpected response shape"),
    }
}

async fn switch_tab<T: Transport>(transport: &mut T, tab_id: String) -> Result<CommandOutput> {
    let result = transport
        .send(Command::TabSwitch {
            tab_id: tab_id.clone(),
        })
        .await?;
    expect_action(result)?;
    Ok(CommandOutput::Ok(format!("Switched to tab {tab_id}")))
}

async fn new_tab<T: Transport>(transport: &mut T, url: &str) -> Result<CommandOutput> {
    let result = transport
        .send(Command::TabNew {
            url: url.to_string(),
        })
        .await?;
    expect_action(result)?;
    Ok(CommandOutput::Data {
        json: serde_json::json!({"success": true, "url": url}),
        human: format!("New tab opened: {}", line_safe(url)),
    })
}

async fn close_tab<T: Transport>(transport: &mut T, tab_id: String) -> Result<CommandOutput> {
    let result = transport
        .send(Command::TabClose {
            tab_id: tab_id.clone(),
        })
        .await?;
    expect_action(result)?;
    Ok(CommandOutput::Ok(format!("Tab {tab_id} closed")))
}

async fn find_tab<T: Transport>(transport: &mut T, pattern: &str) -> Result<CommandOutput> {
    let result = transport.send(Command::TabList).await?;
    match result {
        ResponseData::Tabs { tabs } => {
            let needle = pattern.replace('*', "");
            if let Some(tab) = tabs.iter().find(|t| t.url.contains(&needle)) {
                switch_tab(transport, tab.id.clone()).await
            } else {
                Err(webpilot::WebPilotError::TabNotFound {
                    tab_id: pattern.to_string(),
                }
                .into())
            }
        }
        ResponseData::Error { error } => Err(error.into()),
        _ => anyhow::bail!("Unexpected response shape"),
    }
}

fn expect_action(data: ResponseData) -> Result<()> {
    match data {
        ResponseData::Action { success, error, .. } => lift_error(success, error, ()),
        ResponseData::Error { error } => Err(error.into()),
        _ => anyhow::bail!("Unexpected response shape"),
    }
}
