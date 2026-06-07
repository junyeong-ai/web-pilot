mod assets;
mod cdp;
mod cli;
mod commands;
mod host;
mod lockfile;
mod mcp;
mod output;
mod policy;
pub mod session;
#[cfg(test)]
mod test_support;
mod transport;

/// WebPilot: Browser control tool for AI agents.
///
/// The same binary serves three roles, dispatched at startup:
/// - **CLI**: user-invoked command (default).
/// - **NM Host**: launched by Chrome via Native Messaging. Detected by a
///   strict match on `argv[1]` against the documented Chrome contract:
///   `chrome-extension://<32-char id [a-p]>/?` (no other shape is valid).
///
/// Errors that escape the entry handlers are always `WebPilotError`. Their
/// `exit_code()` method gives the CLI exit code; `Display` gives AI-friendly
/// guidance. There is no string-matching fallback.
#[tokio::main]
async fn main() {
    let result = if is_nm_host_invocation() {
        host::run_host().await
    } else {
        cli::run_cli().await
    };

    if let Err(e) = result {
        let err = into_webpilot_error(e);
        let mode = output::detect_output_mode(std::env::args().any(|a| a == "--json"));
        output::render_error(&err, mode);
        std::process::exit(err.exit_code());
    }
}

/// Strict check against the Chrome Native Messaging API contract.
///
/// Chrome invokes the host binary with the calling extension's origin as
/// `argv[1]`: `chrome-extension://<32-char id>/`. The id alphabet is exactly
/// `a..=p`. We reject anything else so a stray argv that happens to contain
/// "chrome-extension://" cannot trigger host mode.
fn is_nm_host_invocation() -> bool {
    let Some(arg) = std::env::args().nth(1) else {
        return false;
    };
    let Some(rest) = arg.strip_prefix("chrome-extension://") else {
        return false;
    };
    let id = rest.trim_end_matches('/');
    id.len() == 32 && id.chars().all(|c| c.is_ascii_lowercase() && c <= 'p')
}

/// Coerce an `anyhow::Error` into a structured `WebPilotError`.
///
/// Errors raised by WebPilot's own code paths use `WebPilotError` directly and
/// pass through unchanged. Errors from third-party crates land in `Other`
/// with their `Display` text — we never inspect the string to guess a code.
pub(crate) fn into_webpilot_error(e: anyhow::Error) -> webpilot::WebPilotError {
    if let Some(we) = e.downcast_ref::<webpilot::WebPilotError>() {
        return we.clone();
    }
    webpilot::WebPilotError::Other {
        detail: format!("{e:#}"),
    }
}

#[cfg(test)]
mod nm_detection_tests {
    fn check(arg: &str) -> bool {
        let Some(rest) = arg.strip_prefix("chrome-extension://") else {
            return false;
        };
        let id = rest.trim_end_matches('/');
        id.len() == 32 && id.chars().all(|c| c.is_ascii_lowercase() && c <= 'p')
    }

    #[test]
    fn accepts_valid_extension_origin() {
        assert!(check(
            "chrome-extension://abcdefghijklmnopabcdefghijklmnop/"
        ));
        assert!(check("chrome-extension://abcdefghijklmnopabcdefghijklmnop"));
    }

    #[test]
    fn rejects_short_id() {
        assert!(!check("chrome-extension://abc/"));
    }

    #[test]
    fn rejects_id_with_invalid_chars() {
        assert!(!check(
            "chrome-extension://ABCDEFGHIJKLMNOPABCDEFGHIJKLMNOP/"
        ));
        assert!(!check(
            "chrome-extension://qrstuvwxyzabcdefqrstuvwxyzabcdef/"
        )); // beyond [a-p]
    }

    #[test]
    fn rejects_arbitrary_strings_with_substring() {
        assert!(!check("--option=chrome-extension://abc/"));
        assert!(!check("https://example.com"));
    }
}
