//! `webpilot setup nm-host` — register the Native Messaging host manifest.
//!
//! After loading the unpacked extension, Chrome assigns it a 32-character
//! Extension ID. The NM host manifest binds that ID to the absolute path of
//! the `webpilot` binary, so Chrome can spawn the binary as an NM host on
//! demand.

use anyhow::{Context, Result};
use clap::Args;
use std::path::PathBuf;

use crate::output::CommandOutput;

#[derive(Args)]
pub struct NmHostArgs {
    /// Chrome extension ID copied from chrome://extensions.
    #[arg(long)]
    pub extension_id: String,
}

pub fn run(args: NmHostArgs) -> Result<CommandOutput> {
    let ext_id = args.extension_id.trim().to_owned();
    if !is_valid_extension_id(&ext_id) {
        anyhow::bail!(
            "Invalid extension ID: {ext_id}\n  \
             Expected 32 characters in [a-p]. Find it at chrome://extensions \
             with Developer mode enabled."
        );
    }

    // Chrome's NM API requires an absolute path; a relative `webpilot` would
    // be silently mis-launched. Refuse to write a manifest we know is broken.
    let binary_path = std::env::current_exe()
        .context("could not locate own binary path")?
        .canonicalize()
        .context("could not canonicalise own binary path")?;

    let manifest = serde_json::json!({
        "name": "com.webpilot.host",
        "description": "WebPilot — Browser control tool for AI agents",
        "path": binary_path.display().to_string(),
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{ext_id}/")],
    });

    let nm_dir = nm_dir();
    std::fs::create_dir_all(&nm_dir)?;

    let manifest_path = nm_dir.join("com.webpilot.host.json");
    let json = serde_json::to_string_pretty(&manifest).expect("manifest is a static-shape Value");
    std::fs::write(&manifest_path, &json)?;

    let human = format!(
        "✓ NM host registered\n  \
         Manifest: {}\n  \
         Binary:   {}\n  \
         Extension: {ext_id}\n\
         \n  \
         Reload the extension in Chrome, then verify with:\n  \
        \x20\x20webpilot --browser status",
        manifest_path.display(),
        binary_path.display(),
    );

    Ok(CommandOutput::Data {
        json: serde_json::json!({
            "manifest_path": manifest_path.display().to_string(),
            "binary_path": binary_path.display().to_string(),
            "extension_id": ext_id,
        }),
        human,
    })
}

/// Native Messaging host manifest directory for Chrome on this platform.
pub fn nm_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    if cfg!(target_os = "macos") {
        home.join("Library")
            .join("Application Support")
            .join("Google")
            .join("Chrome")
            .join("NativeMessagingHosts")
    } else {
        home.join(".config")
            .join("google-chrome")
            .join("NativeMessagingHosts")
    }
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
