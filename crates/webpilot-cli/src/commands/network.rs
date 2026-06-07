use anyhow::Result;
use clap::{Args, Subcommand};
use webpilot::protocol::{Command, ResponseData};

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
        ResponseData::NetworkEntries { entries: requests } => {
            let human_lines: Vec<String> = requests
                .iter()
                .map(|r| {
                    let status = r
                        .status
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| r.error.clone().unwrap_or_else(|| "?".into()));
                    format!(
                        "{} {} {} → {} ({}ms)",
                        r.req_type, r.method, r.url, status, r.duration_ms as u64
                    )
                })
                .collect();
            Ok(CommandOutput::List {
                items: serde_json::to_value(&requests)?,
                human_lines,
                summary: format!("({} requests)", requests.len()),
            })
        }
        ResponseData::CommandResult { success, error, .. } => {
            lift_error(success, error, CommandOutput::Ok("OK".into()))
        }
        ResponseData::Error { error } => Err(error.into()),
        _ => anyhow::bail!("Unexpected response shape"),
    }
}
