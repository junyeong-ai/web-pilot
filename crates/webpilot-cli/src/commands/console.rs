use anyhow::Result;
use clap::{Args, Subcommand};
use webpilot::protocol::{Command, ResponseData};
use webpilot::types::{ConsoleEntry, ConsoleLevel, line_safe};

use crate::output::CommandOutput;
use crate::transport::{Transport, lift_error};

/// One agent-facing console row: the entry's millisecond timestamp — the value
/// an agent feeds back to `--since` for an incremental read, and the anchor for
/// correlating entries to wall-clock events — then its level and message.
fn console_row(e: &ConsoleEntry) -> String {
    format!("[{}] [{}] {}", e.timestamp, e.level, line_safe(&e.message))
}

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
                .map(console_row)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_row_leads_with_the_timestamp() {
        let row = console_row(&ConsoleEntry {
            level: ConsoleLevel::Error,
            message: "boom".into(),
            timestamp: 1_799_000_000_123,
        });
        // The ms timestamp leads the row so an agent can feed it straight back
        // to `--since` for the next incremental read.
        assert!(row.starts_with("[1799000000123] "), "{row}");
        assert!(row.contains("boom"), "{row}");
    }
}
