use clap::Parser;

use crate::commands::{self, Command as Cmd};
use crate::output::{self, OutputMode};
use crate::transport::{IpcTransport, LocalTransport, Transport};
use anyhow::Result;
use webpilot::WebPilotError;

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

pub async fn run_cli() -> Result<()> {
    let cli = Cli::parse();

    let filter = if cli.verbose { "debug" } else { "warn" };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();

    // Validate settings up front so a malformed `config.toml` fails loudly with
    // a clear message instead of being silently ignored.
    webpilot::settings::init().map_err(|detail| WebPilotError::InvalidArgument { detail })?;

    let mode = output::detect_output_mode(cli.json);

    if cli.browser && cli.context.is_some() {
        return Err(invalid_arg(
            "--context is only valid in headless mode (omit --browser)",
        ));
    }

    // Local commands resolve before any transport opens. `execution()` is the
    // single routing source, so this is the only place that decides "local".
    if matches!(cli.command.execution(), Execution::Local) {
        return run_local(cli.command, mode).await;
    }

    // The MCP server owns its own long-lived transport and stdio loop; it picks
    // the mode from the same flags the CLI does.
    if matches!(cli.command.execution(), Execution::Mcp) {
        return crate::mcp::serve(cli.browser, cli.context).await;
    }

    if cli.browser {
        run_browser_mode(cli.command, mode).await
    } else {
        run_headless_mode(cli.command, mode, cli.context).await
    }
}

/// Mode-independent commands: no transport, identical in both modes.
async fn run_local(command: commands::Command, mode: OutputMode) -> Result<()> {
    let out = match command {
        Cmd::Setup(args) => commands::setup::run(args).await?,
        Cmd::SelfCmd(args) => commands::self_cmd::run(args)?,
        Cmd::Uninstall(args) => commands::uninstall::run(args).await?,
        Cmd::Diff(args) => commands::diff::run(args).await?,
        Cmd::Policy(args) => commands::policy::run(args)?,
        _ => unreachable!("execution() routes only Execution::Local commands here"),
    };
    output::render(out, mode);
    Ok(())
}

/// How a command reaches its handler. Single source of truth for command
/// topology: every `commands::Command` variant maps to exactly one bucket, so
/// adding a command forces a routing decision here at compile time and both
/// mode entry points derive their behaviour from it.
enum Execution {
    /// Resolved in `run_cli` before any transport opens; mode-independent.
    Local,
    /// Bespoke per mode (browser hits the extension; headless short-circuits).
    Status,
    /// Tears down the headless session directly; unavailable in browser mode.
    Quit,
    /// Needs raw CDP via `LocalTransport`; unavailable in browser mode.
    HeadlessOnly,
    /// Identical in both modes through the `Transport` trait.
    TransportGeneric,
    /// Long-lived stdio MCP server; opens its own transport in `run_cli`.
    Mcp,
}

impl Cmd {
    fn execution(&self) -> Execution {
        match self {
            Cmd::Setup(_) | Cmd::SelfCmd(_) | Cmd::Uninstall(_) | Cmd::Diff(_) | Cmd::Policy(_) => {
                Execution::Local
            }
            Cmd::Mcp(_) => Execution::Mcp,
            Cmd::Status => Execution::Status,
            Cmd::Quit => Execution::Quit,
            Cmd::Device(_) | Cmd::Profile(_) | Cmd::Record(_) | Cmd::Context(_) => {
                Execution::HeadlessOnly
            }
            Cmd::Capture(_)
            | Cmd::Action(_)
            | Cmd::Eval(_)
            | Cmd::Wait(_)
            | Cmd::Tab(_)
            | Cmd::Frame(_)
            | Cmd::Dom(_)
            | Cmd::Find(_)
            | Cmd::Network(_)
            | Cmd::Console(_)
            | Cmd::Session(_)
            | Cmd::Fetch(_)
            | Cmd::Cookie(_) => Execution::TransportGeneric,
        }
    }
}

async fn run_browser_mode(command: commands::Command, mode: OutputMode) -> Result<()> {
    let result = match command.execution() {
        Execution::Status => commands::status::run().await,
        Execution::HeadlessOnly | Execution::Quit => Err(headless_only(label_of(&command))),
        Execution::TransportGeneric => {
            dispatch_via_transport(&mut IpcTransport::new(), command).await
        }
        Execution::Local => unreachable!("Local commands are resolved in run_cli"),
        Execution::Mcp => unreachable!("MCP is resolved in run_cli"),
    };
    output::render(result?, mode);
    Ok(())
}

