use clap::Parser;

use crate::commands;
use crate::output;

#[derive(Parser)]
#[command(
    name = "webpilot",
    version,
    about = "Browser control tool for AI agents"
)]
struct Cli {
    #[command(subcommand)]
    command: commands::Command,

    /// Force JSON output (auto-detected when stdout is piped)
    #[arg(long, global = true)]
    json: bool,

    /// Connect to user's Chrome browser instead of headless (for SSO)
    #[arg(long, global = true)]
    browser: bool,

    /// Enable verbose logging to stderr
    #[arg(long, short, global = true)]
    verbose: bool,

    /// Isolated browser context for multi-agent use
    #[arg(long, global = true)]
    context: Option<String>,
}

pub async fn run_cli() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let filter = if cli.verbose { "debug" } else { "warn" };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();

    let output_mode = output::detect_output_mode(cli.json);

    // Headless mode (default): use CDP directly, no Extension needed
    if !cli.browser {
        return crate::headless::run(cli.command, output_mode, cli.context).await;
    }

    // Browser mode (--browser): use Extension + NM Host (for SSO)
    match cli.command {
        commands::Command::Capture(args) => commands::capture::run(args, output_mode).await?,
        commands::Command::Action(args) => commands::action::run(args, output_mode).await?,
        commands::Command::Eval(args) => commands::eval::run(args, output_mode).await?,
        commands::Command::Wait(args) => commands::wait::run(args, output_mode).await?,
        commands::Command::Tabs(args) => commands::tabs::run(args, output_mode).await?,
        commands::Command::Frames(args) => commands::frames::run(args, output_mode).await?,
        commands::Command::Dom(args) => commands::dom::run(args, output_mode).await?,
        commands::Command::Diff(args) => commands::diff::run(args, output_mode).await?,
        commands::Command::Find(args) => commands::find::run(args, output_mode).await?,
        commands::Command::Network(args) => commands::network::run(args, output_mode).await?,
        commands::Command::Console(args) => commands::console::run(args, output_mode).await?,
        commands::Command::Session(args) => commands::session::run(args, output_mode).await?,
        commands::Command::Policy(args) => commands::policy::run(args, output_mode).await?,
        commands::Command::Fetch(args) => commands::fetch::run(args, output_mode).await?,
        commands::Command::Cookies(args) => commands::cookies::run(args, output_mode).await?,
        commands::Command::Status => commands::status::run(output_mode).await?,
        commands::Command::Device(_) => {
            anyhow::bail!(
                "Device emulation is only supported in headless mode (without --browser)"
            );
        }
        commands::Command::Profile(_) => {
            anyhow::bail!("Profiling is only supported in headless mode (without --browser)");
        }
        commands::Command::Record(_) => {
            anyhow::bail!("Recording is only supported in headless mode (without --browser)");
        }
        commands::Command::Context(_) => {
            anyhow::bail!("Context management is only supported in headless mode");
        }
        commands::Command::Install(args) => commands::install::run(args, output_mode).await?,
        commands::Command::Quit => {
            crate::session::quit_session()?;
        }
    }

    Ok(())
}
