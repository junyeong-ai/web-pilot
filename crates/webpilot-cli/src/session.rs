//! Headless Chrome session management via direct CDP.
//! No Extension or Native Messaging needed — uses Chrome DevTools Protocol directly.

use anyhow::{Context, Result};
use std::io::BufRead;
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
    if browsers_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&browsers_dir) {
            let mut versions: Vec<_> = entries.filter_map(|e| e.ok()).collect();
            versions.sort_by(|a, b| b.file_name().cmp(&a.file_name())); // Latest first
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
    {
        if out.status.success() {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !path.is_empty() {
                return Ok(PathBuf::from(path));
            }
        }
    }

    anyhow::bail!("Chrome not found. Install Chrome or set WEBPILOT_CHROME=/path/to/chrome")
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

/// Launch headless Chrome and return CDP WebSocket URL.
pub fn launch_chrome() -> Result<(u32, String)> {
    let chrome = find_chrome()?;
    let user = std::env::var("USER").unwrap_or_else(|_| "default".into());
    let profile_dir = PathBuf::from(format!("/tmp/webpilot-{user}-headless-profile"));
    let _ = std::fs::create_dir_all(&profile_dir);

    // Clean stale DevToolsActivePort
    let _ = std::fs::remove_file(profile_dir.join("DevToolsActivePort"));

    eprintln!("Launching headless Chrome...");

    let mut child = std::process::Command::new(&chrome)
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
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("Failed to launch Chrome")?;

    let pid = child.id();

    // Parse WebSocket URL from stderr: "DevTools listening on ws://..."
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("No stderr from Chrome"))?;
    let reader = std::io::BufReader::new(stderr);

    let mut ws_url = None;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);

    for line in reader.lines() {
        if std::time::Instant::now() > deadline {
            break;
        }
        if let Ok(line) = line {
            if let Some(url) = line.strip_prefix("DevTools listening on ") {
                ws_url = Some(url.trim().to_string());
                break;
            }
        }
    }

    let ws_url = ws_url.ok_or_else(|| {
        let _ = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        anyhow::anyhow!("Chrome started but no DevTools URL. Is this Chrome for Testing?")
    })?;

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
    if let Ok(pid_str) = std::fs::read_to_string(&pid_file) {
        if let Ok(pid) = pid_str.trim().parse::<i32>() {
            let alive = unsafe { libc::kill(pid, 0) == 0 };
            if alive {
                return std::fs::read_to_string(&ws_file)
                    .ok()
                    .map(|s| s.trim().to_string());
            }
        }
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
    if let Ok(pid_str) = std::fs::read_to_string(pid_path()) {
        if let Ok(pid) = pid_str.trim().parse::<i32>() {
            unsafe {
                if libc::kill(pid, 0) == 0 {
                    libc::kill(pid, libc::SIGTERM);
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            }
        }
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
        if let Ok(pid_str) = std::fs::read_to_string(&pid_file) {
            if let Ok(pid) = pid_str.trim().parse::<i32>() {
                unsafe {
                    libc::kill(pid, libc::SIGTERM);
                }
                std::thread::sleep(std::time::Duration::from_secs(1));
                unsafe {
                    if libc::kill(pid, 0) == 0 {
                        libc::kill(pid, libc::SIGKILL);
                    }
                }
            }
        }
        let _ = std::fs::remove_file(&pid_file);
    }
    let _ = std::fs::remove_file(ws_url_path());
    eprintln!("Headless session stopped.");
    Ok(())
}
