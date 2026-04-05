use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use webpilot::ipc;
use webpilot::protocol::{Command, ResponseData};

use crate::output::CommandOutput;

#[derive(Args)]
pub struct DomArgs {
    #[command(subcommand)]
    pub command: DomCommand,
}

#[derive(Subcommand)]
pub enum DomCommand {
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

pub async fn run(args: DomArgs) -> Result<CommandOutput> {
    let request = match &args.command {
        DomCommand::SetHtml { selector, value } => make_set(selector, "html", value, None),
        DomCommand::SetText { selector, value } => make_set(selector, "text", value, None),
        DomCommand::SetAttr {
            selector,
            attr,
            value,
        } => make_set(selector, "attr", value, Some(attr.clone())),
        DomCommand::GetHtml { selector } => make_get(selector, "html", None),
        DomCommand::GetText { selector } => make_get(selector, "text", None),
        DomCommand::GetAttr { selector, attr } => make_get(selector, "attr", Some(attr.clone())),
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
                Ok(CommandOutput::Content {
                    stdout: val.clone(),
                    json: serde_json::json!({"success": true, "value": val}),
                })
            } else if success {
                Ok(CommandOutput::Ok("OK".into()))
            } else if let Some(ref err) = error {
                anyhow::bail!("{}", crate::output::format_error(err));
            } else {
                anyhow::bail!("Unknown error");
            }
        }
        ResponseData::Error { message, .. } => anyhow::bail!("{message}"),
        _ => anyhow::bail!("Unexpected response"),
    }
}

fn make_set(
    selector: &str,
    property: &str,
    value: &str,
    attr: Option<String>,
) -> Result<serde_json::Value> {
    Ok(serde_json::to_value(webpilot::protocol::Request::new(
        1,
        Command::DomSet {
            selector: selector.to_string(),
            property: property.to_string(),
            value: value.to_string(),
            attr,
        },
    ))?)
}

fn make_get(selector: &str, property: &str, attr: Option<String>) -> Result<serde_json::Value> {
    Ok(serde_json::to_value(webpilot::protocol::Request::new(
        1,
        Command::DomGet {
            selector: selector.to_string(),
            property: property.to_string(),
            attr,
        },
    ))?)
}
