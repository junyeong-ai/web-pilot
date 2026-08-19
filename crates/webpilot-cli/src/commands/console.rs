use anyhow::Result;
use clap::{Args, Subcommand};
use webpilot::protocol::{Command, ResponseData};
use webpilot::types::{ConsoleEntry, ConsoleLevel, line_safe};

use crate::output::CommandOutput;
use crate::transport::{Transport, lift_error};

/// One agent-facing console row: the entry's millisecond timestamp — the value
/// an agent feeds back to `--since` for an incremental read, and the anchor for
/// correlating entries to wall-clock events — then what it is and what it said.
fn console_row(e: &ConsoleEntry) -> String {
    format!(
        "[{}] [{}] {}",
        e.timestamp,
        e.label(),
        line_safe(&e.message)
    )
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
        ResponseData::ConsoleEntries {
            entries,
            truncated,
            covers_load,
        } => {
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
            // Both limits ride in the JSON and the human text so neither an MCP nor
            // a CLI agent reads a buffer as the whole story: `truncated` for what
            // the cap dropped, `covers_load` for the window before the recorder
            // existed — an empty read there is the recorder's absence, not the
            // page's silence.
            if truncated {
                crate::output::push_note(
                    &mut human,
                    "--- console buffer at capacity — older entries may have been dropped ---",
                );
            }
            if !covers_load {
                crate::output::push_note(&mut human, crate::output::MONITOR_PARTIAL_NOTE);
            }
            Ok(CommandOutput::Data {
                json: serde_json::json!({
                    "entries": filtered,
                    "truncated": truncated,
                    "covers_load": covers_load,
                }),
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
    use webpilot::types::ConsoleSource;

    #[test]
    fn console_row_leads_with_the_timestamp() {
        let row = console_row(&ConsoleEntry {
            source: ConsoleSource::Console,
            level: ConsoleLevel::Error,
            message: "boom".into(),
            timestamp: 1_799_000_000_123,
        });
        // The ms timestamp leads the row so an agent can feed it straight back
        // to `--since` for the next incremental read.
        assert!(row.starts_with("[1799000000123] "), "{row}");
        assert!(row.contains("[error] boom"), "{row}");
    }

    #[test]
    fn console_row_names_what_the_page_never_called() {
        let row = console_row(&ConsoleEntry {
            source: ConsoleSource::Exception,
            level: ConsoleLevel::Error,
            message: "Uncaught TypeError: x is not a function".into(),
            timestamp: 1_799_000_000_123,
        });
        // An uncaught error and a `console.error` are both error-level; the row
        // has to say which, or an agent reading the text cannot tell a page that
        // logged a message from a page that broke.
        assert!(row.contains("[exception] Uncaught TypeError"), "{row}");
    }
}
