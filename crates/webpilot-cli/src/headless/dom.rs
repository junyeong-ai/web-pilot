use crate::cdp::CdpClient;
use crate::commands;
use crate::output::CommandOutput;
use anyhow::Result;

use super::{invoke_bridge, parse_bridge_response};

pub(crate) async fn run(cdp: &CdpClient, args: commands::dom::DomArgs) -> Result<CommandOutput> {
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
    let raw = invoke_bridge(cdp, &msg.to_string()).await?;
    let resp = parse_bridge_response(raw)?;
    if let Some(val) = resp.data.get("value").and_then(|v| v.as_str()) {
        Ok(CommandOutput::Content {
            stdout: val.to_string(),
            json: serde_json::json!({"success": true, "value": val}),
        })
    } else {
        Ok(CommandOutput::Ok("OK".into()))
    }
}
