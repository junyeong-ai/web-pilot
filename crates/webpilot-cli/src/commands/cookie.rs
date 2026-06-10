use anyhow::Result;
use clap::{Args, Subcommand};
use webpilot::protocol::{Command, ResponseData};
use webpilot::types::{CookieInfo, SameSite, line_safe};

use crate::output::CommandOutput;
use crate::transport::{Transport, lift_error};

/// One agent-facing cookie row: name, a value preview, the domain+path scope,
/// and the security/lifetime flags. Every attribute `CookieInfo` carries is
/// shown — `secure`/`httpOnly`/`hostOnly`, the `sameSite` mode (when set), and
/// the lifetime (`expires=<unix>` or `session`) — so an agent reasoning about a
/// cookie's scope or auth behaviour never has to drop to the JSON to see it.
fn cookie_row(c: &CookieInfo) -> String {
    let mut flags: Vec<String> = Vec::new();
    if c.secure {
        flags.push("secure".into());
    }
    if c.http_only {
        flags.push("httpOnly".into());
    }
    if c.host_only {
        flags.push("hostOnly".into());
    }
    if !matches!(c.same_site, SameSite::Unspecified) {
        flags.push(format!("sameSite={}", c.same_site));
    }
    flags.push(match c.expiration {
        Some(ts) => format!("expires={}", ts as i64),
        None => "session".into(),
    });
    let preview: String = c.value.chars().take(40).collect();
    format!(
        "{} = {} [{}{}] {}",
        line_safe(&c.name),
        line_safe(&preview),
        line_safe(&c.domain),
        line_safe(&c.path),
        flags.join(",")
    )
}

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

            let human_lines: Vec<String> = filtered.iter().map(cookie_row).collect();

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

#[cfg(test)]
mod tests {
    use super::*;

    fn cookie(name: &str) -> CookieInfo {
        CookieInfo {
            name: name.into(),
            value: "v".into(),
            domain: "example.com".into(),
            path: "/".into(),
            secure: false,
            http_only: false,
            same_site: SameSite::Unspecified,
            expiration: None,
            host_only: false,
        }
    }

    #[test]
    fn cookie_row_shows_every_scope_and_security_attribute() {
        let mut c = cookie("sid");
        c.path = "/admin".into();
        c.secure = true;
        c.http_only = true;
        c.host_only = true;
        c.same_site = SameSite::Strict;
        c.expiration = Some(1_799_000_000.0);
        let row = cookie_row(&c);
        // The full scope (domain+path), every flag, the sameSite mode, and a
        // concrete expiry — none of it left only in the JSON.
        assert!(row.contains("[example.com/admin]"), "scope: {row}");
        assert!(row.contains("secure"), "{row}");
        assert!(row.contains("httpOnly"), "{row}");
        assert!(row.contains("hostOnly"), "{row}");
        assert!(row.contains("sameSite=strict"), "{row}");
        assert!(row.contains("expires=1799000000"), "{row}");
    }

    #[test]
    fn cookie_row_marks_session_and_hides_unspecified_samesite() {
        let row = cookie_row(&cookie("tmp"));
        assert!(
            row.contains("session"),
            "a cookie with no expiry is a session cookie: {row}"
        );
        assert!(
            !row.contains("sameSite"),
            "an Unspecified sameSite is not rendered: {row}"
        );
        assert!(
            !row.contains("secure") && !row.contains("httpOnly"),
            "absent flags are not rendered: {row}"
        );
    }
}
