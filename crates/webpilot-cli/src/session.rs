//! Headless Chrome session management via direct CDP.

use anyhow::{Context, Result};
use std::path::PathBuf;
use webpilot::dirs;

const SYSTEM_CHROME_PATHS: &[&str] = &[
    // macOS
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    // Linux standard install locations (`which` below covers PATH installs; these
    // catch a present-but-not-on-PATH binary)
    "/usr/bin/google-chrome",
    "/usr/bin/google-chrome-stable",
    "/usr/bin/chromium",
    "/usr/bin/chromium-browser",
    "/opt/google/chrome/chrome",
];

/// Headless Chrome's launch viewport. `device reset` snaps the page back to
/// these dimensions because CDP `Emulation.clearDeviceMetricsOverride`
/// removes the override flag without triggering a layout pass on its own.
/// Resolved from settings (default 1280×720, `[chrome]` config, or
/// `WEBPILOT_VIEWPORT_*`).
pub fn headless_viewport() -> (u32, u32) {
    let c = &webpilot::settings::get().chrome;
    (c.viewport_width, c.viewport_height)
}

/// Locate a Chrome binary. Prefers Chrome for Testing.
pub fn find_chrome() -> Result<PathBuf> {
    if let Some(path) = &webpilot::settings::get().chrome.binary {
        let p = PathBuf::from(path);
        if p.exists() {
            return Ok(p);
        }
        anyhow::bail!("configured Chrome binary not found: {path}");
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
                    // Linux Chrome-for-Testing layout — the same path the browser
                    // e2e harness resolves, so production discovery can't drift
                    // from the layout the tests assume.
                    "chrome-linux64/chrome",
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

/// Reap the launched Chrome once it exits, so a long-lived launcher (the MCP
/// server) never accretes zombie processes across relaunches. A TARGETED,
/// non-blocking `waitpid(WNOHANG)` on Chrome's PID only — never `-1` — so it
/// cannot race the std::process `ps`/`which` subprocess waits in this file (the
/// reason the tokio `process` feature, whose global SIGCHLD reaper would, is not
/// pulled in). The interval is coarse: the goal is to prevent accumulation, not
/// instant reaping. Ends when Chrome is reaped (`waitpid` returns the PID) or is
/// no longer this process's child (`-1`/ECHILD — already reaped, or this process
/// exited and init adopted it). A short-lived CLI drops the task on exit before
/// it matters; only a long-lived parent runs it to completion.
async fn reap_when_dead(pid: libc::pid_t) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        let mut status: libc::c_int = 0;
        // SAFETY: `waitpid` with `WNOHANG` is non-blocking and only inspects a
        // child of this process; `status` is a valid out-pointer.
        let r = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if r != 0 {
            break;
        }
    }
}

/// Whether `pid` is alive AND is the Chrome WE launched — its command line
/// carries our `--user-data-dir`. Every signal aimed at a PID-file pid is
/// gated on this: after Chrome exits, the OS can recycle its pid to an
/// unrelated same-user process, and a bare liveness check would then have us
/// SIGKILL a stranger. `get_existing_session` deliberately does NOT pay this
/// `ps` cost on the hot path — a recycled pid there just makes the CDP connect
/// fail, and the connect-failure path verifies identity before it kills.
fn is_our_chrome(pid: i32) -> bool {
    if pid <= 0 || !is_process_alive(pid) {
        return false;
    }
    let marker = format!("--user-data-dir={}", dirs::chrome_profile_dir().display());
    std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&marker))
        .unwrap_or(false)
}

/// SIGKILL any Chrome still holding our profile dir that we have no live session
/// for — an orphan from a crash in the window between spawn and writing the pid
/// file (`is_our_chrome`'s pid-gated cleanup can't see a pid it never recorded).
/// Such an orphan keeps the profile's SingletonLock and would wedge a fresh
/// launch. Called only on the launch-fresh path (under the launch lock, once
/// `get_existing_session` found nothing usable), so it never targets a Chrome a
/// live session owns. The `--user-data-dir` marker is unique to this
/// WEBPILOT_HOME, so the user's own Chrome and other contexts are untouched.
fn kill_profile_orphans() {
    let marker = format!("--user-data-dir={}", dirs::chrome_profile_dir().display());
    let Ok(output) = std::process::Command::new("ps")
        .args(["-ax", "-o", "pid=,command="])
        .output()
    else {
        return;
    };
    if !output.status.success() {
        return;
    }
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        // Require BOTH our profile marker AND the `--remote-debugging-port` flag
        // launch_chrome always passes: the marker alone is an argv substring an
        // unrelated process (a grep, an editor, a script naming the path) could
        // carry, and this SIGKILLs blind. The pair identifies the Chrome main
        // process we launched — killing it releases the profile's SingletonLock
        // (a fresh launch reclaims the now-dead lock), and its helpers exit on
        // the broken IPC. Both flags together make a false positive implausible.
        if !line.contains(&marker) || !line.contains("--remote-debugging-port") {
            continue;
        }
        if let Some(pid) = line
            .split_whitespace()
            .next()
            .and_then(|p| p.parse::<i32>().ok())
        {
            let _ = send_signal(pid, libc::SIGKILL);
        }
    }
}

