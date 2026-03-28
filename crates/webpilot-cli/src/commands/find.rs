use anyhow::{Context, Result};
use clap::Args;
use webpilot::ipc;
use webpilot::protocol::{BrowserAction, Command, ResponseData};
use webpilot::types::InteractiveElement;

use crate::output::OutputMode;

#[derive(Args)]
pub struct FindArgs {
    /// Filter by ARIA role or tag name
    #[arg(long)]
    pub role: Option<String>,

    /// Filter by visible text (case-insensitive substring match)
    #[arg(long)]
    pub text: Option<String>,

    /// Filter by associated label
    #[arg(long)]
    pub label: Option<String>,

    /// Filter by placeholder text
    #[arg(long)]
    pub placeholder: Option<String>,

    /// Filter by HTML tag name
    #[arg(long)]
    pub tag: Option<String>,

    /// Click the first matching element
    #[arg(long)]
    pub click: bool,

    /// Type text into the first matching element
    #[arg(long)]
    pub fill: Option<String>,
}

pub async fn run(args: FindArgs, output_mode: OutputMode) -> Result<()> {
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

    // Capture DOM to get current elements
    let request = serde_json::to_value(webpilot::protocol::Request {
        id: 1,
        command: Command::Capture {
            dom: true,
            screenshot: false,
            text: false,
            url: None,
            bounds: false,
            full_page: false,
            accessibility: false,
            occlusion: false,
            annotate: false,
        },
    })?;

    let response = ipc::send_request(&request)
        .await
        .context("Host not running")?;
    let resp: webpilot::protocol::Response = serde_json::from_value(response)?;

    let snapshot = match resp.result {
        ResponseData::Capture {
            dom: Some(snapshot),
            ..
        } => snapshot,
        ResponseData::Capture { dom: None, .. } => {
            anyhow::bail!("No DOM data. Navigate to a web page first.")
        }
        ResponseData::Error { message, .. } => anyhow::bail!("{message}"),
        _ => anyhow::bail!("Unexpected response"),
    };

    // Pre-compute lowercase filters (avoid repeated allocation in loop)
    let role_lower = args.role.as_ref().map(|r| r.to_lowercase());
    let text_lower = args.text.as_ref().map(|t| t.to_lowercase());
    let label_lower = args.label.as_ref().map(|l| l.to_lowercase());
    let ph_lower = args.placeholder.as_ref().map(|p| p.to_lowercase());
    let tag_lower = args.tag.as_ref().map(|t| t.to_lowercase());

    // Filter elements by all criteria (AND)
    let matches: Vec<&InteractiveElement> = snapshot
        .elements
        .iter()
        .filter(|el| {
            if let Some(ref role) = role_lower {
                let ok = el
                    .role
                    .as_ref()
                    .map(|r| r.to_lowercase() == *role)
                    .unwrap_or(false)
                    || el.tag.to_lowercase() == *role;
                if !ok {
                    return false;
                }
            }
            if let Some(ref text) = text_lower {
                let ok = el.text.to_lowercase().contains(text.as_str())
                    || el
                        .name
                        .as_ref()
                        .map(|n| n.to_lowercase().contains(text.as_str()))
                        .unwrap_or(false);
                if !ok {
                    return false;
                }
            }
            if let Some(ref label) = label_lower {
                if !el
                    .label
                    .as_ref()
                    .map(|l| l.to_lowercase().contains(label.as_str()))
                    .unwrap_or(false)
                {
                    return false;
                }
            }
            if let Some(ref ph) = ph_lower {
                if !el
                    .placeholder
                    .as_ref()
                    .map(|p| p.to_lowercase().contains(ph.as_str()))
                    .unwrap_or(false)
                {
                    return false;
                }
            }
            if let Some(ref tag) = tag_lower {
                if el.tag.to_lowercase() != *tag {
                    return false;
                }
            }
            true
        })
        .collect();

    // Output matches
    match output_mode {
        OutputMode::Human => {
            for el in &matches {
                let id_suffix = el
                    .id
                    .as_ref()
                    .map(|id| format!("#{id}"))
                    .unwrap_or_default();
                let landmark = el
                    .landmark
                    .as_ref()
                    .map(|l| format!(" @{l}"))
                    .unwrap_or_default();
                eprintln!(
                    "[{}] {}{id_suffix} \"{}\"{landmark}",
                    el.index, el.tag, el.text
                );
            }
            eprintln!("({} matches)", matches.len());
        }
        OutputMode::Json => {
            println!(
                "{}",
                serde_json::json!({
                    "matches": matches,
                    "count": matches.len(),
                })
            );
        }
    }

    if matches.is_empty() {
        std::process::exit(1);
    }

    let first_index = matches[0].index;

    // Chain action if requested
    if args.click {
        execute_action(BrowserAction::Click { index: first_index }, output_mode).await?;
    } else if let Some(ref text) = args.fill {
        execute_action(
            BrowserAction::Type {
                index: first_index,
                text: text.clone(),
                clear: true,
            },
            output_mode,
        )
        .await?;
    }

    Ok(())
}

async fn execute_action(action: BrowserAction, output_mode: OutputMode) -> Result<()> {
    let request = serde_json::to_value(webpilot::protocol::Request {
        id: 2,
        command: Command::Action {
            action,
            capture: false,
        },
    })?;

    let response = ipc::send_request(&request)
        .await
        .context("Host not running")?;
    let resp: webpilot::protocol::Response = serde_json::from_value(response)?;

    match resp.result {
        ResponseData::Action {
            success,
            error,
            code,
            ..
        } => {
            if !success {
                eprintln!(
                    "{}",
                    crate::output::format_error(
                        error.as_deref().unwrap_or("unknown"),
                        code.as_deref(),
                    )
                );
                std::process::exit(1);
            }
            if output_mode == OutputMode::Human {
                eprintln!("OK");
            }
        }
        ResponseData::Error { message, .. } => anyhow::bail!("{message}"),
        _ => anyhow::bail!("Unexpected response"),
    }
    Ok(())
}