async fn run_headless_mode(
    command: commands::Command,
    mode: OutputMode,
    context: Option<String>,
) -> Result<()> {
    match command.execution() {
        // Short-circuit before launching Chrome so status reports cleanly with
        // no session, and quit can tear an existing one down.
        Execution::Status => return run_headless_status(mode, context.as_deref()).await,
        Execution::Quit => return crate::session::quit_session().await,
        _ => {}
    }

    // `context list` only reads the context store off disk — resolve it before
    // LocalTransport::open so a pure listing never launches Chrome (nor fails when
    // Chrome is unavailable) for filesystem I/O. `context close` disposes a live
    // CDP context and falls through to the session below.
    if let Cmd::Context(args) = &command
        && matches!(args.command, commands::context::ContextCommand::List)
    {
        output::render(commands::context::list_contexts()?, mode);
        return Ok(());
    }

    // Context-management commands operate on the context *store* at the browser
    // level — they must not resolve (and so auto-create) the `--context` they
    // were invoked with, or `context list` would create the very context it is
    // meant to only report, and `context close` would be unreachable once the
    // cap is hit. They open the default connection; every other headless command
    // binds the requested context.
    let open_context = if matches!(command, Cmd::Context(_)) {
        None
    } else {
        context.as_deref()
    };
    let mut transport = LocalTransport::open(open_context).await?;

    let result = match command {
        // Headless-only commands take the LocalTransport directly for raw CDP access.
        Cmd::Profile(args) => commands::profile::run(&mut transport, args).await,
        Cmd::Record(args) => commands::record::run(&mut transport, args).await,
        Cmd::Device(args) => commands::device::run(&mut transport, args).await,
        Cmd::Context(args) => commands::context::run(&mut transport, args).await,
        cmd => dispatch_via_transport(&mut transport, cmd).await,
    };
    output::render(result?, mode);
    Ok(())
}

/// Dispatch any command that is generic over `Transport` (i.e., works
/// identically in browser and headless modes). Mode-specific variants must
/// be intercepted by the caller before reaching here.
async fn dispatch_via_transport<T: Transport>(
    transport: &mut T,
    command: commands::Command,
) -> Result<output::CommandOutput> {
    match command {
        Cmd::Capture(args) => commands::capture::run(transport, args).await,
        Cmd::Action(args) => commands::action::run(transport, args).await,
        Cmd::Eval(args) => commands::eval::run(transport, args).await,
        Cmd::Wait(args) => commands::wait::run(transport, args).await,
        Cmd::Tab(args) => commands::tab::run(transport, args).await,
        Cmd::Frame(args) => commands::frame::run(transport, args).await,
        Cmd::Dom(args) => commands::dom::run(transport, args).await,
        Cmd::Find(args) => commands::find::run(transport, args).await,
        Cmd::Network(args) => commands::network::run(transport, args).await,
        Cmd::Console(args) => commands::console::run(transport, args).await,
        Cmd::Session(args) => commands::session::run(transport, args).await,
        Cmd::Fetch(args) => commands::fetch::run(transport, args).await,
        Cmd::Cookie(args) => commands::cookie::run(transport, args).await,
        Cmd::Status
        | Cmd::Quit
        | Cmd::Device(_)
        | Cmd::Profile(_)
        | Cmd::Record(_)
        | Cmd::Context(_)
        | Cmd::Diff(_)
        | Cmd::Policy(_)
        | Cmd::Setup(_)
        | Cmd::SelfCmd(_)
        | Cmd::Mcp(_)
        | Cmd::Uninstall(_) => unreachable!("non-transport command reached generic dispatch"),
    }
}

async fn run_headless_status(mode: OutputMode, context: Option<&str>) -> Result<()> {
    use webpilot::protocol::{Command, ResponseData, RunMode};

    if crate::session::get_existing_session().is_none() {
        let out =
            commands::status::render(false, RunMode::Headless, None, None, None, None, context);
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

fn label_of(cmd: &commands::Command) -> &'static str {
    match cmd {
        Cmd::Device(_) => "device emulation",
        Cmd::Profile(_) => "CPU profiling",
        Cmd::Record(_) => "frame recording",
        Cmd::Context(_) => "context management",
        Cmd::Quit => "session lifecycle (quit)",
        _ => "this command",
    }
}

fn invalid_arg(detail: &str) -> anyhow::Error {
    WebPilotError::InvalidArgument {
        detail: detail.into(),
    }
    .into()
}

fn headless_only(feature: &str) -> anyhow::Error {
    invalid_arg(&format!(
        "{feature} is only supported in headless mode (omit --browser)"
    ))
}
