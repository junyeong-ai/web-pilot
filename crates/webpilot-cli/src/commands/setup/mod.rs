//! `webpilot setup` — post-install workstation configuration.
//!
//! The binary owns the entire post-install flow: skill installation,
//! Chrome extension extraction, Chrome integration walkthrough, and Native
//! Messaging host registration. The shell installer's only job is to land
//! the binary on PATH; everything beyond that is invoked through this
//! subcommand.
//!
//! Sub-commands are independent and idempotent. Running `webpilot setup`
//! with no sub-command performs the orchestrated walkthrough.

pub mod extension;
pub mod nm_host;
pub mod skill;

use anyhow::Result;
use clap::{Args, Subcommand};

use crate::output::CommandOutput;

#[derive(Args)]
pub struct SetupArgs {
    #[command(subcommand)]
    pub command: Option<SetupCommand>,

    /// Skip prompts and take CI-safe defaults (does NOT launch Chrome).
    #[arg(long, short = 'y', global = true)]
    pub yes: bool,

    /// After extracting the extension, launch chrome://extensions.
    /// Honoured by `setup` and `setup extension`.
    #[arg(long, global = true)]
    pub open: bool,
}

#[derive(Subcommand)]
pub enum SetupCommand {
    /// Install or refresh the Claude Code skill at ~/.claude/skills/webpilot/.
    Skill(skill::SkillArgs),
    /// Extract the Chrome extension and print integration instructions.
    Extension(extension::ExtensionArgs),
    /// Register the Native Messaging host manifest (enables --browser mode).
    #[command(name = "nm-host")]
    NmHost(nm_host::NmHostArgs),
}

pub async fn run(args: SetupArgs) -> Result<CommandOutput> {
    let SetupArgs { command, yes, open } = args;
    match command {
        Some(SetupCommand::Skill(a)) => skill::run(a, yes),
        Some(SetupCommand::Extension(a)) => extension::run(a, yes, open),
        Some(SetupCommand::NmHost(a)) => nm_host::run(a),
        None => orchestrate(yes, open),
    }
}

/// Orchestrated walkthrough: skill + extension + extension-loading guidance.
///
/// `nm-host` registration is *not* invoked here because it requires the
/// 32-character extension ID, which the user can only obtain after loading
/// the unpacked extension into Chrome. The walkthrough ends by printing the
/// exact command the user should run once they have the ID.
///
/// `open=true` (or interactive `y` to the prompt) launches `chrome://extensions`
/// after extraction. `--yes` alone takes the CI-safe default of *not* opening
/// Chrome.
fn orchestrate(yes: bool, open: bool) -> Result<CommandOutput> {
    let skill = skill::install(yes)?;
    let extension = extension::install(ChromeOpen::from_flags(yes, open))?;

    // The extension step already prints the full chrome-loading guide,
    // including the `nm-host` command line. The orchestrator's job is just
    // to chain the two steps — duplicating the next-step instruction here
    // would put the same text on screen twice.
    let human = format!("WebPilot setup\n\n{skill}\n\n{extension}");

    Ok(CommandOutput::Data {
        json: serde_json::json!({
            "skill": skill.json,
            "extension": extension.json,
        }),
        human,
    })
}

/// Internal record returned by sub-step helpers — collected by the
/// orchestrator into a single human/JSON output.
pub(crate) struct StepOutcome {
    pub human: String,
    pub json: serde_json::Value,
}

/// Whether to launch `chrome://extensions` after the extension is extracted.
#[derive(Copy, Clone, Debug)]
pub(crate) enum ChromeOpen {
    /// Always open (user passed `--open`).
    Always,
    /// Don't prompt and don't open (CI: user passed `--yes` without `--open`).
    Never,
    /// Prompt the user; default to opening.
    Ask,
}

impl ChromeOpen {
    pub(crate) fn from_flags(yes: bool, open: bool) -> Self {
        match (yes, open) {
            (_, true) => ChromeOpen::Always,
            (true, false) => ChromeOpen::Never,
            (false, false) => ChromeOpen::Ask,
        }
    }

