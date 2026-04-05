use crate::cdp::CdpClient;
use crate::commands;
use crate::output::OutputMode;
use anyhow::Result;

pub(crate) async fn run(
    cdp: &CdpClient,
    args: commands::console::ConsoleArgs,
    output_mode: OutputMode,
) -> Result<()> {
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
            match output_mode {
                OutputMode::Human => eprintln!("OK"),
                OutputMode::Json => println!("{{\"success\":true}}"),
            }
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
            match output_mode {
                OutputMode::Human => {
                    for e in &entries {
                        eprintln!(
                            "[{}] {}",
                            e.get("level").and_then(|v| v.as_str()).unwrap_or("?"),
                            e.get("message").and_then(|v| v.as_str()).unwrap_or("")
                        );
                    }
                    eprintln!("({} entries)", entries.len());
                }
                OutputMode::Json => println!("{}", serde_json::json!(entries)),
            }
        }
        commands::console::ConsoleCommand::Clear => {
            cdp.evaluate("window.__webpilot_console = []").await?;
            match output_mode {
                OutputMode::Human => eprintln!("OK"),
                OutputMode::Json => println!("{{\"success\":true}}"),
            }
        }
    }
    Ok(())
}
