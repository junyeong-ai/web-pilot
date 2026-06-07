//! `webpilot uninstall` — undo every artefact this binary created.
//!
//! Inverse of `setup` plus binary removal. Idempotent: every step is a
//! best-effort `remove_*` that succeeds even when the artefact is missing,
//! so the command terminates cleanly on a partially-installed system.
//!
//! The `Plan` lists user-visible artefacts; container directories that are
//! pure implementation detail (e.g. the data root that holds only the
//! extension) are cleaned up after their contents are gone, without
//! cluttering the user-facing report.

use anyhow::Result;
use clap::Args;
use std::path::{Path, PathBuf};

use crate::commands::setup::nm_host;
use crate::output::CommandOutput;

#[derive(Args)]
pub struct UninstallArgs {
    /// Skip the confirmation prompt.
    #[arg(long, short = 'y')]
    pub yes: bool,
}

pub async fn run(args: UninstallArgs) -> Result<CommandOutput> {
    let plan = collect_plan();

    if plan.is_empty() {
        return Ok(CommandOutput::Data {
            json: serde_json::json!({ "removed": [] }),
            human: "Nothing to uninstall.".into(),
        });
    }

    eprintln!("WebPilot will remove:");
    for line in plan.report_lines() {
        eprintln!("  {line}");
    }
    eprintln!();

    if !crate::commands::setup::confirm("Proceed?", false, args.yes) {
        return Ok(CommandOutput::Data {
            json: serde_json::json!({ "removed": [], "canceled": true }),
            human: "Cancelled.".into(),
        });
    }

    execute(plan).await
}

async fn execute(plan: Plan) -> Result<CommandOutput> {
    let mut removed: Vec<&'static str> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    if plan.chrome_running {
        match crate::session::quit_session().await {
            Ok(()) => removed.push("chrome"),
            Err(e) => warnings.push(format!("stopping Chrome: {e}")),
        }
    }

    for (label, path) in [
        ("skill", plan.skill.as_deref()),
        ("extension", plan.extension.as_deref()),
        ("nm_host", plan.nm_host.as_deref()),
    ] {
        if let Some(p) = path {
            match remove_path(p) {
                Ok(()) => removed.push(label),
                Err(e) => warnings.push(format!("{label} at {} ({e})", p.display())),
            }
        }
    }

    // The cache root is wherever WEBPILOT_HOME (or the platform default)
    // points. Only the subdirectories WebPilot itself creates are deleted —
    // never a blanket recursive delete of an env-derived path, which a
    // mispointed WEBPILOT_HOME would turn into destroying a directory we do
    // not own. The root itself goes only once it is empty.
    if let Some(root) = plan.cache_root.as_deref() {
        let mut clean = true;
        for sub in ["runtime", "contexts", "artifacts", "chrome-profile"] {
            let p = root.join(sub);
            if !p.exists() {
                continue;
            }
            if let Err(e) = remove_path(&p) {
                clean = false;
                warnings.push(format!("cache {sub} at {} ({e})", p.display()));
            }
        }
        purge_if_empty(root);
        if clean {
            removed.push("cache_root");
        }
    }

    // The data root is the parent of the extension dir. Remove it only if it
    // is empty after the extension has been deleted — leaving any future
    // sibling artefacts untouched.
    purge_if_empty(&webpilot::dirs::data_root_path());

    if let Some(p) = plan.binary.as_deref() {
        match std::fs::remove_file(p) {
            Ok(()) => removed.push("binary"),
            Err(e) => warnings.push(format!("binary at {} ({e})", p.display())),
        }
    }

    let mut human = format!("✓ Uninstalled: {}", removed.join(", "));
    if !warnings.is_empty() {
        human.push_str("\n  Could not remove:");
        for w in &warnings {
            human.push_str(&format!("\n    - {w}"));
        }
    }
    Ok(CommandOutput::Data {
        json: serde_json::json!({ "removed": removed, "warnings": warnings }),
        human,
    })
}

struct Plan {
    chrome_running: bool,
    binary: Option<PathBuf>,
    skill: Option<PathBuf>,
    extension: Option<PathBuf>,
    nm_host: Option<PathBuf>,
    cache_root: Option<PathBuf>,
}

impl Plan {
    fn is_empty(&self) -> bool {
        !self.chrome_running
            && self.binary.is_none()
            && self.skill.is_none()
            && self.extension.is_none()
            && self.nm_host.is_none()
            && self.cache_root.is_none()
    }

    fn report_lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.chrome_running {
            out.push("● Headless Chrome (running)".into());
        }
        for (label, path) in [
            ("Binary", self.binary.as_deref()),
            ("Skill", self.skill.as_deref()),
            ("Extension", self.extension.as_deref()),
            ("NM host", self.nm_host.as_deref()),
            ("Cache root", self.cache_root.as_deref()),
        ] {
            if let Some(p) = path {
                out.push(format!("● {label}: {}", p.display()));
            }
        }
        out
    }
}

fn collect_plan() -> Plan {
    // `collect_plan` MUST be inspection-only: looking for the path of the
    // extension dir must not create the extension dir. We therefore use the
    // pure `_path()` accessors throughout.
    let binary = std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok())
        .filter(|p| p.exists());

    let skill = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(|h| {
            PathBuf::from(h)
                .join(".claude")
                .join("skills")
                .join("webpilot")
        })
        .filter(|p| p.is_dir());

    let extension = Some(webpilot::dirs::extension_dir_path()).filter(|p| p.is_dir());
    let nm_host = Some(nm_host::nm_dir().join("com.webpilot.host.json")).filter(|p| p.is_file());
    let cache_root = Some(webpilot::dirs::root_path()).filter(|p| p.is_dir());

    Plan {
        chrome_running: crate::session::get_existing_session().is_some(),
        binary,
        skill,
        extension,
        nm_host,
        cache_root,
    }
}

fn remove_path(p: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(p)?;
    if meta.file_type().is_dir() {
        std::fs::remove_dir_all(p)
    } else {
        std::fs::remove_file(p)
    }
}

fn purge_if_empty(p: &Path) {
    if let Ok(mut entries) = std::fs::read_dir(p)
        && entries.next().is_none()
    {
        let _ = std::fs::remove_dir(p);
    }
}
