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
    let result = transport
        .send(Command::Wait {
            condition: args.condition,
            timeout_ms: args.timeout.saturating_mul(1000),
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
