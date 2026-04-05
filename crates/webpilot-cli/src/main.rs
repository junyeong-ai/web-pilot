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
        let output_mode = output::detect_output_mode(
            std::env::args().any(|a| a == "--json"),
        );

        if let Some(we) = e.downcast_ref::<webpilot::types::WebPilotError>() {
            // Structured error: deterministic exit code + formatted output
            output::render_error(we, output_mode);
            std::process::exit(we.code.exit_code());
        } else {
            // External error: heuristic exit code + raw message
            let msg = format!("{e:#}");
            eprintln!("Error: {msg}");
            std::process::exit(infer_exit_code(&msg));
        }
    }
}

/// Infer exit code from unstructured error messages (external crate errors only).
/// All WebPilot-originated errors should use `WebPilotError` instead.
fn infer_exit_code(msg: &str) -> i32 {
    let lower = msg.to_lowercase();
    if lower.contains("timeout") || lower.contains("timed out") {
        5
    } else if lower.contains("not connected")
        || lower.contains("not running")
        || lower.contains("failed to connect")
        || lower.contains("connection")
    {
        3
    } else {
        1
    }
}
