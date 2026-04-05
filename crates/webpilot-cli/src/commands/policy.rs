use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use webpilot::ipc;
use webpilot::protocol::{Command, ResponseData};

use crate::output::OutputMode;

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

pub async fn run(args: PolicyArgs, output_mode: OutputMode) -> Result<()> {
    let cmd = match &args.command {
        PolicyCommand::Set { action, verdict } => {
            let action_type: webpilot::types::ActionType =
                serde_json::from_value(serde_json::Value::String(action.clone()))
                    .with_context(|| format!("Unknown action type: {action}"))?;
            let verdict: webpilot::types::PolicyVerdict = serde_json::from_value(
                serde_json::Value::String(verdict.clone()),
            )
            .with_context(|| format!("Unknown verdict: {verdict}. Use 'allow' or 'deny'"))?;
            Command::SetPolicy {
                action_type,
                verdict,
            }
        }
        PolicyCommand::List => Command::GetPolicies,
        PolicyCommand::Clear => Command::ClearPolicies,
    };

    let request = serde_json::to_value(webpilot::protocol::Request::new(1, cmd))?;
    let response = ipc::send_request(&request)
        .await
        .context("Host not running")?;
    let resp: webpilot::protocol::Response = serde_json::from_value(response)?;

    match resp.result {
        ResponseData::Policies { policies } => match output_mode {
            OutputMode::Human => {
                for p in &policies {
                    println!("{}: {}", p.action_type, p.verdict);
                }
                if policies.is_empty() {
                    eprintln!("No policies set");
                }
            }
            OutputMode::Json => println!("{}", serde_json::to_string_pretty(&policies)?),
        },
        ResponseData::PolicyResult { success, error } => match output_mode {
            OutputMode::Human => {
                if success {
                    eprintln!("OK");
                } else if let Some(ref err) = error {
                    eprintln!("{}", crate::output::format_error(err));
                } else {
                    eprintln!("Unknown error");
                }
            }
            OutputMode::Json => println!(
                "{}",
                serde_json::json!({"success": success, "error": error})
            ),
        },
        ResponseData::Error { message, .. } => anyhow::bail!("{message}"),
        _ => {}
    }
    Ok(())
}
