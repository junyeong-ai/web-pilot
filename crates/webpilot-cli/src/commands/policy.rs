use anyhow::Result;
use clap::{Args, Subcommand};
use webpilot::ActionKind;
use webpilot::protocol::{Command, ResponseData};
use webpilot::types::PolicyVerdict;

use crate::output::CommandOutput;
use crate::transport::{Transport, lift_error};

#[derive(Args)]
pub struct PolicyArgs {
    #[command(subcommand)]
    pub command: PolicyCommand,
}

#[derive(Subcommand)]
pub enum PolicyCommand {
    /// Set policy for an action kind.
    Set {
        #[arg(long)]
        action: String,
        #[arg(long)]
        verdict: String,
    },
    /// List all policies.
    List,
    /// Clear all policies.
    Clear,
}

pub async fn run<T: Transport>(transport: &mut T, args: PolicyArgs) -> Result<CommandOutput> {
    let cmd = match &args.command {
        PolicyCommand::Set { action, verdict } => {
            let action: ActionKind =
                action
                    .parse()
                    .map_err(|_| webpilot::WebPilotError::InvalidArgument {
                        detail: format!("unknown action kind '{action}'"),
                    })?;
            let verdict: PolicyVerdict =
                verdict
                    .parse()
                    .map_err(|_| webpilot::WebPilotError::InvalidArgument {
                        detail: format!("unknown verdict '{verdict}' (use 'allow' or 'deny')"),
                    })?;
            Command::PolicySet { action, verdict }
        }
        PolicyCommand::List => Command::PolicyList,
        PolicyCommand::Clear => Command::PolicyClear,
    };

    let result = transport.send(cmd).await?;

    match result {
        ResponseData::Policies { policies } => {
            let human_lines: Vec<String> = policies
                .iter()
                .map(|p| format!("{}: {}", p.action, p.verdict))
                .collect();
            let summary = if policies.is_empty() {
                "No policies set".into()
            } else {
                String::new()
            };
            Ok(CommandOutput::List {
                items: serde_json::to_value(&policies)?,
                human_lines,
                summary,
            })
        }
        ResponseData::PolicyResult { success, error } => {
            lift_error(success, error, CommandOutput::Ok("OK".into()))
        }
        ResponseData::Error { error } => Err(error.into()),
        _ => Ok(CommandOutput::Silent),
    }
}
