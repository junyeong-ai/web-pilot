//! Per-user CDP browser context entries — file-backed state for
//! multi-agent isolation. Used by `LocalTransport::open` (to resolve a
//! context to a page target) and by the `context list/close` CLI command.

use crate::cdp::CdpClient;
use anyhow::Result;
use webpilot::dirs;

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct ContextEntry {
    pub name: String,
    pub cwd: String,
    pub browser_context_id: String,
    pub target_id: String,
    pub chrome_pid: i32,
    pub created_at: u64,
    pub last_used: u64,
}

pub(crate) const MAX_CONTEXTS: usize = 16;
pub(crate) const DEFAULT_TTL_SECS: u64 = 3_600;

pub(crate) fn context_hash(name: &str) -> String {
    use std::hash::{Hash, Hasher};
    let user = std::env::var("USER").unwrap_or_else(|_| "default".into());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    format!("{user}:{name}").hash(&mut hasher);
    format!("{:012x}", hasher.finish())
}

pub(crate) fn context_file_path(name: &str) -> std::path::PathBuf {
    dirs::contexts_dir().join(format!("ctx-{}.json", context_hash(name)))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Resolve a named context to a page target id. Creates the context + page
/// if missing; revives the page if it was closed; surfaces a structured
/// error if the cap is reached.
pub(crate) async fn resolve_context_target(browser: &CdpClient, name: &str) -> Result<String> {
    let file_path = context_file_path(name);
    let now = now_secs();
    let chrome_pid = crate::session::read_pid();

    if let Ok(data) = std::fs::read_to_string(&file_path)
        && let Ok(mut entry) = serde_json::from_str::<ContextEntry>(&data)
    {
        if entry.chrome_pid != chrome_pid {
            let _ = std::fs::remove_file(&file_path);
        } else {
            let live = browser.get_browser_contexts().await?;
            if live.contains(&entry.browser_context_id) {
                let targets = browser.get_targets().await?;
                let has_target = targets.iter().any(|t| {
                    t.get("targetId").and_then(|v| v.as_str()) == Some(&entry.target_id)
                        && t.get("type").and_then(|v| v.as_str()) == Some("page")
                });
                let tid = if has_target {
                    entry.target_id.clone()
                } else {
                    browser
                        .create_target_in_context(&entry.browser_context_id, "about:blank")
                        .await?
                };
                entry.target_id = tid.clone();
                entry.last_used = now;
                let _ = std::fs::write(&file_path, serde_json::to_string(&entry)?);
                return Ok(tid);
            } else {
                let _ = std::fs::remove_file(&file_path);
            }
        }
    }

    gc_expired_contexts(browser, chrome_pid).await;

    let count = std::fs::read_dir(dirs::contexts_dir())
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().starts_with("ctx-"))
                .count()
        })
        .unwrap_or(0);
    if count >= MAX_CONTEXTS {
        return Err(webpilot::WebPilotError::Session {
            detail: format!(
                "maximum {MAX_CONTEXTS} contexts active. Close unused: webpilot context close NAME"
            ),
        }
        .into());
    }

    let ctx_id = browser.create_browser_context().await?;
    let tid = browser
        .create_target_in_context(&ctx_id, "about:blank")
        .await?;

    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let entry = ContextEntry {
        name: name.to_string(),
        cwd,
        browser_context_id: ctx_id,
        target_id: tid.clone(),
        chrome_pid,
        created_at: now,
        last_used: now,
    };
    std::fs::write(&file_path, serde_json::to_string(&entry)?)?;

    Ok(tid)
}

pub(crate) async fn gc_expired_contexts(browser: &CdpClient, current_pid: i32) {
    let ttl = std::env::var("WEBPILOT_CONTEXT_TTL")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_TTL_SECS);
    let now = now_secs();
    let Ok(entries) = std::fs::read_dir(dirs::contexts_dir()) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let fname = entry.file_name().to_string_lossy().to_string();
        if !fname.starts_with("ctx-") || !fname.ends_with(".json") {
            continue;
        }
        let Ok(data) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(ctx) = serde_json::from_str::<ContextEntry>(&data) else {
            let _ = std::fs::remove_file(entry.path());
            continue;
        };
        if ctx.chrome_pid != current_pid {
            let _ = std::fs::remove_file(entry.path());
            continue;
        }
        if now.saturating_sub(ctx.last_used) > ttl {
            let _ = browser
                .dispose_browser_context(&ctx.browser_context_id)
                .await;
            let _ = std::fs::remove_file(entry.path());
        }
    }
}
