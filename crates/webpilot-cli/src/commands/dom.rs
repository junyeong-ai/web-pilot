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
    SetHtml { selector: String, value: String },
    #[command(name = "set-text")]
    SetText { selector: String, value: String },
    #[command(name = "set-attr")]
    SetAttr {
        selector: String,
        attr: String,
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
            if let Some(val) = value {
                Ok(CommandOutput::Content {
                    stdout: val.clone(),
                    json: serde_json::json!({"success": true, "value": val}),
                })
            } else if success {
                Ok(CommandOutput::Ok("OK".into()))
            } else {
                Err(error
                    .map(anyhow::Error::from)
                    .unwrap_or_else(|| anyhow::anyhow!("DOM operation failed")))
            }
        }
        ResponseData::Error { error } => Err(error.into()),
        _ => anyhow::bail!("Unexpected response shape"),
    }
}
