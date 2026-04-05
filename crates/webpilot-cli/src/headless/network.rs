use crate::cdp::CdpClient;
use crate::commands;
use crate::output::OutputMode;
use anyhow::Result;

use super::call_bridge;

pub(crate) async fn run(
    cdp: &CdpClient,
    args: commands::network::NetworkArgs,
    output_mode: OutputMode,
) -> Result<()> {
    match args.command {
        commands::network::NetworkCommand::Start => {
            call_bridge(
                cdp,
                &serde_json::json!({"type": "executeAction", "action": {"action": "noop"}})
                    .to_string(),
            )
            .await
            .ok(); // ensure bridge loaded
            // Inject network monitoring via the same MAIN-world pattern
            cdp.evaluate(r#"
                if (!window.__webpilot_network_active) {
                    window.__webpilot_network_active = true;
                    window.__webpilot_network = [];
                    const origFetch = window.fetch;
                    window.fetch = function(...args) {
                        const [resource, config] = args;
                        const t0 = performance.now();
                        return origFetch.apply(this, args).then(response => {
                            window.__webpilot_network.push({ type: "fetch", url: String(resource), method: config?.method || "GET", status: response.status, duration_ms: Math.round(performance.now() - t0), timestamp: Date.now() });
                            if (window.__webpilot_network.length > 500) window.__webpilot_network.shift();
                            return response;
                        }).catch(err => { window.__webpilot_network.push({ type: "fetch", url: String(resource), method: config?.method || "GET", error: err.message, duration_ms: Math.round(performance.now() - t0), timestamp: Date.now() }); throw err; });
                    };
                }
                true
            "#).await?;
            match output_mode {
                OutputMode::Human => eprintln!("OK"),
                OutputMode::Json => println!("{{\"success\":true}}"),
            }
        }
        commands::network::NetworkCommand::Read { since } => {
            let js = format!(
                "(window.__webpilot_network || []).filter(e => e.timestamp >= {})",
                since.unwrap_or(0)
            );
            let result = cdp.evaluate(&js).await?;
            match output_mode {
                OutputMode::Human => {
                    if let Some(arr) = result.as_array() {
                        for r in arr {
                            eprintln!(
                                "{} {} {} → {}",
                                r.get("type").and_then(|v| v.as_str()).unwrap_or("?"),
                                r.get("method").and_then(|v| v.as_str()).unwrap_or("?"),
                                r.get("url")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("?")
                                    .get(..60)
                                    .unwrap_or(""),
                                r.get("status").and_then(|v| v.as_u64()).unwrap_or(0)
                            );
                        }
                        eprintln!("({} requests)", arr.len());
                    }
                }
                OutputMode::Json => println!("{}", result),
            }
        }
        commands::network::NetworkCommand::Clear => {
            cdp.evaluate("window.__webpilot_network = []").await?;
            match output_mode {
                OutputMode::Human => eprintln!("OK"),
                OutputMode::Json => println!("{{\"success\":true}}"),
            }
        }
    }
    Ok(())
}
