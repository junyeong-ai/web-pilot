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
const ACTION: &str = include_str!("../../webpilot/src/action.rs");
const BRIDGE: &str = include_str!("../../../extension/content/bridge.js");

/// Concatenated source of every background module. The worker is an ES-module
/// graph, so the router (and any future split) can live in any file under
/// `background/` — scanning the directory keeps this test honest across
/// refactors instead of being pinned to one filename.
fn service_worker_source() -> String {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extension/background");
    let mut src = String::new();
    for entry in std::fs::read_dir(&dir).expect("extension/background exists") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) == Some("js") {
            src.push_str(&std::fs::read_to_string(&path).expect("readable module"));
            src.push('\n');
        }
    }
    assert!(!src.is_empty(), "no background modules found in {dir:?}");
    src
}

/// Variant names of a `pub enum <name>` block in `source` — brace-matched so
/// struct-variant field braces don't end the scan early, doc/attr/field lines
/// skipped (a variant starts with an uppercase identifier).
fn enum_variants(source: &str, name: &str) -> BTreeSet<String> {
    // The opening brace is part of the anchor so `Action` can never match the
    // longer `ActionKind` (rustfmt keeps the brace on the same line).
    let anchor = format!("pub enum {name} {{");
    let start = source
        .find(&anchor)
        .unwrap_or_else(|| panic!("{name} enum present"));
    let block = &source[start..];
    let open = block.find('{').expect("enum body opens");

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
        if t.starts_with("//") || t.starts_with('#') || t.starts_with('*') || t.starts_with("/*") {
            continue;
        }
        let ident: String = t
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if ident.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            tags.insert(ident);
        }
    }
    tags
}

/// Variant names of `pub enum Command` — its wire tags.
fn command_wire_tags() -> BTreeSet<String> {
    enum_variants(PROTOCOL, "Command")
}

/// The wire `kind` of every `Action` variant: `serde(rename_all =
/// "snake_case")` over the variant name, reproduced here (`KeyPress` →
/// `key_press`) so the set can be compared with the bridge's `case "kind"`
/// strings.
fn action_wire_kinds() -> BTreeSet<String> {
    enum_variants(ACTION, "Action")
        .into_iter()
        .map(|v| {
            let mut s = String::new();
            for (i, c) in v.chars().enumerate() {
                if c.is_ascii_uppercase() {
                    if i > 0 {
                        s.push('_');
                    }
                    s.push(c.to_ascii_lowercase());
                } else {
                    s.push(c);
                }
            }
            s
        })
        .collect()
}

/// Every `case "kind":` inside the bridge's `switch (action.kind)` body — the
/// content-script twin of `service_worker_cases`, scoped by the same
/// brace-matching so an unrelated switch can't inflate the set.
fn bridge_action_cases() -> BTreeSet<String> {
    let anchor = "switch (action.kind)";
    let start = BRIDGE
        .find(anchor)
        .expect("the bridge's `switch (action.kind)` exists");
    let block = &BRIDGE[start..];
    let open = block.find('{').expect("switch body opens");
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
    let body = &block[open..close];

    let mut cases = BTreeSet::new();
    for (i, _) in body.match_indices("case \"") {
        let rest = &body[i + 6..];
        if let Some(end) = rest.find('"') {
            cases.insert(rest[..end].to_string());
        }
    }
    cases
}

/// Every `case "Tag":` inside the command router's `switch (command.type)`
/// body — and ONLY there. Scoping to that one switch (located by its
/// discriminant, brace-matched to its close) keeps a future unrelated switch
/// from inflating the registered set and masking a genuinely missing arm.
fn service_worker_cases() -> BTreeSet<String> {
    let source = service_worker_source();
    let anchor = "switch (command.type)";
    let start = source
        .find(anchor)
        .expect("the command router's `switch (command.type)` exists");
    let block = &source[start..];
    let open = block.find('{').expect("switch body opens");
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
    let body = &block[open..close];

    let mut cases = BTreeSet::new();
    for (i, _) in body.match_indices("case \"") {
        let rest = &body[i + 6..];
        if let Some(end) = rest.find('"') {
            cases.insert(rest[..end].to_string());
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
    // And the reverse: a router case naming no wire Command is dead code — a
    // removed/renamed command whose JS arm was forgotten, which would silently
    // rot (and could shadow a future variant of the same name with stale
    // behaviour). The two sets must be equal, not merely one-way covered.
    let dead: Vec<&String> = cases.difference(&tags).collect();
    assert!(
        dead.is_empty(),
        "the service-worker router has cases for tags that are not wire Command \
         variants (dead arms — remove them or re-add the variant): {dead:?}",
    );
}

#[test]
fn every_action_kind_has_a_bridge_case() {
    let kinds = action_wire_kinds();
    // Sanity: the parser found the Action enum and the snake_case mapping works.
    assert!(
        kinds.contains("click") && kinds.contains("key_press") && kinds.contains("scroll_to"),
        "action kind parser is broken — found: {kinds:?}"
    );

    // The bridge's `executeAction` switch handles EVERY kind explicitly: page
    // actions run, CDP-native kinds (navigate/drag/hover/key_press/upload) are
    // explicit mis-route rejections. So a new `Action` variant must add a case
    // either way — making this the action-level twin of the Command/router
    // check above: the one boundary the compiler can't see, closed at build
    // time instead of failing at runtime in whichever mode hits it first.
    let cases = bridge_action_cases();
    let missing: Vec<&String> = kinds.difference(&cases).collect();
    assert!(
        missing.is_empty(),
        "bridge.js executeAction has no case for these Action kinds (the action \
         would fall through to the unknown-kind arm at runtime): {missing:?}",
    );
    let dead: Vec<&String> = cases.difference(&kinds).collect();
    assert!(
        dead.is_empty(),
        "bridge.js executeAction has cases that are not Action kinds (dead arms \
         or typos — the Rust side can never send them): {dead:?}",
    );
}
