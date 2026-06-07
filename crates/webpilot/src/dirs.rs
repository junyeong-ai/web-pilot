//! Per-user runtime directories.
//!
//! WebPilot keeps two per-user roots:
//!
//! - **Cache root** (`root()`): ephemeral state — sockets, PID files,
//!   screenshots, headless Chrome profile. Safe for the OS to evict.
//! - **Data root** (`data_root()`): durable state — the unpacked Chrome
//!   extension that the user has loaded into Chrome. Must survive cache
//!   eviction; if it disappears the extension breaks.
//!
//! Each path has two accessors:
//!
//! - `*()` — *materialise* the path on first call (creating ancestors with
//!   mode 0700) and memoise where the path is process-wide constant. Use
//!   from setup/runtime code that needs the location to exist.
//! - `*_path()` — *pure* path computation, no filesystem side effects. Use
//!   from inspection-only code (e.g. `webpilot uninstall`) that must not
//!   create state just by looking.
//!
//! Cache root resolution:
//! 1. `WEBPILOT_HOME` — explicit override (any platform).
//! 2. Linux/BSD: `$XDG_RUNTIME_DIR/webpilot` (preferred — tmpfs, mode 0700).
//!    Fallback: `$XDG_CACHE_HOME/webpilot` then `$HOME/.cache/webpilot`.
//! 3. macOS: `$HOME/Library/Caches/webpilot`.
//! 4. Last resort: `/tmp/webpilot-<user>`.
//!
//! Data root resolution:
//! 1. `WEBPILOT_DATA_HOME` — explicit override.
//! 2. macOS: `$HOME/Library/Application Support/webpilot`.
//! 3. Linux/BSD: `$XDG_DATA_HOME/webpilot` then `$HOME/.local/share/webpilot`.
//! 4. Last resort: `/tmp/webpilot-<user>-data`.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

// --- Layout (single source of truth for on-disk names) ----------------------

const RUNTIME_SUBDIR: &str = "runtime";
const CONTEXTS_SUBDIR: &str = "contexts";
const ARTIFACTS_SUBDIR: &str = "artifacts";
const CHROME_PROFILE_SUBDIR: &str = "chrome-profile";
const EXTENSION_SUBDIR: &str = "extension";

const PID_FILENAME: &str = "headless.pid";
const WS_URL_FILENAME: &str = "headless.ws";
const LAUNCH_LOCK_FILENAME: &str = "launch.lock";
const SOCKET_FILENAME: &str = "ipc.sock";
const CONFIG_FILENAME: &str = "config.toml";

// --- Cache root -------------------------------------------------------------

static ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Per-user WebPilot cache root. Materialises with mode 0700 on first call.
pub fn root() -> &'static Path {
    ROOT.get_or_init(|| materialise(root_path(), Owner::User))
}

/// Pure cache root path — no filesystem side effects.
pub fn root_path() -> PathBuf {
    env_path("WEBPILOT_HOME")
        .or_else(xdg_runtime_dir)
        .or_else(macos_caches)
        .or_else(xdg_cache_dir)
        .or_else(home_cache_dir)
        .unwrap_or_else(tmp_cache_fallback)
}

// --- Cache subdirs ----------------------------------------------------------

pub fn runtime_dir() -> PathBuf {
    materialise(runtime_dir_path(), Owner::User)
}
pub fn runtime_dir_path() -> PathBuf {
    root_path().join(RUNTIME_SUBDIR)
}

pub fn contexts_dir() -> PathBuf {
    materialise(root().join(CONTEXTS_SUBDIR), Owner::User)
}

pub fn artifacts_dir() -> PathBuf {
    materialise(root().join(ARTIFACTS_SUBDIR), Owner::User)
}

pub fn chrome_profile_dir() -> PathBuf {
    materialise(root().join(CHROME_PROFILE_SUBDIR), Owner::User)
}

// --- Cache files ------------------------------------------------------------

