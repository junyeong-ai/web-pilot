use crate::cdp::CdpClient;
use crate::commands;
use crate::output::CommandOutput;
use anyhow::Result;

use super::invoke_bridge;

pub(crate) async fn run(
    cdp: &CdpClient,
    args: commands::network::NetworkArgs,
) -> Result<CommandOutput> {
    match args.command {
        commands::network::NetworkCommand::Start => {
            invoke_bridge(
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
            Ok(CommandOutput::Ok("OK".into()))
        }
        commands::network::NetworkCommand::Read { since } => {
            let js = format!(
                "(window.__webpilot_network || []).filter(e => e.timestamp >= {})",
                since.unwrap_or(0)
            );
            let result = cdp.evaluate(&js).await?;
            let entries = result.as_array().cloned().unwrap_or_default();
            let human_lines: Vec<String> = entries
                .iter()
                .map(|r| {
                    format!(
                        "{} {} {} → {}",
                        r.get("type").and_then(|v| v.as_str()).unwrap_or("?"),
                        r.get("method").and_then(|v| v.as_str()).unwrap_or("?"),
                        r.get("url")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?")
                            .get(..60)
                            .unwrap_or(r.get("url").and_then(|v| v.as_str()).unwrap_or("?")),
                        r.get("status").and_then(|v| v.as_u64()).unwrap_or(0)
                    )
                })
                .collect();
            let summary = format!("({} requests)", entries.len());
            Ok(CommandOutput::List {
                items: result,
                human_lines,
                summary,
            })
        }
        commands::network::NetworkCommand::Clear => {
            cdp.evaluate("window.__webpilot_network = []").await?;
            Ok(CommandOutput::Ok("OK".into()))
        }
    }
}
