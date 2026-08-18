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
    /// `cookie_delete`, `session_export`, `session_import`, `device` (emulation:
    /// viewport + user-agent spoofing), `context_close` (`context close [--all]`
    /// disposes a context and all its tabs), `download` (a file the page makes
    /// the browser write — a `deny` refuses the transfer itself). `navigate` also
    /// covers `capture --url` and `tab new URL`.
    Set {
        #[arg(long)]
        operation: String,
        #[arg(long)]
        verdict: String,
    },
    /// Set the baseline verdict for operations without an explicit rule. Use
    /// `deny` to lock the tool down, then `set` to allowlist what it needs.
    Default {
        /// `allow` or `deny`.
        verdict: String,
    },
    /// List the default verdict and all per-operation rules.
    List,
    /// Reset to the permissive default with no rules.
    Clear,
}

fn parse_verdict(verdict: &str) -> Result<PolicyVerdict> {
    verdict.parse().map_err(|_| {
        WebPilotError::InvalidArgument {
            detail: format!("unknown verdict '{verdict}' (use 'allow' or 'deny')"),
        }
        .into()
    })
}

pub fn run(args: PolicyArgs) -> Result<CommandOutput> {
    match args.command {
        PolicyCommand::Set { operation, verdict } => {
            let operation: PolicyKey = operation.parse().map_err(|_| {
                // Build the valid-operation list from `PolicyKey::ALL` so it can
                // never drift from the enum (a hand-written list silently omitted
                // `device` and `context_close`).
                let valid = PolicyKey::ALL
                    .iter()
                    .map(|k| k.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                WebPilotError::InvalidArgument {
                    detail: format!("unknown operation '{operation}' — valid: {valid}"),
                }
            })?;
            policy::set(operation, parse_verdict(&verdict)?)?;
            Ok(CommandOutput::Ok("OK".into()))
        }
        PolicyCommand::Default { verdict } => {
            policy::set_default(parse_verdict(&verdict)?)?;
            Ok(CommandOutput::Ok("OK".into()))
        }
        PolicyCommand::List => {
            // Surface a corrupt store as an error so `list` agrees with
            // enforcement (which denies on corruption) instead of misreporting
            // the policy while everything is in fact denied.
            let store = policy::load().map_err(|_| WebPilotError::Other {
                detail: "policy store is invalid; run: webpilot policy clear".into(),
            })?;
            let mut rules: Vec<(String, String)> = store
                .rules
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            rules.sort();
            let mut human_lines = vec![format!("default: {}", store.default)];
            human_lines.extend(rules.iter().map(|(op, verdict)| format!("{op}: {verdict}")));
            let items = serde_json::json!({
                "default": store.default.to_string(),
                "rules": rules
                    .iter()
                    .map(|(operation, verdict)| {
                        serde_json::json!({ "operation": operation, "verdict": verdict })
                    })
                    .collect::<Vec<_>>(),
            });
            Ok(CommandOutput::List {
                items,
                human_lines,
                summary: String::new(),
            })
        }
        PolicyCommand::Clear => {
            policy::clear()?;
            Ok(CommandOutput::Ok("OK".into()))
        }
    }
}
