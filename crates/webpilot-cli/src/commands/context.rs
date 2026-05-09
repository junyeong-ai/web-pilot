use anyhow::Result;
use clap::{Args, Subcommand};
use webpilot::WebPilotError;
use webpilot::dirs;

use crate::output::CommandOutput;
use crate::transport::LocalTransport;
use crate::transport::local_context::{ContextEntry, context_file_path};

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
            if !fname.starts_with("ctx-") || !fname.ends_with(".json") {
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
        if let Ok(entries) = std::fs::read_dir(dirs::contexts_dir()) {
            for entry in entries.filter_map(|e| e.ok()) {
                let fname = entry.file_name().to_string_lossy().to_string();
                if !fname.starts_with("ctx-") || !fname.ends_with(".json") {
                    continue;
                }
                if let Ok(data) = std::fs::read_to_string(entry.path())
                    && let Ok(ctx) = serde_json::from_str::<ContextEntry>(&data)
                {
                    let _ = browser
                        .dispose_browser_context(&ctx.browser_context_id)
                        .await;
                }
                let _ = std::fs::remove_file(entry.path());
                count += 1;
            }
        }
        return Ok(CommandOutput::Ok(format!("Closed {count} context(s)")));
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
        let _ = browser
            .dispose_browser_context(&ctx.browser_context_id)
            .await;
    }
    let _ = std::fs::remove_file(&file_path);
    Ok(CommandOutput::Ok(format!("Closed context '{name}'")))
}
