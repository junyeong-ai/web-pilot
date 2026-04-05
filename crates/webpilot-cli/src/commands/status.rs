use anyhow::{Context, Result};
use webpilot::ipc;
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
        Err(_) => Ok(CommandOutput::Data {
            json: serde_json::json!({"connected": false}),
            human: "Not connected. Is the Chrome extension installed?\n  1. Run: webpilot install\n  2. Load the extension in Chrome (chrome://extensions)".into(),
        }),
    }
}
