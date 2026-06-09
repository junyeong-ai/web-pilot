use anyhow::Result;
use clap::{Args, Subcommand};
use webpilot::WebPilotError;
use webpilot::dirs;

use crate::output::CommandOutput;
use crate::transport::LocalTransport;
use crate::transport::local_context::{ContextEntry, context_file_path, is_context_file};

#[derive(Args)]
pub struct ContextArgs {
    #[command(subcommand)]
    pub command: ContextCommand,
}

#[derive(Subcommand)]
pub enum ContextCommand {
    /// List active contexts.
    List,
    /// Close a context or all contexts.
    Close {
        name: Option<String>,
        #[arg(long)]
        all: bool,
    },
}

pub async fn run(local: &mut LocalTransport, args: ContextArgs) -> Result<CommandOutput> {
    match args.command {
        ContextCommand::List => list_contexts(),
        ContextCommand::Close { name, all } => close_contexts(local, name, all).await,
    }
}

fn list_contexts() -> Result<CommandOutput> {
    let mut contexts = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dirs::contexts_dir()) {
        for entry in entries.filter_map(|e| e.ok()) {
            let fname = entry.file_name().to_string_lossy().to_string();
            if !is_context_file(&fname) {
                continue;
            }
            if let Ok(data) = std::fs::read_to_string(entry.path())
                && let Ok(ctx) = serde_json::from_str::<ContextEntry>(&data)
            {
                contexts.push(ctx);
            }
        }
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let human_lines: Vec<String> = if contexts.is_empty() {
        vec!["No active contexts".into()]
    } else {
        contexts
            .iter()
            .map(|ctx| {
                let age = now.saturating_sub(ctx.created_at);
                format!("  {} ({}s old) — {}", ctx.name, age, ctx.cwd)
            })
            .collect()
    };
    let summary = if contexts.is_empty() {
        String::new()
    } else {
        format!("{} context(s)", contexts.len())
    };

    Ok(CommandOutput::List {
        items: serde_json::json!(contexts),
        human_lines,
        summary,
    })
}

async fn close_contexts(
    local: &LocalTransport,
    name: Option<String>,
    all: bool,
) -> Result<CommandOutput> {
    let browser = local.browser();

    if all {
        let mut count = 0;
        let mut kept = 0;
        if let Ok(entries) = std::fs::read_dir(dirs::contexts_dir()) {
            for entry in entries.filter_map(|e| e.ok()) {
                let fname = entry.file_name().to_string_lossy().to_string();
                if !is_context_file(&fname) {
                    continue;
                }
                if let Ok(data) = std::fs::read_to_string(entry.path())
                    && let Ok(ctx) = serde_json::from_str::<ContextEntry>(&data)
                {
                    // Best-effort across the whole set: one context that fails
                    // to dispose must not abort the sweep, but its metadata is
                    // kept (a live context with no record would be orphaned).
                    if dispose_if_live(browser, &ctx.browser_context_id)
                        .await
                        .is_err()
                    {
                        kept += 1;
                        continue;
                    }
                    crate::transport::local::clear_context_state(&ctx.browser_context_id);
                }
                let _ = std::fs::remove_file(entry.path());
                count += 1;
            }
        }
        let msg = if kept > 0 {
            format!("Closed {count} context(s); {kept} kept (failed to dispose — retry)")
        } else {
            format!("Closed {count} context(s)")
        };
        return Ok(CommandOutput::Ok(msg));
    }

    let Some(name) = name else {
        return Err(WebPilotError::InvalidArgument {
            detail: "specify a context name or --all".into(),
        }
        .into());
    };

    let file_path = context_file_path(&name);
    let data = std::fs::read_to_string(&file_path)
        .map_err(|_| WebPilotError::ContextNotFound { name: name.clone() })?;
    if let Ok(ctx) = serde_json::from_str::<ContextEntry>(&data) {
        dispose_if_live(browser, &ctx.browser_context_id).await?;
        crate::transport::local::clear_context_state(&ctx.browser_context_id);
    }
    let _ = std::fs::remove_file(&file_path);
    Ok(CommandOutput::Ok(format!("Closed context '{name}'")))
}

/// Dispose a CDP browser context, tolerating one that is already gone (a
/// stale entry must remain closable) but propagating a failure to dispose a
/// LIVE context — deleting its metadata then would orphan it in Chrome with
/// no record left to close it through.
async fn dispose_if_live(browser: &crate::cdp::CdpClient, browser_context_id: &str) -> Result<()> {
    let live = browser.get_browser_contexts().await?;
    if live.contains(&browser_context_id.to_string()) {
        browser.dispose_browser_context(browser_context_id).await?;
    }
    Ok(())
}
