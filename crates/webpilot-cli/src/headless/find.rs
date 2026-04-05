use crate::cdp::CdpClient;
use crate::commands;
use crate::output::OutputMode;
use anyhow::Result;

use super::call_bridge;

pub(crate) async fn run(
    cdp: &CdpClient,
    args: commands::find::FindArgs,
    output_mode: OutputMode,
) -> Result<()> {
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

    let dom_result = call_bridge(
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

    match output_mode {
        OutputMode::Human => {
            for el in &matches {
                eprintln!("[{}] {} \"{}\"", el.index, el.tag, el.text);
            }
            eprintln!("({} matches)", matches.len());
        }
        OutputMode::Json => println!(
            "{}",
            serde_json::json!({"matches": matches, "count": matches.len()})
        ),
    }

    if matches.is_empty() {
        anyhow::bail!("No matching elements found");
    }

    let first_index = matches[0].index;
    if args.click {
        let action_json = serde_json::json!({"action": "Click", "index": first_index});
        call_bridge(
            cdp,
            &serde_json::json!({"type": "executeAction", "action": action_json}).to_string(),
        )
        .await?;
        if output_mode == OutputMode::Human {
            eprintln!("OK");
        }
    } else if let Some(ref fill_text) = args.fill {
        let action_json = serde_json::json!({"action": "Type", "index": first_index, "text": fill_text, "clear": true});
        call_bridge(
            cdp,
            &serde_json::json!({"type": "executeAction", "action": action_json}).to_string(),
        )
        .await?;
        if output_mode == OutputMode::Human {
            eprintln!("OK");
        }
    }
    Ok(())
}
