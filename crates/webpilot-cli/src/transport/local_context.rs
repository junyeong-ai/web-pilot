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
    flock_file(
        &format!("ctx-{}.lock", context_hash(name)),
        &format!("context '{name}'"),
    )
}

/// Serialize store-wide mutations (GC + cap check + create) across processes.
/// The per-name lock alone cannot enforce `MAX_CONTEXTS`: two processes
/// creating *different* names would each count the same existing entries and
/// both pass the cap. Lock order is always name → store; nothing acquires
/// them the other way, so the pair cannot deadlock.
fn lock_store() -> Result<std::fs::File> {
    flock_file("store.lock", "context store")
}

fn flock_file(filename: &str, what: &str) -> Result<std::fs::File> {
    use std::os::unix::io::AsRawFd;
    let dir = dirs::contexts_dir();
    std::fs::create_dir_all(&dir)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(dir.join(filename))?;
    // SAFETY: flock() is a POSIX advisory lock; no memory-safety implications.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        anyhow::bail!("failed to lock {what}: {}", std::io::Error::last_os_error());
    }
    Ok(file)
}

/// Non-blocking exclusive lock on a context. `Some` = acquired (the context is
/// idle — no other process, and no concurrent resolve, holds it). `None` =
/// would-block (a process is actively resolving/using it). GC uses this to
/// never dispose a context that is in use: it holds `lock_store` then probes
/// each context lock **without waiting**, so the store→name order it takes here
/// (the reverse of resolve's name→store) cannot deadlock — a non-blocking probe
/// never waits on a held name lock, it just skips it.
fn try_lock_context(name: &str) -> Option<std::fs::File> {
    use std::os::unix::io::AsRawFd;
    let dir = dirs::contexts_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(dir.join(format!("ctx-{}.lock", context_hash(name))))
        .ok()?;
    // SAFETY: flock() is a POSIX advisory lock; no memory-safety implications.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return None; // EWOULDBLOCK: actively in use — leave it alone.
    }
    Some(file)
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
    // Held for the whole resolution so concurrent same-name callers can't both
    // create a context (double-checked: the existing-entry path below re-reads
    // under the lock).
    let _lock = lock_context(name)?;

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
                let tid = if has_target {
                    entry.target_id.clone()
                } else {
                    browser
                        .create_target_in_context(&entry.browser_context_id, "about:blank")
                        .await?
                };
                entry.target_id = tid.clone();
                entry.last_used = now;
                let _ = dirs::atomic_write(&file_path, serde_json::to_string(&entry)?.as_bytes());
                return Ok(tid);
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
    // Atomic: a crash mid-write must not leave a torn entry that the next
    // resolve can't parse — that would orphan the live context just created.
    dirs::atomic_write(&file_path, serde_json::to_string(&entry)?.as_bytes())?;

    Ok(tid)
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
            // Never dispose a context that is actively in use: `last_used` only
            // bounds *idle* time, but a process re-using a long-idle context
            // (or running a command longer than the TTL) refreshes it only at
            // the resolve, so a concurrent sweep could otherwise read a stale
            // timestamp and dispose a live context out from under it. The held
            // per-name lock is the liveness signal; if we can't take it without
            // waiting, the context is in use — skip it this sweep. Held through
            // disposal so a resolve can't start mid-dispose.
            let Some(_held) = try_lock_context(&ctx.name) else {
                continue;
            };
            // Deleting the metadata is only safe once the CDP context is
            // actually gone — otherwise a live context would be orphaned in
            // Chrome with no record left to close it through.
            let disposed = browser
                .dispose_browser_context(&ctx.browser_context_id)
                .await;
            let gone = disposed.is_ok()
                || !matches!(
                    browser.get_browser_contexts().await,
                    Ok(live) if live.contains(&ctx.browser_context_id)
                );
            if gone {
                super::local::clear_context_state(&ctx.browser_context_id);
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}
