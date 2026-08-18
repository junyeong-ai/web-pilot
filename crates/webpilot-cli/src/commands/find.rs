use anyhow::Result;
use clap::Args;
use webpilot::Action;
use webpilot::capture::{CaptureField, CaptureOpts};
use webpilot::protocol::{Command, ResponseData};
use webpilot::types::{InteractiveElement, line_safe, line_safe_clip};

use crate::output::CommandOutput;
use crate::transport::{Transport, lift_error};

// Derives serde alongside clap so the MCP `browser_find` tool deserializes the
// same struct the CLI parses — one filter surface, no parallel mapping. Unknown
// fields are rejected so a misspelled filter never silently matches everything.
#[derive(Args, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindArgs {
    #[arg(long)]
    #[serde(default)]
    pub role: Option<String>,
    // text / label / placeholder are arbitrary page text that can start with
    // `-` (e.g. searching for "-50%"); `fill` is typed text (e.g. a negative
    // number). Accept a leading-dash value rather than read it as a flag.
    #[arg(long, allow_hyphen_values = true)]
    #[serde(default)]
    pub text: Option<String>,
    #[arg(long, allow_hyphen_values = true)]
    #[serde(default)]
    pub label: Option<String>,
    #[arg(long, allow_hyphen_values = true)]
    #[serde(default)]
    pub placeholder: Option<String>,
    #[arg(long)]
    #[serde(default)]
    pub tag: Option<String>,
    /// Click the match (the filter must match exactly one element).
    /// Mutually exclusive with --fill.
    #[arg(long, conflicts_with = "fill")]
    #[serde(default)]
    pub click: bool,
    /// Type into the match (the filter must match exactly one element).
    /// Mutually exclusive with --click.
    #[arg(long, allow_hyphen_values = true)]
    #[serde(default)]
    pub fill: Option<String>,
}