/// Acquire the cross-process launch lock (advisory `flock`). Held until the
/// returned file is dropped. Serialises launch and session-invalidation so a
/// relaunch can't interleave with another process tearing the session down.
fn launch_lock_guard() -> Result<std::fs::File> {
    let guard = crate::lockfile::flock_exclusive(&dirs::launch_lock_file(), false)
        .context("acquire launch lock")?;
    Ok(guard.expect("a blocking flock returns Some"))
}

/// Pure path accessors: locate the PID/WS files without materialising the
/// runtime directory. Writers must call [`ensure_runtime`] beforehand.
pub fn pid_path() -> PathBuf {
    dirs::pid_file_path()
}

pub fn ws_url_path() -> PathBuf {
    dirs::ws_url_file_path()
}

/// Materialise the cache + runtime directories. Call this once at any
/// point that writes to the runtime files (PID, WebSocket URL, lock).
fn ensure_runtime() {
    let _ = dirs::runtime_dir();
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

    let (vw, vh) = headless_viewport();
    let mut cmd = std::process::Command::new(&chrome);
    cmd.args([
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
        &format!("--window-size={vw},{vh}"),
        &format!("--user-data-dir={}", profile_dir.display()),
    ]);
    // A sandboxed Chrome can't initialise its setuid sandbox in an unprivileged
    // container (Docker, CI, many cloud sandboxes) and then never reports a
    // DevTools port. Opt-in, off by default — it weakens Chrome's sandbox.
    if webpilot::settings::get().chrome.no_sandbox {
        cmd.arg("--no-sandbox");
    }
    let child = cmd
        .arg("about:blank")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("Failed to launch Chrome")?;

    let pid = child.id();

    // Chrome is a DETACHED singleton, managed across processes by PID file +
    // signals; it must outlive a short-lived CLI so the next invocation
    // re-attaches. `std::process::Child` drop neither kills nor waits, so we drop
    // the handle (the next line) — but a LONG-LIVED launcher (the MCP server)
    // stays Chrome's parent, and a parent that never `wait`s a dead child leaves a
    // zombie; a crashed-and-relaunched Chrome would accrete them. A non-blocking
    // `waitpid(WNOHANG)` reaper task reaps it when it dies, WITHOUT the tokio
    // `process` feature (whose global SIGCHLD handler would race the std::process
    // `ps`/`which` calls elsewhere in this file) and without holding a thread. A
    // short-lived CLI exits before it matters (Chrome reparents to init, reaped
    // there); a long-lived parent reaps each Chrome as it dies.
    std::mem::forget(child);
    tokio::spawn(reap_when_dead(pid as libc::pid_t));

    // Poll DevToolsActivePort (Puppeteer/Playwright standard).
    let deadline = tokio::time::Instant::now() + webpilot::settings::timeouts().chrome_launch;
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

    // Record the running process atomically. If persistence fails, the Chrome
    // we just launched would be unmanaged — kill it before surfacing the error
    // rather than leaking an orphan.
    ensure_runtime();
    // The shared atomic write fsyncs before the rename, so a crash can't leave
    // a torn pid/ws file that the next launch would misread.
    if let Err(e) = dirs::atomic_write(&pid_path(), pid.to_string().as_bytes())
        .and_then(|_| dirs::atomic_write(&ws_url_path(), ws_url.as_bytes()))
    {
        let _ = send_signal(pid as i32, libc::SIGTERM);
        return Err(e.into());
    }

    tracing::info!("Headless Chrome ready (pid {pid})");
    Ok((pid, ws_url))
}

