mod cdp;
mod cli;
mod commands;
mod headless;
mod host;
mod output;
pub mod session;
pub mod stitch;

/// WebPilot: Browser control tool for AI agents.
///
/// Mode detection:
/// - Chrome launches NM host with: `<binary> chrome-extension://ID/ --parent-window=N`
///   → Detected by `chrome-extension://` arg → Host mode
/// - User/AI runs: `webpilot capture --dom` → CLI mode
/// - No arguments → CLI mode (shows help)
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    // Chrome Native Messaging passes `chrome-extension://...` as first arg
    let is_nm_host = args.iter().any(|a| a.starts_with("chrome-extension://"));

    if is_nm_host {
        host::run_host().await
    } else {
        cli::run_cli().await
    }
}
