//! Status command — shared rendering for both modes.
//!
//! `render` produces a `CommandOutput` from a typed `Status` payload.
//! `run` is the browser-mode entry point; the headless path in `cli.rs`
//! reaches `render` directly after opening a `LocalTransport`.

use anyhow::Result;
use webpilot::WebPilotError;
use webpilot::ipc::{self, IpcError};
use webpilot::protocol::{Command, Request, Response, ResponseData, RunMode};
use webpilot::types::{line_safe, line_safe_clip};

use crate::commands::setup::nm_host;
use crate::output::CommandOutput;

/// Render a typed `Status` response into the unified `CommandOutput` shape.
pub fn render(
    connected: bool,
    mode: RunMode,
    tab_url: Option<String>,
    tab_title: Option<String>,
    chrome_version: Option<String>,
    extension_version: Option<String>,
    context_label: Option<&str>,
) -> CommandOutput {
    let mut human = format!("Mode: {mode}");
    human.push_str(&format!("\nConnected: {connected}"));
    if let Some(ctx) = context_label {
        // `line_safe` like every other agent-facing field — a context name
        // carrying a newline must not forge an extra status line.
        human.push_str(&format!("\nContext: {}", line_safe(ctx)));
    }
    if let Some(ref v) = chrome_version {
        human.push_str(&format!("\nChrome: {v}"));
    }
    if let Some(ref v) = extension_version {
        human.push_str(&format!("\nExtension: v{v}"));
    }
    // `tab_title`/`tab_url` are page-controlled; `line_safe_clip` them at the
    // same 200-char cap the DOM footer uses so a crafted title can neither embed
    // a newline to forge a status line nor flood the line with an unbounded
    // string (the `--json` path is already safe via JSON escaping).
    if let Some(ref t) = tab_title {
        human.push_str(&format!("\nTab: {}", line_safe_clip(t, 200)));
    }
    if let Some(ref u) = tab_url {
        human.push_str(&format!("\nURL: {}", line_safe_clip(u, 200)));
    }

    CommandOutput::Data {
        json: serde_json::json!({
            "connected": connected,
            "mode": mode.to_string(),
            "tab_url": tab_url,
            "tab_title": tab_title,
            "chrome_version": chrome_version,
            "extension_version": extension_version,
            "context": context_label,
        }),
        human,
    }
}

/// Browser-mode status entry point. Produces an informative response even
/// when the IPC connection cannot be established.
pub async fn run() -> Result<CommandOutput> {
    // Reach the IPC layer directly rather than through `IpcTransport`, which
    // flattens every `IpcError` into `ConnectionLost`. `diagnose` needs the typed
    // variant — host-not-running vs timeout vs closed — to give the specific,
    // actionable hint; the generic transport error would erase it before the only
    // code that reads it.
    let request = Request::new(1, Command::Status);
    let raw = match ipc::send_request(&serde_json::to_value(&request)?).await {
        Ok(raw) => raw,
        Err(e) => return Ok(diagnose(&e)),
    };
    let response: Response =
        serde_json::from_value(raw).map_err(|e| WebPilotError::ConnectionLost {
            detail: format!("malformed reply from the Native Messaging host: {e}"),
        })?;
    match response.result {
        ResponseData::Status {
            connected,
            mode,
            tab_url,
            tab_title,
            chrome_version,
            extension_version,
        } => Ok(render(
            connected,
            mode,
            tab_url,
            tab_title,
            chrome_version,
            extension_version,
            None,
        )),
        ResponseData::Error { error } => Err(error.into()),
        _ => anyhow::bail!("Unexpected response shape"),
    }
}

/// Render a connection failure as an informative status (not an error). Takes the
/// typed `IpcError` directly — every variant maps to a specific hint, so there is
/// no generic catch-all to fall through to.
fn diagnose(error: &IpcError) -> CommandOutput {
    let (msg, hint) = match error {
        IpcError::HostNotRunning(path) => {
            let hint = match check_nm_manifest() {
                ManifestState::NotFound => {
                    "  NM manifest not found.\n  Run: webpilot setup nm-host".into()
                }
                ManifestState::InvalidJson => {
                    "  NM manifest is corrupted (invalid JSON).\n  Run: webpilot setup nm-host"
                        .into()
                }
                ManifestState::BinaryMissing(p) => format!(
                    "  NM manifest binary not found: {p}\n  Re-register with: webpilot setup nm-host"
                ),
                ManifestState::IdMismatch => format!(
                    "  NM manifest authorises a different extension id than this build \
                     (expected {}).\n  Re-register with: webpilot setup nm-host",
                    crate::assets::expected_extension_id()
                ),
                ManifestState::Ok => {
                    "  NM manifest OK. Ensure the extension is loaded and active in your browser."
                        .into()
                }
            };
            (format!("Host not running (socket: {path})"), hint)
        }
        IpcError::Timeout => (
            "Timed out waiting for host response.".into(),
            "  The NM host may be stuck. Reload the extension in your browser.".into(),
        ),
        IpcError::ConnectionClosed => (
            "Host closed the connection.".into(),
            "  The NM host may have crashed. Reload the extension in your browser.".into(),
        ),
        IpcError::Io(e) => (
            format!("Socket error: {e}"),
            "  Check that the NM host is running and the socket is accessible.".into(),
        ),
        IpcError::Json(e) => (
            format!("Invalid response from host: {e}"),
            "  The NM host sent malformed data. Try reloading the extension.".into(),
        ),
    };

    CommandOutput::Data {
        json: serde_json::json!({"connected": false, "mode": "browser", "error": msg}),
        human: format!("{msg}\n{hint}"),
    }
}