pub async fn run<T: Transport>(transport: &mut T, args: FindArgs) -> Result<CommandOutput> {
    let named: [(&str, &Option<String>); 5] = [
        ("role", &args.role),
        ("text", &args.text),
        ("label", &args.label),
        ("placeholder", &args.placeholder),
        ("tag", &args.tag),
    ];
    if named.iter().all(|(_, f)| f.is_none()) {
        return Err(webpilot::WebPilotError::InvalidArgument {
            detail:
                "at least one filter required: --role, --text, --label, --placeholder, or --tag"
                    .into(),
        }
        .into());
    }
    // A filter PRESENT but empty/whitespace is a no-op that silently matches
    // everything (`--text ""` → `contains("")`) or nothing (`--role ""` → an
    // exact-match miss) — an agent that built the value from a variable which
    // happened to be empty would get a surprising match-all, and on a
    // single-element page `--click`/`--fill` would then proceed as if the
    // filter discriminated. Reject the empty value loudly instead, naming the
    // flag, so the intent is never silently dropped.
    for (name, value) in named {
        if value.as_deref().is_some_and(|s| s.trim().is_empty()) {
            return Err(webpilot::WebPilotError::InvalidArgument {
                detail: format!(
                    "--{name} was given an empty value — a filter must be a non-empty string"
                ),
            }
            .into());
        }
    }
    // clap's `conflicts_with` guards only the CLI parse; serde entry points
    // (the MCP tool) reach here unchecked, so the invariant is enforced where
    // every caller converges — otherwise `click` would silently win and the
    // `fill` text be dropped.
    if args.click && args.fill.is_some() {
        return Err(webpilot::WebPilotError::InvalidArgument {
            detail: "--click and --fill are mutually exclusive — chain one action per find".into(),
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

    // `find` renders only what it matched, which is what lets it reach an
    // element the capture's own listing left out — but a filter broad enough to
    // match most of the page turns that into the unbounded response the listing
    // cap exists to prevent. Bound it by the same knob and report the true total,
    // so a filter that needs narrowing says so instead of arriving as a wall of
    // rows.
    let total = matches.len();
    let listed = &matches[..total.min(webpilot::settings::get().capture.max_elements)];

    let human_lines: Vec<String> = listed
        .iter()
        .map(|el| {
            // Every page-controlled field is line-safed: a `\n` in an id, the
            // text, or a landmark would otherwise forge an extra match row.
            let id_suffix = el
                .id
                .as_deref()
                .map(|i| format!("#{}", line_safe(i)))
                .unwrap_or_default();
            let landmark = el
                .spatial
                .landmark
                .as_deref()
                .map(|l| format!(" @{}", line_safe(l)))
                .unwrap_or_default();
            format!(
                "[{}] {}{id_suffix} \"{}\"{landmark}",
                el.index,
                el.tag,
                line_safe(&el.text)
            )
        })
        .collect();
    let summary = if listed.len() < total {
        format!(
            "({total} matches, {} shown — narrow the filter)",
            listed.len()
        )
    } else {
        format!("({total} matches)")
    };
    let mut items = serde_json::json!({
        "matches": listed,
        "count": total,
        "matches_truncated": listed.len() < total,
    });
    let mut human_lines = human_lines;

    // The strict-selector contract (`frame url` v0.4.152, `tab find` v0.4.169):
    // a chained action on an ambiguous filter would silently act on whichever
    // element matched first — a side-effecting guess with no signal the others
    // existed (the wrong form submitted, the wrong field filled). Fail loud
    // naming the matches; the agent narrows the filter or acts by index. A
    // bare `find` (no action) still lists every match — that is its job.
    if (args.click || args.fill.is_some()) && matches.len() > 1 {
        let listed: Vec<String> = matches
            .iter()
            .take(5)
            .map(|el| format!("[{}] {} \"{}\"", el.index, el.tag, line_safe(&el.text)))
            .collect();
        return Err(webpilot::WebPilotError::InvalidArgument {
            detail: format!(
                "{} elements match the filter; narrow it or act by index (webpilot action click N): {}{}",
                matches.len(),
                listed.join(", "),
                if matches.len() > 5 { ", …" } else { "" }
            ),
        }
        .into());
    }

    let first_index = matches[0].index;

    let effect = if args.click {
        chain_action(
            transport,
            Action::Click {
                index: first_index,
                modifiers: Default::default(),
            },
        )
        .await?
    } else if let Some(text) = args.fill {
        chain_action(
            transport,
            Action::Type {
                index: first_index,
                text,
                clear: true,
            },
        )
        .await?
    } else {
        ChainEffect::default()
    };

    // Surface the chained action's navigation/popup just as a direct `action`
    // does, in both the JSON and the human lines — so `find --click` never hides
    // that the click left the page or opened a tab.
    if let Some(ref url) = effect.url_changed {
        items["url_changed"] = serde_json::json!(url);
        human_lines.push(format!("→ URL changed: {}", line_safe_clip(url, 200)));
    }
    if let Some(ref tab) = effect.new_tab {
        items["new_tab"] = serde_json::to_value(tab).expect("TabInfo serializes");
        human_lines.push(format!(
            "→ New tab opened: {} (switched automatically)",
            line_safe_clip(&tab.url, 200)
        ));
    }

    if !effect.downloads.is_empty() {
        items["downloads"] = serde_json::to_value(&effect.downloads).expect("Download serializes");
        for d in &effect.downloads {
            human_lines.push(format!("→ {}", d.to_line()));
        }
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

/// The navigation/popup effects a chained `--click`/`--fill` produced, so `find`
/// can surface them exactly as a direct `action` does — otherwise an agent using
/// the `find --click` shortcut on a link that navigates or opens a tab would never
/// learn it left the page.
#[derive(Default)]
struct ChainEffect {
    url_changed: Option<String>,
    new_tab: Option<webpilot::types::TabInfo>,
    downloads: Vec<webpilot::types::Download>,
}

async fn chain_action<T: Transport>(transport: &mut T, action: Action) -> Result<ChainEffect> {
    let result = transport
        .send(Command::Action {
            action,
            capture: false,
        })
        .await?;
    match result {
        ResponseData::Action {
            success,
            error,
            url_changed,
            new_tab,
            downloads,
            dom: _,
            capture_error: _,
        } => {
            lift_error(success, error, ())?;
            Ok(ChainEffect {
                url_changed,
                new_tab,
                downloads,
            })
        }
        ResponseData::Error { error } => Err(error.into()),
        _ => anyhow::bail!("Unexpected response shape"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn click_and_fill_are_rejected_together_before_any_io() {
        // The serde entry point carries no clap `conflicts_with`, so the
        // handler itself must reject the combination — before any transport
        // I/O, so the check needs no browser.
        let mut transport = crate::transport::IpcTransport::new();
        let Err(err) = run(
            &mut transport,
            FindArgs {
                role: Some("button".into()),
                text: None,
                label: None,
                placeholder: None,
                tag: None,
                click: true,
                fill: Some("x".into()),
            },
        )
        .await
        else {
            panic!("click+fill must be rejected");
        };
        let e = err.downcast::<webpilot::WebPilotError>().unwrap();
        assert!(matches!(e, webpilot::WebPilotError::InvalidArgument { .. }));
    }

    #[test]
    fn find_args_deserialize_strictly() {
        // Defaults fill absent fields; an unknown field is an error, never a
        // filter that silently matches everything.
        let ok: FindArgs =
            serde_json::from_value(json!({ "label": "Email", "fill": "x" })).unwrap();
        assert_eq!(ok.label.as_deref(), Some("Email"));
        assert_eq!(ok.fill.as_deref(), Some("x"));
        assert!(!ok.click);
        let bad = serde_json::from_value::<FindArgs>(json!({ "lable": "Email" }));
        assert!(bad.is_err(), "unknown filter field must be rejected");
    }
}
