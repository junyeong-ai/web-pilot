//! Headless Chrome session management via direct CDP.

use anyhow::{Context, Result};
use std::path::PathBuf;
use webpilot::dirs;

const SYSTEM_CHROME_PATHS: &[&str] = &[
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
];

/// Headless Chrome's launch viewport. `device reset` snaps the page back to
/// these dimensions because CDP `Emulation.clearDeviceMetricsOverride`
/// removes the override flag without triggering a layout pass on its own.
pub const HEADLESS_VIEWPORT_WIDTH: u32 = 1280;
pub const HEADLESS_VIEWPORT_HEIGHT: u32 = 720;

/// Atomic write: write to a temp file, then rename.
fn atomic_write(path: &std::path::Path, data: &str) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Locate a Chrome binary. Prefers Chrome for Testing.
pub fn find_chrome() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("WEBPILOT_CHROME") {
        let p = PathBuf::from(&path);
        if p.exists() {
            return Ok(p);
        }
        anyhow::bail!("WEBPILOT_CHROME={path} not found");
    }

    // agent-browser layout: ~/.agent-browser/browsers/<version>/chrome-mac-arm64/...
    if let Ok(home) = std::env::var("HOME") {
        let browsers_dir = PathBuf::from(&home).join(".agent-browser/browsers");
        if let Ok(entries) = std::fs::read_dir(&browsers_dir) {
            let mut versions: Vec<_> = entries.filter_map(|e| e.ok()).collect();
            versions.sort_by_key(|b| {
                std::cmp::Reverse(natural_sort_key(&b.file_name().to_string_lossy()))
            });
            for entry in versions {
                let candidates = [
                    "Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
                    "chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
                    "chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
                ];
                for rel in candidates {
                    let p = entry.path().join(rel);
                    if p.exists() {
                        return Ok(p);
                    }
                }
            }
        }
    }

    for c in SYSTEM_CHROME_PATHS {
        let p = PathBuf::from(c);
        if p.exists() {
            return Ok(p);
        }
    }

    if let Ok(out) = std::process::Command::new("which")
        .arg("google-chrome")
        .output()
        && out.status.success()
    {
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !path.is_empty() {
            return Ok(PathBuf::from(path));
        }
    }

    anyhow::bail!("Chrome not found. Install Chrome or set WEBPILOT_CHROME=/path/to/chrome")
}

