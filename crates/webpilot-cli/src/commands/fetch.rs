use anyhow::{Context, Result};
use clap::Args;
use webpilot::ipc;
use webpilot::protocol::{Command, ResponseData};

use crate::output::CommandOutput;

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

pub async fn run(args: FetchArgs) -> Result<CommandOutput> {
    let request = serde_json::to_value(webpilot::protocol::Request::new(
        1,
        Command::Fetch {
            url: args.url,
            method: Some(args.method),
            body: args.body,
        },
    ))?;

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
            if !success {
                if let Some(ref err) = error {
                    anyhow::bail!("{}", crate::output::format_error(err));
                } else {
                    anyhow::bail!("Unknown error");
                }
            }
            let stdout = body.clone().unwrap_or_default();
            Ok(CommandOutput::Content {
                stdout: if stdout.is_empty() {
                    format!("HTTP {}", status.unwrap_or(0))
                } else {
                    stdout
                },
                json: serde_json::json!({"success": success, "status": status, "body": body}),
            })
        }
        ResponseData::Error { message, .. } => anyhow::bail!("{message}"),
        _ => anyhow::bail!("Unexpected response"),
    }
}
