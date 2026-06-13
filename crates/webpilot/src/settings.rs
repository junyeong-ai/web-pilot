//! Layered runtime settings: built-in defaults < `config.toml` < environment.
//!
//! A single resolved [`Settings`] is the one place every tunable is read from.
//! Each field resolves independently as `env.or(file).unwrap_or(default)`, so an
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
    pub capture: Capture,
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
    /// How long the NM host waits for the extension's connect-time version
    /// Ping before failing a command closed. The Ping lands in milliseconds in
    /// practice; a loaded machine may want longer.
    pub version_handshake: Duration,
}

#[derive(Debug, Clone)]
pub struct Chrome {
    /// Explicit Chrome/Chromium binary path. `None` falls back to platform
    /// auto-detection in the session launcher.
    pub binary: Option<String>,
    /// Headless launch viewport. `device reset` snaps the page back to this.
    pub viewport_width: u32,
    pub viewport_height: u32,
    /// Launch Chrome with `--no-sandbox`. Off by default — it weakens Chrome's
    /// process sandbox — but required to run headless in an unprivileged
    /// container (Docker, CI, many cloud sandboxes), where the setuid sandbox
    /// can't initialise and Chrome otherwise never reports a DevTools port.
    pub no_sandbox: bool,
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

#[derive(Debug, Clone)]
pub struct Capture {
    /// Screenshots are downscaled so their longest edge fits this many
    /// pixels. The default (1568) is the largest size Claude's vision ingests
    /// without server-side resizing — bigger only costs tokens and latency.
    pub screenshot_max_long_edge: u32,
    /// Upper bound on frames a single `record` run captures. A recording for
    /// AI analysis is a short clip, not an open-ended capture; bounding it
    /// turns a fat-fingered `--frames 4000000000` into a typed rejection
    /// instead of an hours-long, disk-filling loop.
    pub max_record_frames: u32,
}

/// Validate `config.toml` and cache the resolved settings. Call once at startup
/// (CLI and host both do). Returns a human-readable error if the file exists but
/// can't be read or parsed, so the caller can abort with a clear message instead
/// of running with settings the operator believes are in effect but aren't.
pub fn init() -> std::result::Result<(), String> {
    let file = read_config()?;
    let settings = Settings::resolve(file);
    validate(&settings)?;
    let _ = SETTINGS.set(settings);
    Ok(())
}

/// Invariants every resolved settings instance must satisfy — enforced on both
/// the startup [`init`] path and the lazy [`get`] fallback, so no path can run
/// with values one of them would have rejected.
fn validate(settings: &Settings) -> std::result::Result<(), String> {
    // Values that downstream APIs reject with a panic are rejected here with
    // a message instead (`broadcast::channel` requires capacity >= 1).
    if settings.cdp.event_buffer == 0 {
        return Err("cdp.event_buffer must be greater than 0".into());
    }
    if settings.capture.screenshot_max_long_edge == 0 {
        return Err("capture.screenshot_max_long_edge must be greater than 0".into());
    }
    // A zero viewport dimension reaches Chrome as `--window-size=0,0` and CDP
    // as a 0×0 emulation override — neither rejects it cleanly, so the session
    // degrades instead of failing. Refuse up front like the other
    // zero-breaks-downstream values here.
    if settings.chrome.viewport_width == 0 || settings.chrome.viewport_height == 0 {
        return Err(
            "chrome.viewport_width and chrome.viewport_height must be greater than 0".into(),
        );
    }
    // Every deadline/interval below bounds an operation that must make progress:
    // at zero it either fails instantly (an immediate timeout on navigation, a
    // CDP send, an IPC reply, a Chrome launch, a history/reload settle, the
    // version handshake) or busy-spins (a zero poll/heartbeat interval). The
    // browser extension guards the same values over the Config handshake, so a
    // zero here would also silently diverge the two modes. Reject every one up
    // front rather than degrade to a broken session. (`annotation_paint` is a
    // deliberate pre-screenshot paint delay, not a deadline — zero just means
    // "no delay", a valid choice — so it is intentionally absent.)
    let t = &settings.timeouts;
    let positive: [(&str, std::time::Duration); 9] = [
        ("timeouts.navigation_ms", t.navigation),
        ("timeouts.cdp_send_ms", t.cdp_send),
        ("timeouts.reload_wait_ms", t.reload_wait),
        ("timeouts.back_forward_ms", t.back_forward),
        ("timeouts.poll_interval_ms", t.poll_interval),
        ("timeouts.ipc_response_ms", t.ipc_response),
        ("timeouts.chrome_launch_ms", t.chrome_launch),
        ("timeouts.heartbeat_ms", t.heartbeat),
        ("timeouts.version_handshake_ms", t.version_handshake),
    ];
    for (name, duration) in positive {
        if duration.is_zero() {
            return Err(format!("{name} must be greater than 0"));
        }
    }
    Ok(())
}

/// Process-wide resolved settings. `init` populates this at startup; the lazy
/// fallback (library/test use without `init`) reads the same file. The module
/// contract holds on both paths: an absent file is the all-default state, but
/// a config the operator wrote and got wrong is never quietly ignored — with
/// no error channel here, a malformed/unreadable file or an invalid value
/// panics rather than running with settings `init` would have refused.
pub fn get() -> &'static Settings {
    SETTINGS.get_or_init(|| {
        let file = read_config().unwrap_or_else(|message| panic!("{message}"));
        let settings = Settings::resolve(file);
        if let Err(message) = validate(&settings) {
            panic!("invalid settings: {message}");
        }
        settings
    })
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
                version_handshake: ms(
                    "WEBPILOT_VERSION_HANDSHAKE_TIMEOUT_MS",
                    t.version_handshake_ms,
                    2_000,
                ),
            },
            chrome: Chrome {
                binary: string_var("WEBPILOT_CHROME").or(c.binary),
                viewport_width: u32_var("WEBPILOT_VIEWPORT_WIDTH", c.viewport_width, 1280),
                viewport_height: u32_var("WEBPILOT_VIEWPORT_HEIGHT", c.viewport_height, 720),
                no_sandbox: bool_var("WEBPILOT_CHROME_NO_SANDBOX", c.no_sandbox, false),
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
            capture: Capture {
                screenshot_max_long_edge: u32_var(
                    "WEBPILOT_SCREENSHOT_MAX_LONG_EDGE",
                    file.capture.screenshot_max_long_edge,
                    1568,
                ),
                max_record_frames: u32_var(
                    "WEBPILOT_MAX_RECORD_FRAMES",
                    file.capture.max_record_frames,
                    3600,
                ),
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

/// A boolean env override. `1` / `true` / `yes` / `on` are true; `0` / `false` /
/// `no` / `off` are false (any case). Absent, empty, OR an unrecognized value
/// falls through to the config file, then the built-in default. An unrecognized
/// value (a typo like `tru`) must NOT silently read as `false` and override a
/// correct config-file setting — it is "unset", exactly as empty is, so the
/// precedence stays consistent across every tunable.
fn bool_var(env: &str, file: Option<bool>, default: bool) -> bool {
    let env_val = std::env::var(env).ok().and_then(|v| parse_bool(&v));
    pick(env_val, file, default)
}

/// Parse a boolean setting value. `1` / `true` / `yes` / `on` are true, `0` /
/// `false` / `no` / `off` are false (any case, surrounding space ignored).
/// Anything else — a typo like `tru`, or empty — is `None`: "unset", so it falls
/// through to the next precedence tier instead of silently reading as `false`.
fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
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
    capture: FileCapture,
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
    version_handshake_ms: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileChrome {
    binary: Option<String>,
    viewport_width: Option<u32>,
    viewport_height: Option<u32>,
    no_sandbox: Option<bool>,
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

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileCapture {
    screenshot_max_long_edge: Option<u32>,
    max_record_frames: Option<u32>,
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
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // The DEFAULT path being absent is the all-default state — but a
            // path the operator set EXPLICITLY (`WEBPILOT_CONFIG`) and got
            // wrong must fail loud: silently running on built-in defaults
            // would ignore every setting they intended to apply.
            if std::env::var_os("WEBPILOT_CONFIG").is_some() {
                return Err(format!(
                    "WEBPILOT_CONFIG points at {}, which does not exist",
                    path.display()
                ));
            }
            Ok(FileSettings::default())
        }
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
    fn parse_bool_accepts_canonical_and_falls_through_on_junk() {
        for t in ["1", "true", "TRUE", "yes", "On", " true "] {
            assert_eq!(parse_bool(t), Some(true), "{t:?} should be true");
        }
        for f in ["0", "false", "FALSE", "no", "Off", " 0 "] {
            assert_eq!(parse_bool(f), Some(false), "{f:?} should be false");
        }
        // A typo or junk is `None` — it falls through to the config/default,
        // never a silent `false` that would override a correct config value.
        for n in ["tru", "flase", "", "  ", "2", "enabled", "y"] {
            assert_eq!(parse_bool(n), None, "{n:?} should fall through");
        }
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
