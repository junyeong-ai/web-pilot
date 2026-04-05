use anyhow::{Context, Result};
use clap::Args;
use webpilot::ipc;
use webpilot::protocol::{Command, ResponseData};

use crate::output::OutputMode;

#[derive(Args)]
pub struct EvalArgs {
    /// JavaScript code to evaluate in the page context
    pub code: String,
}

pub async fn run(args: EvalArgs, output_mode: OutputMode) -> Result<()> {
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
            match output_mode {
                OutputMode::Human => {
                    if success {
                        println!("{}", result.unwrap_or_else(|| "undefined".into()));
                    } else if let Some(ref err) = error {
                        eprintln!("{}", crate::output::format_error(err));
                    } else {
                        eprintln!("Unknown error");
                    }
                }
                OutputMode::Json => {
                    println!(
                        "{}",
                        serde_json::json!({"success": success, "result": result, "error": error})
                    );
                }
            }
            if !success {
                if let Some(ref err) = error {
                    anyhow::bail!("{}", crate::output::format_error(err));
                } else {
                    anyhow::bail!("Unknown error");
                }
            }
        }
        ResponseData::Error { message, .. } => anyhow::bail!("{message}"),
        _ => anyhow::bail!("Unexpected response"),
    }

    Ok(())
}
