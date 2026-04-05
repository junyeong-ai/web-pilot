use crate::cdp::CdpClient;
use crate::commands;
use crate::output::OutputMode;
use anyhow::Result;

pub(crate) async fn run(
    cdp: &CdpClient,
    args: commands::tabs::TabsArgs,
    output_mode: OutputMode,
) -> Result<()> {
    match args.command {
        None => {
            // List tabs
            let targets = cdp.get_targets().await?;
            let pages: Vec<_> = targets
                .iter()
                .filter(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
                .collect();
            match output_mode {
                OutputMode::Human => {
                    for t in &pages {
                        let title = t.get("title").and_then(|v| v.as_str()).unwrap_or("");
                        let url = t.get("url").and_then(|v| v.as_str()).unwrap_or("");
                        let id = t.get("targetId").and_then(|v| v.as_str()).unwrap_or("");
                        eprintln!("  [{id}] {title} — {url}");
                    }
                }
                OutputMode::Json => println!("{}", serde_json::json!(pages)),
            }
        }
        Some(commands::tabs::TabsCommand::New { url }) => {
            let result = cdp
                .send("Target.createTarget", Some(serde_json::json!({"url": url})))
                .await?;
            let target_id = result
                .get("targetId")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            match output_mode {
                OutputMode::Human => eprintln!("New tab: {target_id}"),
                OutputMode::Json => println!(
                    "{}",
                    serde_json::json!({"success": true, "targetId": target_id})
                ),
            }
        }
        Some(commands::tabs::TabsCommand::Switch { tab_id }) => {
            cdp.send(
                "Target.activateTarget",
                Some(serde_json::json!({"targetId": tab_id})),
            )
            .await?;
            match output_mode {
                OutputMode::Human => eprintln!("Switched to {tab_id}"),
                OutputMode::Json => println!("{}", serde_json::json!({"success": true})),
            }
        }
        Some(commands::tabs::TabsCommand::Close { tab_id }) => {
            cdp.send(
                "Target.closeTarget",
                Some(serde_json::json!({"targetId": tab_id})),
            )
            .await?;
            match output_mode {
                OutputMode::Human => eprintln!("Closed {tab_id}"),
                OutputMode::Json => println!("{}", serde_json::json!({"success": true})),
            }
        }
        Some(commands::tabs::TabsCommand::Find { url: pattern }) => {
            let targets = cdp.get_targets().await?;
            let pattern_str = pattern.replace('*', "");
            if let Some(t) = targets.iter().find(|t| {
                t.get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .contains(&pattern_str)
            }) {
                let tid = t.get("targetId").and_then(|v| v.as_str()).unwrap_or("");
                cdp.send(
                    "Target.activateTarget",
                    Some(serde_json::json!({"targetId": tid})),
                )
                .await?;
                match output_mode {
                    OutputMode::Human => eprintln!("Switched to {tid}"),
                    OutputMode::Json => {
                        println!("{}", serde_json::json!({"success": true, "targetId": tid}))
                    }
                }
            } else {
                anyhow::bail!("No tab matching '{pattern}'");
            }
        }
    }
    Ok(())
}
