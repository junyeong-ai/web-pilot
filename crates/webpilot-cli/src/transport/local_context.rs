//! Per-user CDP browser context entries — file-backed state for
//! multi-agent isolation. Used by `LocalTransport::open` (to resolve a
//! context to a page target) and by the `context list/close` CLI command.

use crate::cdp::CdpClient;
use anyhow::{Context, Result};
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

/// Serialize resolution of one context name across processes. Without this two
/// agents racing on the same name would each create a CDP browser context and
/// only the last would be recorded, leaking the other until Chrome exits. The
/// returned file handle holds an exclusive `flock` until it is dropped.
fn lock_context(name: &str) -> Result<std::fs::File> {
    let guard = crate::lockfile::flock_exclusive(&context_lock_path(name), false)
        .with_context(|| format!("lock context '{name}'"))?;
    Ok(guard.expect("a blocking flock returns Some"))
}

/// Serialize store-wide mutations (GC + cap check + create) across processes.
/// The per-name lock alone cannot enforce `MAX_CONTEXTS`: two processes
/// creating *different* names would each count the same existing entries and
/// both pass the cap. Lock order is always name → store; nothing acquires
/// them the other way, so the pair cannot deadlock.
fn lock_store() -> Result<std::fs::File> {
    let guard = crate::lockfile::flock_exclusive(&dirs::contexts_dir().join("store.lock"), false)
        .context("lock context store")?;
    Ok(guard.expect("a blocking flock returns Some"))
}

/// The per-context serialization-lock file (held only across a resolve). Hashed
/// so an arbitrary context name maps to a fixed, filesystem-safe path.
fn context_lock_path(name: &str) -> std::path::PathBuf {
    dirs::contexts_dir().join(format!("ctx-{}.lock", context_hash(name)))
}

/// The per-context liveness-lock file. Distinct from the serialization lock so
/// the two never contend: a live transport holds this *shared* for its whole
/// lifetime, while resolves keep taking the serialization lock briefly and the
/// GC probes this one exclusively — none of which blocks another resolve.
fn context_live_path(name: &str) -> std::path::PathBuf {
    dirs::contexts_dir().join(format!("ctx-{}.live", context_hash(name)))
}

/// Shared liveness lock on a context, held by a live transport for its entire
/// lifetime. Shared, so any number of transports can hold it at once without
/// blocking one another (it is *not* a mutex on the context); it exists purely
/// so the GC's non-blocking *exclusive* probe fails while any transport is
/// alive. Blocks only against the GC holding it exclusively mid-disposal — a
/// brief wait, never another resolve.
fn lock_context_live(name: &str) -> Result<std::fs::File> {
    let guard = crate::lockfile::flock_shared(&context_live_path(name), false)
        .with_context(|| format!("liveness-lock context '{name}'"))?;
    Ok(guard.expect("a blocking flock returns Some"))
}

