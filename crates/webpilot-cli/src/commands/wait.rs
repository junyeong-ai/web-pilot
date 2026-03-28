use anyhow::{Context, Result};
use clap::Args;
use webpilot::ipc;
use webpilot::protocol::{Command, ResponseData};

use crate::output::OutputMode;

#[derive(Args)]
pub struct WaitArgs {
    /// CSS selector to wait for
    #[arg(long)]
    pub selector: Option<String>,

    /// Text to wait for on page
    #[arg(long)]
    pub text: Option<String>,

    /// Wait for navigation to complete
    #[arg(long)]
    pub navigation: bool,

    /// Timeout in seconds
    #[arg(long, default_value = "10")]
    pub timeout: u64,
}

pub async fn run(args: WaitArgs, output_mode: OutputMode) -> Result<()> {
    let request = serde_json::to_value(webpilot::protocol::Request {
        id: 1,
        command: Command::Wait {
            selector: args.selector,
            text: args.text,
            navigation: args.navigation,
            timeout_ms: args.timeout * 1000,
        },
    })?;

    let response = ipc::send_request(&request)
        .await
        .context("Failed to connect")?;

    let resp: webpilot::protocol::Response = serde_json::from_value(response)?;

    match resp.result {
        ResponseData::WaitResult { success, error, .. } => {
            match output_mode {
                OutputMode::Human => {
                    if success {
                        eprintln!("OK");
                    } else {
                        eprintln!(
                            "{}",
                            crate::output::format_error(
                                &error.unwrap_or_default(),
                                Some("TIMEOUT"),
                            )
                        );
                    }
                }
                OutputMode::Json => {
                    println!(
                        "{}",
                        serde_json::json!({"success": success, "error": error})
                    );
                }
            }
            if !success {
                std::process::exit(1);
            }
        }
        ResponseData::Error { message, .. } => anyhow::bail!("{message}"),
        _ => anyhow::bail!("Unexpected response"),
    }

    Ok(())
}
