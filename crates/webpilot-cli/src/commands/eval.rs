use anyhow::{Context, Result};
use clap::Args;
use webpilot::ipc;
use webpilot::protocol::{Command, ResponseData};

use crate::output::CommandOutput;

#[derive(Args)]
pub struct EvalArgs {
    /// JavaScript code to evaluate in the page context
    pub code: String,
}

pub async fn run(args: EvalArgs) -> Result<CommandOutput> {
    let request = serde_json::to_value(webpilot::protocol::Request::new(
        1,
        Command::Evaluate { code: args.code },
    ))?;

    let response = ipc::send_request(&request)
        .await
        .context("Failed to connect. Run: webpilot install")?;

    let resp: webpilot::protocol::Response = serde_json::from_value(response)?;

    match resp.result {
        ResponseData::Evaluate {
            success,
            result,
            error,
        } => {
            if !success {
                if let Some(ref err) = error {
                    anyhow::bail!("{}", crate::output::format_error(err));
                } else {
                    anyhow::bail!("Unknown error");
                }
            }
            let stdout = result.unwrap_or_else(|| "undefined".into());
            Ok(CommandOutput::Content {
                stdout: stdout.clone(),
                json: serde_json::json!({"success": true, "result": stdout}),
            })
        }
        ResponseData::Error { message, .. } => anyhow::bail!("{message}"),
        _ => anyhow::bail!("Unexpected response"),
    }
}
