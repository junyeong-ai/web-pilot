use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use webpilot::ipc;
use webpilot::protocol::{Command, ResponseData};

use crate::output::OutputMode;

#[derive(Args)]
pub struct ConsoleArgs {
    #[command(subcommand)]
    pub action: ConsoleAction,
}

#[derive(Subcommand)]
pub enum ConsoleAction {
    /// Start capturing console output
    Start,
    /// Read captured console entries
    Read {
        /// Filter by level (log, error, warn, info)
        #[arg(long)]
        level: Option<String>,
    },
    /// Clear captured entries
    Clear,
}

pub async fn run(args: ConsoleArgs, output_mode: OutputMode) -> Result<()> {
    let cmd = match &args.action {
        ConsoleAction::Start => Command::ConsoleStart,
        ConsoleAction::Read { .. } => Command::ConsoleRead,
        ConsoleAction::Clear => Command::ConsoleClear,
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
        ResponseData::ConsoleEntries { entries } => {
            let filtered = if let ConsoleAction::Read {
                level: Some(ref lvl),
            } = args.action
            {
                entries.into_iter().filter(|e| &e.level == lvl).collect()
            } else {
                entries
            };

            match output_mode {
                OutputMode::Human => {
                    for e in &filtered {
                        eprintln!("[{}] {}", e.level, e.message);
                    }
                    eprintln!("({} entries)", filtered.len());
                }
                OutputMode::Json => println!("{}", serde_json::to_string_pretty(&filtered)?),
            }
        }
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
