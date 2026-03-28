use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use webpilot::ipc;
use webpilot::protocol::{Command, ResponseData};

use crate::output::OutputMode;

#[derive(Args)]
pub struct CookiesArgs {
    #[command(subcommand)]
    pub action: CookiesAction,
}

#[derive(Subcommand)]
pub enum CookiesAction {
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

pub async fn run(args: CookiesArgs, output_mode: OutputMode) -> Result<()> {
    match args.action {
        CookiesAction::List { ref url } | CookiesAction::Get { ref url, .. } => {
            let name_filter = match &args.action {
                CookiesAction::Get { name, .. } => Some(name.clone()),
                _ => None,
            };
            let url = url.clone();

            let request = serde_json::to_value(webpilot::protocol::Request {
                id: 1,
                command: Command::GetCookies { url },
            })?;

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

                    match output_mode {
                        OutputMode::Human => {
                            for c in &filtered {
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
                                println!("{} = {} [{}] {}", c.name, val_preview, c.domain, flags);
                            }
                            eprintln!("({} cookies)", filtered.len());
                        }
                        OutputMode::Json => {
                            println!("{}", serde_json::to_string_pretty(&filtered)?);
                        }
                    }
                }
                ResponseData::Error { message, .. } => anyhow::bail!("{message}"),
                _ => anyhow::bail!("Unexpected response"),
            }
        }

        CookiesAction::Set {
            url,
            name,
            value,
            httponly,
            secure,
        } => {
            let request = serde_json::to_value(webpilot::protocol::Request {
                id: 1,
                command: Command::SetCookie {
                    url,
                    name,
                    value,
                    http_only: httponly,
                    secure,
                },
            })?;

            let response = ipc::send_request(&request)
                .await
                .context("Host not running")?;
            let resp: webpilot::protocol::Response = serde_json::from_value(response)?;

            match resp.result {
                ResponseData::CookieResult { success, error } => {
                    match output_mode {
                        OutputMode::Human => {
                            if success {
                                eprintln!("Cookie set");
                            } else {
                                eprintln!(
                                    "{}",
                                    crate::output::format_error(&error.unwrap_or_default(), None)
                                );
                            }
                        }
                        OutputMode::Json => println!(
                            "{}",
                            serde_json::json!({"success": success, "error": error})
                        ),
                    }
                    if !success {
                        std::process::exit(1);
                    }
                }
                ResponseData::Error { message, .. } => anyhow::bail!("{message}"),
                _ => anyhow::bail!("Unexpected response"),
            }
        }

        CookiesAction::Delete { url, name } => {
            let request = serde_json::to_value(webpilot::protocol::Request {
                id: 1,
                command: Command::DeleteCookie { url, name },
            })?;

            let response = ipc::send_request(&request)
                .await
                .context("Host not running")?;
            let resp: webpilot::protocol::Response = serde_json::from_value(response)?;

            match resp.result {
                ResponseData::CookieResult { success, error } => {
                    match output_mode {
                        OutputMode::Human => {
                            if success {
                                eprintln!("Cookie deleted");
                            } else {
                                eprintln!(
                                    "{}",
                                    crate::output::format_error(&error.unwrap_or_default(), None)
                                );
                            }
                        }
                        OutputMode::Json => println!(
                            "{}",
                            serde_json::json!({"success": success, "error": error})
                        ),
                    }
                    if !success {
                        std::process::exit(1);
                    }
                }
                ResponseData::Error { message, .. } => anyhow::bail!("{message}"),
                _ => anyhow::bail!("Unexpected response"),
            }
        }
    }

    Ok(())
}
