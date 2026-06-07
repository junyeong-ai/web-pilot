use anyhow::Result;
use clap::Args;
use webpilot::protocol::{Command, ResponseData};

use crate::output::CommandOutput;
use crate::transport::{Transport, lift_error};

#[derive(Args)]
pub struct EvalArgs {
    /// JavaScript code to evaluate in the page context.
    // Free-text JS legitimately starts with `-` (a negative literal, unary
    // minus, prefix decrement) — accept it as the value, not a flag.
    #[arg(allow_hyphen_values = true)]
    pub code: String,
}

pub async fn run<T: Transport>(transport: &mut T, args: EvalArgs) -> Result<CommandOutput> {
    let result = transport.send(Command::Eval { code: args.code }).await?;

    match result {
        ResponseData::Eval {
            success,
            result,
            error,
        } => {
            lift_error(success, error, ())?;
            let stdout = result.unwrap_or_else(|| "undefined".into());
            Ok(CommandOutput::Content {
                stdout: stdout.clone(),
                json: serde_json::json!({"success": true, "result": stdout}),
            })
        }
        ResponseData::Error { error } => Err(error.into()),
        _ => anyhow::bail!("Unexpected response shape"),
    }
}
