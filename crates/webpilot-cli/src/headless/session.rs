use crate::cdp::CdpClient;
use crate::commands;
use crate::output::OutputMode;
use anyhow::Result;

use super::call_bridge;

pub(crate) async fn run(
    cdp: &CdpClient,
    args: commands::session::SessionArgs,
    output_mode: OutputMode,
) -> Result<()> {
    match args.command {
        commands::session::SessionCommand::Export { output } => {
            let cookies = cdp.get_cookies().await?;
            let storage = call_bridge(
                cdp,
                &serde_json::json!({"type": "exportStorage"}).to_string(),
            )
            .await
            .unwrap_or(serde_json::json!({"localStorage": {}, "sessionStorage": {}}));
            let data = serde_json::json!({
                "version": 1, "exported_at": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
                "cookies": cookies, "local_storage": storage.get("localStorage"), "session_storage": storage.get("sessionStorage"),
            });
            let dir = std::path::Path::new(webpilot::OUTPUT_DIR);
            let _ = std::fs::create_dir_all(dir);
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let path = if let Some(ref p) = output {
                std::path::PathBuf::from(p)
            } else {
                dir.join(format!("session_{ts}.json"))
            };
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::write(&path, serde_json::to_string_pretty(&data)?)?;
            match output_mode {
                OutputMode::Human => eprintln!("Session exported: {}", path.display()),
                OutputMode::Json => {
                    println!("{}", serde_json::json!({"path": path.to_string_lossy()}))
                }
            }
        }
        commands::session::SessionCommand::Import { path } => {
            let content = std::fs::read_to_string(&path)
                .map_err(|e| anyhow::anyhow!("Cannot read session file: {e}"))?;
            let data: serde_json::Value = serde_json::from_str(&content)?;
            // Import cookies via CDP
            if let Some(cookies) = data.get("cookies").and_then(|v| v.as_array()) {
                for c in cookies {
                    let _ = cdp.send("Network.setCookie", Some(c.clone())).await;
                }
                match output_mode {
                    OutputMode::Human => eprintln!("Imported {} cookies", cookies.len()),
                    OutputMode::Json => println!(
                        "{}",
                        serde_json::json!({"success": true, "cookies_imported": cookies.len()})
                    ),
                }
            }
            // Import localStorage via bridge
            if let Some(ls) = data.get("local_storage") {
                let msg = serde_json::json!({"type": "importStorage", "localStorage": ls});
                call_bridge(cdp, &msg.to_string()).await?;
            }
        }
    }
    Ok(())
}
