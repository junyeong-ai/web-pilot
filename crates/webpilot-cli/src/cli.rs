use clap::Parser;

use crate::commands;
use crate::output::{self, OutputMode};
use crate::transport::{IpcTransport, LocalTransport};

#[derive(Parser)]
#[command(
    name = "webpilot",
    version,
    about = "Browser control tool for AI agents"
)]
struct Cli {
    #[command(subcommand)]
    command: commands::Command,

    /// Force JSON output (auto-detected when stdout is piped).
    #[arg(long, global = true)]
    json: bool,

    /// Connect to user's authenticated Chrome via Native Messaging instead of headless.
    #[arg(long, global = true)]
    browser: bool,

    /// Verbose logging to stderr.
    #[arg(long, short, global = true)]
    verbose: bool,

    /// Isolated browser context for multi-agent use (headless only).
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

    let mode = output::detect_output_mode(cli.json);

    // Mode-independent commands handled before any transport is opened.
    if let commands::Command::Install(args) = cli.command {
        let result = commands::install::run(args).await?;
        output::render(result, mode);
        return Ok(());
    }
    if let commands::Command::Diff(args) = cli.command {
        let result = commands::diff::run(args).await?;
        output::render(result, mode);
        return Ok(());
    }

    if cli.browser {
        run_browser_mode(cli.command, mode).await
    } else {
        run_headless_mode(cli.command, mode, cli.context).await
    }
}

async fn run_browser_mode(command: commands::Command, mode: OutputMode) -> anyhow::Result<()> {
    let mut transport = IpcTransport::new();

    let result = match command {
        commands::Command::Capture(args) => commands::capture::run(&mut transport, args).await,
        commands::Command::Action(args) => commands::action::run(&mut transport, args).await,
        commands::Command::Eval(args) => commands::eval::run(&mut transport, args).await,
        commands::Command::Wait(args) => commands::wait::run(&mut transport, args).await,
        commands::Command::Tab(args) => commands::tab::run(&mut transport, args).await,
        commands::Command::Frame(args) => commands::frame::run(&mut transport, args).await,
        commands::Command::Dom(args) => commands::dom::run(&mut transport, args).await,
        commands::Command::Find(args) => commands::find::run(&mut transport, args).await,
        commands::Command::Network(args) => commands::network::run(&mut transport, args).await,
        commands::Command::Console(args) => commands::console::run(&mut transport, args).await,
        commands::Command::Session(args) => commands::session::run(&mut transport, args).await,
        commands::Command::Policy(args) => commands::policy::run(&mut transport, args).await,
        commands::Command::Fetch(args) => commands::fetch::run(&mut transport, args).await,
        commands::Command::Cookie(args) => commands::cookie::run(&mut transport, args).await,
        commands::Command::Status => commands::status::run().await,
        commands::Command::Device(_) => Err(headless_only("device emulation")),
        commands::Command::Profile(_) => Err(headless_only("CPU profiling")),
        commands::Command::Record(_) => Err(headless_only("frame recording")),
        commands::Command::Context(_) => Err(headless_only("context management")),
        commands::Command::Diff(_) | commands::Command::Install(_) => unreachable!(),
        commands::Command::Quit => {
            crate::session::quit_session().await?;
            return Ok(());
        }
    };

    let cmd_output = result?;
    output::render(cmd_output, mode);
    Ok(())
}

async fn run_headless_mode(
    command: commands::Command,
    mode: OutputMode,
    context: Option<String>,
) -> anyhow::Result<()> {
    // Status without a live session: short-circuit before launching Chrome.
    if matches!(command, commands::Command::Status) {
        return run_headless_status(mode, context.as_deref()).await;
    }

    if matches!(command, commands::Command::Quit) {
        if let Some(ref name) = context {
            crate::transport::local::quit_named_context(name).await?;
        } else {
            crate::session::quit_session().await?;
        }
        return Ok(());
    }

    let mut transport = LocalTransport::open(context.as_deref()).await?;

    let result = match command {
        commands::Command::Capture(args) => commands::capture::run(&mut transport, args).await,
        commands::Command::Action(args) => commands::action::run(&mut transport, args).await,
        commands::Command::Eval(args) => commands::eval::run(&mut transport, args).await,
        commands::Command::Wait(args) => commands::wait::run(&mut transport, args).await,
        commands::Command::Tab(args) => commands::tab::run(&mut transport, args).await,
        commands::Command::Frame(args) => commands::frame::run(&mut transport, args).await,
        commands::Command::Dom(args) => commands::dom::run(&mut transport, args).await,
        commands::Command::Find(args) => commands::find::run(&mut transport, args).await,
        commands::Command::Network(args) => commands::network::run(&mut transport, args).await,
        commands::Command::Console(args) => commands::console::run(&mut transport, args).await,
        commands::Command::Session(args) => commands::session::run(&mut transport, args).await,
        commands::Command::Policy(args) => commands::policy::run(&mut transport, args).await,
        commands::Command::Fetch(args) => commands::fetch::run(&mut transport, args).await,
        commands::Command::Cookie(args) => commands::cookie::run(&mut transport, args).await,

        // Headless-only commands: take the full LocalTransport for direct CDP access.
        commands::Command::Profile(args) => commands::profile::run(&mut transport, args).await,
        commands::Command::Record(args) => commands::record::run(&mut transport, args).await,
        commands::Command::Device(args) => commands::device::run(&mut transport, args).await,
        commands::Command::Context(args) => commands::context::run(&mut transport, args).await,

        // Status / Quit / Diff / Install: handled before this match.
        commands::Command::Status
        | commands::Command::Quit
        | commands::Command::Diff(_)
        | commands::Command::Install(_) => unreachable!(),
    };

    let cmd_output = result?;
    output::render(cmd_output, mode);
    Ok(())
}

async fn run_headless_status(mode: OutputMode, context: Option<&str>) -> anyhow::Result<()> {
    use crate::transport::Transport;
    use webpilot::protocol::{Command, ResponseData, RunMode};

    if crate::session::get_existing_session().is_none() {
        let out = commands::status::render(
            false,
            RunMode::Headless,
            None,
            None,
            None,
            None,
            context,
        );
        output::render(out, mode);
        return Ok(());
    }

    let mut transport = LocalTransport::open(context).await?;
    let result = transport.send(Command::Status).await?;

    let out = match result {
        ResponseData::Status {
            connected,
            mode: run_mode,
            tab_url,
            tab_title,
            chrome_version,
            extension_version,
        } => commands::status::render(
            connected,
            run_mode,
            tab_url,
            tab_title,
            chrome_version,
            extension_version,
            context,
        ),
        ResponseData::Error { error } => return Err(error.into()),
        _ => anyhow::bail!("Unexpected response shape"),
    };
    output::render(out, mode);
    Ok(())
}

fn headless_only(feature: &str) -> anyhow::Error {
    webpilot::WebPilotError::InvalidArgument {
        detail: format!("{feature} is only supported in headless mode (omit --browser)"),
    }
    .into()
}
