use anyhow::{Context, Result};
use webpilot::ipc::{self, IpcError};
use webpilot::protocol::{Command, ResponseData};

use crate::output::CommandOutput;

pub async fn run() -> Result<CommandOutput> {
    let request = serde_json::to_value(webpilot::protocol::Request::new(1, Command::Status))?;

    match ipc::send_request(&request).await {
        Ok(response) => {
            let resp: webpilot::protocol::Response =
                serde_json::from_value(response).context("Invalid response")?;

            match resp.result {
                ResponseData::Status {
                    connected,
                    tab_url,
                    tab_title,
                    extension_version,
                } => {
                    let mut human_parts = vec![
                        format!("Connected: {connected}"),
                        format!("Extension: v{extension_version}"),
                    ];
                    if let Some(ref url) = tab_url {
                        human_parts.push(format!(
                            "Active tab: {}",
                            tab_title.as_deref().unwrap_or_default()
                        ));
                        human_parts.push(format!("URL: {url}"));
                    }
                    Ok(CommandOutput::Data {
                        json: serde_json::json!({
                            "connected": connected,
                            "extension_version": extension_version,
                            "tab_url": tab_url,
                            "tab_title": tab_title,
                        }),
                        human: human_parts.join("\n"),
                    })
                }
                _ => anyhow::bail!("Unexpected response type"),
            }
        }
        Err(e) => {
            let (error_msg, hint) = diagnose(&e);
            Ok(CommandOutput::Data {
                json: serde_json::json!({"connected": false, "error": error_msg}),
                human: format!("{error_msg}\n{hint}"),
            })
        }
    }
}

fn diagnose(error: &IpcError) -> (String, String) {
    match error {
        IpcError::HostNotRunning(path) => {
            let hint = match check_nm_manifest() {
                ManifestState::NotFound => {
                    "  NM manifest not found.\n  Run: webpilot install --extension-id <ID>".into()
                }
                ManifestState::InvalidJson => {
                    "  NM manifest is corrupted (invalid JSON).\n  Run: webpilot install --extension-id <ID>"
                        .into()
                }
                ManifestState::BinaryMissing(bin_path) => {
                    format!(
                        "  NM manifest binary not found: {bin_path}\n  Run: cargo install --path crates/webpilot-cli"
                    )
                }
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

    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return ManifestState::NotFound,
    };

    let manifest: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return ManifestState::InvalidJson,
    };

    if let Some(bin_path) = manifest.get("path").and_then(|v| v.as_str())
        && !std::path::Path::new(bin_path).exists()
    {
        return ManifestState::BinaryMissing(bin_path.to_string());
    }

    ManifestState::Ok
}
