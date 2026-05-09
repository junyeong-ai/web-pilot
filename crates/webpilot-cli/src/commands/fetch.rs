use anyhow::Result;
use clap::Args;
use webpilot::protocol::{Command, ResponseData};

use crate::output::CommandOutput;
use crate::transport::{Transport, lift_error};

#[derive(Args)]
pub struct FetchArgs {
    /// URL to fetch (uses browser cookies/session).
    pub url: String,

    #[arg(long, default_value = "GET")]
    pub method: String,

    #[arg(long)]
    pub body: Option<String>,
}

pub async fn run<T: Transport>(transport: &mut T, args: FetchArgs) -> Result<CommandOutput> {
    let result = transport
        .send(Command::Fetch {
            url: args.url,
            method: Some(args.method),
            body: args.body,
        })
        .await?;

    match result {
        ResponseData::FetchResult {
            success,
            status,
            body,
            error,
        } => {
            lift_error(success, error, ())?;
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
        ResponseData::Error { error } => Err(error.into()),
        _ => anyhow::bail!("Unexpected response shape"),
    }
}
