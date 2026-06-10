//! `webpilot self update` — replace this binary with the latest release.
//!
//! Implementation rules (deliberately conservative):
//!
//! - Networking is shelled out to `curl`. Every macOS and Linux box has it,
//!   and it ships with a battle-tested TLS stack — pulling `reqwest`/`ureq`
//!   into the binary just for one update path would be net-negative.
//! - Checksums are verified with `shasum -a 256` (macOS) or `sha256sum`
//!   (Linux). If neither is present we refuse to proceed; an unverified
//!   self-update is worse than no self-update.
//! - The replace step is `rename(2)` on the same filesystem. On Unix that
//!   is atomic, and the kernel keeps the running binary's inode alive until
//!   this process exits — no need to re-exec.
//! - On macOS the freshly written binary is re-signed ad-hoc so Gatekeeper
//!   does not block subsequent invocations.

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::output::CommandOutput;

const REPO: &str = "junyeong-ai/web-pilot";

#[derive(Args)]
pub struct SelfArgs {
    #[command(subcommand)]
    pub command: SelfCommand,
}

#[derive(Subcommand)]
pub enum SelfCommand {
    /// Replace this binary with a release artefact from GitHub.
    Update(UpdateArgs),
}

#[derive(Args)]
pub struct UpdateArgs {
    /// Pin to a specific version (e.g. `0.3.1`). Defaults to latest.
    #[arg(long)]
    pub version: Option<String>,

    /// Reinstall even if the binary already reports the target version.
    #[arg(long)]
    pub force: bool,
}

pub fn run(args: SelfArgs) -> Result<CommandOutput> {
    match args.command {
        SelfCommand::Update(a) => update(a),
    }
}

fn update(args: UpdateArgs) -> Result<CommandOutput> {
    require_tools()?;
    let target = detect_target()?;
    let current = env!("CARGO_PKG_VERSION").to_owned();
    let target_version = match args.version {
        Some(v) => {
            // The version lands in a URL and in filesystem paths — confine it
            // to release-tag characters so it cannot reshape either.
            let v = normalize_version(&v);
            if v.is_empty()
                || !v
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
            {
                return Err(webpilot::WebPilotError::InvalidArgument {
                    detail: format!("invalid version '{v}' (release tags are [0-9A-Za-z._+-])"),
                }
                .into());
            }
            v
        }
        None => {
            let latest = resolve_latest()?;
            // An implicit downgrade is refused: a rolled-back or yanked
            // "latest" below the running version must be a deliberate pin,
            // never a silent replacement with older code.
            if version_components(&latest) < version_components(&current) {
                return Err(webpilot::WebPilotError::InvalidArgument {
                    detail: format!(
                        "latest release v{latest} is older than the running v{current}; \
                         pin it explicitly with --version {latest} if intended"
                    ),
                }
                .into());
            }
            latest
        }
    };

    if !args.force && current == target_version {
        let human = format!("Already on v{current} — pass --force to reinstall.");
        return Ok(CommandOutput::Data {
            json: serde_json::json!({
                "updated": false,
                "version": current,
            }),
            human,
        });
    }

    let dest = std::env::current_exe()
        .context("locating own binary path")?
        .canonicalize()
        .context("resolving own binary path")?;

    let tmp = TempDir::new()?;
    let archive = format!("webpilot-{target_version}-{target}.tar.gz");
    let url = format!("https://github.com/{REPO}/releases/download/v{target_version}/{archive}");

    // Trust model: TLS to GitHub releases plus the sha256 sidecar from the
    // same channel. That guards transport corruption and CDN tampering, not a
    // compromised release channel itself — authenticating releases against a
    // key pinned in the binary is the upgrade path if that ever changes.
    download(&url, &tmp.path().join(&archive))?;
    download(
        &format!("{url}.sha256"),
        &tmp.path().join(format!("{archive}.sha256")),
    )?;
    verify_checksum(tmp.path(), &archive)?;
    extract(&tmp.path().join(&archive), tmp.path())?;

    let extracted = tmp
        .path()
        .join(format!("webpilot-{target_version}-{target}"))
        .join("webpilot");
    if !extracted.exists() {
        bail!(
            "release archive missing expected binary at {}",
            extracted.display()
        );
    }

    // Sign the downloaded binary BEFORE swapping it in: an ad-hoc signature is
    // what lets it run under macOS Gatekeeper, so a signing failure must abort
    // the update with the old (working) binary still in place — never report a
    // successful update that installed an unrunnable binary.
    if cfg!(target_os = "macos") {
        codesign(&extracted)?;
    }
    atomic_replace(&extracted, &dest)?;

    // The on-disk unpacked extension is version-locked to the binary — browser
    // mode's host rejects any drift with `VersionMismatch`, and the extension's
    // content is purely a function of the binary version. Swapping the binary
    // without refreshing the extension would silently break that lock, so bring
    // the deployed extension to the new version too. Two constraints shape this:
    //   - Only if it was ever deployed: a headless-only install never created
    //     `extension_dir()`, and self-update must not impose browser-mode
    //     artifacts on it. Check the path WITHOUT materialising it.
    //   - Via the NEW binary, not in-process: this process still holds the OLD
    //     embedded extension, so it must shell out to the freshly-installed
    //     `dest` (which carries the new assets) to write them.
    // Best-effort — a failure leaves a working new binary in place and surfaces
    // as a manual follow-up, never as an update failure.
    let extension = if !webpilot::dirs::extension_dir_path().exists() {
        "not_installed"
    } else if Command::new(&dest)
        .args(["setup", "extension", "--yes"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        "refreshed"
    } else {
        "stale"
    };

    let extension_note = match extension {
        // The disk is now coherent; the loaded copy in a running Chrome is not,
        // and only a reload (or a Chrome restart) can pick up the new version.
        "refreshed" => "\n  Browser mode: reload the extension in chrome://extensions to finish.",
        // The refresh did not run — point at the full manual remediation.
        "stale" => {
            "\n  Browser mode: run `webpilot setup extension`, then reload it in chrome://extensions."
        }
        _ => "",
    };

    let human = format!(
        "✓ Updated v{current} → v{target_version}\n  {}{extension_note}",
        dest.display()
    );
    Ok(CommandOutput::Data {
        json: serde_json::json!({
            "updated": true,
            "from": current,
            "to": target_version,
            "path": dest.display().to_string(),
            "extension": extension,
        }),
        human,
    })
}

fn require_tools() -> Result<()> {
    for t in ["curl", "tar"] {
        if Command::new(t).arg("--version").output().is_err() {
            bail!("required tool not found in PATH: {t}");
        }
    }
    if Command::new("sha256sum").arg("--version").output().is_err()
        && Command::new("shasum").arg("--version").output().is_err()
    {
        bail!("neither `sha256sum` nor `shasum` is available — cannot verify download");
    }
    Ok(())
}

fn detect_target() -> Result<String> {
    let os = if cfg!(target_os = "macos") {
        "apple-darwin"
    } else if cfg!(target_os = "linux") {
        "unknown-linux-gnu"
    } else {
        bail!("unsupported OS for self-update");
    };
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        bail!("unsupported architecture for self-update");
    };
    Ok(format!("{arch}-{os}"))
}

