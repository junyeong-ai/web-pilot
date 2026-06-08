use anyhow::Result;
use clap::{Args, Subcommand};
use webpilot::protocol::{Command, ResponseData};
use webpilot::types::ConsoleLevel;

use webpilot::types::line_safe;

use crate::output::CommandOutput;
use crate::transport::{Transport, lift_error};

#[derive(Args)]
pub struct ConsoleArgs {
    #[command(subcommand)]
    pub command: ConsoleCommand,
}

#[derive(Subcommand)]
pub enum ConsoleCommand {
    /// Start capturing console output.
    Start,
    /// Read captured console entries.
    Read {
        /// Filter by level (log, error, warn, info, debug).
        #[arg(long)]
        level: Option<String>,
        /// Only entries at or after this timestamp (ms epoch) — an incremental
        /// cursor for polling without a destructive `console clear`.
        #[arg(long)]
        since: Option<u64>,
    },
    /// Clear captured entries.
    Clear,
}

pub async fn run<T: Transport>(transport: &mut T, args: ConsoleArgs) -> Result<CommandOutput> {
    let cmd = match &args.command {
        ConsoleCommand::Start => Command::ConsoleStart,
        ConsoleCommand::Read { since, .. } => Command::ConsoleRead { since: *since },
        ConsoleCommand::Clear => Command::ConsoleClear,
    };

    let result = transport.send(cmd).await?;

    match result {
        ResponseData::ConsoleEntries { entries, truncated } => {
            let filtered: Vec<_> = match &args.command {
                ConsoleCommand::Read {
                    level: Some(lvl), ..
                } => {
                    let target = lvl.parse::<ConsoleLevel>().map_err(|_| {
                        webpilot::WebPilotError::InvalidArgument {
                            detail: format!("unknown console level '{lvl}'"),
                        }
                    })?;
                    entries.into_iter().filter(|e| e.level == target).collect()
                }
                _ => entries,
            };

            let mut human: String = filtered
                .iter()
                .map(|e| format!("[{}] {}", e.level, line_safe(&e.message)))
                .collect::<Vec<_>>()
                .join("\n");
            // `truncated` rides in both the JSON and the human text so neither an
            // MCP nor a CLI agent reads a full-looking buffer as the whole story.
            if truncated {
                if !human.is_empty() {
                    human.push('\n');
                }
                human.push_str(
                    "--- console buffer at capacity — older entries may have been dropped ---",
                );
            }
            Ok(CommandOutput::Data {
                json: serde_json::json!({ "entries": filtered, "truncated": truncated }),
                human,
            })
        }
        ResponseData::CommandResult { success, error, .. } => {
            lift_error(success, error, CommandOutput::Ok("OK".into()))
        }
        ResponseData::Error { error } => Err(error.into()),
        _ => anyhow::bail!("Unexpected response shape"),
    }
}
