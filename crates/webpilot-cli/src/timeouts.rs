//! Centralized timeout constants with environment variable overrides.
//!
//! Each function reads an optional env var (e.g., `WEBPILOT_CDP_SEND_TIMEOUT_MS`)
//! and falls back to a sensible default. This allows tuning in CI, slow networks,
//! or debugging without recompilation.

use std::time::Duration;

fn from_env_or(var: &str, default_ms: u64) -> Duration {
    if let Ok(val) = std::env::var(var)
        && let Ok(ms) = val.parse::<u64>()
    {
        return Duration::from_millis(ms);
    }
    Duration::from_millis(default_ms)
}

/// CDP WebSocket send timeout (default: 30s).
pub fn cdp_send() -> Duration {
    from_env_or("WEBPILOT_CDP_SEND_TIMEOUT_MS", 30_000)
}

/// Page navigation timeout (default: 15s).
pub fn navigation() -> Duration {
    from_env_or("WEBPILOT_NAVIGATION_TIMEOUT_MS", 15_000)
}

/// Reload wait timeout (default: 10s).
pub fn reload_wait() -> Duration {
    from_env_or("WEBPILOT_RELOAD_TIMEOUT_MS", 10_000)
}

/// Back/Forward wait timeout (default: 5s).
pub fn back_forward() -> Duration {
    from_env_or("WEBPILOT_BACK_FORWARD_TIMEOUT_MS", 5_000)
}

/// Cross-origin target poll interval (default: 300ms).
pub fn poll_interval() -> Duration {
    from_env_or("WEBPILOT_POLL_INTERVAL_MS", 300)
}

/// Post-navigation settle time (default: 200ms).
pub fn post_navigate() -> Duration {
    from_env_or("WEBPILOT_POST_NAVIGATE_MS", 200)
}

/// Post-reconnect settle time (default: 500ms).
pub fn post_reconnect() -> Duration {
    from_env_or("WEBPILOT_POST_RECONNECT_MS", 500)
}

/// IPC response timeout (default: 60s).
pub fn ipc_response() -> Duration {
    from_env_or("WEBPILOT_IPC_TIMEOUT_MS", 60_000)
}

/// Chrome launch deadline (default: 15s).
pub fn chrome_launch() -> Duration {
    from_env_or("WEBPILOT_CHROME_LAUNCH_TIMEOUT_MS", 15_000)
}

/// CDP heartbeat interval (default: 10s).
pub fn heartbeat() -> Duration {
    from_env_or("WEBPILOT_HEARTBEAT_INTERVAL_MS", 10_000)
}
