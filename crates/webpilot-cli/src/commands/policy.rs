use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use webpilot::ipc;
use webpilot::protocol::{Command, ResponseData};

use crate::output::OutputMode;

#[derive(Args)]
pub struct PolicyArgs {
    #[command(subcommand)]
    pub action: PolicyAction,
}

#[derive(Subcommand)]
pub enum PolicyAction {
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
    let cmd = match &args.action {
        PolicyAction::Set { action, verdict } => Command::SetPolicy {
            action_type: action.clone(),
            verdict: verdict.clone(),
        },
        PolicyAction::List => Command::GetPolicies,
        PolicyAction::Clear => Command::ClearPolicies,
    };

    let request = serde_json::to_value(webpilot::protocol::Request {
        id: 1,
        command: cmd,
    })?;
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
                } else {
                    eprintln!(
                        "{}",
                        crate::output::format_error(&error.unwrap_or_default(), None)
                    );
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
