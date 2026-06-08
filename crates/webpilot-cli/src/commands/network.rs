use anyhow::Result;
use clap::{Args, Subcommand};
use webpilot::protocol::{Command, ResponseData};

use webpilot::types::line_safe;

use crate::output::CommandOutput;
use crate::transport::{Transport, lift_error};

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
        } => {
            let mut human: String = requests
                .iter()
                .map(|r| {
                    let status = r
                        .status
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| r.error.clone().unwrap_or_else(|| "?".into()));
                    format!(
                        "{} {} {} → {} ({}ms)",
                        r.req_type,
                        line_safe(&r.method),
                        line_safe(&r.url),
                        status,
                        r.duration_ms as u64
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            // `truncated` rides in both the JSON and the human text so neither an
            // MCP nor a CLI agent reads a full-looking buffer as the whole story.
            if truncated {
                if !human.is_empty() {
                    human.push('\n');
                }
                human.push_str(
                    "--- network buffer at capacity — older requests may have been dropped ---",
                );
            }
            Ok(CommandOutput::Data {
                json: serde_json::json!({ "entries": requests, "truncated": truncated }),
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
