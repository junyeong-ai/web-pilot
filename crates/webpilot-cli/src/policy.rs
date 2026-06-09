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
//! The store lives at `policy_dir()/policies.json` — the DURABLE data root, not
//! the evictable cache, so OS cache eviction can never silently reset deny rules
//! to allow. It is the *only* policy state — both privileged sinks read it
//! through [`enforce`], so a `webpilot policy set` rule takes effect identically
//! everywhere. The extension keeps no policy state of its own.
//!
//! Fail closed: a store that exists but can't be read or parsed denies every
//! gated operation rather than silently allowing it. An absent file is the
//! empty (no-policy) state.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use webpilot::WebPilotError;
use webpilot::dirs;
use webpilot::protocol::Command;
use webpilot::types::{PolicyKey, PolicyVerdict};

fn policy_file() -> PathBuf {
    dirs::policy_dir().join("policies.json")
}

/// Enforce policy for a command before it reaches the browser. Returns the
/// typed `PolicyDenied` error when the command's gated operation is set to
/// `deny`, so callers can both propagate it (`?`) and reconstruct the wire
/// envelope from it.
pub fn enforce(command: &Command) -> std::result::Result<(), WebPilotError> {
    match command.policy_key() {
        Some(key) => enforce_key(key),
        None => Ok(()),
    }
}

/// Enforce policy on a bare [`PolicyKey`] — for an effect that doesn't ride the
/// wire `Command` surface and so can't go through [`enforce`]. The headless-only
/// `device` command (raw CDP, not a `Command`) uses this so a `default deny`
/// policy forbids it like any other effect.
pub fn enforce_key(key: PolicyKey) -> std::result::Result<(), WebPilotError> {
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

/// The policy store: a baseline `default` verdict plus per-operation overrides.
/// Lock a tool down with `default = deny` and allowlist only what it needs; the
/// permissive `default = allow` (also the absent-file state) leaves just the
/// explicit denies in force.
#[derive(Clone)]
pub struct PolicyStore {
    pub(crate) default: PolicyVerdict,
    pub(crate) rules: HashMap<PolicyKey, PolicyVerdict>,
}

impl Default for PolicyStore {
    fn default() -> Self {
        Self {
            default: PolicyVerdict::Allow,
            rules: HashMap::new(),
        }
    }
}

impl PolicyStore {
    /// The effective verdict for a key: its explicit rule, else the default.
    fn verdict_for(&self, key: PolicyKey) -> PolicyVerdict {
        self.rules.get(&key).copied().unwrap_or(self.default)
    }
}

/// Enforcement predicate. Fails closed: an unreadable or unparseable store
/// denies rather than allowing.
pub fn denies(key: PolicyKey) -> bool {
    match load() {
        Ok(store) => store.verdict_for(key) == PolicyVerdict::Deny,
        Err(_) => true,
    }
}

/// Load the store. An absent file is the default (permissive) state; any other
/// read failure or a parse failure is surfaced so enforcement and `list` fail
/// closed rather than silently dropping a deny rule.
pub fn load() -> std::io::Result<PolicyStore> {
    match std::fs::read_to_string(policy_file()) {
        Ok(text) => parse(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(PolicyStore::default()),
        Err(e) => Err(e),
    }
}

/// Parse store JSON. All-or-nothing: an unknown field, operation, or verdict
/// makes the whole store untrusted (`Err`) so `denies()` fails closed, rather
/// than silently dropping the bad entry and letting it through.
fn parse(text: &str) -> std::io::Result<PolicyStore> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Raw {
        #[serde(default)]
        default: Option<String>,
        #[serde(default)]
        rules: HashMap<String, String>,
    }
    let invalid = |msg: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, msg.to_owned());
    let raw: Raw = serde_json::from_str(text)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let default = match raw.default {
        Some(d) => d.parse().map_err(|_| invalid("unknown default verdict"))?,
        None => PolicyVerdict::Allow,
    };
    let mut rules = HashMap::with_capacity(raw.rules.len());
    for (k, v) in raw.rules {
        let key: PolicyKey = k.parse().map_err(|_| invalid("unknown operation"))?;
        let verdict: PolicyVerdict = v.parse().map_err(|_| invalid("unknown verdict"))?;
        rules.insert(key, verdict);
    }
    Ok(PolicyStore { default, rules })
}

