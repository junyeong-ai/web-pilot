use crate::cdp::CdpClient;
use crate::commands;
use crate::output::OutputMode;
use anyhow::Result;

use super::call_bridge;

pub(crate) async fn run(
    cdp: &CdpClient,
    args: commands::dom::DomArgs,
    output_mode: OutputMode,
) -> Result<()> {
    let msg = match &args.command {
        commands::dom::DomCommand::SetHtml { selector, value } => {
            serde_json::json!({"type": "setHtml", "selector": selector, "value": value})
        }
        commands::dom::DomCommand::SetText { selector, value } => {
            serde_json::json!({"type": "setText", "selector": selector, "value": value})
        }
        commands::dom::DomCommand::SetAttr {
            selector,
            attr,
            value,
        } => {
            serde_json::json!({"type": "setAttr", "selector": selector, "attr": attr, "value": value})
        }
        commands::dom::DomCommand::GetHtml { selector } => {
            serde_json::json!({"type": "getHtml", "selector": selector})
        }
        commands::dom::DomCommand::GetText { selector } => {
            serde_json::json!({"type": "getText", "selector": selector})
        }
        commands::dom::DomCommand::GetAttr { selector, attr } => {
            serde_json::json!({"type": "getAttr", "selector": selector, "attr": attr})
        }
    };
    let result = call_bridge(cdp, &msg.to_string()).await?;
    let success = result
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if let Some(val) = result.get("value").and_then(|v| v.as_str()) {
        match output_mode {
            OutputMode::Human => println!("{val}"),
            OutputMode::Json => println!("{}", serde_json::json!({"success": true, "value": val})),
        }
    } else if success {
        match output_mode {
            OutputMode::Human => eprintln!("OK"),
            OutputMode::Json => println!("{{\"success\":true}}"),
        }
    }
    if !success {
        let err = result
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        anyhow::bail!("{err}");
    }
    Ok(())
}