/// Non-blocking exclusive probe of the liveness lock. `Some` = acquired (no live
/// transport holds the shared lock — the context is idle and can be disposed).
/// `None` = would-block (at least one transport is alive). GC holds `lock_store`
/// then probes each context **without waiting**, so the store→name order it
/// takes here (the reverse of resolve's name→store) cannot deadlock — a
/// non-blocking probe never waits on a held lock, it just skips it.
fn try_lock_live(name: &str) -> Option<std::fs::File> {
    // `Err` (a real open/lock failure) and `Ok(None)` (held) both mean "don't
    // touch it" for a best-effort GC probe, so flatten them together.
    crate::lockfile::flock_exclusive(&context_live_path(name), true)
        .ok()
        .flatten()
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
pub(crate) async fn resolve_context_target(
    browser: &CdpClient,
    name: &str,
) -> Result<(String, String, std::fs::File)> {
    // The serialization lock is held only for this resolution: concurrent
    // same-name callers can't both create a context (double-checked: the
    // existing-entry path below re-reads under the lock). It is dropped on
    // return — the transport's liveness signal is the *separate* shared lock.
    let _lock = lock_context(name)?;
    // The shared liveness lock, taken now and handed to the caller for the whole
    // transport lifetime. Held from here so a concurrent GC cannot dispose this
    // context anywhere in the resolve — closing even the read-to-refresh window.
    // Shared, so it never blocks another resolve; the GC's non-blocking
    // exclusive probe simply fails while we hold it. Dropped (released) if this
    // resolve errors before a transport is built.
    let live_lock = lock_context_live(name)?;

    let file_path = context_file_path(name);
    let now = now_secs();
    let chrome_pid = crate::session::read_pid();

    if let Ok(data) = std::fs::read_to_string(&file_path)
        && let Ok(mut entry) = serde_json::from_str::<ContextEntry>(&data)
    {
        if entry.chrome_pid != chrome_pid {
            super::local::clear_context_state(&entry.browser_context_id);
            let _ = std::fs::remove_file(&file_path);
        } else {
            let live = browser.get_browser_contexts().await?;
            if live.contains(&entry.browser_context_id) {
                let targets = browser.get_targets().await?;
                let has_target = targets.iter().any(|t| {
                    t.get("targetId").and_then(|v| v.as_str()) == Some(&entry.target_id)
                        && t.get("type").and_then(|v| v.as_str()) == Some("page")
                });
                let (tid, created) = if has_target {
                    (entry.target_id.clone(), false)
                } else {
                    let tid = browser
                        .create_target_in_context(&entry.browser_context_id, "about:blank")
                        .await?;
                    (tid, true)
                };
                entry.target_id = tid.clone();
                entry.last_used = now;
                // The entry is the cross-process reuse record: a failed write would
                // leave the next process reading a stale target id and creating yet
                // another target — an accumulating orphan. Surface the failure, and
                // if this resolve just created the target, close it so the command
                // fails atomically with nothing leaked.
                if let Err(e) =
                    dirs::atomic_write(&file_path, serde_json::to_string(&entry)?.as_bytes())
                {
                    if created {
                        let _ = browser
                            .send(
                                "Target.closeTarget",
                                Some(serde_json::json!({ "targetId": tid })),
                            )
                            .await;
                    }
                    return Err(e.into());
                }
                return Ok((tid, entry.browser_context_id, live_lock));
            } else {
                super::local::clear_context_state(&entry.browser_context_id);
                let _ = std::fs::remove_file(&file_path);
            }
        }
    }

    // GC, cap check, and create are one atomic store mutation.
    let _store_lock = lock_store()?;

    gc_expired_contexts(browser, chrome_pid).await;

    let count = std::fs::read_dir(dirs::contexts_dir())
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    n.starts_with("ctx-") && n.ends_with(".json")
                })
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
    // Once the CDP browser context exists, ANY later failure (creating its
    // page, or persisting the metadata that records it) must dispose it —
    // otherwise it leaks in Chrome with no record left to reach or close it
    // through. Build the rest under a guard that tears the context down on the
    // error path, and forgets it on success.
    let built: Result<String> = async {
        let tid = browser
            .create_target_in_context(&ctx_id, "about:blank")
            .await?;
        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let entry = ContextEntry {
            name: name.to_string(),
            cwd,
            browser_context_id: ctx_id.clone(),
            target_id: tid.clone(),
            chrome_pid,
            created_at: now,
            last_used: now,
        };
        // Atomic: a crash mid-write must not leave a torn entry that the next
        // resolve can't parse — that would orphan the live context just created.
        dirs::atomic_write(&file_path, serde_json::to_string(&entry)?.as_bytes())?;
        Ok(tid)
    }
    .await;

    if built.is_err() {
        let _ = browser.dispose_browser_context(&ctx_id).await;
    }
    let tid = built?;
    Ok((tid, ctx_id, live_lock))
}

pub(crate) async fn gc_expired_contexts(browser: &CdpClient, current_pid: i32) {
    let ttl = webpilot::settings::get().context.ttl.as_secs();
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
            super::local::clear_context_state(&ctx.browser_context_id);
            let _ = std::fs::remove_file(entry.path());
            continue;
        }
        if now.saturating_sub(ctx.last_used) > ttl {
            // Never dispose a context that is actively in use. `last_used` only
            // bounds *idle* time and is refreshed only at the resolve, so a
            // long-lived transport (an MCP server reusing one transport past the
            // TTL) would otherwise look abandoned. Every live transport holds the
            // context's *shared* liveness lock for its whole lifetime, so this
            // non-blocking *exclusive* probe succeeds only when none is alive: if
            // it fails, the context is live — skip it. Held exclusively through
            // disposal so a resolve can't bind mid-dispose.
            let Some(_held) = try_lock_live(&ctx.name) else {
                continue;
            };
            // Deleting the metadata is only safe once the CDP context is
            // actually gone — otherwise a live context would be orphaned in
            // Chrome with no record left to close it through.
            let disposed = browser
                .dispose_browser_context(&ctx.browser_context_id)
                .await;
            // Delete the record only when disposal is CONFIRMED: the call
            // succeeded, or a re-list positively shows the context absent. A
            // failed `get_browser_contexts` (the CDP socket wedged or dropped
            // mid-sweep — e.g. a Chrome crash-restart between the two calls) is
            // NOT proof it's gone; treating that error as "gone" would delete the
            // only record that can close it, orphaning a possibly-live context
            // that then leaks until Chrome quits. On an unknown result, keep the
            // record and let the next sweep retry.
            let gone = disposed.is_ok()
                || matches!(
                    browser.get_browser_contexts().await,
                    Ok(live) if !live.contains(&ctx.browser_context_id)
                );
            if gone {
                super::local::clear_context_state(&ctx.browser_context_id);
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}
