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

    #[arg(long, allow_hyphen_values = true)]
    pub body: Option<String>,

    /// Request header `NAME:VALUE` (repeatable). Nothing is sent by default — a
    /// JSON body needs an explicit `--header content-type:application/json`.
    #[arg(long = "header", value_name = "NAME:VALUE")]
    pub headers: Vec<String>,
}

pub async fn run<T: Transport>(transport: &mut T, args: FetchArgs) -> Result<CommandOutput> {
    let headers = args
        .headers
        .iter()
        .map(|h| {
            let (name, value) =
                h.split_once(':')
                    .ok_or_else(|| webpilot::WebPilotError::InvalidArgument {
                        detail: format!("--header must be NAME:VALUE, got {h:?}"),
                    })?;
            Ok((name.trim().to_string(), value.trim().to_string()))
        })
        .collect::<Result<Vec<(String, String)>>>()?;

    let result = transport
        .send(Command::Fetch {
            url: args.url,
            method: Some(args.method),
            body: args.body,
            headers,
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
                    // An absent status renders as unknown, never fabricated as
                    // 0 — "HTTP 0" is the XHR network-error convention, so
                    // inventing it would actively mislead. The JSON channel
                    // carries the honest `status: null` either way.
                    match status {
                        Some(s) => format!("HTTP {s}"),
                        None => "HTTP status unknown".into(),
                    }
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
