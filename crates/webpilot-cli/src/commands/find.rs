use anyhow::Result;
use clap::Args;
use webpilot::Action;
use webpilot::capture::{CaptureField, CaptureOpts};
use webpilot::protocol::{Command, ResponseData};
use webpilot::types::InteractiveElement;

use crate::output::CommandOutput;
use crate::transport::{Transport, lift_error};

#[derive(Args)]
pub struct FindArgs {
    #[arg(long)]
    pub role: Option<String>,
    #[arg(long)]
    pub text: Option<String>,
    #[arg(long)]
    pub label: Option<String>,
    #[arg(long)]
    pub placeholder: Option<String>,
    #[arg(long)]
    pub tag: Option<String>,
    /// Click the first match. Mutually exclusive with --fill.
    #[arg(long, conflicts_with = "fill")]
    pub click: bool,
    /// Type into the first match. Mutually exclusive with --click.
    #[arg(long)]
    pub fill: Option<String>,
}

pub async fn run<T: Transport>(transport: &mut T, args: FindArgs) -> Result<CommandOutput> {
    if args.role.is_none()
        && args.text.is_none()
        && args.label.is_none()
        && args.placeholder.is_none()
        && args.tag.is_none()
    {
        return Err(webpilot::WebPilotError::InvalidArgument {
            detail:
                "at least one filter required: --role, --text, --label, --placeholder, or --tag"
                    .into(),
        }
        .into());
    }

    let result = transport
        .send(Command::Capture {
            include: vec![CaptureField::Dom],
            opts: CaptureOpts::default(),
            url: None,
        })
        .await?;

    let snapshot = match result {
        ResponseData::Capture { dom: Some(s), .. } => s,
        ResponseData::Capture { dom: None, .. } => {
            return Err(webpilot::WebPilotError::NoPage.into());
        }
        ResponseData::Error { error } => return Err(error.into()),
        _ => anyhow::bail!("Unexpected response shape"),
    };

    let filter = webpilot::types::ElementFilter {
        role: args.role,
        text: args.text,
        label: args.label,
        placeholder: args.placeholder,
        tag: args.tag,
    };

    let matches: Vec<&InteractiveElement> = snapshot
        .elements
        .iter()
        .filter(|el| el.matches(&filter))
        .collect();

    if matches.is_empty() {
        return Err(webpilot::WebPilotError::SelectorNotFound {
            selector: render_filter(&filter),
        }
        .into());
    }

    let human_lines: Vec<String> = matches
        .iter()
        .map(|el| {
            let id_suffix = el
                .id
                .as_deref()
                .map(|i| format!("#{i}"))
                .unwrap_or_default();
            let landmark = el
                .spatial
                .landmark
                .as_deref()
                .map(|l| format!(" @{l}"))
                .unwrap_or_default();
            format!(
                "[{}] {}{id_suffix} \"{}\"{landmark}",
                el.index, el.tag, el.text
            )
        })
        .collect();
    let summary = format!("({} matches)", matches.len());
    let items = serde_json::json!({"matches": matches, "count": matches.len()});

    let first_index = matches[0].index;

    if args.click {
        chain_action(transport, Action::Click { index: first_index }).await?;
    } else if let Some(text) = args.fill {
        chain_action(
            transport,
            Action::Type {
                index: first_index,
                text,
                clear: true,
            },
        )
        .await?;
    }

    Ok(CommandOutput::List {
        items,
        human_lines,
        summary,
    })
}

/// Render the active filter criteria as a user-readable selector — used in
/// `SelectorNotFound` errors instead of the `Debug` repr of `ElementFilter`.
fn render_filter(filter: &webpilot::types::ElementFilter) -> String {
    let mut parts = Vec::new();
    if let Some(ref v) = filter.role {
        parts.push(format!("role={v}"));
    }
    if let Some(ref v) = filter.text {
        parts.push(format!("text={v:?}"));
    }
    if let Some(ref v) = filter.label {
        parts.push(format!("label={v:?}"));
    }
    if let Some(ref v) = filter.placeholder {
        parts.push(format!("placeholder={v:?}"));
    }
    if let Some(ref v) = filter.tag {
        parts.push(format!("tag={v}"));
    }
    if parts.is_empty() {
        "<no filter>".into()
    } else {
        parts.join(" ")
    }
}

async fn chain_action<T: Transport>(transport: &mut T, action: Action) -> Result<()> {
    let result = transport
        .send(Command::Action {
            action,
            capture: false,
        })
        .await?;
    match result {
        ResponseData::Action { success, error, .. } => lift_error(success, error, ()),
        ResponseData::Error { error } => Err(error.into()),
        _ => anyhow::bail!("Unexpected response shape"),
    }
}
