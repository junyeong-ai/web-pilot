//! `webpilot policy` — manage the local operation-policy store.
//!
//! Policies are pure local configuration (a single JSON file) read identically
//! by both transport modes at enforcement time, so these subcommands never
//! touch the browser — they edit the file directly.

use anyhow::Result;
use clap::{Args, Subcommand};
use webpilot::WebPilotError;
use webpilot::types::{PolicyKey, PolicyVerdict};

use crate::output::CommandOutput;
use crate::policy;

#[derive(Args)]
pub struct PolicyArgs {
    #[command(subcommand)]
    pub command: PolicyCommand,
}

#[derive(Subcommand)]
pub enum PolicyCommand {
    /// Set a safety policy for an operation. Keys gate by effect: any action
    /// kind, plus `eval` (also covers `console`/`network start` injection and
    /// the `frame find` predicate), `fetch`, `dom_set`, `tab_close`,
    /// `cookie_list` (covers `cookie list` and `cookie get`), `cookie_set`,
    /// `cookie_delete`, `session_export`, `session_import`. `navigate` also
    /// covers `capture --url` and `tab new URL`.
    Set {
        #[arg(long)]
        operation: String,
        #[arg(long)]
        verdict: String,
    },
    /// List all policies.
    List,
    /// Clear all policies.
    Clear,
}

pub fn run(args: PolicyArgs) -> Result<CommandOutput> {
    match args.command {
        PolicyCommand::Set { operation, verdict } => {
            let operation: PolicyKey =
                operation
                    .parse()
                    .map_err(|_| WebPilotError::InvalidArgument {
                        detail: format!("unknown operation '{operation}'"),
                    })?;
            let verdict: PolicyVerdict =
                verdict
                    .parse()
                    .map_err(|_| WebPilotError::InvalidArgument {
                        detail: format!("unknown verdict '{verdict}' (use 'allow' or 'deny')"),
                    })?;
            policy::set(operation, verdict)?;
            Ok(CommandOutput::Ok("OK".into()))
        }
        PolicyCommand::List => {
            // Surface a corrupt store as an error so `list` agrees with
            // enforcement (which denies on corruption) instead of misreporting
            // "no policies" while everything is in fact denied.
            let store = policy::load().map_err(|_| WebPilotError::Other {
                detail: "policy store is invalid; run: webpilot policy clear".into(),
            })?;
            let mut entries: Vec<(String, String)> = store
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            entries.sort();
            let human_lines: Vec<String> = entries
                .iter()
                .map(|(op, verdict)| format!("{op}: {verdict}"))
                .collect();
            let summary = if entries.is_empty() {
                "No policies set".into()
            } else {
                String::new()
            };
            let items = serde_json::to_value(
                entries
                    .iter()
                    .map(|(operation, verdict)| {
                        serde_json::json!({ "operation": operation, "verdict": verdict })
                    })
                    .collect::<Vec<_>>(),
            )?;
            Ok(CommandOutput::List {
                items,
                human_lines,
                summary,
            })
        }
        PolicyCommand::Clear => {
            policy::clear()?;
            Ok(CommandOutput::Ok("OK".into()))
        }
    }
}
