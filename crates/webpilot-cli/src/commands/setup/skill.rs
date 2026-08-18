//! `webpilot setup skill` — materialise the embedded Claude Code skill.
//!
//! The skill ships inside the binary (see `crate::assets::SKILL`) so the
//! version on disk always matches the binary version. Re-running this
//! sub-command repairs a deleted or modified install.
//!
//! The skill is the one deployed artefact a user may legitimately edit, so an
//! install has to know whether the copy on disk is WebPilot's own or theirs —
//! a distinction content alone cannot draw, since a stale copy and an edited
//! one both simply differ from what is embedded. Every write records the digest
//! it left behind; a later install compares against that record and refreshes
//! its own copy silently while leaving an edited one alone.

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
    let dest = skill_dir()?;
    let skill_md = dest.join("SKILL.md");

    let embedded = assets::SKILL
        .get_file("SKILL.md")
        .context("embedded skill is missing SKILL.md — build artefact corrupted")?
        .contents();

    let on_disk = std::fs::read(&skill_md).ok();
    let unchanged = on_disk.as_deref() == Some(embedded);

    let action = match decide(
        provenance(on_disk.as_deref(), embedded, read_record().as_deref()),
        unchanged,
        force,
    ) {
        Some(action) => action,
        // Fail-closed default `false`: overwriting a skill WebPilot did not
        // write is destructive, so a non-interactive run (piped/CI, no `--yes`)
        // keeps the local copy instead of silently clobbering it. Only an
        // explicit `y` at the prompt, or `--yes`, replaces it.
        None => {
            if !super::confirm("Skill has local edits — overwrite?", false, yes) {
                return Ok(outcome("kept", &skill_md));
            }
            Action::Updated
        }
    };

    if action.writes() {
        assets::write_dir(&assets::SKILL, &dest)
            .with_context(|| format!("write skill to {}", dest.display()))?;
    }
    write_record(&digest(embedded));

    Ok(outcome(action.label(), &skill_md))
}

/// Who the copy on disk belongs to.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Provenance {
    /// Nothing is deployed.
    Absent,
    /// WebPilot wrote it — either it still matches this binary's copy, or it
    /// matches the digest a previous write recorded.
    Ours,
    /// Content WebPilot cannot account for: hand-edited, or deployed by some
    /// path that left no record. Either way it is the user's to keep.
    Local,
}

fn provenance(on_disk: Option<&[u8]>, embedded: &[u8], record: Option<&str>) -> Provenance {
    let Some(disk) = on_disk else {
        return Provenance::Absent;
    };
    if disk == embedded || record == Some(digest(disk).as_str()) {
        return Provenance::Ours;
    }
    Provenance::Local
}

fn decide(provenance: Provenance, unchanged: bool, force: bool) -> Option<Action> {
    match (provenance, unchanged, force) {
        (Provenance::Absent, _, _) => Some(Action::Installed),
        (Provenance::Ours, true, false) => Some(Action::Unchanged),
        (Provenance::Ours, true, true) => Some(Action::Reinstalled),
        (Provenance::Ours, false, _) => Some(Action::Updated),
        (Provenance::Local, _, true) => Some(Action::Updated),
        (Provenance::Local, _, false) => None,
    }
}

/// Claim the deployed skill as WebPilot's own when this binary is what wrote it.
/// `self update` calls it before replacing the binary: the running build is the
/// last one that can still recognise its own output, and the record it leaves is
/// what lets the incoming build refresh a copy the user never touched.
pub(crate) fn record_if_ours() {
    let Ok(dest) = skill_dir() else { return };
    let Some(embedded) = assets::SKILL.get_file("SKILL.md").map(|f| f.contents()) else {
        return;
    };
    let Ok(disk) = std::fs::read(dest.join("SKILL.md")) else {
        return;
    };
    if disk == embedded {
        write_record(&digest(&disk));
    }
}

/// The deployed `SKILL.md`, if the skill is installed at all.
pub(crate) fn installed_path() -> Option<PathBuf> {
    let skill_md = skill_dir().ok()?.join("SKILL.md");
    skill_md.exists().then_some(skill_md)
}

fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn read_record() -> Option<String> {
    let raw = std::fs::read_to_string(webpilot::dirs::skill_record_path()).ok()?;
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// Best-effort: the install's correctness comes from the file that was written,
/// and a missing record only costs a later refresh its precision — never the
/// deployment itself.
fn write_record(digest: &str) {
    let path = webpilot::dirs::skill_record_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = webpilot::dirs::atomic_write(&path, digest.as_bytes());
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

/// Where Claude Code reads the skill from. Derived from `$HOME`: an unset
/// home is an error, never a `/tmp` guess — installing to a path Claude will
/// never read would report a broken install as success.
fn skill_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|h| !h.as_os_str().is_empty())
        .context("HOME is not set — cannot locate ~/.claude/skills")?;
    Ok(home.join(".claude").join("skills").join("webpilot"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMBEDDED: &[u8] = b"# skill v2";
    const PREVIOUS: &[u8] = b"# skill v1";
    const EDITED: &[u8] = b"# skill v1, with my notes";

    #[test]
    fn a_missing_skill_is_installed() {
        let p = provenance(None, EMBEDDED, None);
        assert_eq!(p, Provenance::Absent);
        assert_eq!(decide(p, false, false).unwrap().label(), "installed");
    }

    #[test]
    fn content_matching_the_binary_is_ours_without_any_record() {
        assert_eq!(provenance(Some(EMBEDDED), EMBEDDED, None), Provenance::Ours);
    }

    #[test]
    fn an_unchanged_skill_is_left_alone_unless_forced() {
        let p = provenance(Some(EMBEDDED), EMBEDDED, None);
        assert_eq!(decide(p, true, false).unwrap().label(), "unchanged");
        assert!(!decide(p, true, false).unwrap().writes());
        assert_eq!(decide(p, true, true).unwrap().label(), "reinstalled");
    }

    /// The refresh this whole record exists for: an older WebPilot wrote the
    /// deployed copy, so a newer one replaces it without asking.
    #[test]
    fn our_own_stale_copy_updates_without_a_prompt() {
        let record = digest(PREVIOUS);
        let p = provenance(Some(PREVIOUS), EMBEDDED, Some(&record));
        assert_eq!(p, Provenance::Ours);
        assert_eq!(decide(p, false, false).unwrap().label(), "updated");
    }

    /// The other half: same staleness, but the bytes are not what WebPilot
    /// recorded — the user edited them, so the decision goes to the prompt.
    #[test]
    fn a_locally_edited_skill_is_never_overwritten_silently() {
        let record = digest(PREVIOUS);
        let p = provenance(Some(EDITED), EMBEDDED, Some(&record));
        assert_eq!(p, Provenance::Local);
        assert!(decide(p, false, false).is_none());
        assert_eq!(decide(p, false, true).unwrap().label(), "updated");
    }

    /// No record at all — a deployment WebPilot cannot account for. Fail closed
    /// and treat it as the user's, rather than assume it is a stale own copy.
    #[test]
    fn an_unaccounted_copy_is_treated_as_the_users() {
        let p = provenance(Some(PREVIOUS), EMBEDDED, None);
        assert_eq!(p, Provenance::Local);
        assert!(decide(p, false, false).is_none());
    }

    #[test]
    fn a_stale_record_does_not_claim_the_users_edits() {
        let stale = digest(b"something else entirely");
        assert_eq!(
            provenance(Some(EDITED), EMBEDDED, Some(&stale)),
            Provenance::Local
        );
    }
}
