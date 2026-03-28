use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use webpilot::ipc;
use webpilot::protocol::{Command, ResponseData};

use crate::output::OutputMode;

#[derive(Args)]
pub struct NetworkArgs {
    #[command(subcommand)]
    pub action: NetworkAction,
}

#[derive(Subcommand)]
pub enum NetworkAction {
    /// Start monitoring fetch/XHR requests
    Start,
    /// Read captured network requests
    Read {
        /// Only show requests after this timestamp (ms since epoch)
        #[arg(long)]
        since: Option<u64>,
    },
    /// Clear captured requests
    Clear,
}

pub async fn run(args: NetworkArgs, output_mode: OutputMode) -> Result<()> {
    let cmd = match &args.action {
        NetworkAction::Start => Command::NetworkStart,
        NetworkAction::Read { since } => Command::NetworkRead { since: *since },
        NetworkAction::Clear => Command::NetworkClear,
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
        ResponseData::NetworkLog { requests } => match output_mode {
            OutputMode::Human => {
                for r in &requests {
                    let status = r
                        .status
                        .map(|s| format!("{s}"))
                        .unwrap_or_else(|| r.error.clone().unwrap_or("?".into()));
                    eprintln!(
                        "{} {} {} → {} ({}ms)",
                        r.req_type, r.method, r.url, status, r.duration_ms as u64
                    );
                }
                eprintln!("({} requests)", requests.len());
            }
            OutputMode::Json => println!("{}", serde_json::to_string_pretty(&requests)?),
        },
        ResponseData::CommandResult { success, error, .. } => {
            if success {
                match output_mode {
                    OutputMode::Human => eprintln!("OK"),
                    OutputMode::Json => println!("{{\"success\":true}}"),
                }
            } else {
                eprintln!(
                    "{}",
                    crate::output::format_error(&error.unwrap_or_default(), None)
                );
            }
        }
        ResponseData::Error { message, .. } => anyhow::bail!("{message}"),
        _ => anyhow::bail!("Unexpected response"),
    }
    Ok(())
}
