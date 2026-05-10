//! Status command — shared rendering for both modes.
//!
//! `render` produces a `CommandOutput` from a typed `Status` payload.
//! `run` is the browser-mode entry point; the headless path in `cli.rs`
//! reaches `render` directly after opening a `LocalTransport`.

use anyhow::Result;
use webpilot::ipc::IpcError;
use webpilot::protocol::{Command, ResponseData, RunMode};

use crate::output::CommandOutput;
use crate::transport::{IpcTransport, Transport};

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
    if let Some(ref t) = tab_title {
        human.push_str(&format!("\nTab: {t}"));
    }
    if let Some(ref u) = tab_url {
        human.push_str(&format!("\nURL: {u}"));
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
    let mut transport = IpcTransport::new();
    match transport.send(Command::Status).await {
        Ok(ResponseData::Status {
            connected,
            mode,
            tab_url,
            tab_title,
            chrome_version,
            extension_version,
        }) => Ok(render(
            connected,
            mode,
            tab_url,
            tab_title,
            chrome_version,
            extension_version,
            None,
        )),
        Ok(ResponseData::Error { error }) => Err(error.into()),
        Ok(_) => anyhow::bail!("Unexpected response shape"),
        Err(e) => Ok(diagnose(&e)),
    }
}

/// Render a connection failure as an informative status (not an error).
fn diagnose(error: &anyhow::Error) -> CommandOutput {
    let (msg, hint) = match error.downcast_ref::<IpcError>() {
        Some(IpcError::HostNotRunning(path)) => {
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
        Some(IpcError::Timeout) => (
            "Timed out waiting for host response.".into(),
            "  The NM host may be stuck. Reload the extension in Chrome.".into(),
        ),
        Some(IpcError::ConnectionClosed) => (
            "Host closed the connection.".into(),
            "  The NM host may have crashed. Reload the extension in Chrome.".into(),
        ),
        Some(IpcError::Io(e)) => (
            format!("Socket error: {e}"),
            "  Check that the NM host is running and the socket is accessible.".into(),
        ),
        Some(IpcError::Json(e)) => (
            format!("Invalid response from host: {e}"),
            "  The NM host sent malformed data. Try reloading the extension.".into(),
        ),
        None => (
            format!("Status query failed: {error:#}"),
            "  Check that the host is running and reachable.".into(),
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
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
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
