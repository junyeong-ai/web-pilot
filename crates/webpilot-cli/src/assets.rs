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

use base64::Engine as _;
use include_dir::{Dir, include_dir};
use sha2::{Digest, Sha256};
use std::io;
use std::path::Path;
use std::sync::OnceLock;

pub static SKILL: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../../.claude/skills/webpilot");

pub static EXTENSION: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../../extension");

/// Version string of the extension baked into this binary, read from the
/// embedded `manifest.json`. The host compares it against the version the
/// installed extension reports, so a stale install is caught at connect time
/// rather than surfacing as a subtle protocol mismatch later.
pub fn expected_extension_version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(|| {
        let manifest = EXTENSION
            .get_file("manifest.json")
            .expect("embedded extension always carries manifest.json")
            .contents_utf8()
            .expect("manifest.json is valid UTF-8");
        let parsed: serde_json::Value =
            serde_json::from_str(manifest).expect("embedded manifest.json is valid JSON");
        parsed["version"]
            .as_str()
            .expect("manifest.json carries a string version")
            .to_owned()
    })
}

/// The Chrome extension id this binary's extension resolves to, derived from
/// the embedded manifest's pinned public `key`.
///
/// Because the manifest carries a fixed `key`, Chrome assigns a **stable** id
/// regardless of the unpacked-load path or machine — and that id is exactly
/// what Chrome computes: SHA-256 of the DER-decoded key, first 16 bytes, each
/// nibble mapped `0..=15` → `'a'..='p'`. Deriving it here from the same embedded
/// key means `setup nm-host` can authorise the right `chrome-extension://` origin
/// on its own, so the user never has to copy the id out of `chrome://extensions`.
pub fn expected_extension_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        let manifest = EXTENSION
            .get_file("manifest.json")
            .expect("embedded extension always carries manifest.json")
            .contents_utf8()
            .expect("manifest.json is valid UTF-8");
        let parsed: serde_json::Value =
            serde_json::from_str(manifest).expect("embedded manifest.json is valid JSON");
        let key_b64 = parsed["key"]
            .as_str()
            .expect("embedded manifest.json pins a public `key` (stable extension id)");
        let der = base64::engine::general_purpose::STANDARD
            .decode(key_b64)
            .expect("manifest `key` is valid base64 DER");
        let digest = Sha256::digest(der);
        let mut id = String::with_capacity(32);
        for byte in &digest[..16] {
            id.push((b'a' + (byte >> 4)) as char);
            id.push((b'a' + (byte & 0x0f)) as char);
        }
        id
    })
}

/// Materialise an embedded `Dir` onto disk under `dest`.
///
/// A clean replace: existing files are overwritten and anything the embedded
/// tree no longer carries is pruned, so the result is a pure function of the
/// binary version — never a union of every version ever installed. That matters
/// because `self update` re-runs `setup extension` over an existing install, so
/// a file dropped or renamed between releases must not linger in the deployed
/// extension. A `setup` command can still be invoked repeatedly to repair a
/// damaged install. Permissions are `0o755` on directories and `0o644` on files
/// (Unix); Chrome needs the extension tree to be world-readable, so `0o700`
/// would break it.
pub fn write_dir(dir: &Dir<'_>, dest: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dest)?;
    set_dir_mode(dest);

    // The immediate-child names this level should hold after the write. Every
    // other entry found on disk is a leftover from a prior version and is
    // pruned below.
    let mut expected: std::collections::HashSet<std::ffi::OsString> =
        std::collections::HashSet::new();

    for entry in dir.entries() {
        match entry {
            include_dir::DirEntry::Dir(d) => {
                let suffix = strip_root(d.path(), dir.path());
                if let Some(name) = suffix.file_name() {
                    expected.insert(name.to_owned());
                }
                write_dir(d, &dest.join(suffix))?;
            }
            include_dir::DirEntry::File(f) => {
                if is_excluded(f.path()) {
                    continue;
                }
                let suffix = strip_root(f.path(), dir.path());
                if let Some(name) = suffix.file_name() {
                    expected.insert(name.to_owned());
                }
                let target = dest.join(suffix);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                // Atomic per file: an interrupted `setup` must not leave a
                // truncated `manifest.json` / `bridge.js` that Chrome then
                // fails to load — each file flips from old to new in one rename.
                webpilot::dirs::atomic_write(&target, f.contents())?;
                set_file_mode(&target);
            }
        }
    }

    prune_unexpected(dest, &expected);
    Ok(())
}

