//! Shared IPC helper for browser-mode commands.

use anyhow::{Context, Result};

/// Send a protocol command via IPC and return the typed response.
#[allow(dead_code)]
pub async fn send_command(
    command: webpilot::protocol::Command,
) -> Result<webpilot::protocol::Response> {
    let request = serde_json::to_value(webpilot::protocol::Request::new(1, command))?;
    let response = webpilot::ipc::send_request(&request)
        .await
        .context("WebPilot host not running. Run: webpilot install")?;
    serde_json::from_value(response).context("Invalid response from host")
}
