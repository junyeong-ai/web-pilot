//! Operation policy: a single file-backed allow/deny store, enforced at the
//! boundary where a command crosses into a real browser.
//!
//! That boundary is *not* the same in both modes. In headless mode the CLI
//! process drives Chrome directly, so [`enforce`] runs in `LocalTransport::send`.
//! In browser mode the CLI only writes to a Unix socket — the Native Messaging
//! **host** is the process that reaches the user's authenticated Chrome, so the
//! host calls [`enforce`] before forwarding a request to the extension.
//! Enforcing in the host (not the CLI-side `IpcTransport`) is what makes a deny
//! rule a real boundary: another local process writing raw JSON to the socket
//! still hits it.
//!
//! The store lives at `artifacts_dir()/policies.json` and is the *only* policy
//! state — both privileged sinks read it through [`enforce`], so a `webpilot
//! policy set` rule takes effect identically everywhere. The extension keeps no
//! policy state of its own.
//!
//! Fail closed: a store that exists but can't be read or parsed denies every
//! gated operation rather than silently allowing it. An absent file is the
//! empty (no-policy) state.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use webpilot::WebPilotError;
use webpilot::dirs;
use webpilot::protocol::Command;
use webpilot::types::{PolicyKey, PolicyVerdict};

fn policy_file() -> PathBuf {
    dirs::artifacts_dir().join("policies.json")
}

/// Enforce policy for a command before it reaches the browser. Returns the
/// typed `PolicyDenied` error when the command's gated operation is set to
/// `deny`, so callers can both propagate it (`?`) and reconstruct the wire
/// envelope from it.
pub fn enforce(command: &Command) -> std::result::Result<(), WebPilotError> {
    let Some(key) = command.policy_key() else {
        return Ok(());
    };
    if denies(key) {
        return Err(WebPilotError::PolicyDenied {
            operation: key.to_string(),
        });
    }
    Ok(())
}

/// Validate a wire `command` value as a typed [`Command`] and enforce policy on
/// it. This is the host's gate for socket traffic: a value that does not
/// deserialize is `InvalidArgument` (rejected, never forwarded), so a payload
/// the strict Rust types reject but the loose JS bridge would coerce — e.g. a
/// string where a `u32` index is required — cannot slip past a deny rule. A
/// gated-and-denied command is `PolicyDenied`. On success the parsed command is
/// returned (the original wire value is forwarded unchanged by the caller).
pub fn parse_and_enforce(
    command: &serde_json::Value,
) -> std::result::Result<Command, WebPilotError> {
    let command: Command =
        serde_json::from_value(command.clone()).map_err(|e| WebPilotError::InvalidArgument {
            detail: format!("malformed command: {e}"),
        })?;
    enforce(&command)?;
    Ok(command)
}

/// Enforcement predicate. Fails closed: an unreadable or unparseable store
/// denies rather than allowing.
pub fn denies(key: PolicyKey) -> bool {
    match load() {
        Ok(store) => store.get(&key) == Some(&PolicyVerdict::Deny),
        Err(_) => true,
    }
}

/// Load the store. An absent file is the empty state; any other read failure or
/// a parse failure is surfaced so enforcement and `list` fail closed rather
/// than silently dropping a deny rule.
pub fn load() -> std::io::Result<HashMap<PolicyKey, PolicyVerdict>> {
    match std::fs::read_to_string(policy_file()) {
        Ok(text) => parse(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
        Err(e) => Err(e),
    }
}

/// Parse store JSON. All-or-nothing: an unknown operation or verdict makes the
/// whole store untrusted (`Err`) so `denies()` fails closed, rather than
/// silently dropping the bad entry and letting it through.
fn parse(text: &str) -> std::io::Result<HashMap<PolicyKey, PolicyVerdict>> {
    let invalid = |msg: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, msg.to_owned());
    let raw: HashMap<String, String> = serde_json::from_str(text)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut store = HashMap::with_capacity(raw.len());
    for (k, v) in raw {
        let key: PolicyKey = k.parse().map_err(|_| invalid("unknown operation"))?;
        let verdict: PolicyVerdict = v.parse().map_err(|_| invalid("unknown verdict"))?;
        store.insert(key, verdict);
    }
    Ok(store)
}

/// Set one operation's verdict, preserving the rest. A corrupt store is
/// overwritten cleanly rather than written back as junk.
pub fn set(operation: PolicyKey, verdict: PolicyVerdict) -> Result<()> {
    let mut store = load().unwrap_or_default();
    store.insert(operation, verdict);
    write(&store)
}

/// Clear every policy.
pub fn clear() -> Result<()> {
    write(&HashMap::new())
}

fn write(store: &HashMap<PolicyKey, PolicyVerdict>) -> Result<()> {
    let raw: HashMap<String, String> = store
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let data = serde_json::to_string_pretty(&raw)?;
    // Write to a per-process temp file then rename: a concurrent reader (the
    // enforcement path) never observes a torn file, so it cannot fail-closed on
    // a half-written store, and concurrent setters can't interleave bytes.
    let path = policy_file();
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_store_parses() {
        assert_eq!(
            parse(r#"{"click":"deny","eval":"allow"}"#).unwrap().len(),
            2
        );
    }

    #[test]
    fn empty_store_is_ok() {
        assert!(parse("{}").unwrap().is_empty());
    }

    #[test]
    fn malformed_json_is_error() {
        assert!(parse("{not json").is_err());
    }

    #[test]
    fn unknown_operation_is_error() {
        assert!(parse(r#"{"teleport":"deny"}"#).is_err());
    }

    #[test]
    fn unknown_verdict_is_error() {
        assert!(parse(r#"{"click":"maybe"}"#).is_err());
    }

    #[test]
    fn extended_keys_parse() {
        // Every non-action key added to PolicyKey must round-trip through the
        // on-disk store (Display/FromStr via serde_plain).
        let s = parse(
            r#"{"session_export":"deny","cookie_list":"deny","cookie_set":"deny","dom_set":"deny","tab_close":"deny","session_import":"allow"}"#,
        )
        .unwrap();
        assert_eq!(s.len(), 6);
    }

    // `parse_and_enforce` is the host's security gate for raw socket traffic.
    // The parse half is pure (no store), so it is unit-tested directly here;
    // the enforce half is covered by `parse`/`denies` above and the headless
    // E2E policy-deny scenario.
    #[test]
    fn parse_and_enforce_rejects_type_mismatch() {
        // A string index is what the loose JS bridge would coerce-and-run; the
        // strict typed parse must reject it as InvalidArgument so it is never
        // forwarded to the browser.
        let bad = serde_json::json!({"type": "Action", "action": {"kind": "click", "index": "1"}});
        let err = parse_and_enforce(&bad).expect_err("string index must be rejected");
        assert!(matches!(err, WebPilotError::InvalidArgument { .. }));
        assert_eq!(err.exit_code(), 7);
    }

    #[test]
    fn parse_and_enforce_accepts_well_formed_ungated_command() {
        // A read-only command has no policy key, so enforcement short-circuits
        // without touching the store (keeping this assertion store-independent)
        // and the parsed command is returned for forwarding.
        let ok = serde_json::json!({"type": "Status"});
        let cmd = parse_and_enforce(&ok).expect("status is valid and ungated");
        assert!(cmd.policy_key().is_none());
    }
}