/// Natural-order sort key. Splits a name into runs of digits and non-digits so
/// that `"10.0"` sorts after `"9.0"` instead of before it.
fn natural_sort_key(s: &str) -> Vec<NatPart> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut in_digit = false;
    for c in s.chars() {
        let now_digit = c.is_ascii_digit();
        if !buf.is_empty() && now_digit != in_digit {
            out.push(if in_digit {
                NatPart::Num(buf.parse().unwrap_or(0))
            } else {
                NatPart::Str(std::mem::take(&mut buf))
            });
            buf = String::new();
        }
        buf.push(c);
        in_digit = now_digit;
    }
    if !buf.is_empty() {
        out.push(if in_digit {
            NatPart::Num(buf.parse().unwrap_or(0))
        } else {
            NatPart::Str(buf)
        });
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum NatPart {
    Str(String),
    Num(u64),
}

/// `kill(pid, signal)` wrapper. Returns Ok(true) if delivered, Ok(false) if no
/// such process, Err otherwise.
fn send_signal(pid: i32, signal: i32) -> Result<bool> {
    // SAFETY: kill() is a POSIX syscall with no memory safety implications.
    // pid comes from our own PID file; signal is a known constant.
    let ret = unsafe { libc::kill(pid, signal) };
    if ret == 0 {
        return Ok(true);
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ESRCH) {
        Ok(false)
    } else {
        Err(err.into())
    }
}

fn is_process_alive(pid: i32) -> bool {
    send_signal(pid, 0).unwrap_or(false)
}

pub fn pid_path() -> PathBuf {
    dirs::pid_file()
}

pub fn ws_url_path() -> PathBuf {
    dirs::ws_url_file()
}

pub fn read_pid() -> i32 {
    std::fs::read_to_string(pid_path())
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
        .unwrap_or(0)
}

pub async fn launch_chrome() -> Result<(u32, String)> {
    let chrome = find_chrome()?;
    let profile_dir = dirs::chrome_profile_dir();

    // Clean stale DevToolsActivePort
    let devtools_port_file = profile_dir.join("DevToolsActivePort");
    let _ = std::fs::remove_file(&devtools_port_file);

    tracing::info!("Launching headless Chrome...");

    let child = std::process::Command::new(&chrome)
        .args([
            "--headless=new",
            "--remote-debugging-port=0",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-background-networking",
            "--disable-component-update",
            "--disable-default-apps",
            "--disable-popup-blocking",
            "--disable-sync",
            "--disable-features=Translate",
            "--enable-features=NetworkService,NetworkServiceInProcess",
            "--password-store=basic",
            "--use-mock-keychain",
            &format!("--window-size={HEADLESS_VIEWPORT_WIDTH},{HEADLESS_VIEWPORT_HEIGHT}"),
            &format!("--user-data-dir={}", profile_dir.display()),
            "about:blank",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("Failed to launch Chrome")?;

    let pid = child.id();

    // Detach: Chrome runs independently, managed via PID file + signals.
    std::mem::forget(child);

    // Poll DevToolsActivePort (Puppeteer/Playwright standard).
    let deadline = tokio::time::Instant::now() + crate::timeouts::chrome_launch();
    let ws_url = loop {
        if tokio::time::Instant::now() > deadline {
            let _ = send_signal(pid as i32, libc::SIGTERM);
            anyhow::bail!("Chrome started but no DevTools URL. Is this Chrome for Testing?");
        }
        if let Ok(content) = std::fs::read_to_string(&devtools_port_file) {
            let lines: Vec<&str> = content.lines().collect();
            if lines.len() >= 2 {
                let port = lines[0].trim();
                let path = lines[1].trim();
                break format!("ws://127.0.0.1:{port}{path}");
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };

    atomic_write(&pid_path(), &pid.to_string())?;
    atomic_write(&ws_url_path(), &ws_url)?;

    tracing::info!("Headless Chrome ready (pid {pid})");
    Ok((pid, ws_url))
}

/// Read the WebSocket URL of an existing healthy session, or `None`.
pub fn get_existing_session() -> Option<String> {
    let pid_str = std::fs::read_to_string(pid_path()).ok()?;
    let pid: i32 = pid_str.trim().parse().ok()?;

    if !is_process_alive(pid) {
        let _ = std::fs::remove_file(ws_url_path());
        let _ = std::fs::remove_file(pid_path());
        return None;
    }

    std::fs::read_to_string(ws_url_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Ensure a headless session is running. Uses an advisory file lock so that
/// concurrent agents do not race to launch Chrome.
pub async fn ensure_session() -> Result<String> {
    if let Some(url) = get_existing_session() {
        return Ok(url);
    }

    let lock_path = dirs::launch_lock_file();
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .context("Failed to create launch lock file")?;

    use std::os::unix::io::AsRawFd;
    // SAFETY: flock() is a POSIX advisory lock; no memory safety implications.
    let ret = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX) };
    if ret != 0 {
        anyhow::bail!(
            "Failed to acquire launch lock: {}",
            std::io::Error::last_os_error()
        );
    }

    if let Some(url) = get_existing_session() {
        return Ok(url);
    }

    // Clean up an orphaned Chrome whose PID file we own.
    if let Ok(pid_str) = std::fs::read_to_string(pid_path())
        && let Ok(pid) = pid_str.trim().parse::<i32>()
        && is_process_alive(pid)
    {
        let _ = send_signal(pid, libc::SIGTERM);
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    let _ = std::fs::remove_file(pid_path());
    let _ = std::fs::remove_file(ws_url_path());

    let (_, ws_url) = launch_chrome().await?;
    Ok(ws_url)
}

/// Shut down the entire headless Chrome session. Idempotent.
pub async fn quit_session() -> Result<()> {
    let pid_file = pid_path();
    if let Ok(pid_str) = std::fs::read_to_string(&pid_file)
        && let Ok(pid) = pid_str.trim().parse::<i32>()
    {
        let _ = send_signal(pid, libc::SIGTERM);
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        if is_process_alive(pid) {
            let _ = send_signal(pid, libc::SIGKILL);
        }
        let _ = std::fs::remove_file(&pid_file);
    }
    let _ = std::fs::remove_file(ws_url_path());

    // Per-(context|default) active-frame and active-tab state — session-scoped,
    // gone with Chrome.
    if let Ok(entries) = std::fs::read_dir(dirs::runtime_dir()) {
        for entry in entries.filter_map(|e| e.ok()) {
            let n = entry.file_name();
            let name = n.to_string_lossy();
            if name.starts_with("active_frame_") || name.starts_with("active_tab_") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    // Context state files
    let contexts = dirs::contexts_dir();
    if let Ok(entries) = std::fs::read_dir(&contexts) {
        for entry in entries.filter_map(|e| e.ok()) {
            if entry.file_name().to_string_lossy().starts_with("ctx-") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    // Chrome profile dir
    let profile_dir = dirs::chrome_profile_dir();
    if profile_dir.exists() {
        let _ = std::fs::remove_dir_all(&profile_dir);
    }

    // Launch lock
    let _ = std::fs::remove_file(dirs::launch_lock_file());

    tracing::info!("Headless session stopped.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_sort_orders_versions() {
        let mut keys: Vec<_> = ["9.0.123", "10.0.5", "10.0.20", "9.5.7"]
            .iter()
            .map(|s| (natural_sort_key(s), *s))
            .collect();
        keys.sort_by(|a, b| a.0.cmp(&b.0));
        let sorted: Vec<&str> = keys.iter().map(|(_, s)| *s).collect();
        assert_eq!(sorted, ["9.0.123", "9.5.7", "10.0.5", "10.0.20"]);
    }
}
