use anyhow::Result;
use clap::{Args, Subcommand};
use webpilot::protocol::{Command, ResponseData};
use webpilot::types::line_safe;

use crate::output::CommandOutput;
use crate::transport::{Transport, lift_error};

#[derive(Args)]
pub struct CookieArgs {
    #[command(subcommand)]
    pub command: CookieCommand,
}

#[derive(Subcommand)]
pub enum CookieCommand {
    /// List all cookies for a URL.
    List { url: String },
    /// Get a specific cookie.
    Get { url: String, name: String },
    /// Set a cookie.
    Set {
        url: String,
        name: String,
        #[arg(allow_hyphen_values = true)]
        value: String,
        #[arg(long)]
        httponly: bool,
        #[arg(long)]
        secure: bool,
        /// SameSite attribute: strict, lax, or none. Omit for Chrome's default.
        #[arg(long)]
        same_site: Option<webpilot::types::SameSite>,
        /// Absolute expiry as Unix-epoch seconds. Omit for a session cookie.
        #[arg(long)]
        expires: Option<f64>,
    },
    /// Delete a cookie.
    Delete { url: String, name: String },
}

pub async fn run<T: Transport>(transport: &mut T, args: CookieArgs) -> Result<CommandOutput> {
    match args.command {
        CookieCommand::List { ref url } | CookieCommand::Get { ref url, .. } => {
            let name_filter = match &args.command {
                CookieCommand::Get { name, .. } => Some(name.clone()),
                _ => None,
            };
            let result = transport
                .send(Command::CookieList { url: url.clone() })
                .await?;
            let cookies = match result {
                ResponseData::Cookies { cookies } => cookies,
                ResponseData::Error { error } => return Err(error.into()),
                _ => anyhow::bail!("Unexpected response shape"),
            };

            let filtered: Vec<_> = if let Some(ref n) = name_filter {
                cookies.into_iter().filter(|c| &c.name == n).collect()
            } else {
                cookies
            };

            // `cookie get NAME` asks for a SPECIFIC cookie — an absent one is a
            // typed not-found (exit 4), like `find` / `action click` on a missing
            // target, not a `(0 cookies)` list reported as success (exit 0) that an
            // agent checking an auth cookie's presence by exit code would misread.
            // `cookie list` (no name) keeps returning an empty list — listing zero
            // is a valid result, not a miss.
            if let Some(ref n) = name_filter
                && filtered.is_empty()
            {
                return Err(webpilot::WebPilotError::CookieNotFound { name: n.clone() }.into());
            }

            let human_lines: Vec<String> = filtered
                .iter()
                .map(|c| {
                    let flags = [
                        if c.secure { "secure" } else { "" },
                        if c.http_only { "httpOnly" } else { "" },
                    ]
                    .iter()
                    .filter(|s| !s.is_empty())
                    .copied()
                    .collect::<Vec<_>>()
                    .join(",");
                    let preview: String = c.value.chars().take(40).collect();
                    format!(
                        "{} = {} [{}] {}",
                        line_safe(&c.name),
                        line_safe(&preview),
                        line_safe(&c.domain),
                        flags
                    )
                })
                .collect();

            if name_filter.is_some() && filtered.len() == 1 {
                return Ok(CommandOutput::Content {
                    stdout: format!(
                        "{} = {}",
                        line_safe(&filtered[0].name),
                        line_safe(&filtered[0].value)
                    ),
                    json: serde_json::to_value(&filtered[0])?,
                });
            }

            Ok(CommandOutput::List {
                items: serde_json::to_value(&filtered)?,
                human_lines,
                summary: format!("({} cookies)", filtered.len()),
            })
        }
        CookieCommand::Set {
            url,
            name,
            value,
            httponly,
            secure,
            same_site,
            expires,
        } => {
            simple(
                transport,
                Command::CookieSet {
                    url,
                    name,
                    value,
                    http_only: httponly,
                    secure,
                    same_site,
                    expires,
                },
                "Cookie set",
            )
            .await
        }
        CookieCommand::Delete { url, name } => {
            simple(
                transport,
                Command::CookieDelete { url, name },
                "Cookie deleted",
            )
            .await
        }
    }
}

async fn simple<T: Transport>(
    transport: &mut T,
    cmd: Command,
    ok_msg: &str,
) -> Result<CommandOutput> {
    let result = transport.send(cmd).await?;
    match result {
        ResponseData::CookieResult { success, error } => {
            lift_error(success, error, CommandOutput::Ok(ok_msg.into()))
        }
        ResponseData::Error { error } => Err(error.into()),
        _ => anyhow::bail!("Unexpected response shape"),
    }
}
