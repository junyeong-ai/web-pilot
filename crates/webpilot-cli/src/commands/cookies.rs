use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use webpilot::ipc;
use webpilot::protocol::{Command, ResponseData};

use crate::output::CommandOutput;

#[derive(Args)]
pub struct CookiesArgs {
    #[command(subcommand)]
    pub command: CookieCommand,
}

#[derive(Subcommand)]
pub enum CookieCommand {
    /// List all cookies for a URL
    List { url: String },
    /// Get a specific cookie
    Get { url: String, name: String },
    /// Set a cookie
    Set {
        url: String,
        name: String,
        value: String,
        /// Mark as HttpOnly
        #[arg(long)]
        httponly: bool,
        /// Mark as Secure
        #[arg(long)]
        secure: bool,
    },
    /// Delete a cookie
    Delete { url: String, name: String },
}

pub async fn run(args: CookiesArgs) -> Result<CommandOutput> {
    match args.command {
        CookieCommand::List { ref url } | CookieCommand::Get { ref url, .. } => {
            let name_filter = match &args.command {
                CookieCommand::Get { name, .. } => Some(name.clone()),
                _ => None,
            };
            let url = url.clone();

            let request = serde_json::to_value(webpilot::protocol::Request::new(
                1,
                Command::CookieList { url },
            ))?;

            let response = ipc::send_request(&request)
                .await
                .context("WebPilot host not running")?;

            let resp: webpilot::protocol::Response = serde_json::from_value(response)?;

            match resp.result {
                ResponseData::Cookies { cookies } => {
                    let filtered: Vec<_> = if let Some(ref name) = name_filter {
                        cookies.into_iter().filter(|c| &c.name == name).collect()
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
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(",");
                            let val_preview = if c.value.len() > 40 {
                                &c.value[..40]
                            } else {
                                &c.value
                            };
                            format!("{} = {} [{}] {}", c.name, val_preview, c.domain, flags)
                        })
                        .collect();
                    let summary = format!("({} cookies)", filtered.len());

                    // For single cookie Get, use Content pattern
                    if name_filter.is_some() && filtered.len() == 1 {
                        return Ok(CommandOutput::Content {
                            stdout: format!(
                                "{} = {}",
                                filtered[0].name, filtered[0].value
                            ),
                            json: serde_json::to_value(&filtered[0])?,
                        });
                    }

                    Ok(CommandOutput::List {
                        items: serde_json::to_value(&filtered)?,
                        human_lines,
                        summary,
                    })
                }
                ResponseData::Error { message, .. } => anyhow::bail!("{message}"),
                _ => anyhow::bail!("Unexpected response"),
            }
        }

        CookieCommand::Set {
            url,
            name,
            value,
            httponly,
            secure,
        } => {
            let request = serde_json::to_value(webpilot::protocol::Request::new(
                1,
                Command::CookieSet {
                    url,
                    name,
                    value,
                    http_only: httponly,
                    secure,
                },
            ))?;

            let response = ipc::send_request(&request)
                .await
                .context("Host not running")?;
            let resp: webpilot::protocol::Response = serde_json::from_value(response)?;

            match resp.result {
                ResponseData::CookieResult { success, error } => {
                    if !success {
                        if let Some(ref err) = error {
                            anyhow::bail!("{}", crate::output::format_error(err));
                        } else {
                            anyhow::bail!("Unknown error");
                        }
                    }
                    Ok(CommandOutput::Ok("Cookie set".into()))
                }
                ResponseData::Error { message, .. } => anyhow::bail!("{message}"),
                _ => anyhow::bail!("Unexpected response"),
            }
        }

        CookieCommand::Delete { url, name } => {
            let request = serde_json::to_value(webpilot::protocol::Request::new(
                1,
                Command::CookieDelete { url, name },
            ))?;

            let response = ipc::send_request(&request)
                .await
                .context("Host not running")?;
            let resp: webpilot::protocol::Response = serde_json::from_value(response)?;

            match resp.result {
                ResponseData::CookieResult { success, error } => {
                    if !success {
                        if let Some(ref err) = error {
                            anyhow::bail!("{}", crate::output::format_error(err));
                        } else {
                            anyhow::bail!("Unknown error");
                        }
                    }
                    Ok(CommandOutput::Ok("Cookie deleted".into()))
                }
                ResponseData::Error { message, .. } => anyhow::bail!("{message}"),
                _ => anyhow::bail!("Unexpected response"),
            }
        }
    }
}
