use anyhow::Result;
use clap::{Args, Subcommand};
use webpilot::protocol::{Command, ResponseData};
use webpilot::types::{NetworkEntry, line_safe};

use crate::output::CommandOutput;
use crate::transport::{Transport, lift_error};

/// One agent-facing network row: the entry's millisecond timestamp — the
/// `--since` anchor for an incremental read — then request type, method, URL,
/// the status (or the error text, or `?`), and the duration.
fn network_row(e: &NetworkEntry) -> String {
    // A numeric status is already injection-safe; an error string is run through
    // `line_safe` like every other agent-facing field.
    let status = e.status.map(|s| s.to_string()).unwrap_or_else(|| {
        e.error
            .as_deref()
            .map(|s| line_safe(s).into_owned())
            .unwrap_or_else(|| "?".into())
    });
    format!(
        "[{}] {} {} {} → {} ({}ms)",
        e.timestamp,
        line_safe(&e.req_type),
        line_safe(&e.method),
        line_safe(&e.url),
        status,
        e.duration_ms as u64
    )
}

#[derive(Args)]
pub struct NetworkArgs {
    #[command(subcommand)]
    pub command: NetworkCommand,
}

#[derive(Subcommand)]
pub enum NetworkCommand {
    /// Start monitoring fetch/XHR requests.
    Start,
    /// Read captured network requests.
    Read {
        #[arg(long)]
        since: Option<u64>,
    },
    /// Clear captured requests.
    Clear,
}

pub async fn run<T: Transport>(transport: &mut T, args: NetworkArgs) -> Result<CommandOutput> {
    let cmd = match args.command {
        NetworkCommand::Start => Command::NetworkStart,
        NetworkCommand::Read { since } => Command::NetworkRead { since },
        NetworkCommand::Clear => Command::NetworkClear,
    };

    let result = transport.send(cmd).await?;

    match result {
        ResponseData::NetworkEntries {
            entries: requests,
            truncated,
            covers_load,
        } => {
            let mut human: String = requests
                .iter()
                .map(network_row)
                .collect::<Vec<_>>()
                .join("\n");
            // Both limits ride in the JSON and the human text — see `console read`:
            // `truncated` for what the cap dropped, `covers_load` for the window
            // before the recorder existed, where no requests means nothing was
            // watching rather than a page that made none.
            if truncated {
                crate::output::push_note(
                    &mut human,
                    "--- network buffer at capacity — older requests may have been dropped ---",
                );
            }
            if !covers_load {
                crate::output::push_note(&mut human, crate::output::MONITOR_PARTIAL_NOTE);
            }
            Ok(CommandOutput::Data {
                json: serde_json::json!({
                    "entries": requests,
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

    fn entry() -> NetworkEntry {
        NetworkEntry {
            req_type: "fetch".into(),
            url: "https://x/api".into(),
            method: "GET".into(),
            status: Some(200),
            error: None,
            duration_ms: 45.0,
            timestamp: 1_799_000_000_123,
        }
    }

    #[test]
    fn network_row_leads_with_timestamp_and_shows_status() {
        let row = network_row(&entry());
        // The ms timestamp leads the row (the `--since` anchor); the status and
        // duration follow.
        assert!(row.starts_with("[1799000000123] "), "{row}");
        assert!(row.contains("200"), "{row}");
        assert!(row.contains("45ms"), "{row}");
    }

    #[test]
    fn network_row_falls_back_to_error_then_question_mark() {
        let mut e = entry();
        e.status = None;
        e.error = Some("net::ERR_FAILED".into());
        assert!(
            network_row(&e).contains("net::ERR_FAILED"),
            "a statusless entry shows its error"
        );
        e.error = None;
        assert!(
            network_row(&e).contains("→ ?"),
            "no status and no error renders the unknown marker"
        );
    }
}
