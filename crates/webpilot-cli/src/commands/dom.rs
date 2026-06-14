use anyhow::Result;
use clap::{Args, Subcommand};
use webpilot::protocol::{Command, DomProperty, ResponseData};

use crate::output::CommandOutput;
use crate::transport::Transport;

#[derive(Args)]
pub struct DomArgs {
    #[command(subcommand)]
    pub command: DomCommand,
}

#[derive(Subcommand)]
pub enum DomCommand {
    #[command(name = "set-html")]
    SetHtml {
        selector: String,
        #[arg(allow_hyphen_values = true)]
        value: String,
    },
    #[command(name = "set-text")]
    SetText {
        selector: String,
        #[arg(allow_hyphen_values = true)]
        value: String,
    },
    #[command(name = "set-attr")]
    SetAttr {
        selector: String,
        attr: String,
        #[arg(allow_hyphen_values = true)]
        value: String,
    },
    #[command(name = "get-html")]
    GetHtml { selector: String },
    #[command(name = "get-text")]
    GetText { selector: String },
    #[command(name = "get-attr")]
    GetAttr { selector: String, attr: String },
}

pub async fn run<T: Transport>(transport: &mut T, args: DomArgs) -> Result<CommandOutput> {
    // A get returns a value (possibly absent); a set returns only success.
    // They must render differently — an absent attribute is an empty result,
    // not the "OK" of a completed write. The label names WHAT was read, so an
    // absent value (`value: null`) reads as "(no attribute 'href' …)" on the
    // human/MCP surface instead of an empty line indistinguishable from a
    // present-but-empty value (`disabled=""`), which the JSON `value` already
    // tells apart (`null` vs `""`).
    let get_label = match &args.command {
        DomCommand::GetAttr { attr, .. } => Some(format!("attribute '{attr}'")),
        DomCommand::GetText { .. } => Some("text".to_string()),
        DomCommand::GetHtml { .. } => Some("html".to_string()),
        _ => None,
    };
    let cmd = match args.command {
        DomCommand::SetHtml { selector, value } => Command::DomSet {
            selector,
            property: DomProperty::Html,
            value,
        },
        DomCommand::SetText { selector, value } => Command::DomSet {
            selector,
            property: DomProperty::Text,
            value,
        },
        DomCommand::SetAttr {
            selector,
            attr,
            value,
        } => Command::DomSet {
            selector,
            property: DomProperty::Attr { name: attr },
            value,
        },
        DomCommand::GetHtml { selector } => Command::DomGet {
            selector,
            property: DomProperty::Html,
        },
        DomCommand::GetText { selector } => Command::DomGet {
            selector,
            property: DomProperty::Text,
        },
        DomCommand::GetAttr { selector, attr } => Command::DomGet {
            selector,
            property: DomProperty::Attr { name: attr },
        },
    };

    let result = transport.send(cmd).await?;

    match result {
        ResponseData::CommandResult {
            success,
            value,
            error,
        } => {
            // Failure is checked FIRST: a response carrying `success: false` must
            // never map to a success output, even if it also carried a value —
            // failure-mapped-to-success is the one shape this surface must refuse.
            if !success {
                Err(error
                    .map(anyhow::Error::from)
                    .unwrap_or_else(|| anyhow::anyhow!("DOM operation failed")))
            } else if let Some(val) = value {
                Ok(CommandOutput::Content {
                    stdout: val.clone(),
                    json: serde_json::json!({"success": true, "value": val}),
                    note: None,
                })
            } else if let Some(label) = get_label {
                // Get succeeded but the property/attribute is absent: an
                // explicitly-null result — distinct from a write's "OK" AND
                // from a present-but-empty value. The note names it so the
                // human/MCP surface doesn't collapse "absent" into the same
                // empty line a `disabled=""` would print.
                Ok(CommandOutput::Content {
                    stdout: String::new(),
                    json: serde_json::json!({"success": true, "value": null}),
                    note: Some(format!("(no {label} on the matched element)")),
                })
            } else {
                Ok(CommandOutput::Ok("OK".into()))
            }
        }
        ResponseData::Error { error } => Err(error.into()),
        _ => anyhow::bail!("Unexpected response shape"),
    }
}
