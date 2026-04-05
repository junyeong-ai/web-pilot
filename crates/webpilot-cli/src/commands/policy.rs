use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use webpilot::ipc;
use webpilot::protocol::{Command, ResponseData};

use crate::output::CommandOutput;

#[derive(Args)]
pub struct PolicyArgs {
    #[command(subcommand)]
    pub command: PolicyCommand,
}

#[derive(Subcommand)]
pub enum PolicyCommand {
    /// Set policy for an action type
    Set {
        /// Action type (click, type, navigate, etc.)
        #[arg(long)]
        action: String,
        /// Verdict (allow, deny)
        #[arg(long)]
        verdict: String,
    },
    /// List all policies
    List,
    /// Clear all policies
    Clear,
}

pub async fn run(args: PolicyArgs) -> Result<CommandOutput> {
    let cmd = match &args.command {
        PolicyCommand::Set { action, verdict } => {
            let action_type: webpilot::types::ActionType =
                serde_json::from_value(serde_json::Value::String(action.clone()))
                    .with_context(|| format!("Unknown action type: {action}"))?;
            let verdict: webpilot::types::PolicyVerdict = serde_json::from_value(
                serde_json::Value::String(verdict.clone()),
            )
            .with_context(|| format!("Unknown verdict: {verdict}. Use 'allow' or 'deny'"))?;
            Command::PolicySet {
                action_type,
                verdict,
            }
        }
        PolicyCommand::List => Command::PolicyList,
        PolicyCommand::Clear => Command::PolicyClear,
    };

    let request = serde_json::to_value(webpilot::protocol::Request::new(1, cmd))?;
    let response = ipc::send_request(&request)
        .await
        .context("Host not running")?;
    let resp: webpilot::protocol::Response = serde_json::from_value(response)?;

    match resp.result {
        ResponseData::Policies { policies } => {
            let human_lines: Vec<String> = policies
                .iter()
                .map(|p| format!("{}: {}", p.action_type, p.verdict))
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
            if success {
                Ok(CommandOutput::Ok("OK".into()))
            } else if let Some(ref err) = error {
                anyhow::bail!("{}", crate::output::format_error(err));
            } else {
                anyhow::bail!("Unknown error");
            }
        }
        ResponseData::Error { message, .. } => anyhow::bail!("{message}"),
        _ => Ok(CommandOutput::Silent),
    }
}
