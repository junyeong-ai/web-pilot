use anyhow::Result;
use clap::{Args, Subcommand};
use webpilot::protocol::{Command, ResponseData};
use webpilot::types::{CookieInfo, SameSite, line_safe, line_safe_clip};

use crate::output::CommandOutput;
use crate::transport::{Transport, lift_error};

/// One agent-facing cookie row: name, a value preview, the domain+path scope,
/// and the security/lifetime flags. Every attribute `CookieInfo` carries is
/// shown — `secure`/`httpOnly`/`hostOnly`, the `sameSite` mode (when set), the
/// CHIPS partition (when partitioned), and the lifetime (`expires=<unix>` or
/// `session`) — so an agent reasoning about a cookie's scope or auth behaviour
/// never has to drop to the JSON to see it.
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
    if let Some(pk) = &c.partition_key {
        // The partition is part of the cookie's IDENTITY: a partitioned `sid`
        // and an unpartitioned `sid` are different cookies, so the row must
        // tell them apart.
        let xsite = if pk.has_cross_site_ancestor {
            ",xsite"
        } else {
            ""
        };
        flags.push(format!(
            "partitioned={}{xsite}",
            line_safe_clip(&pk.top_level_site, 200)
        ));
    }
    flags.push(match c.expiration {
        Some(ts) => format!("expires={}", ts as i64),
        None => "session".into(),
    });
    // Preview the value, marking truncation with `…` so a long value (a JWT, a
    // session blob) is never silently clipped — a list row that looked complete
    // but wasn't would mislead an agent comparing it to the full JSON. The exact
    // value is `cookie get NAME`.
    let preview: String = c.value.chars().take(40).collect();
    let value_cell = if preview.chars().count() < c.value.chars().count() {
        format!("{preview}…")
    } else {
        preview
    };
    format!(
        "{} = {} [{}{}] {}",
        line_safe_clip(&c.name, 200),
        line_safe(&value_cell),
        line_safe_clip(&c.domain, 200),
        line_safe_clip(&c.path, 200),
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
                    note: None,
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
            let result = transport
                .send(Command::CookieDelete {
                    url,
                    name: name.clone(),
                })
                .await?;
            match result {
                ResponseData::CookieResult {
                    success,
                    deleted,
                    error,
                } => {
                    // The count makes "all of them" verifiable: same-name
                    // cookies coexist across scopes (domain vs host-only,
                    // paths) and every matching scope was deleted.
                    let n = deleted.unwrap_or(1);
                    lift_error(
                        success,
                        error,
                        CommandOutput::Ok(format!("Deleted {n} cookie(s) named '{name}'")),
                    )
                }
                ResponseData::Error { error } => Err(error.into()),
                _ => anyhow::bail!("Unexpected response shape"),
            }
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
        ResponseData::CookieResult { success, error, .. } => {
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
            partition_key: None,
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
    fn cookie_row_names_the_partition() {
        // The partition is part of the cookie's IDENTITY — a partitioned `sid`
        // and an unpartitioned `sid` are different cookies, and the row must
        // tell them apart (the unpartitioned row carries no partition flag).
        let mut c = cookie("sid");
        c.partition_key = Some(webpilot::types::PartitionKey {
            top_level_site: "https://example.com".into(),
            has_cross_site_ancestor: false,
        });
        let row = cookie_row(&c);
        assert!(row.contains("partitioned=https://example.com"), "{row}");
        assert!(
            !row.contains(",xsite"),
            "first-party key has no xsite: {row}"
        );

        c.partition_key.as_mut().unwrap().has_cross_site_ancestor = true;
        let row = cookie_row(&c);
        assert!(
            row.contains("partitioned=https://example.com,xsite"),
            "a cross-site-ancestor key is marked: {row}"
        );
        assert!(
            !cookie_row(&cookie("sid")).contains("partitioned"),
            "an unpartitioned cookie carries no partition flag"
        );
    }

    #[test]
    fn cookie_row_marks_a_truncated_value_so_it_is_never_silently_clipped() {
        // A short value renders whole, no marker.
        let short = cookie("s");
        let row = cookie_row(&short);
        assert!(
            row.contains("= v ") && !row.contains('…'),
            "short value whole: {row}"
        );
        // A 40+ char value is previewed with a trailing `…` — an agent
        // comparing the row to the full JSON must see it was clipped, not read
        // a partial value as complete.
        let mut long = cookie("s");
        // 'Z' appears nowhere else in the row (name/domain/path/flags), so the
        // count below measures exactly the value preview.
        long.value = "Z".repeat(80);
        let row = cookie_row(&long);
        assert!(
            row.contains('…'),
            "a clipped value must carry the … marker: {row}"
        );
        assert!(
            row.matches('Z').count() == 40,
            "exactly the 40-char preview is shown before the marker: {row}"
        );
    }

    #[test]
    fn cookie_row_caps_page_controlled_name_domain_and_path() {
        // A cookie's name/domain/path are page-controlled (a page sets them via
        // `document.cookie` / `Set-Cookie`), so a hostile page could flood the
        // row with a megabyte name — `line_safe_clip` bounds every one of them at
        // the same 200-char cap the DOM footer uses. The exact value stays in the
        // JSON; the human row must never become one unbounded line.
        let mut c = cookie(&"N".repeat(5000));
        c.domain = "D".repeat(5000);
        c.path = format!("/{}", "P".repeat(5000));
        let row = cookie_row(&c);
        assert!(
            row.matches('N').count() <= 200,
            "name capped at 200: {} Ns",
            row.matches('N').count()
        );
        assert!(
            row.matches('D').count() <= 200,
            "domain capped at 200: {} Ds",
            row.matches('D').count()
        );
        assert!(
            row.matches('P').count() <= 200,
            "path capped at 200: {} Ps",
            row.matches('P').count()
        );
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
