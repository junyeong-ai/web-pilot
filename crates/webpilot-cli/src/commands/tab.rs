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
    // Report the tab's real landed URL/title from the response — the transport
    // settles the new tab and resolves any redirect, so we render what it
    // actually opened, never a blind echo of the requested URL.
    let landed = match result {
        ResponseData::Action {
            success,
            error,
            new_tab,
            ..
        } => {
            lift_error(success, error, ())?;
            // `tab new` always settles and returns the opened tab; a success with
            // no `new_tab` is a protocol violation, not an occasion to echo the
            // requested URL — that would report the requested address as the landed
            // one, masking a redirect (or the missing tab) behind a plausible lie.
            new_tab
                .map(|t| t.url)
                .ok_or_else(|| anyhow::anyhow!("tab new reported success but returned no tab"))?
        }
        ResponseData::Error { error } => return Err(error.into()),
        _ => anyhow::bail!("Unexpected response shape"),
    };
    Ok(CommandOutput::Data {
        json: serde_json::json!({"success": true, "url": landed}),
        human: format!("New tab opened: {}", line_safe(&landed)),
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
    // An empty or all-`*` pattern matches every tab — reject it rather than
    // silently switch to the first one. Same `*`-glob as `frame url`, shared so
    // the two URL selectors can't drift.
    if webpilot::url_glob::is_blank(pattern) {
        return Err(webpilot::WebPilotError::InvalidArgument {
            detail: "tab find --url pattern must contain a non-wildcard character".into(),
        }
        .into());
    }
    let result = transport.send(Command::TabList).await?;
    match result {
        ResponseData::Tabs { tabs } => {
            // A pattern matching MORE than one tab is ambiguous: switching to
            // whichever listed first would silently retarget the agent — the
            // same contract `frame url` enforces. Fail loud with the match list
            // so the agent refines the pattern or picks an id directly.
            let hits: Vec<_> = tabs
                .iter()
                .filter(|t| webpilot::url_glob::matches(pattern, &t.url))
                .collect();
            match hits.as_slice() {
                [] => Err(webpilot::WebPilotError::TabNotFound {
                    tab_id: pattern.to_string(),
                }
                .into()),
                [tab] => switch_tab(transport, tab.id.clone()).await,
                many => {
                    let urls = many
                        .iter()
                        .map(|t| webpilot::types::line_safe(&t.url).into_owned())
                        .collect::<Vec<_>>()
                        .join(", ");
                    Err(webpilot::WebPilotError::InvalidArgument {
                        detail: format!(
                            "{} tabs match \"{pattern}\" — refine it or use `tab switch <id>` to pick one: {urls}",
                            many.len()
                        ),
                    }
                    .into())
                }
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
