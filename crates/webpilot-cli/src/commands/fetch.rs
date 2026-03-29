use anyhow::{Context, Result};
use clap::Args;
use webpilot::ipc;
use webpilot::protocol::{Command, ResponseData};

use crate::output::OutputMode;

#[derive(Args)]
pub struct FetchArgs {
    /// URL to fetch (uses browser cookies/session)
    pub url: String,

    /// HTTP method
    #[arg(long, default_value = "GET")]
    pub method: String,

    /// Request body (JSON string)
    #[arg(long)]
    pub body: Option<String>,
}

pub async fn run(args: FetchArgs, output_mode: OutputMode) -> Result<()> {
    let request = serde_json::to_value(webpilot::protocol::Request {
        id: 1,
        command: Command::Fetch {
            url: args.url,
            method: Some(args.method),
            body: args.body,
        },
    })?;

    let response = ipc::send_request(&request)
        .await
        .context("Host not running")?;
    let resp: webpilot::protocol::Response = serde_json::from_value(response)?;

    match resp.result {
        ResponseData::FetchResult {
            success,
            status,
            body,
            error,
        } => {
            let err_str = error.unwrap_or_default();
            match output_mode {
                OutputMode::Human => {
                    if success {
                        if let Some(ref b) = body {
                            println!("{b}");
                        }
                        eprintln!("HTTP {}", status.unwrap_or(0));
                    } else {
                        eprintln!("{}", crate::output::format_error(&err_str, None));
                    }
                }
                OutputMode::Json => {
                    println!(
                        "{}",
                        serde_json::json!({"success": success, "status": status, "body": body, "error": err_str})
                    );
                }
            }
            if !success {
                anyhow::bail!("{}", crate::output::format_error(&err_str, None));
            }
        }
        ResponseData::Error { message, .. } => anyhow::bail!("{message}"),
        _ => anyhow::bail!("Unexpected response"),
    }
    Ok(())
}