/// Default IPC socket path. Overridable via `WEBPILOT_SOCKET`.
/// Materialises the runtime dir so a server can `bind()` immediately.
pub fn socket_path() -> PathBuf {
    if let Some(s) = env_path("WEBPILOT_SOCKET") {
        return s;
    }
    runtime_dir().join(SOCKET_FILENAME)
}

/// Headless Chrome PID file. Materialises the runtime dir.
pub fn pid_file() -> PathBuf {
    runtime_dir().join(PID_FILENAME)
}
/// Pure PID file path — no filesystem side effects.
pub fn pid_file_path() -> PathBuf {
    runtime_dir_path().join(PID_FILENAME)
}

/// Headless Chrome WebSocket URL file. Materialises the runtime dir.
pub fn ws_url_file() -> PathBuf {
    runtime_dir().join(WS_URL_FILENAME)
}
/// Pure WS URL file path — no filesystem side effects.
pub fn ws_url_file_path() -> PathBuf {
    runtime_dir_path().join(WS_URL_FILENAME)
}

/// Chrome launch advisory lock file. Materialises the runtime dir.
pub fn launch_lock_file() -> PathBuf {
    runtime_dir().join(LAUNCH_LOCK_FILENAME)
}

/// Pure path to the optional user settings file (`config.toml` under the cache
/// root), overridable via `WEBPILOT_CONFIG`. Pure: reading settings must not
/// create state, and path resolution itself is never config-driven (it would
/// be circular), so this performs no filesystem side effects.
pub fn config_file_path() -> PathBuf {
    env_path("WEBPILOT_CONFIG").unwrap_or_else(|| root_path().join(CONFIG_FILENAME))
}

// --- Data root --------------------------------------------------------------

static DATA_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Per-user durable data root. Materialises with mode 0700 on first call.
///
/// Use this for files that must survive OS cache eviction — most
/// importantly the unpacked Chrome extension, which Chrome holds an
/// absolute path to once the user has loaded it.
pub fn data_root() -> &'static Path {
    DATA_ROOT.get_or_init(|| materialise(data_root_path(), Owner::User))
}

/// Pure data root path — no filesystem side effects.
pub fn data_root_path() -> PathBuf {
    env_path("WEBPILOT_DATA_HOME")
        .or_else(macos_app_support)
        .or_else(xdg_data_dir)
        .or_else(home_local_share)
        .unwrap_or_else(tmp_data_fallback)
}

// --- Data subdirs -----------------------------------------------------------

/// Unpacked Chrome extension directory — what the user points
/// `chrome://extensions → Load unpacked` at. Materialises on call.
pub fn extension_dir() -> PathBuf {
    materialise(data_root().join(EXTENSION_SUBDIR), Owner::User)
}
/// Pure extension dir path — no filesystem side effects.
pub fn extension_dir_path() -> PathBuf {
    data_root_path().join(EXTENSION_SUBDIR)
}

// --- Resolution helpers -----------------------------------------------------

/// Read an environment-variable override, treating an empty value as unset.
fn env_path(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Read `$HOME`, treating an empty value as unset.
fn home_dir() -> Option<PathBuf> {
    env_path("HOME")
}

fn xdg_runtime_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        return None;
    }
    let p = env_path("XDG_RUNTIME_DIR")?;
    p.exists().then(|| p.join("webpilot"))
}

fn macos_caches() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    Some(home_dir()?.join("Library").join("Caches").join("webpilot"))
}

fn xdg_cache_dir() -> Option<PathBuf> {
    Some(env_path("XDG_CACHE_HOME")?.join("webpilot"))
}

fn home_cache_dir() -> Option<PathBuf> {
    Some(home_dir()?.join(".cache").join("webpilot"))
}

fn tmp_cache_fallback() -> PathBuf {
    PathBuf::from(format!("/tmp/webpilot-{}", current_user()))
}

fn macos_app_support() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    Some(
        home_dir()?
            .join("Library")
            .join("Application Support")
            .join("webpilot"),
    )
}

