use crate::cdp::CdpClient;
use crate::commands;
use crate::output::CommandOutput;
use anyhow::Result;

pub(crate) async fn run(
    cdp: &CdpClient,
    args: commands::console::ConsoleArgs,
) -> Result<CommandOutput> {
    match args.command {
        commands::console::ConsoleCommand::Start => {
            cdp.evaluate(r#"
                if (!window.__webpilot_console) {
                    window.__webpilot_console = [];
                    const orig = { log: console.log, error: console.error, warn: console.warn, info: console.info };
                    ["log", "error", "warn", "info"].forEach(m => {
                        console[m] = (...args) => {
                            window.__webpilot_console.push({ level: m, message: args.map(a => { try { return String(a); } catch { return "[object]"; } }).join(" "), timestamp: Date.now() });
                            if (window.__webpilot_console.length > 500) window.__webpilot_console.shift();
                            orig[m].apply(console, args);
                        };
                    });
                }
                true
            "#).await?;
            Ok(CommandOutput::Ok("OK".into()))
        }
        commands::console::ConsoleCommand::Read { level } => {
            let result = cdp.evaluate("window.__webpilot_console || []").await?;
            let entries: Vec<serde_json::Value> = if let Some(arr) = result.as_array() {
                if let Some(ref lvl) = level {
                    arr.iter()
                        .filter(|e| e.get("level").and_then(|v| v.as_str()) == Some(lvl.as_str()))
                        .cloned()
                        .collect()
                } else {
                    arr.clone()
                }
            } else {
                vec![]
            };
            let human_lines: Vec<String> = entries
                .iter()
                .map(|e| {
                    format!(
                        "[{}] {}",
                        e.get("level").and_then(|v| v.as_str()).unwrap_or("?"),
                        e.get("message").and_then(|v| v.as_str()).unwrap_or("")
                    )
                })
                .collect();
            let summary = format!("({} entries)", entries.len());
            Ok(CommandOutput::List {
                items: serde_json::json!(entries),
                human_lines,
                summary,
            })
        }
        commands::console::ConsoleCommand::Clear => {
            cdp.evaluate("window.__webpilot_console = []").await?;
            Ok(CommandOutput::Ok("OK".into()))
        }
    }
}