/// Set one operation's verdict, preserving the rest. A corrupt store is an
/// error (not silently reset), so a single `set` can never erase existing rules.
pub fn set(operation: PolicyKey, verdict: PolicyVerdict) -> Result<()> {
    with_store_lock(|| {
        let mut store = load()?;
        store.rules.insert(operation, verdict);
        write(&store)
    })
}

/// Set the baseline verdict applied to every operation without an explicit rule.
pub fn set_default(verdict: PolicyVerdict) -> Result<()> {
    with_store_lock(|| {
        let mut store = load()?;
        store.default = verdict;
        write(&store)
    })
}

/// Reset to the permissive default with no rules.
pub fn clear() -> Result<()> {
    with_store_lock(|| write(&PolicyStore::default()))
}

/// Run a load→mutate→write critical section under an exclusive advisory lock so
/// two concurrent setters can't both read the same store, mutate different keys,
/// and have the last writer silently drop the other's change — a lost `deny`
/// would leave a gated effect open. The read/enforce path needs no lock: it
/// reads through `atomic_write`'s temp+rename, so it only ever sees a complete
/// old-or-new store, never a torn one.
fn with_store_lock<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    let _lock = crate::lockfile::flock_exclusive(&dirs::policy_dir().join("policies.lock"), false)
        .context("lock policy store")?
        .expect("a blocking flock returns Some");
    f()
    // `_lock` drops here, releasing the flock.
}

fn write(store: &PolicyStore) -> Result<()> {
    // BTreeMap for a stable, diff-friendly key order on disk.
    let rules: std::collections::BTreeMap<String, String> = store
        .rules
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let data = serde_json::to_string_pretty(&serde_json::json!({
        "default": store.default.to_string(),
        "rules": rules,
    }))?;
    // Atomic temp+rename: a concurrent reader (the enforcement path) never
    // observes a torn file, so it cannot fail-closed on a half-written store,
    // and concurrent setters can't interleave bytes.
    webpilot::dirs::atomic_write(&policy_file(), data.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_store_parses() {
        let s = parse(r#"{"rules":{"click":"deny","eval":"allow"}}"#).unwrap();
        assert_eq!(s.rules.len(), 2);
        assert_eq!(s.default, PolicyVerdict::Allow);
    }

    #[test]
    fn empty_store_is_permissive() {
        let s = parse("{}").unwrap();
        assert!(s.rules.is_empty());
        assert_eq!(s.default, PolicyVerdict::Allow);
    }

    #[test]
    fn default_deny_denies_unset_operations_and_allowlists_explicit_ones() {
        let s = parse(r#"{"default":"deny","rules":{"click":"allow"}}"#).unwrap();
        assert_eq!(s.verdict_for(PolicyKey::Click), PolicyVerdict::Allow);
        assert_eq!(s.verdict_for(PolicyKey::Eval), PolicyVerdict::Deny);
    }

    #[test]
    fn malformed_json_is_error() {
        assert!(parse("{not json").is_err());
    }

    #[test]
    fn unknown_operation_is_error() {
        assert!(parse(r#"{"rules":{"teleport":"deny"}}"#).is_err());
    }

    #[test]
    fn unknown_verdict_is_error() {
        assert!(parse(r#"{"rules":{"click":"maybe"}}"#).is_err());
    }

    #[test]
    fn unknown_default_is_error() {
        assert!(parse(r#"{"default":"maybe"}"#).is_err());
    }

    #[test]
    fn unknown_top_level_field_is_rejected() {
        // A tampered/garbled store must fail closed, not partially apply.
        assert!(parse(r#"{"rules":{},"backdoor":true}"#).is_err());
    }

    #[test]
    fn extended_keys_parse() {
        // Every non-action key added to PolicyKey must round-trip through the
        // on-disk store (Display/FromStr via serde_plain).
        let s = parse(
            r#"{"rules":{"eval":"deny","fetch":"deny","session_export":"deny","cookie_list":"deny","cookie_set":"deny","cookie_delete":"deny","dom_set":"deny","tab_close":"deny","session_import":"allow","device":"deny"}}"#,
        )
        .unwrap();
        assert_eq!(s.rules.len(), 10);
        // `device` (headless-only emulation gate) must resolve like any other key.
        assert_eq!(s.verdict_for(PolicyKey::Device), PolicyVerdict::Deny);
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
