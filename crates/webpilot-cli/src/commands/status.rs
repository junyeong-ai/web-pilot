use anyhow::{Context, Result};
use webpilot::ipc;
use webpilot::protocol::{Command, ResponseData};

use crate::output::OutputMode;

pub async fn run(output_mode: OutputMode) -> Result<()> {
    let request = serde_json::to_value(webpilot::protocol::Request {
        id: 1,
        command: Command::Status,
    })?;

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
                } => match output_mode {
                    OutputMode::Human => {
                        println!("Connected: {connected}");
                        println!("Extension: v{extension_version}");
                        if let Some(url) = tab_url {
                            println!("Active tab: {}", tab_title.unwrap_or_default());
                            println!("URL: {url}");
                        }
                    }
                    OutputMode::Json => {
                        println!(
                            "{}",
                            serde_json::json!({
                                "connected": connected,
                                "extension_version": extension_version,
                                "tab_url": tab_url,
                                "tab_title": tab_title,
                            })
                        );
                    }
                },
                _ => anyhow::bail!("Unexpected response type"),
            }
        }
        Err(_) => {
            match output_mode {
                OutputMode::Human => {
                    eprintln!("Not connected. Is the Chrome extension installed?");
                    eprintln!("  1. Run: webpilot install");
                    eprintln!("  2. Load the extension in Chrome (chrome://extensions)");
                }
                OutputMode::Json => {
                    println!("{}", serde_json::json!({"connected": false}));
                }
            }
            std::process::exit(1);
        }
    }

    Ok(())
}
