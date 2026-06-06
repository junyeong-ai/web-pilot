//! Layered runtime settings: built-in defaults < `config.toml` < environment.
//!
//! A single resolved [`Settings`] is the one place every tunable is read from,
//! replacing the scattered `std::env::var` calls that used to live in
//! `timeouts`, the CDP client, the session launcher, and the context GC. Each
//! field resolves independently as `env.or(file).unwrap_or(default)`, so an
//! environment override always wins, a `config.toml` value is the team default,
//! and the built-in default is the floor.
//!
//! Path resolution (where state lives) is deliberately *not* config-driven — it
//! would be circular, since the config file's own location derives from it — so
//! it stays in [`crate::dirs`], env- and platform-only.
//!
//! A `config.toml` that exists but is malformed (unknown key, bad value) is a
//! hard, loud error: [`init`] returns it and the process exits with a clear
//! message at startup. There is no silent fall-through to defaults — a config
//! the operator wrote but got wrong must never be quietly ignored.
//!
//! Environment overrides are deliberately the *lenient* tier: a present-but-
//! unparseable `WEBPILOT_*` value (e.g. a typo) is treated as absent and the
//! next-precedence value (config file, then default) wins. The config file is
//! the deliberate, declarative settings surface and fails loud; env vars are
//! ad-hoc per-invocation overrides where falling through to a valid lower tier
//! is the expected, least-surprising behaviour.
//!
//! `OnceLock` caches the resolved settings for the process lifetime: a
//! long-lived host does not observe edits to `config.toml` until restarted.

use std::sync::OnceLock;
use std::time::Duration;

use serde::Deserialize;

static SETTINGS: OnceLock<Settings> = OnceLock::new();

/// Fully-resolved settings. Obtain the process-wide instance via [`get`].
#[derive(Debug, Clone)]
pub struct Settings {
    pub timeouts: Timeouts,
    pub chrome: Chrome,
    pub context: Context,
    pub cdp: Cdp,
}

#[derive(Debug, Clone)]
pub struct Timeouts {
    pub cdp_send: Duration,
    pub navigation: Duration,
    pub reload_wait: Duration,
    pub back_forward: Duration,
    pub poll_interval: Duration,
    pub annotation_paint: Duration,
    pub ipc_response: Duration,
    pub chrome_launch: Duration,
    pub heartbeat: Duration,
}

#[derive(Debug, Clone)]
pub struct Chrome {
    /// Explicit Chrome/Chromium binary path. `None` falls back to platform
    /// auto-detection in the session launcher.
    pub binary: Option<String>,
    /// Headless launch viewport. `device reset` snaps the page back to this.
    pub viewport_width: u32,
    pub viewport_height: u32,
}

#[derive(Debug, Clone)]
pub struct Context {
    /// Idle lifetime before a multi-agent context is garbage-collected.
    pub ttl: Duration,
}

#[derive(Debug, Clone)]
pub struct Cdp {
    /// Broadcast buffer for CDP events (per connection).
    pub event_buffer: usize,
}

/// Validate `config.toml` and cache the resolved settings. Call once at startup
/// (CLI and host both do). Returns a human-readable error if the file exists but
/// can't be read or parsed, so the caller can abort with a clear message instead
/// of running with settings the operator believes are in effect but aren't.
pub fn init() -> std::result::Result<(), String> {
    let file = read_config()?;
    let _ = SETTINGS.set(Settings::resolve(file));
    Ok(())
}

/// Process-wide resolved settings. `init` populates this at startup; the lazy
/// fallback (library/test use without `init`) reads the same file and, only for
/// that path, tolerates an absent/unreadable file as defaults.
pub fn get() -> &'static Settings {
    SETTINGS.get_or_init(|| Settings::resolve(read_config().unwrap_or_default()))
}

/// Shorthand for `get().timeouts`.
pub fn timeouts() -> &'static Timeouts {
    &get().timeouts
}

impl Settings {
    fn resolve(file: FileSettings) -> Self {
        let t = file.timeouts;
        let c = file.chrome;
        Settings {
            timeouts: Timeouts {
                cdp_send: ms("WEBPILOT_CDP_SEND_TIMEOUT_MS", t.cdp_send_ms, 30_000),
                navigation: ms("WEBPILOT_NAVIGATION_TIMEOUT_MS", t.navigation_ms, 15_000),
                reload_wait: ms("WEBPILOT_RELOAD_TIMEOUT_MS", t.reload_wait_ms, 10_000),
                back_forward: ms("WEBPILOT_BACK_FORWARD_TIMEOUT_MS", t.back_forward_ms, 5_000),
                poll_interval: ms("WEBPILOT_POLL_INTERVAL_MS", t.poll_interval_ms, 300),
                annotation_paint: ms("WEBPILOT_ANNOTATION_PAINT_MS", t.annotation_paint_ms, 200),
                ipc_response: ms("WEBPILOT_IPC_TIMEOUT_MS", t.ipc_response_ms, 60_000),
                chrome_launch: ms(
                    "WEBPILOT_CHROME_LAUNCH_TIMEOUT_MS",
                    t.chrome_launch_ms,
                    15_000,
                ),
                heartbeat: ms("WEBPILOT_HEARTBEAT_INTERVAL_MS", t.heartbeat_ms, 10_000),
            },
            chrome: Chrome {
                binary: string_var("WEBPILOT_CHROME").or(c.binary),
                viewport_width: u32_var("WEBPILOT_VIEWPORT_WIDTH", c.viewport_width, 1280),
                viewport_height: u32_var("WEBPILOT_VIEWPORT_HEIGHT", c.viewport_height, 720),
            },
            context: Context {
                ttl: Duration::from_secs(u64_var(
                    "WEBPILOT_CONTEXT_TTL",
                    file.context.ttl_secs,
                    3_600,
                )),
            },
            cdp: Cdp {
                event_buffer: usize_var("WEBPILOT_CDP_EVENT_BUFFER", file.cdp.event_buffer, 256),
            },
        }
    }
}

