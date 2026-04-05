mod cdp;
mod cli;
mod commands;
mod headless;
mod host;
mod output;
pub mod session;
pub mod stitch;
mod timeouts;

/// WebPilot: Browser control tool for AI agents.
///
/// Mode detection:
/// - Chrome launches NM host with: `<binary> chrome-extension://ID/ --parent-window=N`
///   → Detected by `chrome-extension://` arg → Host mode
/// - User/AI runs: `webpilot capture --dom` → CLI mode
/// - No arguments → CLI mode (shows help)
#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Chrome Native Messaging passes `chrome-extension://...` as first arg
    let is_nm_host = args.iter().any(|a| a.starts_with("chrome-extension://"));

    let result = if is_nm_host {
        host::run_host().await
    } else {
        cli::run_cli().await
    };

    if let Err(e) = result {
        let msg = format!("{e:#}");
        eprintln!("Error: {msg}");
        std::process::exit(classify_exit_code(&msg));
    }
}

/// Map error messages to standardized exit codes for AI agent consumption.
fn classify_exit_code(msg: &str) -> i32 {
    let msg_lower = msg.to_lowercase();
    if msg_lower.contains("elementnotfound")
        || msg_lower.contains("element not found")
        || msg_lower.contains("out of range")
    {
        4 // Element not found
    } else if msg_lower.contains("timeout") || msg_lower.contains("timed out") {
        5 // Timeout
    } else if msg_lower.contains("policydenied")
        || msg_lower.contains("policy") && msg_lower.contains("denied")
    {
        6 // Policy denied
    } else if msg_lower.contains("not connected")
        || msg_lower.contains("not running")
        || msg_lower.contains("failed to connect")
    {
        3 // Connection error
    } else {
        1 // General error
    }
}
