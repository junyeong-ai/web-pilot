use anyhow::Result;
use clap::Args;
use webpilot::protocol::{Command, ResponseData};
use webpilot::wait::WaitCondition;

use crate::output::CommandOutput;
use crate::transport::{Transport, lift_error};

#[derive(Args)]
pub struct WaitArgs {
    #[command(subcommand)]
    pub condition: WaitCondition,

    #[arg(long, global = true, default_value_t = 10)]
    pub timeout: u64,
}

pub async fn run<T: Transport>(transport: &mut T, args: WaitArgs) -> Result<CommandOutput> {
    // The CLI flag is whole seconds; the wire (and the MCP surface) speak
    // milliseconds.
    dispatch(transport, args.condition, args.timeout.saturating_mul(1000)).await
}

/// Send a wait with a millisecond timeout — the wire-native unit. The CLI
/// converts its seconds flag here; the MCP `browser_wait` tool passes its
/// `timeout_ms` straight through, so a sub-second request isn't rounded up to a
/// whole second by detouring through the seconds-based `WaitArgs`.
pub async fn dispatch<T: Transport>(
    transport: &mut T,
    condition: WaitCondition,
    timeout_ms: u64,
) -> Result<CommandOutput> {
    let result = transport
        .send(Command::Wait {
            condition,
            timeout_ms,
        })
        .await?;

    match result {
        ResponseData::Wait { success, error } => {
            lift_error(success, error, ())?;
            Ok(CommandOutput::Ok("OK".into()))
        }
        ResponseData::Error { error } => Err(error.into()),
        _ => anyhow::bail!("Unexpected response shape"),
    }
}
