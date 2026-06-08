//! Status command — shared rendering for both modes.
//!
//! `render` produces a `CommandOutput` from a typed `Status` payload.
//! `run` is the browser-mode entry point; the headless path in `cli.rs`
//! reaches `render` directly after opening a `LocalTransport`.

use anyhow::Result;
use webpilot::WebPilotError;
use webpilot::ipc::{self, IpcError};
use webpilot::protocol::{Command, Request, Response, ResponseData, RunMode};
use webpilot::types::line_safe;

use crate::output::CommandOutput;

/// Render a typed `Status` response into the unified `CommandOutput` shape.
pub fn render(
    connected: bool,
    mode: RunMode,
    tab_url: Option<String>,
    tab_title: Option<String>,
    chrome_version: Option<String>,
    extension_version: Option<String>,
    context_label: Option<&str>,
) -> CommandOutput {
    let mut human = format!("Mode: {mode}");
    human.push_str(&format!("\nConnected: {connected}"));
    if let Some(ctx) = context_label {
        human.push_str(&format!("\nContext: {ctx}"));
    }
    if let Some(ref v) = chrome_version {
        human.push_str(&format!("\nChrome: {v}"));
    }
    if let Some(ref v) = extension_version {
        human.push_str(&format!("\nExtension: v{v}"));
    }
    // `tab_title`/`tab_url` are page-controlled; line-safe them so a crafted
    // title can't embed a newline and forge a status line (the `--json` path is
    // already safe via JSON escaping).
    if let Some(ref t) = tab_title {
        human.push_str(&format!("\nTab: {}", line_safe(t)));
    }
    if let Some(ref u) = tab_url {
        human.push_str(&format!("\nURL: {}", line_safe(u)));
    }

    CommandOutput::Data {
        json: serde_json::json!({
            "connected": connected,
            "mode": mode.to_string(),
            "tab_url": tab_url,
            "tab_title": tab_title,
            "chrome_version": chrome_version,
            "extension_version": extension_version,
            "context": context_label,
        }),
        human,
    }
}

/// Browser-mode status entry point. Produces an informative response even
/// when the IPC connection cannot be established.
pub async fn run() -> Result<CommandOutput> {
    // Reach the IPC layer directly rather than through `IpcTransport`, which
    // flattens every `IpcError` into `ConnectionLost`. `diagnose` needs the typed
    // variant — host-not-running vs timeout vs closed — to give the specific,
    // actionable hint; the generic transport error would erase it before the only
    // code that reads it.
    let request = Request::new(1, Command::Status);
    let raw = match ipc::send_request(&serde_json::to_value(&request)?).await {
        Ok(raw) => raw,
        Err(e) => return Ok(diagnose(&e)),
    };
    let response: Response =
        serde_json::from_value(raw).map_err(|e| WebPilotError::ConnectionLost {
            detail: format!("malformed reply from the Native Messaging host: {e}"),
        })?;
    match response.result {
        ResponseData::Status {
            connected,
            mode,
            tab_url,
            tab_title,
            chrome_version,
            extension_version,
        } => Ok(render(
            connected,
            mode,
            tab_url,
            tab_title,
            chrome_version,
            extension_version,
            None,
        )),
        ResponseData::Error { error } => Err(error.into()),
        _ => anyhow::bail!("Unexpected response shape"),
    }
}

/// Render a connection failure as an informative status (not an error). Takes the
/// typed `IpcError` directly — every variant maps to a specific hint, so there is
/// no generic catch-all to fall through to.
fn diagnose(error: &IpcError) -> CommandOutput {
    let (msg, hint) = match error {
        IpcError::HostNotRunning(path) => {
            let hint = match check_nm_manifest() {
                ManifestState::NotFound => {
                    "  NM manifest not found.\n  Run: webpilot setup nm-host --extension-id <ID>".into()
                }
                ManifestState::InvalidJson => {
                    "  NM manifest is corrupted (invalid JSON).\n  Run: webpilot setup nm-host --extension-id <ID>"
                        .into()
                }
                ManifestState::BinaryMissing(p) => format!(
                    "  NM manifest binary not found: {p}\n  Re-register with: webpilot setup nm-host --extension-id <ID>"
                ),
                ManifestState::Ok => {
                    "  NM manifest OK. Ensure the extension is loaded and active in Chrome.".into()
                }
            };
            (format!("Host not running (socket: {path})"), hint)
        }
        IpcError::Timeout => (
            "Timed out waiting for host response.".into(),
            "  The NM host may be stuck. Reload the extension in Chrome.".into(),
        ),
        IpcError::ConnectionClosed => (
            "Host closed the connection.".into(),
            "  The NM host may have crashed. Reload the extension in Chrome.".into(),
        ),
        IpcError::Io(e) => (
            format!("Socket error: {e}"),
            "  Check that the NM host is running and the socket is accessible.".into(),
        ),
        IpcError::Json(e) => (
            format!("Invalid response from host: {e}"),
            "  The NM host sent malformed data. Try reloading the extension.".into(),
        ),
    };

    CommandOutput::Data {
        json: serde_json::json!({"connected": false, "mode": "browser", "error": msg}),
        human: format!("{msg}\n{hint}"),
    }
}

enum ManifestState {
    NotFound,
    InvalidJson,
    BinaryMissing(String),
    Ok,
}

fn check_nm_manifest() -> ManifestState {
    // No home directory means no per-user Chrome NM dir to inspect — report
    // not-found rather than probing a fallback path that cannot be Chrome's.
    let home = match std::env::var("HOME") {
        Ok(h) if !h.is_empty() => h,
        _ => return ManifestState::NotFound,
    };
    let path = if cfg!(target_os = "macos") {
        std::path::PathBuf::from(&home).join(
            "Library/Application Support/Google/Chrome/NativeMessagingHosts/com.webpilot.host.json",
        )
    } else {
        std::path::PathBuf::from(&home)
            .join(".config/google-chrome/NativeMessagingHosts/com.webpilot.host.json")
    };

    let Ok(content) = std::fs::read_to_string(&path) else {
        return ManifestState::NotFound;
    };
    let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&content) else {
        return ManifestState::InvalidJson;
    };
    if let Some(bin) = manifest.get("path").and_then(|v| v.as_str())
        && !std::path::Path::new(bin).exists()
    {
        return ManifestState::BinaryMissing(bin.to_owned());
    }
    ManifestState::Ok
}
