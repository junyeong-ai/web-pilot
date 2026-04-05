use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use webpilot::ipc;
use webpilot::protocol::{Command, ResponseData};

use crate::output::CommandOutput;

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

pub async fn run(args: ConsoleArgs) -> Result<CommandOutput> {
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

            let human_lines: Vec<String> = filtered
                .iter()
                .map(|e| format!("[{}] {}", e.level, e.message))
                .collect();
            let summary = format!("({} entries)", filtered.len());
            Ok(CommandOutput::List {
                items: serde_json::to_value(&filtered)?,
                human_lines,
                summary,
            })
        }
        ResponseData::CommandResult { success, error, .. } => {
            if success {
                Ok(CommandOutput::Ok("OK".into()))
            } else if let Some(ref err) = error {
                anyhow::bail!("{}", crate::output::format_error(err));
            } else {
                anyhow::bail!("Unknown error");
            }
        }
        ResponseData::Error { message, .. } => anyhow::bail!("{message}"),
        _ => anyhow::bail!("Unexpected response"),
    }
}