    /// Whether to launch Chrome.
    ///
    /// `Ask` only prompts when stdin is a real terminal — automation contexts
    /// (`echo y | webpilot setup`, hooks, scripts) take the same path as
    /// `Never`, because silently popping a GUI from a pipe is a surprise.
    pub(crate) fn should_open(self) -> bool {
        use std::io::IsTerminal;
        match self {
            ChromeOpen::Always => true,
            ChromeOpen::Never => false,
            ChromeOpen::Ask if !std::io::stdin().is_terminal() => false,
            ChromeOpen::Ask => confirm("Open chrome://extensions?", true, false),
        }
    }
}

impl std::fmt::Display for StepOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.human)
    }
}

/// Render a path with `$HOME` collapsed to `~`. Used for human output
/// throughout the setup flow so paths fit in one line.
///
/// An empty `HOME` is treated as unset so we never produce `~/<full path>`.
pub(crate) fn home_relative(p: &std::path::Path) -> String {
    home_relative_with(p, std::env::var_os("HOME").as_deref())
}

fn home_relative_with(p: &std::path::Path, home: Option<&std::ffi::OsStr>) -> String {
    if let Some(h) = home.filter(|h| !h.is_empty())
        && let Ok(rest) = p.strip_prefix(h)
    {
        return format!("~/{}", rest.display());
    }
    p.display().to_string()
}

/// TTY-aware yes/no confirmation.
///
/// - `yes = true` (the user passed `--yes`/`-y`): returns `true`
///   unconditionally, mirroring `apt -y` / `dnf -y`. Side-effecting GUI
///   actions like launching Chrome do *not* use this helper — they go
///   through the explicit [`ChromeOpen`] policy instead, so `--yes` cannot
///   accidentally trigger one.
/// - `yes = false` and stdin is not a terminal: returns `default` (we
///   cannot prompt, so we apply the documented default).
/// - Otherwise: prompts the user.
pub(crate) fn confirm(prompt: &str, default: bool, yes: bool) -> bool {
    use std::io::{BufRead, IsTerminal, Write, stderr, stdin};

    if yes {
        return true;
    }
    if !stdin().is_terminal() {
        return default;
    }

    let suffix = if default { "[Y/n]" } else { "[y/N]" };
    let _ = write!(stderr(), "  {prompt} {suffix} ");
    let _ = stderr().flush();

    let mut buf = String::new();
    if stdin().lock().read_line(&mut buf).is_err() {
        return default;
    }
    match buf.trim() {
        "" => default,
        s => matches!(s, "y" | "Y" | "yes" | "YES" | "Yes"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::ffi::OsStr;
    use std::path::Path;

    #[test]
    fn home_relative_substitutes_home() {
        let got = home_relative_with(Path::new("/me/foo/bar"), Some(OsStr::new("/me")));
        assert_eq!(got, "~/foo/bar");
    }

    #[test]
    fn home_relative_keeps_outside_home() {
        let got = home_relative_with(Path::new("/etc/foo"), Some(OsStr::new("/me")));
        assert_eq!(got, "/etc/foo");
    }

    #[test]
    fn home_relative_treats_empty_home_as_unset() {
        let got = home_relative_with(Path::new("/users/me/foo"), Some(OsStr::new("")));
        assert_eq!(got, "/users/me/foo");
    }

    #[test]
    fn home_relative_treats_missing_home_as_unset() {
        let got = home_relative_with(Path::new("/users/me/foo"), None);
        assert_eq!(got, "/users/me/foo");
    }

    #[test]
    fn chrome_open_explicit_open_wins_over_yes() {
        assert!(matches!(
            ChromeOpen::from_flags(true, true),
            ChromeOpen::Always
        ));
    }

    #[test]
    fn chrome_open_yes_alone_is_never() {
        assert!(matches!(
            ChromeOpen::from_flags(true, false),
            ChromeOpen::Never
        ));
    }

    #[test]
    fn chrome_open_no_flags_asks() {
        assert!(matches!(
            ChromeOpen::from_flags(false, false),
            ChromeOpen::Ask
        ));
    }

    #[test]
    fn confirm_yes_proceeds_regardless_of_default() {
        assert!(
            confirm("?", true, true),
            "yes + default-true should be true"
        );
        assert!(
            confirm("?", false, true),
            "yes + default-false MUST be true (apt -y semantic)"
        );
    }
}
