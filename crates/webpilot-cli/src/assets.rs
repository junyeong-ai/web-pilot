//! Compile-time embedded assets.
//!
//! The Claude Code skill and Chrome extension are baked into the binary so
//! that a single `webpilot` artefact carries everything needed to bootstrap a
//! workstation. The version of the embedded assets matches the binary
//! version exactly — there is no out-of-band fetch, no version drift.
//!
//! Layout:
//! - `SKILL` — the project's `.claude/skills/webpilot/` tree.
//! - `EXTENSION` — the Chrome extension's `extension/` tree (manifest.json,
//!   bridge.js, service worker, popup, sidepanel, icons).
//!
//! macOS `.DS_Store` cruft is skipped at materialisation time by `is_excluded`
//! (it can reappear in the source tree between builds); everything else under
//! the trees is written verbatim.

use include_dir::{Dir, include_dir};
use std::io;
use std::path::Path;

pub static SKILL: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../../.claude/skills/webpilot");

pub static EXTENSION: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../../extension");

/// Materialise an embedded `Dir` onto disk under `dest`.
///
/// Existing files at the destination are overwritten so a `setup` command can
/// be invoked repeatedly to repair a damaged install. Permissions are
/// `0o755` on directories and `0o644` on files (Unix); Chrome needs the
/// extension tree to be world-readable, so `0o700` would break it.
pub fn write_dir(dir: &Dir<'_>, dest: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dest)?;
    set_dir_mode(dest);

    for entry in dir.entries() {
        match entry {
            include_dir::DirEntry::Dir(d) => {
                let suffix = strip_root(d.path(), dir.path());
                write_dir(d, &dest.join(suffix))?;
            }
            include_dir::DirEntry::File(f) => {
                if is_excluded(f.path()) {
                    continue;
                }
                let suffix = strip_root(f.path(), dir.path());
                let target = dest.join(suffix);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&target, f.contents())?;
                set_file_mode(&target);
            }
        }
    }
    Ok(())
}

fn strip_root<'a>(p: &'a Path, root: &Path) -> &'a Path {
    p.strip_prefix(root).unwrap_or(p)
}

fn is_excluded(p: &Path) -> bool {
    p.file_name().and_then(|n| n.to_str()) == Some(".DS_Store")
}

#[cfg(unix)]
fn set_dir_mode(p: &Path) {
    set_mode(p, 0o755);
}

#[cfg(unix)]
fn set_file_mode(p: &Path) {
    set_mode(p, 0o644);
}

#[cfg(unix)]
fn set_mode(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn set_dir_mode(_: &Path) {}
#[cfg(not(unix))]
fn set_file_mode(_: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_contains_skill_md() {
        assert!(SKILL.get_file("SKILL.md").is_some());
    }

    #[test]
    fn extension_contains_manifest_and_bridge() {
        assert!(EXTENSION.get_file("manifest.json").is_some());
        assert!(EXTENSION.get_file("content/bridge.js").is_some());
        assert!(EXTENSION.get_file("background/service-worker.js").is_some());
    }

    #[test]
    fn extension_contains_icons() {
        assert!(EXTENSION.get_file("icons/icon16.png").is_some());
        assert!(EXTENSION.get_file("icons/icon128.png").is_some());
    }
}
