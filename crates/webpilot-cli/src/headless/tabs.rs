use crate::commands;
use crate::output::CommandOutput;
use anyhow::Result;

use super::HeadlessContext;

pub(crate) async fn run(
    ctx: &HeadlessContext,
    args: commands::tabs::TabsArgs,
) -> Result<CommandOutput> {
    let cdp = &ctx.browser;

    match args.command {
        None => {
            // List tabs — filter by browserContextId when in a context
            let targets = cdp.get_targets().await?;
            let pages: Vec<_> = targets
                .iter()
                .filter(|t| {
                    let is_page = t.get("type").and_then(|v| v.as_str()) == Some("page");
                    if !is_page {
                        return false;
                    }
                    if let Some(ref ctx_id) = ctx.browser_context_id {
                        t.get("browserContextId").and_then(|v| v.as_str()) == Some(ctx_id)
                    } else {
                        true
                    }
                })
                .cloned()
                .collect();
            let human_lines: Vec<String> = pages
                .iter()
                .map(|t| {
                    let title = t.get("title").and_then(|v| v.as_str()).unwrap_or("");
                    let url = t.get("url").and_then(|v| v.as_str()).unwrap_or("");
                    let id = t.get("targetId").and_then(|v| v.as_str()).unwrap_or("");
                    format!("  [{id}] {title} — {url}")
                })
                .collect();
            Ok(CommandOutput::List {
                items: serde_json::json!(pages),
                human_lines,
                summary: String::new(),
            })
        }
        Some(commands::tabs::TabCommand::New { url }) => {
            // Create tab in the correct browser context
            let result = if let Some(ref ctx_id) = ctx.browser_context_id {
                cdp.create_target_in_context(ctx_id, &url).await?
            } else {
                let r = cdp
                    .send("Target.createTarget", Some(serde_json::json!({"url": url})))
                    .await?;
                r.get("targetId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            Ok(CommandOutput::Data {
                json: serde_json::json!({"success": true, "targetId": result}),
                human: format!("New tab: {result}"),
            })
        }
        Some(commands::tabs::TabCommand::Switch { tab_id }) => {
            cdp.send(
                "Target.activateTarget",
                Some(serde_json::json!({"targetId": tab_id})),
            )
            .await?;
            Ok(CommandOutput::Ok(format!("Switched to {tab_id}")))
        }
        Some(commands::tabs::TabCommand::Close { tab_id }) => {
            cdp.send(
                "Target.closeTarget",
                Some(serde_json::json!({"targetId": tab_id})),
            )
            .await?;
            Ok(CommandOutput::Ok(format!("Closed {tab_id}")))
        }
        Some(commands::tabs::TabCommand::Find { url: pattern }) => {
            let targets = cdp.get_targets().await?;
            let pattern_str = pattern.replace('*', "");
            let filtered: Vec<_> = targets
                .iter()
                .filter(|t| {
                    let is_page = t.get("type").and_then(|v| v.as_str()) == Some("page");
                    if !is_page {
                        return false;
                    }
                    if let Some(ref ctx_id) = ctx.browser_context_id {
                        t.get("browserContextId").and_then(|v| v.as_str()) == Some(ctx_id)
                    } else {
                        true
                    }
                })
                .collect();
            if let Some(t) = filtered.iter().find(|t| {
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
                Ok(CommandOutput::Data {
                    json: serde_json::json!({"success": true, "targetId": tid}),
                    human: format!("Switched to {tid}"),
                })
            } else {
                anyhow::bail!("No tab matching '{pattern}'");
            }
        }
    }
}
