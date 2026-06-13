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
            let body = body.unwrap_or_default();
            // The HTTP status is part of every fetch result, but the body is
            // what a shell pipes — so the status rides the `note` (stderr +
            // MCP text) rather than the stdout body, where a `404` with a body
            // used to vanish from the human/MCP surface while the JSON kept it.
            // An absent status is "unknown", never fabricated as 0 ("HTTP 0" is
            // the XHR network-error convention, so inventing it would mislead).
            let note = match status {
                Some(s) => format!("HTTP {s}"),
                None => "HTTP status unknown".into(),
            };
            Ok(CommandOutput::Content {
                stdout: body.clone(),
                json: serde_json::json!({"success": success, "status": status, "body": body}),
                note: Some(note),
            })
        }
        ResponseData::Error { error } => Err(error.into()),
        _ => anyhow::bail!("Unexpected response shape"),
    }
}
