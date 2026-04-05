use crate::cdp::CdpClient;
use crate::commands;
use crate::output::CommandOutput;
use anyhow::Result;

use super::invoke_bridge;

pub(crate) async fn run(cdp: &CdpClient, args: commands::find::FindArgs) -> Result<CommandOutput> {
    if args.role.is_none()
        && args.text.is_none()
        && args.label.is_none()
        && args.placeholder.is_none()
        && args.tag.is_none()
    {
        anyhow::bail!(
            "At least one filter required: --role, --text, --label, --placeholder, or --tag"
        );
    }

    let dom_result = invoke_bridge(
        cdp,
        &serde_json::json!({"type": "extractDOM", "options": {}}).to_string(),
    )
    .await?;
    let elements: Vec<webpilot::types::InteractiveElement> = serde_json::from_value(
        dom_result
            .get("elements")
            .cloned()
            .unwrap_or(serde_json::Value::Array(vec![])),
    )?;

    let filter = webpilot::types::ElementFilter {
        role: args.role.clone(),
        text: args.text.clone(),
        label: args.label.clone(),
        placeholder: args.placeholder.clone(),
        tag: args.tag.clone(),
    };

    let matches: Vec<&webpilot::types::InteractiveElement> =
        elements.iter().filter(|el| el.matches(&filter)).collect();

    if matches.is_empty() {
        anyhow::bail!("No matching elements found");
    }

    let human_lines: Vec<String> = matches
        .iter()
        .map(|el| format!("[{}] {} \"{}\"", el.index, el.tag, el.text))
        .collect();
    let summary = format!("({} matches)", matches.len());
    let items = serde_json::json!({"matches": matches, "count": matches.len()});

    let first_index = matches[0].index;
    if args.click {
        let action_json = serde_json::json!({"action": "Click", "index": first_index});
        invoke_bridge(
            cdp,
            &serde_json::json!({"type": "executeAction", "action": action_json}).to_string(),
        )
        .await?;
    } else if let Some(ref fill_text) = args.fill {
        let action_json = serde_json::json!({"action": "Type", "index": first_index, "text": fill_text, "clear": true});
        invoke_bridge(
            cdp,
            &serde_json::json!({"type": "executeAction", "action": action_json}).to_string(),
        )
        .await?;
    }

    Ok(CommandOutput::List {
        items,
        human_lines,
        summary,
    })
}
