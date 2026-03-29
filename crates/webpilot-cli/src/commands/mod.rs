pub mod action;
pub mod capture;
pub mod console;
pub mod cookies;
pub mod device;
pub mod diff;
pub mod dom;
pub mod eval;
pub mod fetch;
pub mod find;
pub mod frames;
pub mod install;
pub mod network;
pub mod policy;
pub mod profile;
pub mod record;
pub mod session;
pub mod status;
pub mod tabs;
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
    Tabs(tabs::TabsArgs),
    /// Navigate and manage iframes
    Frames(frames::FramesArgs),
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
    /// Set action safety policies (allow/deny)
    Policy(policy::PolicyArgs),
    /// Manage cookies
    Cookies(cookies::CookiesArgs),
    /// Emulate device viewport and user agent
    Device(device::DeviceArgs),
    /// CPU performance profiling
    Profile(profile::ProfileArgs),
    /// Record sequential frames for AI analysis
    Record(record::RecordArgs),
    /// Check connection status
    Status,
    /// Install Native Messaging host manifest (for --browser mode)
    Install(install::InstallArgs),
    /// Shut down headless Chrome session
    Quit,
}