fn xdg_data_dir() -> Option<PathBuf> {
    Some(env_path("XDG_DATA_HOME")?.join("webpilot"))
}

fn home_local_share() -> Option<PathBuf> {
    Some(home_dir()?.join(".local").join("share").join("webpilot"))
}

fn tmp_data_fallback() -> PathBuf {
    PathBuf::from(format!("/tmp/webpilot-{}-data", current_user()))
}

fn current_user() -> String {
    std::env::var("USER").unwrap_or_else(|_| "default".into())
}

// --- Atomic writes -----------------------------------------------------------

/// Write `contents` to `path` atomically: a reader (or a killed writer) never
/// observes a torn file. The temp file is created in the SAME directory as the
/// target so the final `rename` is a same-filesystem metadata swap, and it is
/// removed on a mid-write failure so a crash leaves no `.tmp` litter.
///
/// The temp name carries the pid AND a process-unique counter, and is opened
/// `create_new` (O_EXCL): two concurrent writers — even same-pid async tasks
/// targeting the same path — never share a temp file, so one can't rename the
/// other's bytes out from under it.
pub fn atomic_write(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);

    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "tmp".into());
    let nonce = SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{file_name}.{}.{nonce}.tmp", std::process::id()));

    let write = || -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(contents)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    };
    write().inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

// --- Materialisation --------------------------------------------------------

#[derive(Copy, Clone)]
enum Owner {
    /// Mode 0700 — owner-only. The default for everything in this crate.
    User,
}

fn materialise(p: PathBuf, owner: Owner) -> PathBuf {
    // The builder's mode applies to every directory it creates, so ancestors
    // are born 0700 instead of umask-default — they never exist, even
    // briefly, wider than the leaf. Failures stay best-effort here: the file
    // operation that follows surfaces them with real context.
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let _ = std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&p);
    }
    #[cfg(not(unix))]
    let _ = std::fs::create_dir_all(&p);
    apply_mode(&p, owner);
    p
}

#[cfg(unix)]
fn apply_mode(p: &Path, owner: Owner) {
    use std::os::unix::fs::PermissionsExt;
    let mode = match owner {
        Owner::User => 0o700,
    };
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn apply_mode(_: &Path, _: Owner) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pure and mutating accessors must agree on the layout. If the
    /// constant for `runtime` / `extension` / etc. is renamed, both must move
    /// in lockstep — these assertions catch the desync.
    #[test]
    fn pure_paths_match_their_mutating_counterparts() {
        assert_eq!(root_path(), root().to_path_buf());
        assert_eq!(data_root_path(), data_root().to_path_buf());
        assert_eq!(extension_dir_path(), extension_dir());
        assert_eq!(runtime_dir_path(), runtime_dir());
        assert_eq!(pid_file_path(), pid_file());
        assert_eq!(ws_url_file_path(), ws_url_file());
    }

    #[test]
    fn extension_lives_under_data_root() {
        let ext = extension_dir_path();
        let data = data_root_path();
        assert!(
            ext.starts_with(&data),
            "{} should start with {}",
            ext.display(),
            data.display()
        );
        assert_eq!(ext.file_name().unwrap(), EXTENSION_SUBDIR);
    }

    #[test]
    fn pid_and_ws_live_under_runtime_dir() {
        assert_eq!(pid_file_path().parent().unwrap(), runtime_dir_path());
        assert_eq!(ws_url_file_path().parent().unwrap(), runtime_dir_path());
        assert_eq!(pid_file_path().file_name().unwrap(), PID_FILENAME);
        assert_eq!(ws_url_file_path().file_name().unwrap(), WS_URL_FILENAME);
    }

    #[test]
    fn runtime_dir_lives_under_root() {
        assert_eq!(runtime_dir_path().parent().unwrap(), root_path());
        assert_eq!(runtime_dir_path().file_name().unwrap(), RUNTIME_SUBDIR);
    }
}
