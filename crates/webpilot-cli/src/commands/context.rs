use anyhow::Result;
use clap::{Args, Subcommand};
use webpilot::WebPilotError;
use webpilot::dirs;
use webpilot::types::line_safe;

use crate::output::CommandOutput;
use crate::transport::LocalTransport;
use crate::transport::local_context::{
    ContextEntry, context_file_path, is_context_file, try_lock_live,
};

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
        /// A target is required up front — deferring the "name or --all" check
        /// into the handler let the bare invocation launch Chrome on its way to
        /// the rejection.
        #[arg(required_unless_present = "all")]
        name: Option<String>,
        #[arg(long, conflicts_with = "name")]
        all: bool,
    },
}

pub async fn run(local: &mut LocalTransport, args: ContextArgs) -> Result<CommandOutput> {
    match args.command {
        ContextCommand::Close { name, all } => close_contexts(local, name, all).await,
        // `list` is resolved before a transport opens (a disk read must never
        // launch Chrome) — see `run_headless_mode`. Kept exhaustive in case a
        // future caller reaches `run` directly: it still needs no browser.
        ContextCommand::List => list_contexts(),
    }
}

/// Read the context store off disk. Pure filesystem I/O — no browser — so the CLI
/// resolves `context list` through this BEFORE opening a transport, never paying
/// a Chrome launch (or failing when Chrome is unavailable) to list contexts.
pub fn list_contexts() -> Result<CommandOutput> {
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
                // `line_safe` like every other agent-facing field: a name or
                // cwd carrying a newline must not forge an extra list row.
                format!(
                    "  {} ({}s old) — {}",
                    line_safe(&ctx.name),
                    age,
                    line_safe(&ctx.cwd)
                )
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
    // Disposing a context destroys it and all its tabs — and `--all` can wipe
    // other agents' contexts. It reaches CDP directly (not via the gated
    // `LocalTransport::send`), so gate it here, like `device`: a strictly more
    // destructive effect than the gated `tab_close`, which `default deny` must
    // forbid. (`context list`, a read, is not gated.)
    crate::policy::enforce_key(webpilot::types::PolicyKey::ContextClose)?;

    let browser = local.browser();
    // The context THIS transport is bound to, if any. Closing your own context is
    // always allowed — your transport holds its shared liveness lock, so the probe
    // below would see it as "in use" and falsely refuse, yet you are deliberately
    // closing it. Only a CROSS close (a context another live process holds) is
    // refused, so one agent can't evict another's running session.
    let own = local.browser_context_id.as_deref();

    if all {
        let mut closed = 0;
        let mut in_use = 0; // a live transport elsewhere holds it — left running
        let mut failed = 0; // disposal errored (CDP) — record kept for a retry
        if let Ok(entries) = std::fs::read_dir(dirs::contexts_dir()) {
            for entry in entries.filter_map(|e| e.ok()) {
                let fname = entry.file_name().to_string_lossy().to_string();
                if !is_context_file(&fname) {
                    continue;
                }
                let Ok(data) = std::fs::read_to_string(entry.path()) else {
                    continue;
                };
                let Ok(ctx) = serde_json::from_str::<ContextEntry>(&data) else {
                    continue;
                };
                // `--all` is a cleanup, not an eviction: skip a context another
                // live process holds (the GC's invariant) rather than wipe a
                // running agent. Hold the exclusive liveness lock through disposal
                // so a resolve can't bind it mid-dispose; our OWN context can't be
                // probed (we hold its shared lock), so dispose it directly.
                let _held = if own == Some(ctx.browser_context_id.as_str()) {
                    None
                } else {
                    match try_lock_live(&ctx.name) {
                        Some(h) => Some(h),
                        None => {
                            in_use += 1;
                            continue;
                        }
                    }
                };
                // Best-effort across the set: one context that fails to dispose
                // must not abort the sweep, but its record is kept (a live context
                // with no record would be orphaned in Chrome).
                if dispose_if_live(browser, &ctx.browser_context_id)
                    .await
                    .is_err()
                {
                    failed += 1;
                    continue;
                }
                crate::transport::local::clear_context_state(&ctx.browser_context_id);
                let _ = std::fs::remove_file(entry.path());
                closed += 1;
            }
        }
        let mut msg = format!("Closed {closed} context(s)");
        if in_use > 0 {
            msg.push_str(&format!("; {in_use} kept (in use)"));
        }
        if failed > 0 {
            msg.push_str(&format!("; {failed} kept (failed to dispose — retry)"));
        }
        return Ok(CommandOutput::Ok(msg));
    }

    let Some(name) = name else {
        unreachable!("clap requires a name unless --all is present");
    };

    let file_path = context_file_path(&name);
    let data = std::fs::read_to_string(&file_path)
        .map_err(|_| WebPilotError::ContextNotFound { name: name.clone() })?;
    if let Ok(ctx) = serde_json::from_str::<ContextEntry>(&data) {
        // A named close targets ONE context: if another live process holds it,
        // fail loud rather than evict that agent — the caller stops it first.
        // Closing your own context is always allowed. The exclusive liveness lock
        // is held through disposal (TOCTOU-safe), exactly as the GC sweep does.
        let _held = if own == Some(ctx.browser_context_id.as_str()) {
            None
        } else {
            match try_lock_live(&name) {
                Some(h) => Some(h),
                None => return Err(WebPilotError::ContextInUse { name }.into()),
            }
        };
        dispose_if_live(browser, &ctx.browser_context_id).await?;
        crate::transport::local::clear_context_state(&ctx.browser_context_id);
    }
    let _ = std::fs::remove_file(&file_path);
    Ok(CommandOutput::Ok(format!(
        "Closed context '{}'",
        line_safe(&name)
    )))
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
