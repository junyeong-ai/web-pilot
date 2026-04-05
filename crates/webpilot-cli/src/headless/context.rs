use crate::cdp::CdpClient;
use crate::commands;
use crate::output::CommandOutput;
use anyhow::Result;

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
pub(crate) const DEFAULT_TTL_SECS: u64 = 3600; // 1 hour

pub(crate) fn context_hash(name: &str) -> String {
    use std::hash::{Hash, Hasher};
    let user = std::env::var("USER").unwrap_or_else(|_| "default".into());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    format!("{user}:{name}").hash(&mut hasher);
    format!("{:012x}", hasher.finish())
}

pub(crate) fn context_file_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(webpilot::OUTPUT_DIR).join(format!("ctx-{}.json", context_hash(name)))
}

pub(crate) async fn resolve_context_target(browser: &CdpClient, name: &str) -> Result<String> {
    let file_path = context_file_path(name);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Chrome PID check
    let chrome_pid = crate::session::read_pid();

    // Check cached context file
    if let Ok(data) = std::fs::read_to_string(&file_path)
        && let Ok(mut entry) = serde_json::from_str::<ContextEntry>(&data)
    {
        // PID mismatch -> stale
        if entry.chrome_pid != chrome_pid {
            let _ = std::fs::remove_file(&file_path);
        } else {
            // Verify context is alive via CDP
            let live = browser.get_browser_contexts().await?;
            if live.contains(&entry.browser_context_id) {
                // Context alive -> verify target
                let targets = browser.get_targets().await?;
                let has_target = targets.iter().any(|t| {
                    t.get("targetId").and_then(|v| v.as_str()) == Some(&entry.target_id)
                        && t.get("type").and_then(|v| v.as_str()) == Some("page")
                });

                let tid = if has_target {
                    entry.target_id.clone()
                } else {
                    // Target closed -> recreate in same context
                    browser
                        .create_target_in_context(&entry.browser_context_id, "about:blank")
                        .await?
                };

                // Update last_used
                entry.target_id = tid.clone();
                entry.last_used = now;
                let _ = std::fs::write(&file_path, serde_json::to_string(&entry)?);
                return Ok(tid);
            } else {
                let _ = std::fs::remove_file(&file_path);
            }
        }
    }

    // New context creation
    gc_expired_contexts(browser, chrome_pid).await;

    // Check active context count
    let count = std::fs::read_dir(std::path::Path::new(webpilot::OUTPUT_DIR))
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().starts_with("ctx-"))
                .count()
        })
        .unwrap_or(0);
    if count >= MAX_CONTEXTS {
        anyhow::bail!(
            "Maximum {MAX_CONTEXTS} contexts active. Close unused: webpilot context close NAME"
        );
    }

    let ctx_id = browser.create_browser_context().await?;
    let tid = browser
        .create_target_in_context(&ctx_id, "about:blank")
        .await?;

    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
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
    let _ = std::fs::create_dir_all(std::path::Path::new(webpilot::OUTPUT_DIR));
    std::fs::write(&file_path, serde_json::to_string(&entry)?)?;

    Ok(tid)
}

pub(crate) async fn gc_expired_contexts(browser: &CdpClient, current_pid: i32) {
    let ttl = std::env::var("WEBPILOT_CONTEXT_TTL")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_TTL_SECS);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let dir = std::path::Path::new(webpilot::OUTPUT_DIR);
    let Ok(entries) = std::fs::read_dir(dir) else {
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
        // PID mismatch -> stale
        if ctx.chrome_pid != current_pid {
            let _ = std::fs::remove_file(entry.path());
            continue;
        }
        // TTL expired — dispose browser context via CDP, then remove file
        if now - ctx.last_used > ttl {
            let _ = browser
                .dispose_browser_context(&ctx.browser_context_id)
                .await;
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

pub(crate) async fn run(
    browser: &CdpClient,
    args: commands::context::ContextArgs,
) -> Result<CommandOutput> {
    match args.command {
        commands::context::ContextCommand::List => {
            let dir = std::path::Path::new(webpilot::OUTPUT_DIR);
            let mut contexts = Vec::new();
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let fname = entry.file_name().to_string_lossy().to_string();
                    if fname.starts_with("ctx-")
                        && fname.ends_with(".json")
                        && let Ok(data) = std::fs::read_to_string(entry.path())
                        && let Ok(ctx) = serde_json::from_str::<ContextEntry>(&data)
                    {
                        contexts.push(ctx);
                    }
                }
            }
            let human_lines: Vec<String> = if contexts.is_empty() {
                vec!["No active contexts".into()]
            } else {
                contexts
                    .iter()
                    .map(|ctx| {
                        let age = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs()
                            - ctx.created_at;
                        format!("  {} ({}s old) — {}", ctx.name, age, ctx.cwd)
                    })
                    .collect()
            };
            let summary = if contexts.is_empty() {
                String::new()
            } else {
                format!("{} context(s)", contexts.len())
            };
            Ok(CommandOutput::List {
                items: serde_json::json!(contexts),
                human_lines,
                summary,
            })
        }
        commands::context::ContextCommand::Close { name, all } => {
            if all {
                // Dispose all browser contexts and remove files
                let dir = std::path::Path::new(webpilot::OUTPUT_DIR);
                let mut count = 0;
                if let Ok(entries) = std::fs::read_dir(dir) {
                    for entry in entries.filter_map(|e| e.ok()) {
                        let fname = entry.file_name().to_string_lossy().to_string();
                        if fname.starts_with("ctx-") && fname.ends_with(".json") {
                            if let Ok(data) = std::fs::read_to_string(entry.path())
                                && let Ok(ctx) = serde_json::from_str::<ContextEntry>(&data)
                            {
                                let _ = browser
                                    .dispose_browser_context(&ctx.browser_context_id)
                                    .await;
                            }
                            let _ = std::fs::remove_file(entry.path());
                            count += 1;
                        }
                    }
                }
                Ok(CommandOutput::Ok(format!("Closed {count} context(s)")))
            } else if let Some(name) = name {
                let file_path = context_file_path(&name);
                if let Ok(data) = std::fs::read_to_string(&file_path) {
                    if let Ok(ctx) = serde_json::from_str::<ContextEntry>(&data) {
                        let _ = browser
                            .dispose_browser_context(&ctx.browser_context_id)
                            .await;
                    }
                    let _ = std::fs::remove_file(&file_path);
                    Ok(CommandOutput::Ok(format!("Closed context '{name}'")))
                } else {
                    anyhow::bail!("Context '{name}' not found");
                }
            } else {
                anyhow::bail!("Specify a context name or --all");
            }
        }
    }
}