enum ManifestState {
    NotFound,
    InvalidJson,
    BinaryMissing(String),
    /// A manifest exists and is well-formed, but authorises a different extension
    /// id than this build's — a wrong `--extension-id` override that `status`
    /// would otherwise report as a healthy registration.
    IdMismatch,
    Ok,
}

/// Inspect one candidate manifest path. `None` = no manifest there (don't count
/// it); `Some(state)` classifies a manifest that does exist.
fn evaluate_manifest(path: &std::path::Path) -> Option<ManifestState> {
    let content = std::fs::read_to_string(path).ok()?;
    let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&content) else {
        return Some(ManifestState::InvalidJson);
    };
    if let Some(bin) = manifest.get("path").and_then(|v| v.as_str())
        && !std::path::Path::new(bin).exists()
    {
        return Some(ManifestState::BinaryMissing(bin.to_owned()));
    }
    // The manifest must authorise this binary's own extension id, or the loaded
    // WebPilot extension can never connect to it.
    let expected = format!(
        "chrome-extension://{}/",
        crate::assets::expected_extension_id()
    );
    let authorizes_expected = manifest
        .get("allowed_origins")
        .and_then(|v| v.as_array())
        .is_some_and(|origins| {
            origins
                .iter()
                .filter_map(|o| o.as_str())
                .any(|o| o == expected)
        });
    if !authorizes_expected {
        return Some(ManifestState::IdMismatch);
    }
    Some(ManifestState::Ok)
}

/// Classify the host registration across every Chrome-family NM directory.
/// One healthy manifest is enough — that's the browser the extension was loaded
/// into; otherwise the most actionable problem among the manifests that exist.
fn check_nm_manifest() -> ManifestState {
    let Ok(paths) = nm_host::nm_manifest_paths() else {
        return ManifestState::NotFound;
    };
    let states: Vec<ManifestState> = paths.iter().filter_map(|p| evaluate_manifest(p)).collect();
    if states.is_empty() {
        return ManifestState::NotFound;
    }
    if states.iter().any(|s| matches!(s, ManifestState::Ok)) {
        return ManifestState::Ok;
    }
    states
        .into_iter()
        .max_by_key(|s| match s {
            ManifestState::BinaryMissing(_) => 3,
            ManifestState::IdMismatch => 2,
            ManifestState::InvalidJson => 1,
            ManifestState::NotFound | ManifestState::Ok => 0,
        })
        .unwrap_or(ManifestState::NotFound)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::CommandOutput;

    #[test]
    fn render_caps_a_page_controlled_flooded_title_and_url() {
        // `tab_title`/`tab_url` are page-controlled (a page sets `document.title`
        // and its own URL); a hostile page could otherwise flood `status` with one
        // unbounded line. `line_safe_clip` bounds each at 200 — the cap the DOM
        // footer already uses — while the full value stays in the JSON.
        // `Z` / `q` appear nowhere in the framing ("Mode/Connected/Tab/URL",
        // "Headless", "https://evil.test/"), so each count measures exactly the
        // rendered value, not the labels around it.
        let out = render(
            true,
            RunMode::Headless,
            Some("https://evil.test/".to_owned() + &"q".repeat(50_000)),
            Some("Z".repeat(50_000)),
            None,
            None,
            None,
        );
        let CommandOutput::Data { human, .. } = out else {
            panic!("status renders Data");
        };
        // No single line in the human output exceeds the cap (+ a little framing).
        for line in human.lines() {
            assert!(
                line.chars().count() <= 256,
                "no agent-facing status line floods: {} chars",
                line.chars().count()
            );
        }
        assert!(human.matches('Z').count() <= 200, "title capped at 200");
        assert!(human.matches('q').count() <= 200, "url capped at 200");
    }

    #[test]
    fn evaluate_manifest_classifies_every_state() {
        let dir = std::env::temp_dir().join(format!("wp-status-nm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let id = crate::assets::expected_extension_id();
        let realbin = std::env::current_exe().unwrap();
        let write = |name: &str, body: String| {
            let p = dir.join(name);
            std::fs::write(&p, body).unwrap();
            p
        };

        // No file at the path → not counted.
        assert!(evaluate_manifest(&dir.join("absent.json")).is_none());
        // Corrupt JSON.
        assert!(matches!(
            evaluate_manifest(&write("bad.json", "{ not json".into())),
            Some(ManifestState::InvalidJson)
        ));
        // Points at a binary that doesn't exist.
        assert!(matches!(
            evaluate_manifest(&write(
                "binmissing.json",
                format!(
                    r#"{{"path":"/no/such/bin","allowed_origins":["chrome-extension://{id}/"]}}"#
                )
            )),
            Some(ManifestState::BinaryMissing(_))
        ));
        // Well-formed, binary present, but authorises a DIFFERENT id (a wrong
        // `--extension-id` override) — the case a healthy-looking manifest hides.
        assert!(matches!(
            evaluate_manifest(&write(
                "mismatch.json",
                format!(
                    r#"{{"path":"{}","allowed_origins":["chrome-extension://abcdefghijklmnopabcdefghijklmnop/"]}}"#,
                    realbin.display()
                )
            )),
            Some(ManifestState::IdMismatch)
        ));
        // Healthy: binary present and authorises this build's own id.
        assert!(matches!(
            evaluate_manifest(&write(
                "ok.json",
                format!(
                    r#"{{"path":"{}","allowed_origins":["chrome-extension://{id}/"]}}"#,
                    realbin.display()
                )
            )),
            Some(ManifestState::Ok)
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
