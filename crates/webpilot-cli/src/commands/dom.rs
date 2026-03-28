use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use webpilot::ipc;
use webpilot::protocol::{Command, ResponseData};

use crate::output::OutputMode;

#[derive(Args)]
pub struct DomArgs {
    #[command(subcommand)]
    pub action: DomAction,
}

#[derive(Subcommand)]
pub enum DomAction {
    /// Set element innerHTML
    #[command(name = "set-html")]
    SetHtml { selector: String, value: String },
    /// Set element textContent
    #[command(name = "set-text")]
    SetText { selector: String, value: String },
    /// Set element attribute
    #[command(name = "set-attr")]
    SetAttr {
        selector: String,
        attr: String,
        value: String,
    },
    /// Get element innerHTML
    #[command(name = "get-html")]
    GetHtml { selector: String },
    /// Get element textContent
    #[command(name = "get-text")]
    GetText { selector: String },
    /// Get element attribute value
    #[command(name = "get-attr")]
    GetAttr { selector: String, attr: String },
}

pub async fn run(args: DomArgs, output_mode: OutputMode) -> Result<()> {
    let request = match &args.action {
        DomAction::SetHtml { selector, value } => make_set(selector, "html", value, None),
        DomAction::SetText { selector, value } => make_set(selector, "text", value, None),
        DomAction::SetAttr {
            selector,
            attr,
            value,
        } => make_set(selector, "attr", value, Some(attr.clone())),
        DomAction::GetHtml { selector } => make_get(selector, "html", None),
        DomAction::GetText { selector } => make_get(selector, "text", None),
        DomAction::GetAttr { selector, attr } => make_get(selector, "attr", Some(attr.clone())),
    }?;

    let response = ipc::send_request(&request)
        .await
        .context("Host not running")?;
    let resp: webpilot::protocol::Response = serde_json::from_value(response)?;

    match resp.result {
        ResponseData::CommandResult {
            success,
            value,
            error,
        } => {
            if let Some(val) = value {
                match output_mode {
                    OutputMode::Human => println!("{val}"),
                    OutputMode::Json => println!("{}", serde_json::json!({"value": val})),
                }
            }
            if !success {
                anyhow::bail!("{}", error.unwrap_or_default());
            }
        }
        ResponseData::Error { message, .. } => anyhow::bail!("{message}"),
        _ => anyhow::bail!("Unexpected response"),
    }
    Ok(())
}

fn make_set(
    selector: &str,
    property: &str,
    value: &str,
    attr: Option<String>,
) -> Result<serde_json::Value> {
    Ok(serde_json::to_value(webpilot::protocol::Request {
        id: 1,
        command: Command::SetDom {
            selector: selector.to_string(),
            property: property.to_string(),
            value: value.to_string(),
            attr,
        },
    })?)
}

fn make_get(selector: &str, property: &str, attr: Option<String>) -> Result<serde_json::Value> {
    Ok(serde_json::to_value(webpilot::protocol::Request {
        id: 1,
        command: Command::GetDom {
            selector: selector.to_string(),
            property: property.to_string(),
            attr,
        },
    })?)
}
