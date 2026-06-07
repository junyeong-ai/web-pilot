//! Parity backstop for the one boundary the compiler can't see.
//!
//! Adding a command is compiler-forced on the Rust side (every `match` over
//! `protocol::Command` is exhaustive), but the browser-mode service worker is
//! plain JavaScript: a new command can compile, pass headless tests, and still
//! fall into the worker's `Unknown command` arm. This test closes that gap by
//! asserting every wire `Command` variant has a service-worker router case —
//! so a forgotten browser handler fails the build, not a user in browser mode.
//!
//! It parses source on purpose: the `Command` enum is `#[serde(tag = "type")]`
//! with no rename, so each variant name IS its wire tag, and the worker
//! switches on exactly those tags.

use std::collections::BTreeSet;

const PROTOCOL: &str = include_str!("../../webpilot/src/protocol.rs");
const SERVICE_WORKER: &str = include_str!("../../../extension/background/service-worker.js");

/// Variant names of `pub enum Command` — its wire tags.
fn command_wire_tags() -> BTreeSet<String> {
    let start = PROTOCOL
        .find("pub enum Command")
        .expect("Command enum present");
    let block = &PROTOCOL[start..];
    let open = block.find('{').expect("enum body opens");

    // Walk to the brace that closes the enum, tracking nesting so struct-variant
    // field braces don't end the scan early.
    let mut depth = 0usize;
    let mut close = open;
    for (i, b) in block[open..].bytes().enumerate() {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    close = open + i;
                    break;
                }
            }
            _ => {}
        }
    }

    let mut tags = BTreeSet::new();
    for line in block[open + 1..close].lines() {
        let t = line.trim_start();
        // Skip doc comments, attributes, and struct-variant field lines (which
        // start lowercase). A variant starts with an uppercase identifier.
        if t.starts_with("//") || t.starts_with('#') || t.starts_with('*') || t.starts_with("/*")
        {
            continue;
        }
        let ident: String = t
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if ident
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase())
        {
            tags.insert(ident);
        }
    }
    tags
}

/// Every `case "Tag":` the service worker handles. The command router cases are
/// PascalCase wire tags; lowercase bridge-style cases live in bridge.js, not
/// here, so an uppercase-initial filter isolates the router.
fn service_worker_cases() -> BTreeSet<String> {
    let mut cases = BTreeSet::new();
    for (i, _) in SERVICE_WORKER.match_indices("case \"") {
        let rest = &SERVICE_WORKER[i + 6..];
        if let Some(end) = rest.find('"') {
            let name = &rest[..end];
            if name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                cases.insert(name.to_string());
            }
        }
    }
    cases
}

#[test]
fn every_command_has_a_service_worker_handler() {
    let tags = command_wire_tags();
    // Sanity: the parser actually found the enum, not an empty set.
    assert!(
        tags.contains("Capture") && tags.contains("Action") && tags.contains("TabClose"),
        "command tag parser is broken — found: {tags:?}"
    );

    let cases = service_worker_cases();
    let missing: Vec<&String> = tags.difference(&cases).collect();
    assert!(
        missing.is_empty(),
        "service-worker.js has no router case for these wire Command tags \
         (browser mode would answer them with `Unknown command`): {missing:?}",
    );
}
