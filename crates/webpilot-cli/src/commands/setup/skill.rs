//! `webpilot setup skill` — materialise the embedded Claude Code skill.
//!
//! The skill ships inside the binary (see `crate::assets::SKILL`) so the
//! version on disk always matches the binary version. Re-running this
//! sub-command repairs a deleted or modified install.

use anyhow::{Context, Result};
use clap::Args;
use std::path::PathBuf;

use crate::assets;
use crate::output::CommandOutput;

use super::{StepOutcome, home_relative};

#[derive(Args)]
pub struct SkillArgs {
    /// Replace the on-disk skill even if it is unchanged.
    #[arg(long)]
    pub force: bool,
}

pub fn run(args: SkillArgs, yes: bool) -> Result<CommandOutput> {
    let outcome = install_inner(yes, args.force)?;
    Ok(CommandOutput::Data {
        json: outcome.json,
        human: outcome.human,
    })
}

/// Install or refresh the skill, prompting before overwriting unchanged
/// content unless `--yes` was passed.
pub(crate) fn install(yes: bool) -> Result<StepOutcome> {
    install_inner(yes, false)
}

fn install_inner(yes: bool, force: bool) -> Result<StepOutcome> {
    let dest = skill_dir();
    let skill_md = dest.join("SKILL.md");

    let embedded = assets::SKILL
        .get_file("SKILL.md")
        .context("embedded skill is missing SKILL.md — build artefact corrupted")?
        .contents();

    let on_disk = std::fs::read(&skill_md).ok();
    let exists = on_disk.is_some();
    let unchanged = on_disk.as_deref() == Some(embedded);

    let action = match (exists, unchanged, force) {
        (false, _, _) => Action::Installed,
        (true, true, false) => Action::Unchanged,
        (true, true, true) => Action::Reinstalled,
        (true, false, true) => Action::Updated,
        (true, false, false) => {
            if !super::confirm(
                "Skill differs from embedded version — overwrite?",
                true,
                yes,
            ) {
                return Ok(outcome("kept", &skill_md));
            }
            Action::Updated
        }
    };

    if action.writes() {
        assets::write_dir(&assets::SKILL, &dest)
            .with_context(|| format!("write skill to {}", dest.display()))?;
    }

    Ok(outcome(action.label(), &skill_md))
}

#[derive(Copy, Clone)]
enum Action {
    Installed,
    Reinstalled,
    Updated,
    Unchanged,
}

impl Action {
    fn writes(self) -> bool {
        !matches!(self, Action::Unchanged)
    }
    fn label(self) -> &'static str {
        match self {
            Action::Installed => "installed",
            Action::Reinstalled => "reinstalled",
            Action::Updated => "updated",
            Action::Unchanged => "unchanged",
        }
    }
}

fn outcome(action: &str, skill_md: &std::path::Path) -> StepOutcome {
    StepOutcome {
        human: format!("✓ Skill {action:<11} {}", home_relative(skill_md)),
        json: serde_json::json!({
            "path": skill_md.display().to_string(),
            "action": action,
        }),
    }
}

fn skill_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    home.join(".claude").join("skills").join("webpilot")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pure decision table — exercised without touching the filesystem.
    fn decide(exists: bool, unchanged: bool, force: bool) -> Option<Action> {
        match (exists, unchanged, force) {
            (false, _, _) => Some(Action::Installed),
            (true, true, false) => Some(Action::Unchanged),
            (true, true, true) => Some(Action::Reinstalled),
            (true, false, true) => Some(Action::Updated),
            (true, false, false) => None, // would require a confirm prompt
        }
    }

    #[test]
    fn fresh_install_writes() {
        let a = decide(false, false, false).unwrap();
        assert_eq!(a.label(), "installed");
        assert!(a.writes());
    }

    #[test]
    fn unchanged_without_force_skips_write() {
        let a = decide(true, true, false).unwrap();
        assert_eq!(a.label(), "unchanged");
        assert!(!a.writes());
    }

    #[test]
    fn unchanged_with_force_reinstalls() {
        let a = decide(true, true, true).unwrap();
        assert_eq!(a.label(), "reinstalled");
        assert!(a.writes());
    }

    #[test]
    fn changed_with_force_updates_silently() {
        let a = decide(true, false, true).unwrap();
        assert_eq!(a.label(), "updated");
        assert!(a.writes());
    }

    #[test]
    fn changed_without_force_requires_prompt() {
        assert!(decide(true, false, false).is_none());
    }
}