/// Resolve the current `latest` tag by following the redirect on the
/// `releases/latest` URL.
///
/// GitHub redirects this URL to `…/releases/tag/vX.Y.Z` only when a Release
/// object is published. If only a git tag exists (no Release), the redirect
/// goes to the bare `…/releases` listing — we treat that as "no release"
/// rather than silently picking up the literal token "releases".
///
/// This is faster than the JSON API and not subject to the unauthenticated
/// 60/hr rate limit.
fn resolve_latest() -> Result<String> {
    let out = Command::new("curl")
        .args([
            "-fsSLI",
            "--connect-timeout",
            "10",
            "-o",
            "/dev/null",
            "-w",
            "%{url_effective}",
            &format!("https://github.com/{REPO}/releases/latest"),
        ])
        .output()
        .context("curl")?;
    if !out.status.success() {
        bail!("could not resolve latest release (HTTP error)");
    }
    let url = String::from_utf8(out.stdout).context("non-UTF8 redirect URL")?;
    parse_tag_from_redirect(&url).with_context(|| {
        format!("no published release at github.com/{REPO} — pass --version vX.Y.Z explicitly")
    })
}

/// Pull the version out of GitHub's `releases/latest` redirect target.
///
/// Returns `Some("0.2.0")` for `https://github.com/owner/repo/releases/tag/v0.2.0`,
/// `None` for `https://github.com/owner/repo/releases` (no Release published).
fn parse_tag_from_redirect(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/');
    let after = url.rsplit_once("/releases/tag/")?.1;
    let tag = after.split(['/', '?', '#']).next()?.trim();
    if tag.is_empty() {
        return None;
    }
    Some(normalize_version(tag))
}

fn normalize_version(s: &str) -> String {
    s.trim().trim_start_matches('v').to_owned()
}

/// Dotted-numeric components for ordering release versions ("0.3.10" above
/// "0.3.9", where a string compare would not).
fn version_components(v: &str) -> Vec<u64> {
    v.split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().unwrap_or(0))
        .collect()
}

