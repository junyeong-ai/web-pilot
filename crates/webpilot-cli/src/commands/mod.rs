pub mod action;
pub mod capture;
pub mod console;
pub mod context;
pub mod cookie;
pub mod device;
pub mod diff;
pub mod dom;
pub mod eval;
pub mod fetch;
pub mod find;
pub mod frame;
pub mod network;
pub mod policy;
pub mod profile;
pub mod record;
pub mod self_cmd;
pub mod session;
pub mod setup;
pub mod status;
pub mod tab;
pub mod uninstall;
pub mod wait;

use clap::Subcommand;

#[derive(Subcommand)]
pub enum Command {
    /// Capture page state (DOM, screenshot, text)
    Capture(capture::CaptureArgs),
    /// Execute a browser action
    Action(action::ActionArgs),
    /// Evaluate JavaScript in the page context
    Eval(eval::EvalArgs),
    /// Wait for a condition (element, text, navigation)
    Wait(wait::WaitArgs),
    /// Manage browser tabs
    Tab(tab::TabArgs),
    /// Navigate and manage iframes
    Frame(frame::FrameArgs),
    /// Read/write DOM elements (innerHTML, textContent, attributes)
    Dom(dom::DomArgs),
    /// Fetch URL using browser session cookies
    Fetch(fetch::FetchArgs),
    /// Compare DOM snapshots or screenshots
    Diff(diff::DiffArgs),
    /// Find elements by role, text, label, or placeholder
    Find(find::FindArgs),
    /// Monitor network requests (fetch/XHR)
    Network(network::NetworkArgs),
    /// Capture JS console output
    Console(console::ConsoleArgs),
    /// Export/import session state (cookies + localStorage)
    Session(session::SessionArgs),
    /// Gate operations (actions, eval, fetch) with allow/deny policies
    Policy(policy::PolicyArgs),
    /// Manage cookies
    Cookie(cookie::CookieArgs),
    /// Emulate device viewport and user agent
    Device(device::DeviceArgs),
    /// CPU performance profiling
    Profile(profile::ProfileArgs),
    /// Record sequential frames for AI analysis
    Record(record::RecordArgs),
    /// Check connection status
    Status,
    /// Manage isolated browser contexts for multi-agent use
    Context(context::ContextArgs),
    /// Install the Claude skill, Chrome extension, and NM host (post-install setup)
    Setup(setup::SetupArgs),
    /// Self-update from the latest GitHub release
    #[command(name = "self")]
    SelfCmd(self_cmd::SelfArgs),
    /// Remove every artefact this binary created
    Uninstall(uninstall::UninstallArgs),
    /// Run as an MCP server over stdio for AI-agent hosts
    Mcp(crate::mcp::McpArgs),
    /// Stop the entire headless Chrome session. Use `context close NAME` to close one context.
    Quit,
}
