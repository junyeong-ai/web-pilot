use anyhow::Result;
use clap::Args;
use std::path::PathBuf;

use crate::output::OutputMode;

#[derive(Args)]
pub struct InstallArgs {
    /// Chrome extension ID (from chrome://extensions after loading)
    #[arg(long)]
    pub extension_id: Option<String>,
}

pub async fn run(args: InstallArgs, output_mode: OutputMode) -> Result<()> {
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

    match output_mode {
        OutputMode::Human => {
            println!("Native Messaging host installed!");
            println!("  Manifest: {}", manifest_path.display());
            println!("  Binary:   {}", binary_path.display());
            println!();
            if ext_id == "EXTENSION_ID_HERE" {
                println!("Next steps:");
                println!("  1. Load extension in Chrome:");
                println!("     chrome://extensions -> Developer mode -> Load unpacked");
                println!("     Select: extension/");
                println!("  2. Copy the Extension ID and re-run:");
                println!("     webpilot install --extension-id <ID>");
                println!("  3. Reload the extension in Chrome");
                println!("  4. Test: webpilot status");
            } else {
                println!("Extension ID: {ext_id}");
                println!("Reload the extension in Chrome, then test: webpilot status");
            }
        }
        OutputMode::Json => {
            println!(
                "{}",
                serde_json::json!({
                    "installed": true,
                    "manifest_path": manifest_path.display().to_string(),
                    "binary_path": binary_path.display().to_string(),
                    "extension_id": ext_id,
                })
            );
        }
    }

    Ok(())
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
