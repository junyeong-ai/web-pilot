use anyhow::Result;
use clap::Args;
use std::path::PathBuf;

use crate::output::CommandOutput;

#[derive(Args)]
pub struct InstallArgs {
    /// Chrome extension ID (from chrome://extensions after loading)
    #[arg(long)]
    pub extension_id: Option<String>,
}

pub async fn run(args: InstallArgs) -> Result<CommandOutput> {
    let binary_path = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("webpilot"));

    let ext_id = args
        .extension_id
        .unwrap_or_else(|| "EXTENSION_ID_HERE".to_string());

    let host_manifest = serde_json::json!({
        "name": "com.webpilot.host",
        "description": "WebPilot - Browser control tool for AI agents",
        "path": binary_path.display().to_string(),
        "type": "stdio",
        "allowed_origins": [
            format!("chrome-extension://{ext_id}/")
        ]
    });

    let nm_dir = dirs_nm();
    std::fs::create_dir_all(&nm_dir)?;

    let manifest_path = nm_dir.join("com.webpilot.host.json");
    let json = serde_json::to_string_pretty(&host_manifest)?;
    std::fs::write(&manifest_path, &json)?;

    let human = if ext_id == "EXTENSION_ID_HERE" {
        format!(
            "Native Messaging host installed!\n  Manifest: {}\n  Binary:   {}\n\nNext steps:\n  1. Load extension in Chrome:\n     chrome://extensions -> Developer mode -> Load unpacked\n     Select: extension/\n  2. Copy the Extension ID and re-run:\n     webpilot install --extension-id <ID>\n  3. Reload the extension in Chrome\n  4. Test: webpilot status",
            manifest_path.display(),
            binary_path.display()
        )
    } else {
        format!(
            "Native Messaging host installed!\n  Manifest: {}\n  Binary:   {}\n\nExtension ID: {ext_id}\nReload the extension in Chrome, then test: webpilot status",
            manifest_path.display(),
            binary_path.display()
        )
    };

    Ok(CommandOutput::Data {
        json: serde_json::json!({
            "installed": true,
            "manifest_path": manifest_path.display().to_string(),
            "binary_path": binary_path.display().to_string(),
            "extension_id": ext_id,
        }),
        human,
    })
}

fn dirs_nm() -> PathBuf {
    if cfg!(target_os = "macos") {
        dirs_home()
            .join("Library")
            .join("Application Support")
            .join("Google")
            .join("Chrome")
            .join("NativeMessagingHosts")
    } else {
        dirs_home()
            .join(".config")
            .join("google-chrome")
            .join("NativeMessagingHosts")
    }
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}
