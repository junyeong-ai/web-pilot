use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use webpilot::ipc;
use webpilot::protocol::{Command, ResponseData};

use crate::output::OutputMode;

#[derive(Args)]
pub struct NetworkArgs {
    #[command(subcommand)]
    pub command: NetworkCommand,
}

#[derive(Subcommand)]
pub enum NetworkCommand {
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
    let cmd = match &args.command {
        NetworkCommand::Start => Command::NetworkStart,
        NetworkCommand::Read { since } => Command::NetworkRead { since: *since },
        NetworkCommand::Clear => Command::NetworkClear,
    };

    let request = serde_json::to_value(webpilot::protocol::Request::new(1, cmd))?;
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
            } else if let Some(ref err) = error {
                eprintln!("{}", crate::output::format_error(err));
            } else {
                eprintln!("Unknown error");
            }
        }
        ResponseData::Error { message, .. } => anyhow::bail!("{message}"),
        _ => anyhow::bail!("Unexpected response"),
    }
    Ok(())
}