fn download(url: &str, dest: &Path) -> Result<()> {
    let status = Command::new("curl")
        .args([
            "-fsSL",
            "--retry",
            "3",
            "--retry-delay",
            "1",
            "--connect-timeout",
            "10",
            "-o",
        ])
        .arg(dest)
        .arg(url)
        .status()
        .context("curl")?;
    if !status.success() {
        bail!("download failed: {url}");
    }
    Ok(())
}

fn verify_checksum(dir: &Path, archive: &str) -> Result<()> {
    let tool = if Command::new("sha256sum").arg("--version").output().is_ok() {
        "sha256sum"
    } else {
        "shasum"
    };
    let mut cmd = Command::new(tool);
    if tool == "shasum" {
        cmd.args(["-a", "256"]);
    }
    cmd.arg("-c").arg(format!("{archive}.sha256"));
    cmd.current_dir(dir);
    let status = cmd.status().with_context(|| format!("running {tool}"))?;
    if !status.success() {
        bail!("checksum verification failed");
    }
    Ok(())
}

fn extract(archive: &Path, dest: &Path) -> Result<()> {
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .status()
        .context("tar")?;
    if !status.success() {
        bail!("tar extraction failed");
    }
    Ok(())
}

/// Atomic replace.
///
/// `src` lives in a temp directory that is typically on a different
/// filesystem (e.g., macOS `/var/folders/.../T` vs. `~/.local/bin`), so a
/// direct `rename(2)` would fail with `EXDEV`. The two-step here:
/// 1. `copy` the new binary into the destination directory under a hidden
///    staging name — same filesystem as `dest`, so no cross-FS issues.
/// 2. `rename` staging → `dest`. This step *is* atomic on Unix and the
///    kernel keeps the running binary's old inode alive until exit, so a
///    self-update can replace its own executable safely.
fn atomic_replace(src: &Path, dest: &Path) -> Result<()> {
    let parent = dest
        .parent()
        .context("destination has no parent directory")?;
    let staged = parent.join(format!(".webpilot.new.{}", std::process::id()));
    // Every failure after the staged file may exist removes it — a partial
    // copy must not linger next to the real binary.
    let stage = || -> Result<()> {
        std::fs::copy(src, &staged).with_context(|| format!("copy to {}", staged.display()))?;
        set_executable(&staged)?;
        std::fs::rename(&staged, dest).with_context(|| format!("rename -> {}", dest.display()))
    };
    if let Err(e) = stage() {
        let _ = std::fs::remove_file(&staged);
        return Err(e);
    }
    Ok(())
}

#[cfg(unix)]
fn set_executable(p: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(p)?.permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(p, perm)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_: &Path) -> Result<()> {
    Ok(())
}

fn codesign(p: &Path) -> Result<()> {
    let status = Command::new("codesign")
        .args(["--force", "--sign", "-"])
        .arg(p)
        .status()?;
    if !status.success() {
        anyhow::bail!(
            "codesign failed — the updated binary would be unrunnable on macOS, so the update was aborted with the existing binary kept"
        );
    }
    Ok(())
}

/// Self-cleaning temp directory.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Result<Self> {
        let base = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = base.join(format!("webpilot-update-{pid}-{nanos}"));
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_leading_v() {
        assert_eq!(normalize_version("v0.2.0"), "0.2.0");
        assert_eq!(normalize_version("0.2.0"), "0.2.0");
        assert_eq!(normalize_version("  v1.0.0  "), "1.0.0");
    }

    #[test]
    fn detect_target_known_platform() {
        let t = detect_target().unwrap();
        assert!(t.contains("-apple-darwin") || t.contains("-unknown-linux-gnu"));
        assert!(t.starts_with("x86_64-") || t.starts_with("aarch64-"));
    }

    #[test]
    fn parse_tag_extracts_version_from_release_redirect() {
        assert_eq!(
            parse_tag_from_redirect("https://github.com/o/r/releases/tag/v0.2.0").as_deref(),
            Some("0.2.0")
        );
        assert_eq!(
            parse_tag_from_redirect("https://github.com/o/r/releases/tag/v1.2.3-rc.1").as_deref(),
            Some("1.2.3-rc.1")
        );
        assert_eq!(
            parse_tag_from_redirect("https://github.com/o/r/releases/tag/v0.2.0/").as_deref(),
            Some("0.2.0")
        );
    }

    #[test]
    fn parse_tag_returns_none_when_no_release_published() {
        assert!(parse_tag_from_redirect("https://github.com/o/r/releases").is_none());
        assert!(parse_tag_from_redirect("https://github.com/o/r").is_none());
        assert!(parse_tag_from_redirect("https://github.com/o/r/releases/tag/").is_none());
    }
}