/// Read the WebSocket URL of an existing healthy session, or `None`.
///
/// This is a *read-only inspection*: it must not materialise the runtime
/// directory just to look at the PID file. We use the pure path accessors
/// (`*_path()`) and let the callers at write sites create the dir.
pub fn get_existing_session() -> Option<String> {
    let pid_path = dirs::pid_file_path();
    let ws_path = dirs::ws_url_file_path();
    let pid_str = std::fs::read_to_string(&pid_path).ok()?;
    let pid: i32 = pid_str.trim().parse().ok()?;

    if !is_process_alive(pid) {
        // Read-only on purpose: a dead pid means "no live session", but do NOT
        // delete the files here. This runs BEFORE the launch lock, so between
        // reading this (stale, dead) pid and deleting, a concurrent launcher could
        // write a fresh pid/ws under the lock — and this delete would clobber them,
        // orphaning the Chrome it just spawned. The stale files are reaped under
        // the lock in `ensure_session`, where no write can race the delete.
        return None;
    }

    std::fs::read_to_string(&ws_path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Tear down a session that proved unreachable — Chrome exited (or hung)
/// between the liveness check and the CDP connect — but ONLY if the recorded
/// session is still the one we failed on (`failed_ws_url`). Under the launch
/// lock, this compare-and-invalidate stops a concurrent `open` that already
/// relaunched a fresh session from being torn down by a slow loser: the loser
/// sees the new URL on disk, recognises it is no longer its dead session, and
/// leaves it alone. A still-alive PID is killed only when it is verifiably our
/// Chrome (never a recycled-pid stranger).
pub fn invalidate_session_if_current(failed_ws_url: &str) {
    let _lock = match launch_lock_guard() {
        Ok(l) => l,
        Err(_) => return,
    };
    let current = std::fs::read_to_string(ws_url_path())
        .ok()
        .map(|s| s.trim().to_string());
    if current.as_deref() != Some(failed_ws_url) {
        // A concurrent open already replaced this session — don't touch it.
        return;
    }
    if let Ok(pid_str) = std::fs::read_to_string(pid_path())
        && let Ok(pid) = pid_str.trim().parse::<i32>()
        && is_our_chrome(pid)
    {
        let _ = send_signal(pid, libc::SIGKILL);
    }
    let _ = std::fs::remove_file(pid_path());
    let _ = std::fs::remove_file(ws_url_path());
}

/// Ensure a headless session is running. Uses an advisory file lock so that
/// concurrent agents do not race to launch Chrome.
pub async fn ensure_session() -> Result<String> {
    if let Some(url) = get_existing_session() {
        return Ok(url);
    }

    let _lock = launch_lock_guard()?;

    if let Some(url) = get_existing_session() {
        return Ok(url);
    }

    // Clean up an orphaned Chrome whose PID file we own — but only if it is
    // verifiably our Chrome, never a process that reused the recorded pid.
    if let Ok(pid_str) = std::fs::read_to_string(pid_path())
        && let Ok(pid) = pid_str.trim().parse::<i32>()
        && is_our_chrome(pid)
    {
        let _ = send_signal(pid, libc::SIGTERM);
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    let _ = std::fs::remove_file(pid_path());
    let _ = std::fs::remove_file(ws_url_path());

    // Reap any Chrome still holding our profile that we have no session for — an
    // orphan from a crash before the pid file was written. Left alive it keeps
    // the profile's SingletonLock and would wedge the launch below.
    kill_profile_orphans();

    let (_, ws_url) = launch_chrome().await?;
    Ok(ws_url)
}

/// Shut down the entire headless Chrome session. Idempotent.
pub async fn quit_session() -> Result<()> {
    // Serialize against a concurrent launch (`ensure_session` holds the same
    // lock): without it, quit could delete the pid/ws files a racing launch is
    // mid-write on, or SIGKILL a Chrome it just spawned before the opener has
    // read its ws URL. A blocking acquire waits out any launch in flight.
    let _lock = launch_lock_guard()?;
    let pid_file = pid_path();
    // Signal only a verifiably-our-Chrome pid: after a crash the recorded pid
    // can belong to an unrelated process that reused it.
    if let Ok(pid_str) = std::fs::read_to_string(&pid_file)
        && let Ok(pid) = pid_str.trim().parse::<i32>()
        && is_our_chrome(pid)
    {
        let _ = send_signal(pid, libc::SIGTERM);
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        if is_our_chrome(pid) {
            let _ = send_signal(pid, libc::SIGKILL);
        }
    }
    let _ = std::fs::remove_file(&pid_file);
    let _ = std::fs::remove_file(ws_url_path());

    // Per-(context|default) active-frame, active-tab, armed-monitor, and
    // device-emulation state — all session-scoped, gone with Chrome.
    if let Ok(entries) = std::fs::read_dir(dirs::runtime_dir()) {
        for entry in entries.filter_map(|e| e.ok()) {
            let n = entry.file_name();
            let name = n.to_string_lossy();
            if name.starts_with("active_frame_")
                || name.starts_with("active_tab_")
                || name.starts_with("monitor_")
                || name.starts_with("device_")
            {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    // Context state files (leave the `.lock` coordination files in place).
    let contexts = dirs::contexts_dir();
    if let Ok(entries) = std::fs::read_dir(&contexts) {
        for entry in entries.filter_map(|e| e.ok()) {
            if crate::transport::local_context::is_context_file(
                &entry.file_name().to_string_lossy(),
            ) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    // Chrome profile dir
    let profile_dir = dirs::chrome_profile_dir();
    if profile_dir.exists() {
        let _ = std::fs::remove_dir_all(&profile_dir);
    }

    // The launch lock file is a coordination primitive, not session state:
    // a concurrent agent may hold an `flock` on it right now. Unlinking it
    // would let the next launcher create a fresh inode and lock past us,
    // defeating the mutex — so it is intentionally left in place.

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
