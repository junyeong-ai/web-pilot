//! Per-user runtime directories.
//!
//! All WebPilot artifacts (sockets, PID files, captured screenshots,
//! context state) live under a single per-user root, with mode 0700.
//!
//! Resolution order:
//! 1. `WEBPILOT_HOME` — explicit override (any platform).
//! 2. Linux/BSD: `$XDG_RUNTIME_DIR/webpilot` (preferred — tmpfs, mode 0700).
//!    Fallback: `$XDG_CACHE_HOME/webpilot` then `$HOME/.cache/webpilot`.
//! 3. macOS: `$HOME/Library/Caches/webpilot`.
//! 4. Last resort: `/tmp/webpilot-<user>`.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Per-user WebPilot root. Created on first call, mode 0700.
pub fn root() -> &'static Path {
    ROOT.get_or_init(resolve_root)
}

fn resolve_root() -> PathBuf {
    let dir = std::env::var_os("WEBPILOT_HOME")
        .map(PathBuf::from)
        .or_else(xdg_runtime_dir)
        .or_else(macos_caches)
        .or_else(xdg_cache_dir)
        .or_else(home_cache_dir)
        .unwrap_or_else(tmp_fallback);
    let _ = std::fs::create_dir_all(&dir);
    apply_owner_only(&dir);
    dir
}

fn xdg_runtime_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        return None;
    }
    let p = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?);
    p.exists().then(|| p.join("webpilot"))
}

fn macos_caches() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    Some(
        PathBuf::from(std::env::var_os("HOME")?)
            .join("Library")
            .join("Caches")
            .join("webpilot"),
    )
}

fn xdg_cache_dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("XDG_CACHE_HOME")?).join("webpilot"))
}

fn home_cache_dir() -> Option<PathBuf> {
    Some(
        PathBuf::from(std::env::var_os("HOME")?)
            .join(".cache")
            .join("webpilot"),
    )
}

fn tmp_fallback() -> PathBuf {
    let user = std::env::var("USER").unwrap_or_else(|_| "default".into());
    PathBuf::from(format!("/tmp/webpilot-{user}"))
}

#[cfg(unix)]
fn apply_owner_only(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn apply_owner_only(_: &Path) {}

/// Subdirectory under the root. Creates if missing, mode 0700.
fn subdir(name: &str) -> PathBuf {
    let p = root().join(name);
    let _ = std::fs::create_dir_all(&p);
    apply_owner_only(&p);
    p
}

/// Sockets, PID files, lock files (ephemeral runtime state).
pub fn runtime_dir() -> PathBuf {
    subdir("runtime")
}

/// Per-context state files (`ctx-<hash>.json`).
pub fn contexts_dir() -> PathBuf {
    subdir("contexts")
}

/// Saved artifacts: screenshots, PDFs, sessions.
pub fn artifacts_dir() -> PathBuf {
    subdir("artifacts")
}

/// Default IPC socket path. Overridable via `WEBPILOT_SOCKET`.
pub fn socket_path() -> PathBuf {
    if let Some(s) = std::env::var_os("WEBPILOT_SOCKET") {
        return PathBuf::from(s);
    }
    runtime_dir().join("ipc.sock")
}

/// Headless Chrome PID file.
pub fn pid_file() -> PathBuf {
    runtime_dir().join("headless.pid")
}

/// Headless Chrome WebSocket URL file.
pub fn ws_url_file() -> PathBuf {
    runtime_dir().join("headless.ws")
}

/// Chrome launch advisory lock file.
pub fn launch_lock_file() -> PathBuf {
    runtime_dir().join("launch.lock")
}

/// Headless Chrome user-data directory.
pub fn chrome_profile_dir() -> PathBuf {
    subdir("chrome-profile")
}