// ── Field resolvers (env > file > default) ───────────────────────────────────

fn ms(env: &str, file: Option<u64>, default: u64) -> Duration {
    Duration::from_millis(u64_var(env, file, default))
}

fn u64_var(env: &str, file: Option<u64>, default: u64) -> u64 {
    pick(parse_env(env), file, default)
}

fn u32_var(env: &str, file: Option<u32>, default: u32) -> u32 {
    pick(parse_env(env), file, default)
}

fn usize_var(env: &str, file: Option<usize>, default: usize) -> usize {
    pick(parse_env(env), file, default)
}

/// Precedence: env override, then config file, then built-in default.
fn pick<T>(env: Option<T>, file: Option<T>, default: T) -> T {
    env.or(file).unwrap_or(default)
}

fn parse_env<T: std::str::FromStr>(var: &str) -> Option<T> {
    std::env::var(var).ok().and_then(|v| v.parse().ok())
}

fn string_var(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|v| !v.is_empty())
}

// ── On-disk shape (all optional; absent → fall through to default) ───────────

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileSettings {
    timeouts: FileTimeouts,
    chrome: FileChrome,
    context: FileContext,
    cdp: FileCdp,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileTimeouts {
    cdp_send_ms: Option<u64>,
    navigation_ms: Option<u64>,
    reload_wait_ms: Option<u64>,
    back_forward_ms: Option<u64>,
    poll_interval_ms: Option<u64>,
    annotation_paint_ms: Option<u64>,
    ipc_response_ms: Option<u64>,
    chrome_launch_ms: Option<u64>,
    heartbeat_ms: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileChrome {
    binary: Option<String>,
    viewport_width: Option<u32>,
    viewport_height: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileContext {
    ttl_secs: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileCdp {
    event_buffer: Option<usize>,
}

/// Read and parse `config.toml`. An absent file is the empty (all-default)
/// state; a present-but-unreadable or present-but-invalid file is an error,
/// surfaced verbatim (path + cause) so the operator can fix it. `deny_unknown_fields`
/// means a single typo fails the whole load — deliberately, so a misspelled key
/// is reported rather than silently dropped alongside the rest of the config.
fn read_config() -> std::result::Result<FileSettings, String> {
    let path = crate::dirs::config_file_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text)
            .map_err(|e| format!("invalid settings at {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(FileSettings::default()),
        Err(e) => Err(format!("cannot read settings at {}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `pick` is the whole precedence rule, env-independent and pure — test it
    // directly rather than `resolve`, which reads live process env and would
    // be non-deterministic under a developer's exported `WEBPILOT_*`.
    #[test]
    fn pick_prefers_env_then_file_then_default() {
        assert_eq!(pick(Some(1), Some(2), 3), 1, "env wins");
        assert_eq!(pick(None, Some(2), 3), 2, "file beats default");
        assert_eq!(pick(None::<u64>, None, 3), 3, "default is the floor");
    }

    #[test]
    fn toml_parses_nested_tables() {
        let text = r#"
            [timeouts]
            navigation_ms = 12345

            [chrome]
            binary = "/opt/chrome"
            viewport_width = 800

            [context]
            ttl_secs = 60
        "#;
        let file: FileSettings = toml::from_str(text).unwrap();
        assert_eq!(file.timeouts.navigation_ms, Some(12345));
        assert_eq!(file.chrome.binary.as_deref(), Some("/opt/chrome"));
        assert_eq!(file.chrome.viewport_width, Some(800));
        assert_eq!(file.context.ttl_secs, Some(60));
    }

    #[test]
    fn unknown_keys_are_rejected() {
        // A typo anywhere must fail the load (so `init` reports it) rather than
        // silently dropping that key. `deny_unknown_fields` is what enforces it.
        let text = "[timeouts]\nnavigaton_ms = 1\n";
        assert!(toml::from_str::<FileSettings>(text).is_err());
    }
}
