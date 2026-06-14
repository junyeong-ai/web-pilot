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
use std::path::PathBuf;

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
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{ext_id}/")],
    });

    let nm_dir = nm_dir()?;
    std::fs::create_dir_all(&nm_dir)?;

    let manifest_path = nm_dir.join(format!("{NM_HOST_NAME}.json"));
    let json = serde_json::to_string_pretty(&manifest).expect("manifest is a static-shape Value");
    // Atomic: an interrupted write must not truncate a working manifest and
    // leave browser mode unable to launch the host.
    webpilot::dirs::atomic_write(&manifest_path, json.as_bytes())?;

    let human = format!(
        "✓ NM host registered{}\n  \
         Manifest: {}\n  \
         Binary:   {}\n  \
         Extension: {ext_id}\n\
         \n  \
         Once the unpacked extension is loaded in Chrome, verify with:\n  \
        \x20\x20webpilot --browser status",
        if auto {
            " (extension id auto-detected)"
        } else {
            ""
        },
        manifest_path.display(),
        binary_path.display(),
    );

    Ok(StepOutcome {
        json: serde_json::json!({
            "manifest_path": manifest_path.display().to_string(),
            "binary_path": binary_path.display().to_string(),
            "extension_id": ext_id,
            "auto_detected": auto,
        }),
        human,
    })
}

/// Native Messaging host manifest directory for Chrome on this platform.
///
/// Derived from `$HOME`: an unset home is an error, never a `/tmp` guess —
/// writing the manifest somewhere Chrome will never read it would report a
/// broken registration as success.
pub fn nm_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|h| !h.as_os_str().is_empty())
        .context("HOME is not set — cannot locate Chrome's Native Messaging directory")?;
    Ok(if cfg!(target_os = "macos") {
        home.join("Library")
            .join("Application Support")
            .join("Google")
            .join("Chrome")
            .join("NativeMessagingHosts")
    } else {
        home.join(".config")
            .join("google-chrome")
            .join("NativeMessagingHosts")
    })
}

/// The Native Messaging host name — the manifest's `name`, the file stem Chrome
/// looks up, and what the extension connects to. One source for all three.
pub const NM_HOST_NAME: &str = "com.webpilot.host";

/// Full path to the installed host manifest. The one place `setup`, `status`,
/// and `uninstall` agree on where it lives.
pub fn nm_manifest_path() -> Result<PathBuf> {
    Ok(nm_dir()?.join(format!("{NM_HOST_NAME}.json")))
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
}
