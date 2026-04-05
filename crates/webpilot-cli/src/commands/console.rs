use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use webpilot::ipc;
use webpilot::protocol::{Command, ResponseData};

use crate::output::OutputMode;

#[derive(Args)]
pub struct ConsoleArgs {
    #[command(subcommand)]
    pub command: ConsoleCommand,
}

#[derive(Subcommand)]
pub enum ConsoleCommand {
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
    let cmd = match &args.command {
        ConsoleCommand::Start => Command::ConsoleStart,
        ConsoleCommand::Read { .. } => Command::ConsoleRead,
        ConsoleCommand::Clear => Command::ConsoleClear,
    };

    let request = serde_json::to_value(webpilot::protocol::Request::new(1, cmd))?;
    let response = ipc::send_request(&request)
        .await
        .context("Host not running")?;
    let resp: webpilot::protocol::Response = serde_json::from_value(response)?;

    match resp.result {
        ResponseData::ConsoleEntries { entries } => {
            let filtered = if let ConsoleCommand::Read {
                level: Some(ref lvl),
            } = args.command
            {
                if let Some(level) = webpilot::types::ConsoleLevel::parse(lvl) {
                    entries.into_iter().filter(|e| e.level == level).collect()
                } else {
                    entries
                }
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
