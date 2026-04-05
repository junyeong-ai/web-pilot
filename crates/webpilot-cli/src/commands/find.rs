use anyhow::{Context, Result};
use clap::Args;
use webpilot::ipc;
use webpilot::protocol::{BrowserAction, Command, ResponseData};
use webpilot::types::InteractiveElement;

use crate::output::CommandOutput;

#[derive(Args)]
/// At least one filter is required
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

pub async fn run(args: FindArgs) -> Result<CommandOutput> {
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
    let request = serde_json::to_value(webpilot::protocol::Request::new(
        1,
        Command::Capture {
            dom: true,
            screenshot: false,
            text: false,
            url: None,
            bounds: false,
            full_page: false,
            accessibility: false,
            occlusion: false,
            annotate: false,
            pdf: false,
        },
    ))?;

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

    let filter = webpilot::types::ElementFilter {
        role: args.role.clone(),
        text: args.text.clone(),
        label: args.label.clone(),
        placeholder: args.placeholder.clone(),
        tag: args.tag.clone(),
    };

    let matches: Vec<&InteractiveElement> = snapshot
        .elements
        .iter()
        .filter(|el| el.matches(&filter))
        .collect();

    // Build output
    let human_lines: Vec<String> = matches
        .iter()
        .map(|el| {
            let id_suffix = el
                .id
                .as_ref()
                .map(|id| format!("#{id}"))
                .unwrap_or_default();
            let landmark = el
                .spatial
                .landmark
                .as_ref()
                .map(|l| format!(" @{l}"))
                .unwrap_or_default();
            format!(
                "[{}] {}{id_suffix} \"{}\"{landmark}",
                el.index, el.tag, el.text
            )
        })
        .collect();
    let summary = format!("({} matches)", matches.len());

    let output = CommandOutput::List {
        items: serde_json::json!({
            "matches": matches,
            "count": matches.len(),
        }),
        human_lines,
        summary,
    };

    if matches.is_empty() {
        anyhow::bail!("No matching elements found");
    }

    let first_index = matches[0].index;

    // Chain action if requested
    if args.click {
        execute_action(BrowserAction::Click { index: first_index }).await?;
    } else if let Some(ref text) = args.fill {
        execute_action(BrowserAction::Type {
            index: first_index,
            text: text.clone(),
            clear: true,
        })
        .await?;
    }

    Ok(output)
}

async fn execute_action(action: BrowserAction) -> Result<()> {
    let request = serde_json::to_value(webpilot::protocol::Request::new(
        2,
        Command::Action {
            action,
            capture: false,
        },
    ))?;

    let response = ipc::send_request(&request)
        .await
        .context("Host not running")?;
    let resp: webpilot::protocol::Response = serde_json::from_value(response)?;

    match resp.result {
        ResponseData::Action { success, error, .. } => {
            if !success {
                if let Some(ref err) = error {
                    anyhow::bail!("{}", crate::output::format_error(err));
                } else {
                    anyhow::bail!("Unknown error");
                }
            }
        }
        ResponseData::Error { message, .. } => anyhow::bail!("{message}"),
        _ => anyhow::bail!("Unexpected response"),
    }
    Ok(())
}
