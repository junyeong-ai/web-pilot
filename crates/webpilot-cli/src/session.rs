//! Headless Chrome session management via direct CDP.
//! No Extension or Native Messaging needed — uses Chrome DevTools Protocol directly.

use anyhow::{Context, Result};
use std::path::PathBuf;

/// Chrome for Testing paths (installed by agent-browser or manually).
const CHROME_FOR_TESTING_PATHS: &[&str] = &[
    // agent-browser installed (latest version dirs checked at runtime)
    // macOS standard Chrome
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
];

/// Find a Chrome binary. Prefers Chrome for Testing (no single-instance lock).
pub fn find_chrome() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("WEBPILOT_CHROME") {
        let p = PathBuf::from(&path);
        if p.exists() {
            return Ok(p);
        }
        anyhow::bail!("WEBPILOT_CHROME={path} not found");
    }

    // Check agent-browser's Chrome for Testing (preferred — no single-instance lock)
    let home = std::env::var("HOME").unwrap_or_default();
    let browsers_dir = PathBuf::from(&home).join(".agent-browser/browsers");
    if browsers_dir.exists()
        && let Ok(entries) = std::fs::read_dir(&browsers_dir)
    {
        let mut versions: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        versions.sort_by_key(|b| std::cmp::Reverse(b.file_name())); // Latest first
        for entry in versions {
            let app = entry
                .path()
                .join("Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing");
            if app.exists() {
                return Ok(app);
            }
            // Alternative layout
            let app2 = entry.path()
                    .join("chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing");
            if app2.exists() {
                return Ok(app2);
            }
        }
    }

    // Fallback: system Chrome
    for c in CHROME_FOR_TESTING_PATHS {
        let p = PathBuf::from(c);
        if p.exists() {
            return Ok(p);
        }
    }

    // Linux PATH
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

/// Send a signal to a process. Returns Ok(true) if delivered, Ok(false) if process doesn't exist.
fn send_signal(pid: i32, signal: i32) -> Result<bool> {
    // SAFETY: kill() is a standard POSIX syscall with no memory safety implications.
    // pid is validated from our PID file; signal is a well-known constant.
    let ret = unsafe { libc::kill(pid, signal) };
    if ret == 0 {
        return Ok(true);
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ESRCH) {
        Ok(false) // No such process
    } else {
        Err(err.into())
    }
}

/// Check if a process is alive (signal 0 probe).
fn is_process_alive(pid: i32) -> bool {
    send_signal(pid, 0).unwrap_or(false)
}

/// PID file path.
pub fn pid_path() -> PathBuf {
    let user = std::env::var("USER").unwrap_or_else(|_| "default".into());
    PathBuf::from(format!("/tmp/webpilot-{user}-headless.pid"))
}

/// WebSocket URL file path.
pub fn ws_url_path() -> PathBuf {
    let user = std::env::var("USER").unwrap_or_else(|_| "default".into());
    PathBuf::from(format!("/tmp/webpilot-{user}-headless.ws"))
}

/// Read the Chrome PID from the PID file. Returns 0 if unavailable.
pub fn read_pid() -> i32 {
    std::fs::read_to_string(pid_path())
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
        .unwrap_or(0)
}

/// Launch headless Chrome and return CDP WebSocket URL.
pub fn launch_chrome() -> Result<(u32, String)> {
    let chrome = find_chrome()?;
    let user = std::env::var("USER").unwrap_or_else(|_| "default".into());
    let profile_dir = PathBuf::from(format!("/tmp/webpilot-{user}-headless-profile"));
    let _ = std::fs::create_dir_all(&profile_dir);

    // Clean stale DevToolsActivePort
    let devtools_port_file = profile_dir.join("DevToolsActivePort");
    let _ = std::fs::remove_file(&devtools_port_file);

    eprintln!("Launching headless Chrome...");

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
            "--window-size=1280,720",
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
    // No piped handles → no SIGPIPE risk when CLI exits.
    std::mem::forget(child);

    // Read WebSocket URL from DevToolsActivePort file (Puppeteer/Playwright standard).
    // Chrome writes this file asynchronously after startup.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let ws_url = loop {
        if std::time::Instant::now() > deadline {
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
        std::thread::sleep(std::time::Duration::from_millis(100));
    };

    // Save PID and WS URL
    std::fs::write(pid_path(), pid.to_string())?;
    std::fs::write(ws_url_path(), &ws_url)?;

    eprintln!("Headless Chrome ready (pid {pid})");
    Ok((pid, ws_url))
}

/// Get WebSocket URL for an already-running headless session.
pub fn get_existing_session() -> Option<String> {
    let ws_file = ws_url_path();
    let pid_file = pid_path();

    if !ws_file.exists() || !pid_file.exists() {
        return None;
    }

    // Check if Chrome is still alive
    if let Ok(pid_str) = std::fs::read_to_string(&pid_file)
        && let Ok(pid) = pid_str.trim().parse::<i32>()
        && is_process_alive(pid)
    {
        return std::fs::read_to_string(&ws_file)
            .ok()
            .map(|s| s.trim().to_string());
    }

    // Stale files
    let _ = std::fs::remove_file(&ws_file);
    let _ = std::fs::remove_file(&pid_file);
    None
}

/// Ensure a headless session is running. Returns CDP WebSocket URL.
pub fn ensure_session() -> Result<String> {
    if let Some(url) = get_existing_session() {
        return Ok(url);
    }

    // Clean up orphaned Chrome
    if let Ok(pid_str) = std::fs::read_to_string(pid_path())
        && let Ok(pid) = pid_str.trim().parse::<i32>()
        && is_process_alive(pid)
    {
        let _ = send_signal(pid, libc::SIGTERM);
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    let _ = std::fs::remove_file(pid_path());
    let _ = std::fs::remove_file(ws_url_path());

    let (_, ws_url) = launch_chrome()?;
    Ok(ws_url)
}

/// Shut down headless Chrome session.
pub fn quit_session() -> Result<()> {
    let pid_file = pid_path();
    if pid_file.exists() {
        if let Ok(pid_str) = std::fs::read_to_string(&pid_file)
            && let Ok(pid) = pid_str.trim().parse::<i32>()
        {
            let _ = send_signal(pid, libc::SIGTERM);
            std::thread::sleep(std::time::Duration::from_secs(1));
            if is_process_alive(pid) {
                let _ = send_signal(pid, libc::SIGKILL);
            }
        }
        let _ = std::fs::remove_file(&pid_file);
    }
    let _ = std::fs::remove_file(ws_url_path());

    // Clean up context files
    let dir = std::path::Path::new(webpilot::OUTPUT_DIR);
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            if entry.file_name().to_string_lossy().starts_with("ctx-") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    eprintln!("Headless session stopped.");
    Ok(())
}
