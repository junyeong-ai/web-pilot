//! `webpilot setup extension` — materialise the embedded Chrome extension.
//!
//! Chrome cannot be programmatically asked to load an unpacked extension, so
//! the binary's job is twofold:
//! 1. Extract the embedded extension tree to a stable, durable path
//!    (`webpilot::dirs::extension_dir()`).
//! 2. Walk the user through `chrome://extensions → Load unpacked`.
//!
//! The path is durable on purpose: Chrome stores an absolute path to the
//! unpacked directory, so we cannot place the files under the cache root.

use anyhow::{Context, Result};
use clap::Args;
use std::process::Command;

use crate::assets;
use crate::output::CommandOutput;

use super::{ChromeOpen, StepOutcome, home_relative};

#[derive(Args)]
pub struct ExtensionArgs {
    /// Print the destination path and exit; do not extract.
    #[arg(long)]
    pub path: bool,
}

pub fn run(args: ExtensionArgs, yes: bool, open: bool) -> Result<CommandOutput> {
    if args.path {
        // `--path` is a query, not an action: report the destination without
        // creating it.
        let dir = webpilot::dirs::extension_dir_path();
        return Ok(CommandOutput::Data {
            json: serde_json::json!({ "path": dir.display().to_string() }),
            human: dir.display().to_string(),
        });
    }

    let mut outcome = install(ChromeOpen::from_flags(yes, open))?;
    // Standalone `setup extension`: point the user at the one remaining step.
    // The id is derived, so `setup nm-host` needs no argument. (The orchestrated
    // `setup` runs nm-host itself, so it composes its own message instead.)
    outcome.human.push_str(
        "\n\n  Then enable browser mode (extension id auto-detected):\n    webpilot setup nm-host",
    );
    Ok(CommandOutput::Data {
        json: outcome.json,
        human: outcome.human,
    })
}

/// Extract the extension tree and apply the chosen Chrome-open policy.
///
/// The extension is always overwritten — its content is purely a function of
/// the binary version, so a prompt would never have a useful answer.
pub(crate) fn install(chrome: ChromeOpen) -> Result<StepOutcome> {
    let dest = webpilot::dirs::extension_dir();

    assets::write_dir(&assets::EXTENSION, &dest)
        .with_context(|| format!("write extension to {}", dest.display()))?;

    let opened = chrome.should_open() && open_chrome_extensions().is_ok();

    let mut human = String::new();
    human.push_str(&format!(
        "✓ Extension extracted to {}\n",
        home_relative(&dest)
    ));
    human.push_str("  Load it in Chrome:\n");
    human.push_str("    1. Open chrome://extensions\n");
    human.push_str("    2. Enable Developer mode (top-right toggle)\n");
    human.push_str("    3. Click \"Load unpacked\" and select:\n");
    human.push_str(&format!("       {}", dest.display()));

    Ok(StepOutcome {
        json: serde_json::json!({
            "path": dest.display().to_string(),
            "opened_chrome_extensions": opened,
        }),
        human,
    })
}

fn open_chrome_extensions() -> Result<()> {
    let url = "chrome://extensions";
    let (cmd, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("open", &["-a", "Google Chrome", url])
    } else {
        ("xdg-open", &[url])
    };
    let status = Command::new(cmd).args(args).status()?;
    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("opener exited with {status}")
    }
}