/// Remove on-disk entries at `dest` that the embedded tree no longer carries.
///
/// Best-effort: the install's correctness comes from the files that WERE
/// written, and a stale leftover is inert (Chrome loads only what the new
/// `manifest.json` references), so a leftover that can't be removed must not
/// fail an otherwise-successful `setup` — it is untidy, not broken.
fn prune_unexpected(dest: &Path, expected: &std::collections::HashSet<std::ffi::OsString>) {
    let Ok(entries) = std::fs::read_dir(dest) else {
        return;
    };
    for entry in entries.flatten() {
        if expected.contains(&entry.file_name()) {
            continue;
        }
        let path = entry.path();
        let _ = if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
    }
}

fn strip_root<'a>(p: &'a Path, root: &Path) -> &'a Path {
    p.strip_prefix(root).unwrap_or(p)
}

fn is_excluded(p: &Path) -> bool {
    // `.DS_Store`: macOS cruft that can reappear in the source tree between
    // builds. `icon.svg`: the source the PNG icons were drawn from — the manifest
    // references only the PNGs, so the SVG has no place in the loaded extension.
    matches!(
        p.file_name().and_then(|n| n.to_str()),
        Some(".DS_Store" | "icon.svg")
    )
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
    fn extension_id_is_derived_from_the_pinned_manifest_key() {
        let id = expected_extension_id();
        // The Chrome unpacked-extension id alphabet: 32 chars in [a-p].
        assert_eq!(id.len(), 32);
        assert!(id.bytes().all(|b| (b'a'..=b'p').contains(&b)));
        // Pinned constant: the manifest `key` is fixed, so the id never moves.
        // If this assertion fails, the `key` changed — which shifts every prior
        // install's id and every NM host's allowed_origins, so it must be a
        // deliberate, coordinated change, not an accident.
        assert_eq!(id, "jfghnlpbmpkplmemfemnkfckelipodfk");
    }

    #[test]
    fn extension_contains_icons() {
        assert!(EXTENSION.get_file("icons/icon16.png").is_some());
        assert!(EXTENSION.get_file("icons/icon128.png").is_some());
    }

    #[test]
    fn write_dir_prunes_artifacts_of_a_previous_version() {
        // Materialise the real embedded extension, then plant artifacts a prior
        // version might have left: a stray top-level file and a whole stray
        // subdirectory. A second materialise must remove them while keeping the
        // genuine tree — proving the install is a clean function of the binary
        // version, not an accumulation of every version ever installed (the
        // exact path `self update` now re-runs over an existing install).
        let tmp = std::env::temp_dir().join(format!("webpilot-assets-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        write_dir(&EXTENSION, &tmp).expect("first materialise");

        let orphan_file = tmp.join("STALE_FROM_OLD_VERSION.js");
        std::fs::write(&orphan_file, b"// removed in a later release").unwrap();
        let orphan_dir = tmp.join("removed_subdir");
        std::fs::create_dir_all(&orphan_dir).unwrap();
        std::fs::write(orphan_dir.join("x.js"), b"x").unwrap();

        write_dir(&EXTENSION, &tmp).expect("second materialise");

        assert!(!orphan_file.exists(), "a top-level orphan must be pruned");
        assert!(
            !orphan_dir.exists(),
            "an orphan subdirectory must be pruned"
        );
        assert!(
            tmp.join("manifest.json").exists(),
            "the real tree must survive the prune"
        );
        assert!(
            tmp.join("content/bridge.js").exists(),
            "nested real files must survive the prune"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn embedded_extension_version_tracks_the_binary() {
        // The module contract ("matches the binary version exactly") and, more
        // importantly, the host's stale-install gate depend on the extension
        // version advancing with each release. It once froze at `1.0.0` while the
        // binary moved to `0.4.x`, so every installed extension compared equal to
        // the bundled one and `VersionMismatch` could never fire — a stale
        // extension ran silently after an upgrade. This test fails the build if
        // `extension/manifest.json` is left behind, forcing a lockstep bump.
        assert_eq!(
            expected_extension_version(),
            env!("CARGO_PKG_VERSION"),
            "extension/manifest.json version must match the workspace version — \
             bump it in lockstep with Cargo.toml"
        );
    }
}
