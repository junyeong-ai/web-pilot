//! `webpilot setup nm-host` — register the Native Messaging host manifest.
//!
//! The manifest binds the extension's id to the absolute path of the `webpilot`
//! binary, so Chrome can spawn it as an NM host on demand. The id does **not**
//! have to be read off `chrome://extensions`: the embedded manifest pins a
//! public `key`, so the extension's id is a stable constant this binary derives
//! itself (`assets::expected_extension_id`). `--extension-id` is therefore
//! optional — pass it only to authorise a *different* build.

use anyhow::{Context, Result};
use clap::Args;
use std::path::{Path, PathBuf};

use super::StepOutcome;
use crate::assets;
use crate::output::CommandOutput;

#[derive(Args)]
pub struct NmHostArgs {
    /// Extension id to authorise. Defaults to this binary's own extension id,
    /// derived from the embedded manifest key (stable across machines). Pass it
    /// only to authorise a different build.
    #[arg(long)]
    pub extension_id: Option<String>,
}

pub fn run(args: NmHostArgs) -> Result<CommandOutput> {
    let outcome = install(args.extension_id)?;
    Ok(CommandOutput::Data {
        json: outcome.json,
        human: outcome.human,
    })
}

/// Write the NM host manifest authorising `extension_id`, or this binary's own
/// derived id when `None`. Shared by `setup nm-host` and the orchestrated
/// `setup` walkthrough.
pub(crate) fn install(extension_id: Option<String>) -> Result<StepOutcome> {
    let (ext_id, auto) = match extension_id {
        Some(id) => (id.trim().to_owned(), false),
        None => (assets::expected_extension_id().to_owned(), true),
    };
    if !is_valid_extension_id(&ext_id) {
        // A malformed extension ID is user input — a typed InvalidArgument (exit 7),
        // not a generic Other (exit 1): exit codes name the error class, never
        // inferred from a message. (The derived id is always valid; this guards
        // an explicit override.)
        return Err(webpilot::WebPilotError::InvalidArgument {
            detail: format!(
                "invalid extension ID: {ext_id} — expected 32 characters in [a-p], \
                 found at chrome://extensions with Developer mode enabled"
            ),
        }
        .into());
    }

    let targets = nm_target_dirs()?;
    if targets.is_empty() {
        // No fallback: a manifest written into a browser directory that does not
        // exist registers the host with nothing. Report it honestly and write
        // nothing, rather than print a ✓ for a registration that can't work.
        return Ok(StepOutcome {
            json: serde_json::json!({
                "manifest_paths": [],
                "extension_id": ext_id,
                "auto_detected": auto,
            }),
            human: format!(
                "  NM host not registered — no Chrome-family browser found.\n  \
                 Install Chrome (or Chromium / Brave / Edge), then: webpilot setup nm-host\n  \
                 Extension: {ext_id}"
            ),
        });
    }

    // Chrome's NM API requires an absolute path; a relative `webpilot` would
    // be silently mis-launched. Refuse to write a manifest we know is broken.
    let binary_path = std::env::current_exe()
        .context("could not locate own binary path")?
        .canonicalize()
        .context("could not canonicalise own binary path")?;

    let manifest = serde_json::json!({
        "name": NM_HOST_NAME,
        "description": "WebPilot — Browser control tool for AI agents",
        "path": binary_path.display().to_string(),
        "type": NM_HOST_TYPE,
        "allowed_origins": [format!("chrome-extension://{ext_id}/")],
    });

    let json = serde_json::to_string_pretty(&manifest).expect("manifest is a static-shape Value");
    // Register with every Chrome-family browser actually installed (its profile
    // dir exists) — the standard native-messaging pattern: the manifest is inert
    // until that browser loads the WebPilot extension, so browser mode then works
    // wherever you load it (Chrome, Chromium, Brave, Edge, a Chrome channel)
    // without guessing which one. Atomic per file: an interrupted write must not
    // truncate a working manifest.
    let mut manifest_paths = Vec::new();
    for dir in targets {
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{NM_HOST_NAME}.json"));
        webpilot::dirs::atomic_write(&path, json.as_bytes())?;
        manifest_paths.push(path);
    }

    let manifests: String = manifest_paths
        .iter()
        .map(|p| format!("\n  Manifest: {}", p.display()))
        .collect();
    let human = format!(
        "✓ NM host registered{}{}\n  \
         Binary:   {}\n  \
         Extension: {ext_id}\n\
         \n  \
         Once the unpacked extension is loaded in your browser, verify with:\n  \
        \x20\x20webpilot --browser status",
        if auto {
            " (extension id auto-detected)"
        } else {
            ""
        },
        manifests,
        binary_path.display(),
    );

    Ok(StepOutcome {
        json: serde_json::json!({
            "manifest_paths": manifest_paths
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>(),
            "binary_path": binary_path.display().to_string(),
            "extension_id": ext_id,
            "auto_detected": auto,
        }),
        human,
    })
}

