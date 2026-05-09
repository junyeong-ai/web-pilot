use anyhow::Result;
use clap::{Args, Subcommand};
use webpilot::protocol::{Command, ResponseData};

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
        value: String,
        #[arg(long)]
        httponly: bool,
        #[arg(long)]
        secure: bool,
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
                    format!("{} = {} [{}] {}", c.name, preview, c.domain, flags)
                })
                .collect();

            if name_filter.is_some() && filtered.len() == 1 {
                return Ok(CommandOutput::Content {
                    stdout: format!("{} = {}", filtered[0].name, filtered[0].value),
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
        } => simple(
            transport,
            Command::CookieSet {
                url,
                name,
                value,
                http_only: httponly,
                secure,
            },
            "Cookie set",
        )
        .await,
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
