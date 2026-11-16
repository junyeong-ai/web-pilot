//! `webpilot setup skill` — materialise the embedded agent skill in every
//! skills directory an agent host reads ([`skill_dirs`]).
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
    /// Rewrite the skill even when it is current, and overwrite local edits without asking.
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
    let embedded = assets::SKILL
        .get_file("SKILL.md")
        .context("embedded skill is missing SKILL.md — build artefact corrupted")?
        .contents();
    // Read once, before any write: every directory is judged against what
    // WebPilot wrote LAST time, not against a write this run just made.
    let record = read_record();

    let mut installs = Vec::new();
    for dest in skill_dirs()? {
        let skill_md = dest.join("SKILL.md");
        let on_disk = std::fs::read(&skill_md).ok();
        let unchanged = on_disk.as_deref() == Some(embedded);
        let action = match decide(
            provenance(on_disk.as_deref(), embedded, record.as_deref()),
            unchanged,
            force,
        ) {
            Some(action) => action,
            // Fail-closed default `false`: overwriting a skill WebPilot did not
            // write is destructive, so a non-interactive run (piped/CI, no
            // `--yes`) keeps the local copy instead of silently clobbering it.
            // Only an explicit `y` at the prompt, or `--yes`, replaces it.
            None if !super::confirm(
                &format!("{} has local edits — overwrite?", home_relative(&skill_md)),
                false,
                yes,
            ) =>
            {
                Action::Kept
            }
            None => Action::Updated,
        };
        if action.writes() {
            assets::write_dir(&assets::SKILL, &dest, assets::Leftovers::Keep)
                .with_context(|| format!("write skill to {}", dest.display()))?;
        }
        installs.push((action, skill_md));
    }
    if installs.iter().any(|(action, _)| *action != Action::Kept) {
        write_record(&digest(embedded));
    }
    Ok(outcome(&installs))
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
    let Some(embedded) = assets::SKILL.get_file("SKILL.md").map(|f| f.contents()) else {
        return;
    };
    let ours = skill_dirs()
        .unwrap_or_default()
        .iter()
        .any(|dest| std::fs::read(dest.join("SKILL.md")).is_ok_and(|disk| disk == embedded));
    if ours {
        write_record(&digest(embedded));
    }
}

/// Whether the skill is deployed in any skills directory.
pub(crate) fn installed() -> bool {
    skill_dirs()
        .unwrap_or_default()
        .iter()
        .any(|dest| dest.join("SKILL.md").exists())
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
    let _ = webpilot::dirs::atomic_write(&webpilot::dirs::skill_record_file(), digest.as_bytes());
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Action {
    Installed,
    Reinstalled,
    Updated,
    Unchanged,
    /// Local edits the user declined to overwrite.
    Kept,
}

impl Action {
    fn writes(self) -> bool {
        !matches!(self, Action::Unchanged | Action::Kept)
    }
    fn label(self) -> &'static str {
        match self {
            Action::Installed => "installed",
            Action::Reinstalled => "reinstalled",
            Action::Updated => "updated",
            Action::Unchanged => "unchanged",
            Action::Kept => "kept",
        }
    }
}

/// `action` sums the directories up for a caller that reads one word — `self
/// update` reads it from the next release's binary — and `installs` itemises
/// them: local edits kept anywhere win, then a shared action, else `updated`.
fn outcome(installs: &[(Action, PathBuf)]) -> StepOutcome {
    StepOutcome {
        human: installs
            .iter()
            .map(|(action, skill_md)| {
                format!("✓ Skill {:<11} {}", action.label(), home_relative(skill_md))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        json: serde_json::json!({
            "action": summary(installs).label(),
            "installs": installs
                .iter()
                .map(|(action, skill_md)| serde_json::json!({
                    "path": skill_md.display().to_string(),
                    "action": action.label(),
                }))
                .collect::<Vec<_>>(),
        }),
    }
}

fn summary(installs: &[(Action, PathBuf)]) -> Action {
    if installs.iter().any(|(action, _)| *action == Action::Kept) {
        return Action::Kept;
    }
    match installs {
        [(first, _), rest @ ..] if rest.iter().all(|(action, _)| action == first) => *first,
        _ => Action::Updated,
    }
}

/// Every directory the skill is deployed to: `~/.claude/skills` for Claude Code,
/// and `~/.agents/skills` for Codex and the other hosts of the shared Agent
/// Skills layout. Claude Code does not read the second, so no host lists the
/// skill twice. Derived from `$HOME`: an unset home is an error, never a `/tmp`
/// guess — installing to a path no agent reads would report a broken install
/// as success.
pub(crate) fn skill_dirs() -> Result<Vec<PathBuf>> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|h| !h.as_os_str().is_empty())
        .context("HOME is not set — cannot locate the skills directories")?;
    Ok([".claude", ".agents"]
        .into_iter()
        .map(|host| home.join(host).join("skills").join("webpilot"))
        .collect())
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
    fn the_summary_names_kept_edits_first_then_a_shared_action() {
        let at = |action| (action, PathBuf::from("x"));
        assert_eq!(
            summary(&[at(Action::Updated), at(Action::Kept)]),
            Action::Kept
        );
        assert_eq!(
            summary(&[at(Action::Unchanged), at(Action::Unchanged)]),
            Action::Unchanged
        );
        assert_eq!(
            summary(&[at(Action::Unchanged), at(Action::Installed)]),
            Action::Updated
        );
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