/// Resolve `$HOME`. An unset home is an error, never a `/tmp` guess — writing a
/// manifest somewhere no browser will read it would report a broken
/// registration as success.
fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|h| !h.as_os_str().is_empty())
        .context("HOME is not set — cannot locate a browser's Native Messaging directory")
}

/// Every per-user Native-Messaging directory a Chrome-family browser reads on
/// this platform. A manifest is honoured only by a browser that has the WebPilot
/// extension loaded, so listing every candidate is inert for absent browsers — it
/// just authorises the host wherever the extension ends up. This single list is
/// the one place to support another Chromium-based browser.
fn nm_candidate_dirs() -> Result<Vec<PathBuf>> {
    Ok(nm_candidate_dirs_under(&home_dir()?))
}

/// Pure core of [`nm_candidate_dirs`]: the candidate set rooted at `home`,
/// separated from the `$HOME` lookup so it is testable without env mutation.
fn nm_candidate_dirs_under(home: &Path) -> Vec<PathBuf> {
    let leaf = "NativeMessagingHosts";
    if cfg!(target_os = "macos") {
        let base = home.join("Library").join("Application Support");
        [
            "Google/Chrome",
            "Google/Chrome Beta",
            "Google/Chrome Dev",
            "Google/Chrome Canary",
            "Google/Chrome for Testing",
            "Chromium",
            "BraveSoftware/Brave-Browser",
            "Microsoft Edge",
        ]
        .iter()
        .map(|b| base.join(b).join(leaf))
        .collect()
    } else {
        let base = home.join(".config");
        [
            "google-chrome",
            "google-chrome-beta",
            "google-chrome-unstable",
            "chromium",
            "BraveSoftware/Brave-Browser",
            "microsoft-edge",
        ]
        .iter()
        .map(|b| base.join(b).join(leaf))
        .collect()
    }
}

/// The directories `setup` registers in: every Chrome-family browser actually
/// installed — its profile root (the parent of `NativeMessagingHosts`) exists,
/// which a browser creates on first run. Possibly empty: a machine with no
/// Chrome-family browser has nothing to register with, and the caller reports
/// that honestly rather than writing a manifest no browser will ever read.
fn nm_target_dirs() -> Result<Vec<PathBuf>> {
    Ok(nm_candidate_dirs()?
        .into_iter()
        .filter(|d| d.parent().is_some_and(Path::is_dir))
        .collect())
}

/// The Native Messaging host name — the manifest's `name`, the file stem Chrome
/// looks up, and what the extension connects to. One source for all three (and
/// `tests/browser_parity.rs` pins the extension's `connectNative` literal to it).
pub const NM_HOST_NAME: &str = "com.webpilot.host";

/// The Native Messaging transport. Fixed by Chrome's NM spec; one source so the
/// manifest writer and the `status` validator can never disagree on it.
pub const NM_HOST_TYPE: &str = "stdio";

/// Every candidate host-manifest path on this platform — the set `setup` may
/// write, `status` inspects, and `uninstall` removes. One source for all three.
pub fn nm_manifest_paths() -> Result<Vec<PathBuf>> {
    Ok(nm_candidate_dirs()?
        .into_iter()
        .map(|d| d.join(format!("{NM_HOST_NAME}.json")))
        .collect())
}

/// Chrome extension IDs are 32 characters, each in `[a-p]`.
fn is_valid_extension_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_lowercase() && b <= b'p')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_extension_id() {
        assert!(is_valid_extension_id("abcdefghijklmnopabcdefghijklmnop"));
    }

    #[test]
    fn rejects_invalid_chars() {
        assert!(!is_valid_extension_id("ABCDEFGHIJKLMNOPABCDEFGHIJKLMNOP"));
        assert!(!is_valid_extension_id("qrstuvwxyzabcdefqrstuvwxyzabcdef"));
    }

    #[test]
    fn rejects_wrong_length() {
        assert!(!is_valid_extension_id("abc"));
        assert!(!is_valid_extension_id("abcdefghijklmnopabcdefghijklmnopX"));
    }

    #[test]
    fn targets_are_only_browsers_whose_profile_root_exists() {
        let home = std::env::temp_dir().join(format!("wp-nm-targets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let candidates = nm_candidate_dirs_under(&home);
        assert!(
            candidates.len() >= 4,
            "platform should list several browsers"
        );

        // No browser installed yet → no targets (and the caller writes nothing).
        let targets = |c: &[PathBuf]| -> Vec<PathBuf> {
            c.iter()
                .filter(|d| d.parent().is_some_and(Path::is_dir))
                .cloned()
                .collect()
        };
        assert!(targets(&candidates).is_empty());

        // Create the profile roots of exactly the first two browsers (the parent
        // of `NativeMessagingHosts`); only those two become targets.
        std::fs::create_dir_all(candidates[0].parent().unwrap()).unwrap();
        std::fs::create_dir_all(candidates[1].parent().unwrap()).unwrap();
        let got = targets(&candidates);
        assert_eq!(got.len(), 2, "only installed browsers are targets: {got:?}");
        assert_eq!(got[0], candidates[0]);
        assert_eq!(got[1], candidates[1]);

        let _ = std::fs::remove_dir_all(&home);
    }
}
