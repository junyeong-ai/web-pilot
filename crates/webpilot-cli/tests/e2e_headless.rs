//! Behavioral end-to-end tests: drive the real `webpilot` binary against a
//! local fixture server in headless mode, asserting on exit codes and output.
//!
//! These launch a real Chrome, so they are opt-in: set `WEBPILOT_E2E=1` to run
//! them. CI's `e2e` job provisions Chrome (via `WEBPILOT_CHROME`) and sets the
//! flag; a plain `cargo test` skips them so it stays green on machines without
//! a browser.
//!
//!   WEBPILOT_E2E=1 cargo test -p webpilot-cli --test e2e_headless -- --nocapture
//!
//! The whole flow runs as one test because it owns a single headless session
//! (keyed by an isolated `WEBPILOT_HOME`) and tears it down at the end.

mod common;

use std::path::PathBuf;
use std::process::{Command, Output};

use common::{code, spawn_server, stdout};

const BIN: &str = env!("CARGO_BIN_EXE_webpilot");

/// The captured index of the element with the given DOM id, as a CLI argument.
fn index_of(cap: &Output, id: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(&stdout(cap)).expect("capture json");
    v["elements"]
        .as_array()
        .expect("elements array")
        .iter()
        .find(|e| e["id"] == id)
        .and_then(|e| e["index"].as_u64())
        .unwrap_or_else(|| panic!("element #{id} not captured: {}", stdout(cap)))
        .to_string()
}

/// A downloaded file's contents once the transfer has landed. The report names
/// the path the moment the download STARTS, so a read taken immediately after
/// the command can beat the bytes — poll rather than sleep, and fail loudly on a
/// path that never fills in.
fn settled_file(path: &PathBuf) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Ok(body) = std::fs::read_to_string(path)
            && !body.is_empty()
        {
            return body;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "downloaded file never landed at {path:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

struct Fixture {
    home: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Best-effort teardown of the headless session this run launched.
        let _ = Command::new(BIN)
            .arg("quit")
            .env("WEBPILOT_HOME", &self.home)
            .output();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

impl Fixture {
    /// Run `webpilot <args>` against this isolated session, capturing output.
    fn run(&self, args: &[&str]) -> Output {
        self.run_env(args, &[])
    }

    /// As `run`, but with extra environment variables — used to exercise
    /// settings-dependent behaviour (e.g. a low CDP-send timeout) deterministically.
    fn run_env(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(BIN);
        cmd.args(args).env("WEBPILOT_HOME", &self.home);
        for (k, v) in env {
            cmd.env(k, v);
        }
        // Force JSON regardless of how the test harness wires stdio.
        cmd.arg("--json").output().expect("spawn webpilot")
    }
}

#[test]
fn headless_behavioral_flow() {
    if std::env::var("WEBPILOT_E2E").is_err() {
        eprintln!("skipping e2e (set WEBPILOT_E2E=1 to run)");
        return;
    }

    let base = spawn_server();
    let home = std::env::temp_dir().join(format!("webpilot-e2e-{}", std::process::id()));
    let fx = Fixture { home: home.clone() };

    // 0. A zero CDP-send timeout would make every request expire instantly —
    //    settings validation must refuse it loudly at startup (InvalidArgument),
    //    not degrade to a session that times out on the first command.
    let bad_cfg = fx.run_env(&["status"], &[("WEBPILOT_CDP_SEND_TIMEOUT_MS", "0")]);
    assert_eq!(
        code(&bad_cfg),
        7,
        "a zero cdp_send timeout must be refused at startup: {}",
        stdout(&bad_cfg)
    );
    // The same guard covers every deadline that breaks at zero, not just
    // cdp_send: a zero IPC-reply or Chrome-launch timeout would also fail the
    // session instantly, so validation must reject each one up front.
    for var in [
        "WEBPILOT_IPC_TIMEOUT_MS",
        "WEBPILOT_CHROME_LAUNCH_TIMEOUT_MS",
    ] {
        let bad = fx.run_env(&["status"], &[(var, "0")]);
        assert_eq!(
            code(&bad),
            7,
            "a zero {var} must be refused at startup: {}",
            stdout(&bad)
        );
    }

    // 1. Capture the page: button + input are indexed, the iframe is surfaced
    //    as an out-of-scope subframe (capture is main-frame scoped).
    let cap = fx.run(&["capture", "--include", "dom", "--url", &base]);
    assert_eq!(code(&cap), 0, "capture failed: {}", stdout(&cap));
    let dom = stdout(&cap);
    let snapshot: serde_json::Value = serde_json::from_str(&dom).expect("capture json");
    let elements = snapshot["elements"].as_array().expect("elements array");
    assert!(
        elements.iter().any(|e| e["tag"] == "button"),
        "button must be indexed: {dom}"
    );
    assert!(
        elements.iter().any(|e| e["tag"] == "input"),
        "input must be indexed: {dom}"
    );
    assert_eq!(
        snapshot["subframes"], 2,
        "from the main frame, subframes counts every nested http iframe — the /frame iframe and the /nested iframe inside it: {dom}"
    );
    // A cursor:pointer wrapper carrying no semantic tag/role/marker is a click
    // target only on the INNERMOST such element — but a hidden (`display:none`)
    // interactive descendant, dropped from the snapshot, must not shadow it as a
    // mere wrapper, or a real click target goes unaddressable.
    assert!(
        elements.iter().any(|e| e["id"] == "cardwrap"),
        "a cursor:pointer wrapper with only a hidden interactive child must stay indexed: {dom}"
    );
    // The INVERSE containment direction: `button{cursor:pointer}` (near-universal)
    // makes inner label/icon spans INHERIT the pointer cursor. The cursor:pointer
    // pass must NOT emit such a child — it is contained by an already-collected
    // interactive node (the button), so clicking it just clicks the button. The
    // button is indexed; its presentational `#ptrbtnlabel` span must NOT be, or
    // every styled button/link mints a phantom duplicate of itself.
    assert!(
        elements.iter().any(|e| e["id"] == "ptrbtn"),
        "a cursor:pointer button must be indexed: {dom}"
    );
    assert!(
        !elements.iter().any(|e| e["id"] == "ptrbtnlabel"),
        "a presentational child that only INHERITS its interactive ancestor's cursor:pointer must NOT be indexed (no phantom duplicate): {dom}"
    );
    // The redundancy guard must NOT over-fire: a VISIBLE cursor:pointer child of
    // a collected-but-INVISIBLE semantic ancestor (a `visibility:hidden` <a href>
    // wrapping a `visibility:visible` child) must STAY indexed. The ancestor is
    // in `seen` (the semantic pass has no visibility gate) but dropped from the
    // snapshot; without the `isVisible(c)` guard on the inner-containment branch
    // it dragged the real, clickable child down with it — unaddressable.
    assert!(
        elements.iter().any(|e| e["id"] == "vischild"),
        "a visible cursor:pointer child of a visibility:hidden interactive wrapper must stay indexed: {dom}"
    );
    assert!(
        !elements.iter().any(|e| e["id"] == "hiddenchild"),
        "a display:none element must never be indexed: {dom}"
    );
    // An explicit `draggable="true"` is an interaction affordance: the `drag`
    // action addresses elements by index, so a declared drag source must be
    // capturable even with no other marker/semantic tag.
    assert!(
        elements.iter().any(|e| e["id"] == "dragme"),
        "a draggable=\"true\" element must be indexed (it is the drag action's addressee): {dom}"
    );
    // A `role="presentation"` element carrying a click marker is a real click
    // target: ARIA none/presentation STRIP the implicit role, so the marker
    // heuristic must treat it like a plain div, not skip it as a semantic control.
    assert!(
        elements.iter().any(|e| e["id"] == "presdiv"),
        "a role=presentation element with onclick must be indexed (none/presentation = no role): {dom}"
    );
    // A role the semantic allowlist does NOT collect must NOT veto an affordance
    // the element ALSO carries. The marker/cursor passes used to skip any element
    // with an explicit role, which dropped every non-allowlisted WIDGET role
    // (`option`/`treeitem`/`gridcell`/…), a clickable structural card, and an
    // invalid role — so a custom listbox/combobox/menu/tree returned its
    // container but none of its items. Each of these carries a real affordance
    // (onclick, roving tabindex) and must be indexed; the element with the
    // STRONGER signal (a role AND a marker) must never vanish while a bare
    // `<div onclick>` is kept.
    for id in ["ariaopt", "ariaoptrove", "ariatreeitem", "invalidrole"] {
        assert!(
            elements.iter().any(|e| e["id"] == id),
            "a non-allowlisted role with an explicit affordance must be indexed (#{id} absent): {dom}"
        );
    }
    // A standalone ARIA widget role is collected on the role ALONE — no
    // onclick/tabindex/cursor:pointer needed. `#kbopt` (a `role=option` in an
    // `aria-activedescendant` listbox) and `#kbspin` (a `role=spinbutton`) carry
    // NO affordance, the keyboard-driven widget pattern; the semantic pass must
    // still index them, or the agent sees the listbox container but cannot pick
    // an item. (Their `[role~="…"]` collection is the complete widget taxonomy,
    // not the old 11-role subset.)
    for id in ["kbopt", "kbspin"] {
        assert!(
            elements.iter().any(|e| e["id"] == id),
            "a standalone ARIA widget role with NO affordance must be indexed by the semantic pass (#{id} absent): {dom}"
        );
    }
    // `in_viewport:false` must SURVIVE the wire (the bridge's false-strip
    // keep-list): it is the offscreen signal — `[offscreen]` in the rendered
    // DOM, and the annotation overlay skips it — not a mere property absence.
    // `#deepbtn` sits below a 3000px spacer, so the fresh capture must carry
    // the flag explicitly false.
    let deep = elements
        .iter()
        .find(|e| e["id"] == "deepbtn")
        .expect("the below-fold #deepbtn must be indexed");
    assert_eq!(
        deep["in_viewport"],
        serde_json::Value::Bool(false),
        "a below-fold element must carry in_viewport:false on the wire (offscreen signal): {dom}"
    );

    let button_index = elements
        .iter()
        .find(|e| e["tag"] == "button")
        .and_then(|e| e["index"].as_u64())
        .expect("button index") as u32;

    // 1b. `--include text` must include text inside open shadow roots, like the
    //     DOM snapshot does — `innerText` alone stops at the shadow boundary and
    //     would silently drop a web component's own prose with no truncated
    //     signal. The shadow root carries "shadowonlyprose" (no slot), so the text
    //     dump must surface it without double-counting any slotted content.
    let text_cap = fx.run(&["capture", "--include", "text"]);
    assert_eq!(
        code(&text_cap),
        0,
        "text capture failed: {}",
        stdout(&text_cap)
    );
    let text_json: serde_json::Value =
        serde_json::from_str(&stdout(&text_cap)).expect("text capture json");
    assert!(
        text_json["text_content"]
            .as_str()
            .is_some_and(|t| t.contains("shadowonlyprose")),
        "capture --include text must include open-shadow-root text, not just light innerText: {}",
        stdout(&text_cap)
    );
    // An UNASSIGNED slot renders its FALLBACK children from the shadow side —
    // text the base innerText never sees and an assigned-slot skip must not
    // drop. The assigned slot's light content ("SL") must still appear exactly
    // once (no double-count).
    let tx = text_json["text_content"].as_str().unwrap_or("");
    assert!(
        tx.contains("slotfallbackprose"),
        "slot FALLBACK text must reach the text capture: {tx}"
    );
    assert_eq!(
        tx.matches("SL").count(),
        1,
        "assigned slotted content must appear exactly once (no double-count): {tx}"
    );

    // 1c. `wait --until text` matches like `find --text` / the text capture:
    //     case-insensitive and shadow-piercing. "go" must match the "Go" button
    //     (case), and the shadow-only "shadowonlyprose" must match via
    //     "SHADOWONLYPROSE" (case + shadow) — raw innerText alone misses both.
    assert_eq!(
        code(&fx.run(&["wait", "--timeout", "3", "text", "go"])),
        0,
        "wait text must be case-insensitive (match 'Go' for 'go'): {}",
        stdout(&fx.run(&["wait", "--timeout", "3", "text", "go"]))
    );
    assert_eq!(
        code(&fx.run(&["wait", "--timeout", "3", "text", "SHADOWONLYPROSE"])),
        0,
        "wait text must pierce open shadow roots (match shadow-only 'shadowonlyprose'): {}",
        stdout(&fx.run(&["wait", "--timeout", "3", "text", "SHADOWONLYPROSE"]))
    );
    // ...and collapse whitespace like find --text: <p>whitespace<br>collapse<br>
    // marker</p> has innerText with newlines, so "whitespace collapse marker"
    // (single spaces) must still match.
    assert_eq!(
        code(&fx.run(&[
            "wait",
            "--timeout",
            "3",
            "text",
            "whitespace collapse marker"
        ])),
        0,
        "wait text must collapse whitespace to match a <br>-separated phrase: {}",
        stdout(&fx.run(&[
            "wait",
            "--timeout",
            "3",
            "text",
            "whitespace collapse marker"
        ]))
    );

    // 1c-sel. `wait selector` pierces open shadow roots too, like capture/find
    //     and `wait text` above: `#shadowbtn` lives only in `#shadowhost`'s open
    //     shadow root, so a plain `document.querySelector` misses it and the
    //     wait would time out on an element capture already indexes.
    assert_eq!(
        code(&fx.run(&["wait", "--timeout", "3", "selector", "#shadowbtn"])),
        0,
        "wait selector must pierce open shadow roots (find shadow-only #shadowbtn): {}",
        stdout(&fx.run(&["wait", "--timeout", "3", "selector", "#shadowbtn"]))
    );

    // 1d. `console read` / `network read` BEFORE the corresponding `start` is a
    //     typed not-active error (exit 7), not an empty buffer reported as
    //     success — which an agent would misread as "the page logged nothing /
    //     made no requests" when the monitor was simply never armed. (Same
    //     empty-success-vs-typed-not-found theme as `cookie get`.) Runs before any
    //     `console start` below.
    assert_eq!(
        code(&fx.run(&["console", "read"])),
        7,
        "console read before console start must be a typed not-active error (exit 7), not an empty success: {}",
        stdout(&fx.run(&["console", "read"]))
    );
    assert_eq!(
        code(&fx.run(&["network", "read"])),
        7,
        "network read before network start must be a typed not-active error (exit 7): {}",
        stdout(&fx.run(&["network", "read"]))
    );
    // `clear` holds the same contract — and must NOT create the buffer it
    // failed to find: an unconditional `= []` would defeat the read's
    // hook-absent guard (the `undefined` sentinel) in a document whose re-arm
    // an `eval` deny suppressed, turning a later read into an empty success
    // while the monitor is off.
    assert_eq!(
        code(&fx.run(&["console", "clear"])),
        7,
        "console clear before start must be typed (sentinel-preserving), not a buffer-creating success: {}",
        stdout(&fx.run(&["console", "clear"]))
    );
    assert_eq!(
        code(&fx.run(&["network", "clear"])),
        7,
        "network clear before start must be typed (sentinel-preserving): {}",
        stdout(&fx.run(&["network", "clear"]))
    );

    // 2. Click the button by its captured index; its onclick sets the title.
    let click = fx.run(&["action", "click", &button_index.to_string()]);
    assert_eq!(code(&click), 0, "click failed: {}", stdout(&click));
    let title = fx.run(&["eval", "document.title"]);
    assert!(
        stdout(&title).contains("clicked"),
        "click should have run the handler: {}",
        stdout(&title)
    );

    // 2-disabled. A disabled control can't be activated by a real user; a
    //   synthetic click would fire its handler anyway, so the action must reject
    //   it (InvalidArgument, exit 7) and the handler must NOT run — never a
    //   phantom success that mutates page state a real user couldn't.
    let disabled_idx = index_of(&cap, "disabledbtn");
    let dclick = fx.run(&["action", "click", &disabled_idx]);
    assert_eq!(
        code(&dclick),
        7,
        "clicking a disabled control must be InvalidArgument (7): {}",
        stdout(&dclick)
    );
    let dtitle = fx.run(&["eval", "document.title"]);
    assert!(
        !stdout(&dtitle).contains("SHOULD-NOT-FIRE"),
        "a disabled control's click handler must never fire: {}",
        stdout(&dtitle)
    );
    // Restore the title for any later step that might read it.
    let _ = fx.run(&["eval", "document.title = 'clicked'"]);

    // 2-disabled-select. The disabled-control rejection is consistent across
    //   actions: selecting in a disabled <select> is also InvalidArgument (7),
    //   never a phantom change event a real user couldn't trigger.
    let dsel_idx = index_of(&cap, "dsel");
    let dselect = fx.run(&["action", "select", &dsel_idx, "x"]);
    assert_eq!(
        code(&dselect),
        7,
        "selecting in a disabled <select> must be InvalidArgument (7): {}",
        stdout(&dselect)
    );

    // 2-disabled-inherited. Disabled state inherited from an ancestor (a
    //   `<fieldset disabled>` control, an `<option>` in a disabled `<optgroup>`)
    //   is real disabled state — the IDL `.disabled` property misses it, so the
    //   `:disabled`-based check must be used everywhere. The snapshot must mark
    //   the fieldset-disabled input disabled (so the agent sees it), and a select
    //   of an option inside a disabled optgroup must be rejected (7) while the
    //   sibling enabled option still selects (0).
    let fsfield = snapshot["elements"]
        .as_array()
        .expect("elements")
        .iter()
        .find(|e| e["id"] == "fsfield")
        .expect("the fieldset-disabled input is indexed");
    assert_eq!(
        fsfield["disabled"], true,
        "a control inside <fieldset disabled> must be captured as disabled: {dom}"
    );
    let ogsel_idx = index_of(&cap, "ogsel");
    let og_disabled = fx.run(&["action", "select", &ogsel_idx, "ogx"]);
    assert_eq!(
        code(&og_disabled),
        7,
        "selecting an <option> inside a disabled <optgroup> must be InvalidArgument (7): {}",
        stdout(&og_disabled)
    );
    let og_enabled = fx.run(&["action", "select", &ogsel_idx, "ogy"]);
    assert_eq!(
        code(&og_enabled),
        0,
        "the sibling enabled <option> must still select: {}",
        stdout(&og_enabled)
    );
    // A real selection fires `input` AND `change`. A <select> wired to `oninput`
    // (or a framework that observes input) must see the choice — firing only
    // `change` would silently drop it while the command reports success. The
    // rejected disabled-option select above fired neither, so both counters read
    // exactly 1 from this one enabled select.
    let og_evt = fx.run(&["eval", "window.__oginput===1 && window.__ogchange===1"]);
    let oge: serde_json::Value = serde_json::from_str(&stdout(&og_evt)).expect("eval json");
    assert_eq!(
        oge["result"].as_str(),
        Some("true"),
        "action select must fire both `input` and `change` exactly once each: {}",
        stdout(&og_evt)
    );

    // 2a-multi. On a `<select multiple>`, `action select` ADDS to the selection
    //   instead of replacing it: assigning `.value` would deselect every other
    //   chosen option, so an agent could never pick more than one. Choose two
    //   options in turn; both must remain selected, and the capture must render
    //   both (the multi-select renderer shows every selected option).
    let cap_ms = fx.run(&["capture", "--include", "dom"]);
    let ms_idx = index_of(&cap_ms, "multisel");
    assert_eq!(
        code(&fx.run(&["action", "select", &ms_idx, "mb"])),
        0,
        "selecting mb in the multi-select failed"
    );
    assert_eq!(
        code(&fx.run(&["action", "select", &ms_idx, "mc"])),
        0,
        "selecting mc in the multi-select failed"
    );
    let ms_sel = fx.run(&[
        "eval",
        "[...document.getElementById('multisel').selectedOptions].map(o=>o.value).join(',')",
    ]);
    let msj: serde_json::Value = serde_json::from_str(&stdout(&ms_sel)).expect("eval json");
    assert_eq!(
        msj["result"].as_str(),
        Some("\"mb,mc\""),
        "a multi-select must accumulate both choices (additive), not clobber the first: {}",
        stdout(&ms_sel)
    );

    // 2a1. Free-text values that start with `-` are values, not flags
    //      (allow_hyphen_values): an agent evaluating a negative expression or
    //      typing a negative number must not hit a clap usage error — and a
    //      trailing flag after such a value must still parse.
    let neg = fx.run(&["eval", "-7 * 6"]);
    assert_eq!(
        code(&neg),
        0,
        "leading-dash eval must not be a clap error: {}",
        stdout(&neg)
    );
    let nj: serde_json::Value = serde_json::from_str(&stdout(&neg)).expect("eval json");
    assert_eq!(
        nj["result"].as_str(),
        Some("-42"),
        "leading-dash eval must evaluate: {}",
        stdout(&neg)
    );
    let q_index = index_of(&cap, "q");
    let typed = fx.run(&["action", "type", &q_index, "-99", "--clear"]);
    assert_eq!(
        code(&typed),
        0,
        "type of a leading-dash value with --clear failed: {}",
        stdout(&typed)
    );
    let tv = fx.run(&["eval", "document.getElementById('q').value === '-99'"]);
    let tvj: serde_json::Value = serde_json::from_str(&stdout(&tv)).expect("eval json");
    assert_eq!(
        tvj["result"].as_str(),
        Some("true"),
        "the leading-dash value must land exactly and --clear must still apply: {}",
        stdout(&tv)
    );

    // 2a-max. `maxlength` bounds what a user can type; a programmatic set past
    //     it would hold a value the UI can never produce while reporting
    //     success. `#maxed` caps at 3: typing 4 chars is a typed
    //     InvalidArgument naming the cap, BEFORE any mutation; 3 chars land.
    let maxed_index = index_of(&cap, "maxed");
    let over = fx.run(&["action", "type", &maxed_index, "abcd", "--clear"]);
    assert_eq!(
        code(&over),
        7,
        "typing past maxlength must be a typed InvalidArgument, not a silent over-cap set: {}",
        stdout(&over)
    );
    assert!(
        stdout(&over).contains("maxlength"),
        "the over-cap error must name the constraint: {}",
        stdout(&over)
    );
    assert!(
        stdout(&fx.run(&["eval", "document.getElementById('maxed').value === ''"]))
            .contains("true"),
        "a rejected over-cap type must mutate NOTHING"
    );
    assert_eq!(
        code(&fx.run(&["action", "type", &maxed_index, "abc", "--clear"])),
        0,
        "typing exactly maxlength chars must succeed"
    );

    // 2a-domset. `dom set-*` enforces the strict-selector contract for writes:
    //     an ambiguous selector (here "input", many matches) is a typed
    //     InvalidArgument naming the count — never a silent mutation of
    //     whichever matched first. A unique selector still writes, and
    //     `dom get-*` keeps standard first-match read semantics.
    let amb_set = fx.run(&["dom", "set-text", "input", "x"]);
    assert_eq!(
        code(&amb_set),
        7,
        "an ambiguous dom set must be a typed InvalidArgument, not a first-match write: {}",
        stdout(&amb_set)
    );
    // The count is a NUMBER in the message (not just the phrase), parsed
    // rather than hard-coded so fixture growth doesn't break the assertion.
    let amb_out = stdout(&amb_set);
    let phrase_at = amb_out
        .find(" elements match")
        .expect("the dom-set ambiguity error must name the count");
    let count: String = amb_out[..phrase_at]
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    assert!(
        count.parse::<usize>().is_ok_and(|n| n > 1),
        "a numeric match count must precede 'elements match': {amb_out}"
    );
    assert_eq!(
        code(&fx.run(&["dom", "set-text", "#wsp", "rewritten"])),
        0,
        "a unique dom set must write"
    );
    assert!(
        stdout(&fx.run(&["dom", "get-text", "#wsp"])).contains("rewritten"),
        "the unique dom set must actually land"
    );
    // The selector surface pierces open shadow roots, like the element index
    // and `wait selector`: a component's field is readable and writable
    // without eval. `#shadowbtn` lives only in the shadow root — get reads it,
    // a unique set writes it (an inert data attribute), and a selector with a
    // light-DOM element AND a shadow twin ("p": light #wsp + the shadow
    // prose) is TWO candidates — ambiguous, never a silent light-only write.
    assert!(
        stdout(&fx.run(&["dom", "get-text", "#shadowbtn"])).contains("shadow"),
        "dom get must pierce an open shadow root"
    );
    assert_eq!(
        code(&fx.run(&["dom", "set-attr", "#shadowbtn", "data-wp", "marked"])),
        0,
        "a unique shadow-root dom set must write"
    );
    assert!(
        stdout(&fx.run(&["dom", "get-attr", "#shadowbtn", "data-wp"])).contains("marked"),
        "the shadow-root attribute write must read back"
    );
    // An ABSENT attribute reads back as an explicit `value: null` (JSON) —
    // distinct from a present-but-empty `""`, so an agent can tell "no such
    // attribute" from "the attribute is empty". The human/MCP surface gets the
    // `(no attribute …)` note for the same reason; here the --json path proves
    // the structural null.
    let absent_attr = fx.run(&["dom", "get-attr", "#shadowbtn", "data-absent"]);
    assert_eq!(
        code(&absent_attr),
        0,
        "reading an absent attribute is a success, not an error: {}",
        stdout(&absent_attr)
    );
    let aaj: serde_json::Value =
        serde_json::from_str(&stdout(&absent_attr)).expect("absent-attr json");
    assert!(
        aaj["value"].is_null(),
        "an absent attribute must read as value:null, not an empty string: {}",
        stdout(&absent_attr)
    );
    assert_eq!(
        code(&fx.run(&["dom", "set-text", "p", "x"])),
        7,
        "a selector matching a light element AND a shadow twin must be ambiguous: {}",
        stdout(&fx.run(&["dom", "set-text", "p", "x"]))
    );

    // 2a-dlg. Javascript dialogs never wedge the page, and both modes share
    //     accept-with-default semantics: with a CDP client holding Page
    //     enabled, Chrome STOPS its headless auto-dismiss and waits for
    //     Page.handleJavaScriptDialog — without the dialog responder a bare
    //     alert() would block the renderer until every command times out.
    //     confirm() answers true and prompt() returns its default, the same
    //     contract the browser-mode override implements.
    let alerted = fx.run(&["eval", "alert('hi'); 'ok'"]);
    assert_eq!(
        code(&alerted),
        0,
        "an alert must be auto-answered, never wedge the renderer: {}",
        stdout(&alerted)
    );
    assert!(
        stdout(&alerted).contains("ok"),
        "eval must complete past the alert: {}",
        stdout(&alerted)
    );
    let cap_dlg = fx.run(&["capture", "--include", "dom"]);
    let dlg_index = index_of(&cap_dlg, "dlg");
    assert_eq!(
        code(&fx.run(&["action", "click", &dlg_index])),
        0,
        "clicking the dialog button failed"
    );
    assert!(
        stdout(&fx.run(&[
            "eval",
            "window.__dlg && window.__dlg[0] === true && window.__dlg[1] === 'dv' ? 'parity' : JSON.stringify(window.__dlg)",
        ]))
        .contains("parity"),
        "confirm must answer true and prompt must return its default (accept-with-default)"
    );
    // The click scrolled #dlg into view; restore the top so later sections'
    // captures keep the full element set (the cursor:pointer pass is
    // deliberately viewport-bounded — an off-screen pointer-only target like
    // #cardwrap reappears only once scrolled back).
    assert_eq!(
        code(&fx.run(&["eval", "scrollTo(0,0); 'reset'"])),
        0,
        "scroll reset failed"
    );
    // `--occlusion` must not mislabel a shadow-root control:
    // document.elementFromPoint retargets a shadow-interior hit to its HOST,
    // which tree-scoped contains() would call a blocker — the deep hit-test
    // descends into the shadow root instead. #shadowbtn is uncovered, so it
    // must not read occluded.
    let occ = fx.run(&["capture", "--include", "dom", "--occlusion"]);
    assert_eq!(code(&occ), 0, "occlusion capture failed: {}", stdout(&occ));
    let occ_json: serde_json::Value = serde_json::from_str(&stdout(&occ)).expect("occ json");
    let sb = occ_json["elements"]
        .as_array()
        .expect("occ elements")
        .iter()
        .find(|e| e["id"] == "shadowbtn")
        .expect("shadowbtn indexed in occlusion capture")
        .clone();
    assert_ne!(
        sb["occluded"],
        serde_json::Value::Bool(true),
        "an uncovered shadow-root control must not be falsely occluded by its own host: {}",
        sb
    );
    // A shadow button whose visible content is a SLOTTED light span: the hit
    // over its label lands on light DOM, which the composed walk relates back
    // through the slot assignment — never "occluded by its own label".
    let slb = occ_json["elements"]
        .as_array()
        .expect("occ elements")
        .iter()
        .find(|e| e["id"] == "slotbtn")
        .expect("slotbtn indexed in occlusion capture")
        .clone();
    assert_ne!(
        slb["occluded"],
        serde_json::Value::Bool(true),
        "a shadow control with slotted light content must not be occluded by its own slotted label: {}",
        slb
    );

    // 2a-ce. `type` into a contenteditable APPENDS at the end, like an <input>:
    //     after a programmatic focus the caret sits at a stale/start position,
    //     so the bridge collapses the selection to the end before inserting.
    //     `#ce` starts with "hello"; typing "more" (default, no --clear) must
    //     yield "hellomore", never "morehello".
    let ce_index = index_of(&cap, "ce");
    let _ = fx.run(&[
        "eval",
        "window.__ceInputs = 0; document.getElementById('ce').addEventListener('input', () => window.__ceInputs++); 'armed'",
    ]);
    let ce_typed = fx.run(&["action", "type", &ce_index, "more"]);
    assert_eq!(
        code(&ce_typed),
        0,
        "type into a contenteditable failed: {}",
        stdout(&ce_typed)
    );
    let ce_text = fx.run(&["eval", "document.getElementById('ce').textContent"]);
    let cej: serde_json::Value = serde_json::from_str(&stdout(&ce_text)).expect("eval json");
    assert_eq!(
        cej["result"].as_str(),
        Some("\"hellomore\""),
        "type into a contenteditable must append at the end (hello+more), not prepend: {}",
        stdout(&ce_text)
    );
    // ...and exactly ONE `input` event for one insert: execCommand fires its
    // own, and the bridge's fallback dispatch must fire only when the native
    // one did not — unconditional dispatch doubled the event (a raw oninput
    // counter or an append-per-input editor saw a phantom second edit).
    let ce_inputs = fx.run(&["eval", "window.__ceInputs"]);
    let cij: serde_json::Value = serde_json::from_str(&stdout(&ce_inputs)).expect("eval json");
    assert_eq!(
        cij["result"].as_str(),
        Some("1"),
        "one contenteditable insert must fire exactly one input event: {}",
        stdout(&ce_inputs)
    );
    // 2a-num. A typed control silently sanitizes a value it can't parse to "".
    //     Typing "abc" into `<input type=number>` leaves it blank, so a success
    //     would claim a value that never landed — it must be a typed
    //     InvalidArgument (7). A valid number still lands and reports success.
    let num_index = index_of(&cap, "num");
    let num_bad = fx.run(&["action", "type", &num_index, "abc"]);
    assert_eq!(
        code(&num_bad),
        7,
        "typing a non-numeric value into <input type=number> must be InvalidArgument (7), not a success that left the field empty: {}",
        stdout(&num_bad)
    );
    let num_ok = fx.run(&["action", "type", &num_index, "42"]);
    assert_eq!(
        code(&num_ok),
        0,
        "typing a valid number must still succeed: {}",
        stdout(&num_ok)
    );
    let num_val = fx.run(&["eval", "document.getElementById('num').value"]);
    let nvj: serde_json::Value = serde_json::from_str(&stdout(&num_val)).expect("eval json");
    assert_eq!(
        nvj["result"].as_str(),
        Some("\"42\""),
        "the valid number must land in the field: {}",
        stdout(&num_val)
    );
    // 2a-num-restore. A REJECTED append must be a clean no-op. Appending "xyz" to
    //     the field's "42" sets "42xyz", which the number control sanitizes to ""
    //     — but the agent never asked to discard the valid "42". The rejection
    //     restores it, so a failed type never destroys existing state (the
    //     maxlength guard rejects BEFORE mutating; this path must match).
    let num_restore = fx.run(&["action", "type", &num_index, "xyz"]);
    assert_eq!(
        code(&num_restore),
        7,
        "appending an invalid value into a number field must be rejected (7): {}",
        stdout(&num_restore)
    );
    let num_after = fx.run(&["eval", "document.getElementById('num').value"]);
    let naj: serde_json::Value = serde_json::from_str(&stdout(&num_after)).expect("eval json");
    assert_eq!(
        naj["result"].as_str(),
        Some("\"42\""),
        "a rejected append must restore the prior value, never blank the field: {}",
        stdout(&num_after)
    );

    // 2a-sip. The contenteditable input-probe survives a rich editor that
    //     stopImmediatePropagation()s on its own capture listener (registered
    //     before ours): a same-node probe would be starved into "never fired"
    //     and double-dispatch — the document-capture probe sees the native
    //     event first. The editor's own counter must read 1. (Runs AFTER the
    //     `cap`-indexed tests above: injecting ce2 shifts later snapshots'
    //     indices, and the reload below wipes it.)
    let _ = fx.run(&[
        "eval",
        "document.body.insertAdjacentHTML('beforeend', '<div id=ce2 contenteditable></div>'); \
         window.__ce2Inputs = 0; \
         document.getElementById('ce2').addEventListener('input', (e) => { window.__ce2Inputs++; e.stopImmediatePropagation(); }, { capture: true }); 'armed'",
    ]);
    let ce2_cap = fx.run(&["capture", "--include", "dom"]);
    let ce2_index = index_of(&ce2_cap, "ce2");
    assert_eq!(
        code(&fx.run(&["action", "type", &ce2_index, "x"])),
        0,
        "type into the stopImmediatePropagation contenteditable failed"
    );
    let ce2_inputs = fx.run(&["eval", "window.__ce2Inputs"]);
    let c2j: serde_json::Value = serde_json::from_str(&stdout(&ce2_inputs)).expect("eval json");
    assert_eq!(
        c2j["result"].as_str(),
        Some("1"),
        "a stopImmediatePropagation editor must still see exactly one input \
         (no starved-probe double dispatch): {}",
        stdout(&ce2_inputs)
    );

    // 2a. A same-URL reload rebuilds the document, clearing the old execution
    //      contexts. The bridge re-injects into the fresh isolated world on its
    //      own (the persistent addScriptToEvaluateOnNewDocument), and the open-
    //      time listener — still bound to this unswapped session — repopulates
    //      the maps, so a capture right after must resolve, never hang.
    let reloaded = fx.run(&["action", "reload"]);
    assert_eq!(code(&reloaded), 0, "reload failed: {}", stdout(&reloaded));
    let after_reload = fx.run(&["capture", "--include", "dom"]);
    assert_eq!(
        code(&after_reload),
        0,
        "capture after a same-URL reload failed: {}",
        stdout(&after_reload)
    );
    let ar: serde_json::Value = serde_json::from_str(&stdout(&after_reload)).expect("capture json");
    assert!(
        ar["elements"]
            .as_array()
            .is_some_and(|a| a.iter().any(|e| e["tag"] == "button")),
        "the bridge must repopulate in the reloaded document's isolated world: {}",
        stdout(&after_reload)
    );

    // 2b. `key-press` is a real CDP input event, not a synthetic one: focus the
    //     text input, seed a value with the caret at the end, press Backspace,
    //     and the browser must natively delete a character. A synthetic
    //     KeyboardEvent cannot edit a field — this would fail under the old
    //     dispatch, locking in the native-input behaviour.
    let seed = fx.run(&[
        "eval",
        "const i=document.getElementById('q'); i.value='ab'; i.focus(); i.setSelectionRange(2,2); 'ok'",
    ]);
    assert_eq!(code(&seed), 0, "seed eval failed: {}", stdout(&seed));
    let bksp = fx.run(&["action", "key-press", "Backspace"]);
    assert_eq!(code(&bksp), 0, "key-press failed: {}", stdout(&bksp));
    // Assert the EXACT result via a boolean eval: a failed eval would print an
    // error (which can itself contain 'a' and not "ab"), passing a naive
    // string-contains check without proving the field became "a".
    let val = fx.run(&["eval", "document.getElementById('q').value === 'a'"]);
    assert_eq!(code(&val), 0, "value eval failed: {}", stdout(&val));
    let vj: serde_json::Value = serde_json::from_str(&stdout(&val)).expect("eval json");
    assert_eq!(
        vj["result"].as_str(),
        Some("true"),
        "Backspace must natively delete exactly one char (ab -> a): {}",
        stdout(&val)
    );

    // 2b-chord. A modifier chord brackets the modifier's own key events around
    //     the main key (rawKeyDown … keyUp), so renderer-level editing commands
    //     fire: shift+ArrowLeft from caret-at-end must EXTEND the selection by
    //     one character — the mask-only dispatch left the selection untouched.
    let seed2 = fx.run(&[
        "eval",
        "(()=>{const i=document.getElementById('q'); i.value='hello'; i.focus(); i.setSelectionRange(5,5); return 'ok';})()",
    ]);
    assert_eq!(code(&seed2), 0, "chord seed failed: {}", stdout(&seed2));
    assert_eq!(
        code(&fx.run(&["action", "key-press", "--shift", "ArrowLeft"])),
        0,
        "shift+ArrowLeft failed"
    );
    let sel = fx.run(&[
        "eval",
        "document.getElementById('q').selectionStart===4 && document.getElementById('q').selectionEnd===5",
    ]);
    let sj: serde_json::Value = serde_json::from_str(&stdout(&sel)).expect("eval json");
    assert_eq!(
        sj["result"].as_str(),
        Some("true"),
        "shift+ArrowLeft must extend the selection (4..5) via the bracketed modifier: {}",
        stdout(&sel)
    );

    // 2b-modclick. `click --ctrl/--shift` carries the modifier flags through
    //     the synthetic event sequence, so the page's own handlers see them
    //     (app-level ctrl multi-select, shift range-select). Browser-level
    //     defaults (open-in-new-tab) intentionally don't apply to a synthetic
    //     click — that path is `tab new URL`. Pre-fix no surface could express
    //     click modifiers at all.
    let _ = fx.run(&[
        "eval",
        "document.body.insertAdjacentHTML('beforeend', '<button id=modbtn onclick=\"this.dataset.mods=[event.ctrlKey,event.shiftKey,event.altKey,event.metaKey].join()\">mod</button>'); 'ok'",
    ]);
    let mod_cap = fx.run(&["capture", "--include", "dom"]);
    assert_eq!(code(&mod_cap), 0, "modbtn capture failed");
    let modbtn = index_of(&mod_cap, "modbtn");
    assert_eq!(
        code(&fx.run(&["action", "click", &modbtn, "--ctrl", "--shift"])),
        0,
        "modifier click failed"
    );
    let mods_read = fx.run(&["eval", "document.getElementById('modbtn').dataset.mods"]);
    let mj: serde_json::Value = serde_json::from_str(&stdout(&mods_read)).expect("eval json");
    assert_eq!(
        mj["result"].as_str(),
        Some("\"true,true,false,false\""),
        "the page's click handler must see ctrl+shift (and not alt/meta): {}",
        stdout(&mods_read)
    );

    // 2b-click. A click must focus a focusable target, like a real click —
    //     mousedown's default action moves focus, firing focus/focusin and
    //     establishing the browser focus a following native key_press lands on
    //     (the documented click-then-type contract). A purely synthetic dispatch
    //     skips that default, so without the explicit focus the click would leave
    //     focus elsewhere. Navigate fresh, click the empty text input, and it
    //     must become document.activeElement.
    assert_eq!(code(&fx.run(&["action", "navigate", &base])), 0);
    let cap_cf = fx.run(&["capture", "--include", "dom"]);
    let q_click_idx = index_of(&cap_cf, "q");
    assert_eq!(
        code(&fx.run(&["action", "click", &q_click_idx])),
        0,
        "click on the text input failed"
    );
    let active = fx.run(&[
        "eval",
        "document.activeElement === document.getElementById('q')",
    ]);
    let aj: serde_json::Value = serde_json::from_str(&stdout(&active)).expect("eval json");
    assert_eq!(
        aj["result"].as_str(),
        Some("true"),
        "a click must focus a focusable target (so a following key_press lands there), \
         not leave focus elsewhere: {}",
        stdout(&active)
    );

    // 2b-shift. A `--shift` key-press of a LETTER produces its uppercase form,
    //     like a real Shift+letter — the shift flag alone leaves the character
    //     lowercase. Focus the text input, press Shift+a, and the field must
    //     receive "A" (a non-letter's shift stays layout-agnostic and untouched).
    let seed_shift = fx.run(&[
        "eval",
        "const i=document.getElementById('q'); i.value=''; i.focus(); 'ok'",
    ]);
    assert_eq!(
        code(&seed_shift),
        0,
        "shift seed eval failed: {}",
        stdout(&seed_shift)
    );
    assert_eq!(
        code(&fx.run(&["action", "key-press", "a", "--shift"])),
        0,
        "Shift+a key-press failed"
    );
    let sval = fx.run(&["eval", "document.getElementById('q').value === 'A'"]);
    let svj: serde_json::Value = serde_json::from_str(&stdout(&sval)).expect("eval json");
    assert_eq!(
        svj["result"].as_str(),
        Some("true"),
        "key-press a --shift must insert uppercase 'A', not lowercase 'a': {}",
        stdout(&sval)
    );

    // 2b-fkey. A canonical function key (`F1`) is a valid key-press; a
    //     non-canonical name (`F01`, a leading zero) is NOT a real DOM key code
    //     and must be rejected as InvalidArgument (exit 7), not silently
    //     normalized to F1 — matching the browser's strict `^F([1-9]|1[0-2])$`
    //     so the same name never succeeds in one mode and fails in the other.
    let fk_ok = fx.run(&["action", "key-press", "F1"]);
    assert_eq!(
        code(&fk_ok),
        0,
        "canonical F1 must be a valid key-press: {}",
        stdout(&fk_ok)
    );
    let fk_bad = fx.run(&["action", "key-press", "F01"]);
    assert_eq!(
        code(&fk_bad),
        7,
        "a non-canonical F-key (F01) must be InvalidArgument, not normalized to F1: {}",
        stdout(&fk_bad)
    );

    // 2b-space. `key-press Space` delivers the spacebar's CANONICAL DOM key " ",
    //     not the literal token "Space" — Chrome rejects "Space" as a `key` value
    //     (it arrives as an empty e.key), so a listener keying on `e.key === " "`
    //     would miss the Space spelling. Capture the keydown's `key` and assert it
    //     is a single space.
    let seed_space = fx.run(&[
        "eval",
        "window.__sk=null; const spi=document.getElementById('q'); spi.value=''; spi.focus(); \
         spi.addEventListener('keydown',e=>{window.__sk=e.key},{once:true}); 'ok'",
    ]);
    assert_eq!(
        code(&seed_space),
        0,
        "space seed eval failed: {}",
        stdout(&seed_space)
    );
    assert_eq!(
        code(&fx.run(&["action", "key-press", "Space"])),
        0,
        "key-press Space failed"
    );
    let skv = fx.run(&["eval", "window.__sk === ' '"]);
    let skj: serde_json::Value = serde_json::from_str(&stdout(&skv)).expect("eval json");
    assert_eq!(
        skj["result"].as_str(),
        Some("true"),
        "key-press Space must deliver the canonical DOM key \" \", not the empty key Chrome \
         gives for the \"Space\" token: {}",
        stdout(&skv)
    );

    // 2b-tabnewleak. `tab new` to an unreachable URL is a typed NavigationFailed
    //     (exit 8) that leaves NO trace: the orphan chrome-error tab is closed and
    //     the pin returns to the working page — `navigate`'s no-leak contract.
    //     (Before the fix it stranded the error tab and drifted the pin onto it.)
    //     Bind port 0 (the OS hands back a free one) then drop the listener, so the
    //     port is GUARANTEED nothing-listening — robust against the flake a fixed
    //     ephemeral port causes when a concurrent process has it transiently bound
    //     (the nav would then succeed and this assertion fail spuriously).
    let dead_port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind a free port")
        .local_addr()
        .expect("local addr")
        .port();
    let bad_url = format!("http://127.0.0.1:{dead_port}/nope");
    let bad_tab = fx.run(&["tab", "new", &bad_url]);
    assert_eq!(
        code(&bad_tab),
        8,
        "tab new to an unreachable URL must be NavigationFailed (8): {}",
        stdout(&bad_tab)
    );
    let pin_ok = fx.run(&["eval", "location.href.startsWith('chrome-error')"]);
    let pin_j: serde_json::Value = serde_json::from_str(&stdout(&pin_ok)).expect("eval json");
    assert_eq!(
        pin_j["result"].as_str(),
        Some("false"),
        "a failed tab new must not drift the pin onto a chrome-error tab: {}",
        stdout(&pin_ok)
    );
    let tabs_after = fx.run(&["tab"]);
    assert!(
        !stdout(&tabs_after).contains("chrome-error"),
        "a failed tab new must close the orphan error tab (no leak): {}",
        stdout(&tabs_after)
    );

    // 2c. `upload` sets a file on the input the index addressed — resolved by
    //     snapshot identity and handed to CDP as an object reference, never a
    //     live document-order re-query a page could redirect. Prove the file
    //     landed on #file; uploading onto a non-file element is a typed
    //     InvalidArgument (exit 7), caught at the bridge before any CDP sink.
    let cap_up = fx.run(&["capture", "--include", "dom"]);
    assert_eq!(code(&cap_up), 0);
    let file_index = index_of(&cap_up, "file");
    let upload_src = home.join("upload-src.txt");
    std::fs::write(&upload_src, b"payload").expect("write upload fixture");
    let src = upload_src.to_str().unwrap();
    let up = fx.run(&["action", "upload", &file_index, src]);
    assert_eq!(code(&up), 0, "upload failed: {}", stdout(&up));
    let fcount = fx.run(&["eval", "document.getElementById('file').files.length"]);
    let fc: serde_json::Value = serde_json::from_str(&stdout(&fcount)).expect("eval json");
    assert_eq!(
        fc["result"].as_str(),
        Some("1"),
        "upload must place exactly one file on #file: {}",
        stdout(&fcount)
    );
    let bad = fx.run(&["action", "upload", &button_index.to_string(), src]);
    assert_eq!(
        code(&bad),
        7,
        "upload onto a non-file element must be InvalidArgument: {}",
        stdout(&bad)
    );
    // A file input is captured even when UNPAINTED (display:none here) — the
    // standard upload UX hides the input behind a styled trigger, and `upload`
    // sets the file over CDP regardless of paint, so the agent must be able to
    // address it by index. (#hiddenfile is `display:none`, so a visible-action
    // gate would otherwise drop it and make the input unreachable.)
    let hidden_file_index = index_of(&cap_up, "hiddenfile");
    let uph = fx.run(&["action", "upload", &hidden_file_index, src]);
    assert_eq!(
        code(&uph),
        0,
        "upload to a display:none file input must succeed (the common hidden-input pattern): {}",
        stdout(&uph)
    );
    let hfc = fx.run(&["eval", "document.getElementById('hiddenfile').files.length"]);
    let hfcj: serde_json::Value = serde_json::from_str(&stdout(&hfc)).expect("eval json");
    assert_eq!(
        hfcj["result"].as_str(),
        Some("1"),
        "upload must place exactly one file on the hidden #hiddenfile: {}",
        stdout(&hfc)
    );
    // A missing upload file is resolved (and rejected) in the CLI before the
    // wire, so it's a typed InvalidArgument — not a raw CDP error from Chrome.
    let missing = fx.run(&["action", "upload", &file_index, "/no/such/upload/file.txt"]);
    assert_eq!(
        code(&missing),
        7,
        "a missing upload file must be a typed InvalidArgument: {}",
        stdout(&missing)
    );

    // 2d. The object handoff reaches a file input inside an OPEN SHADOW ROOT —
    //     the snapshot pierces shadow, and an object reference (unlike a
    //     document-root selector) crosses the boundary the CDP node lookup
    //     can't. Capture indexes it; upload lands the file on the shadow input.
    let shadow_index = index_of(&cap_up, "shadowfile");
    let up_shadow = fx.run(&["action", "upload", &shadow_index, src]);
    assert_eq!(
        code(&up_shadow),
        0,
        "shadow-root upload failed: {}",
        stdout(&up_shadow)
    );
    let scount = fx.run(&[
        "eval",
        "document.getElementById('shadowhost').shadowRoot.getElementById('shadowfile').files.length",
    ]);
    let sc: serde_json::Value = serde_json::from_str(&stdout(&scount)).expect("eval json");
    assert_eq!(
        sc["result"].as_str(),
        Some("1"),
        "object-handoff upload must reach a shadow-root file input: {}",
        stdout(&scount)
    );

    // 2f. A click on a control inside an open shadow root must dispatch a
    //     `composed` event so it crosses the shadow boundary: the page's
    //     document-level delegated click listener only fires for the shadow
    //     button if the event escapes the shadow root. Without `composed:true`
    //     the click would be a silent no-op for any host/document delegation.
    let shadow_btn = index_of(&cap_up, "shadowbtn");
    let shadow_click = fx.run(&["action", "click", &shadow_btn]);
    assert_eq!(
        code(&shadow_click),
        0,
        "shadow-root button click failed: {}",
        stdout(&shadow_click)
    );
    let title_after = fx.run(&["eval", "document.title"]);
    assert!(
        stdout(&title_after).contains("shadow-delegated"),
        "a shadow-root click must reach the document's delegated listener (composed event): {}",
        stdout(&title_after)
    );

    // 2g. `focused` must pierce open shadow roots. `document.activeElement` names
    //     only the outermost shadow HOST, so focusing a control inside a shadow
    //     root and reading it back naively reports `focused:false`. Focus the
    //     shadow button by index, re-capture, and confirm the snapshot marks it
    //     focused — proving per-element `focused` resolves via deepActiveElement.
    let cap_sf = fx.run(&["capture", "--include", "dom"]);
    let sf_btn = index_of(&cap_sf, "shadowbtn");
    assert_eq!(
        code(&fx.run(&["action", "focus", &sf_btn])),
        0,
        "focusing a shadow-root control must succeed"
    );
    let cap_sf_after = fx.run(&["capture", "--include", "dom"]);
    let sf_json: serde_json::Value =
        serde_json::from_str(&stdout(&cap_sf_after)).expect("capture json");
    let sf_focused = sf_json["elements"]
        .as_array()
        .expect("elements array")
        .iter()
        .find(|e| e["id"] == "shadowbtn")
        .and_then(|e| e["focused"].as_bool())
        .unwrap_or(false);
    assert!(
        sf_focused,
        "a focused control inside an open shadow root must report focused:true \
         (deepActiveElement pierces the shadow host): {}",
        stdout(&cap_sf_after)
    );

    // 2h. `landmark` must also pierce open shadow roots: `#shadowhost` is wrapped
    //     in <nav>, so a control inside its shadow root sits within that landmark
    //     in the flat tree. A bare `parentElement` walk stops at the shadow host
    //     and would report no landmark; the flat-tree walk crosses to the host
    //     and finds the <nav>.
    let sf_landmark = sf_json["elements"]
        .as_array()
        .expect("elements array")
        .iter()
        .find(|e| e["id"] == "shadowbtn")
        .and_then(|e| e["landmark"].as_str())
        .unwrap_or("");
    assert_eq!(
        sf_landmark,
        "navigation",
        "a control inside an open shadow root must inherit the landmark wrapping \
         its host (flat-tree walk crosses the shadow boundary), reported by its \
         canonical ARIA role: {}",
        stdout(&cap_sf_after)
    );
    // The REVERSE projection: a slotted LIGHT control renders inside a SHADOW
    // landmark (the flat tree the a11y tree follows), while its light
    // ancestors carry none — the walk must follow the slot assignment.
    let slotted_landmark = sf_json["elements"]
        .as_array()
        .expect("elements array")
        .iter()
        .find(|e| e["id"] == "slottedbtn")
        .and_then(|e| e["landmark"].as_str())
        .unwrap_or("");
    assert_eq!(
        slotted_landmark,
        "complementary",
        "a slotted control must inherit its flat-tree (shadow-side) landmark \
         (`<aside>` → @complementary), not its light ancestors': {}",
        stdout(&cap_sf_after)
    );
    // A landmark expressed as a `role=` on a plain element (the common
    // design-system pattern, e.g. `<div role="navigation">`) must be detected
    // just like the `<nav>` tag — both report the canonical @navigation. Before
    // the ARIA-role landmark set, a `role=`-only landmark was missed entirely.
    let aria_nav_landmark = sf_json["elements"]
        .as_array()
        .expect("elements array")
        .iter()
        .find(|e| e["id"] == "arianavbtn")
        .and_then(|e| e["landmark"].as_str())
        .unwrap_or("");
    assert_eq!(
        aria_nav_landmark,
        "navigation",
        "a control inside `<div role=\"navigation\">` must report @navigation, \
         not be missed because the landmark was a role= rather than a <nav> tag: {}",
        stdout(&cap_sf_after)
    );
    // An `<article>`'s `<header>` is a GENERIC heading area, not THE page banner
    // (ARIA: header/footer are landmarks only when NOT scoped to a sectioning
    // element). Reporting @banner there would be a false page-level claim — the
    // walk must skip the sectioning-nested header and find no landmark.
    let art_header_landmark = sf_json["elements"]
        .as_array()
        .expect("elements array")
        .iter()
        .find(|e| e["id"] == "artheaderbtn")
        .map(|e| e["landmark"].as_str().unwrap_or(""))
        .expect("artheaderbtn present");
    assert_eq!(
        art_header_landmark,
        "",
        "an <article>'s <header> is not the page @banner — it must report no \
         landmark, not a false page-level claim: {}",
        stdout(&cap_sf_after)
    );

    // Q5: a CSS-hidden `<option>` (`display:none`, the common dynamic-filter
    // pattern) must NOT be selectable — `action select` rejects it typed, like a
    // disabled one, not a silent assignment a real user could never make.
    let hidopt_cap = fx.run(&["capture", "--include", "dom"]);
    let hidopt_idx = index_of(&hidopt_cap, "hidopt");
    let hid_sel = fx.run(&["action", "select", &hidopt_idx, "hid"]);
    assert_eq!(
        code(&hid_sel),
        7,
        "selecting a display:none <option> must be a typed InvalidArgument (7), \
         not a silent assignment: {}",
        stdout(&hid_sel)
    );
    let vis_sel = fx.run(&["action", "select", &hidopt_idx, "vis"]);
    assert_eq!(
        code(&vis_sel),
        0,
        "a visible option must still select: {}",
        stdout(&vis_sel)
    );

    // 2e. `fetch` runs as a debugger-routed MAIN-world eval in both modes (no
    //     contextId, CSP-exempt) and returns the response body — a same-origin
    //     GET against the fixture server must come back with the page markup.
    let fetched = fx.run(&["fetch", &base]);
    assert_eq!(code(&fetched), 0, "fetch failed: {}", stdout(&fetched));
    let fj: serde_json::Value = serde_json::from_str(&stdout(&fetched)).expect("fetch json");
    assert!(
        fj["body"]
            .as_str()
            .is_some_and(|b| b.contains("shadowhost") || b.contains("<button")),
        "fetch must return the page body: {}",
        stdout(&fetched)
    );
    // The HTTP status rides every fetch result (JSON structurally; the human/MCP
    // surface gets it via the `note`, so a 404-with-body never hides its status
    // on one channel while the JSON keeps it). 200 here for the live fixture.
    assert_eq!(
        fj["status"].as_u64(),
        Some(200),
        "fetch JSON must carry the HTTP status: {}",
        stdout(&fetched)
    );

    // 2e-bin. A binary (non-UTF8) response fails loud rather than handing back a
    //     lossy-decoded string under a success status — the agent must never
    //     mistake mojibake for the real body. `/binary` serves raw non-UTF8
    //     bytes; the error names the cause.
    let bin = fx.run(&["fetch", &format!("{base}/binary")]);
    assert_eq!(
        code(&bin),
        1,
        "fetch of a binary body must fail loud (exit 1), not succeed with mojibake: {}",
        stdout(&bin)
    );
    assert!(
        stdout(&bin).contains("not valid UTF-8"),
        "the binary-fetch error must name the cause (not valid UTF-8): {}",
        stdout(&bin)
    );

    // 2e-find. A chained `find --click`/`--fill` enforces the strict-selector
    //     contract (`frame url` v0.4.152, `tab find` v0.4.169): an ambiguous
    //     filter fails loud BEFORE any side effect, never a silent act on the
    //     first match. The fixture has many <input>s, so `--tag input` is
    //     ambiguous; the placeholder filter is unique and still chains. The
    //     handler is mode-generic (shared Capture → Action path), so both
    //     modes get the contract.
    let ambiguous = fx.run(&["find", "--tag", "input", "--fill", "never-lands"]);
    assert_eq!(
        code(&ambiguous),
        7,
        "an ambiguous find --fill must be a typed InvalidArgument, not a silent first-match fill: {}",
        stdout(&ambiguous)
    );
    assert!(
        stdout(&ambiguous).contains("elements match"),
        "the ambiguity error must name the match count: {}",
        stdout(&ambiguous)
    );
    // 2e-empty. A PRESENT-but-empty filter (`--text ""`) is a no-op that
    //     silently matches every element (contains("")) — an agent that built
    //     the value from an empty variable would get a surprising match-all and,
    //     on a one-element page, `--click`/`--fill` would proceed as if it
    //     discriminated. Reject the empty value loudly, naming the flag.
    let empty_filter = fx.run(&["find", "--text", ""]);
    assert_eq!(
        code(&empty_filter),
        7,
        "an empty --text filter must be a typed InvalidArgument, not a match-all: {}",
        stdout(&empty_filter)
    );
    assert!(
        stdout(&empty_filter).contains("--text") && stdout(&empty_filter).contains("empty"),
        "the empty-filter error must name the flag and the cause: {}",
        stdout(&empty_filter)
    );
    // `href=""` is a real link to the current page (ARIA role `link`): the
    // wire must keep the empty string (`?? undefined`, not `||`), or the
    // implicit role is stripped and `--role link` misses it.
    let selfref = fx.run(&["find", "--role", "link", "--text", "selfref"]);
    assert_eq!(
        code(&selfref),
        0,
        "an empty-href anchor must carry the implicit link role: {}",
        stdout(&selfref)
    );

    let unique = fx.run(&[
        "find",
        "--placeholder",
        "Search",
        "--fill",
        "wp-find-unique",
    ]);
    assert_eq!(
        code(&unique),
        0,
        "a unique find --fill must chain: {}",
        stdout(&unique)
    );
    assert!(
        stdout(&fx.run(&[
            "eval",
            "document.getElementById('q').value === 'wp-find-unique'"
        ]))
        .contains("true"),
        "the unique match must actually be filled"
    );

    // 2f. The bridge runs in an isolated world, so page JS that overwrites the
    //     MAIN-world `__webpilot_handle` / `__webpilot_state` cannot corrupt how
    //     an index resolves. Tamper in MAIN, then capture + click and confirm
    //     both still hit the real elements — the snapshot-integrity guarantee
    //     the isolated bridge buys, locked against a hostile page.
    let tamper = fx.run(&[
        "eval",
        "window.__webpilot_handle=()=>({success:true,elements:[]}); window.__webpilot_state={snapshot:[]}; 'tampered'",
    ]);
    assert_eq!(code(&tamper), 0, "tamper eval failed: {}", stdout(&tamper));
    let tj: serde_json::Value = serde_json::from_str(&stdout(&tamper)).expect("eval json");
    assert_eq!(
        tj["result"].as_str(),
        Some("\"tampered\""),
        "the MAIN-world tamper must actually apply, else isolation is untested: {}",
        stdout(&tamper)
    );
    let cap_t = fx.run(&["capture", "--include", "dom"]);
    assert_eq!(
        code(&cap_t),
        0,
        "capture must survive MAIN tampering: {}",
        stdout(&cap_t)
    );
    let tampered: serde_json::Value = serde_json::from_str(&stdout(&cap_t)).expect("capture json");
    assert!(
        tampered["elements"]
            .as_array()
            .is_some_and(|a| a.iter().any(|e| e["tag"] == "button")),
        "isolated bridge must capture real elements despite a hijacked MAIN __webpilot_state: {}",
        stdout(&cap_t)
    );
    let go_idx = index_of(&cap_t, "go");
    let clk = fx.run(&["action", "click", &go_idx]);
    assert_eq!(
        code(&clk),
        0,
        "click must survive MAIN tampering: {}",
        stdout(&clk)
    );
    let title = fx.run(&["eval", "document.title"]);
    assert!(
        stdout(&title).contains("clicked"),
        "click must resolve via the isolated bridge, not the hijacked MAIN snapshot: {}",
        stdout(&title)
    );

    // 2g. The bridge runs in EACH frame's isolated world, not just the top one:
    //     switch into the child iframe, then capture + click resolve against
    //     that subframe's `webpilot_bridge` context. The old MAIN-world bridge
    //     worked in subframes, so the isolated one must too — proves the
    //     auto-injected world is created and routed per frame, not just the top.
    let sw_frame = fx.run(&["frame", "url", "/frame"]);
    assert_eq!(
        code(&sw_frame),
        0,
        "switch into child iframe failed: {}",
        stdout(&sw_frame)
    );
    let frame_cap = fx.run(&["capture", "--include", "dom"]);
    assert_eq!(
        code(&frame_cap),
        0,
        "capture inside iframe failed: {}",
        stdout(&frame_cap)
    );
    // The subframe count is scoped to the ACTIVE frame: switched into /frame, it
    // must report the one http iframe nested inside it (/nested), not 0 — a nested
    // iframe inside a switched frame must stay discoverable.
    let frame_snap: serde_json::Value =
        serde_json::from_str(&stdout(&frame_cap)).expect("frame capture json");
    assert_eq!(
        frame_snap["subframes"],
        1,
        "a switched frame's capture must count its OWN nested http iframes: {}",
        stdout(&frame_cap)
    );
    // The accessibility tree must follow the active frame, like DOM/screenshot do:
    // while switched into the iframe it must describe the iframe's own controls,
    // not the root document's. (An unscoped getFullAXTree would return the root.)
    let ax_cap = fx.run(&["capture", "--include", "accessibility"]);
    assert_eq!(
        code(&ax_cap),
        0,
        "accessibility capture inside an iframe failed: {}",
        stdout(&ax_cap)
    );
    let ax_json: serde_json::Value =
        serde_json::from_str(&stdout(&ax_cap)).expect("ax capture json");
    let ax_path = ax_json["accessibility_path"]
        .as_str()
        .expect("accessibility_path");
    let ax_tree = std::fs::read_to_string(ax_path).expect("read ax tree");
    assert!(
        ax_tree.contains("inner link"),
        "AX tree while switched into the iframe must be scoped to it (carry its own 'inner link'), not the root document"
    );

    let link_idx = index_of(&frame_cap, "link");
    let frame_click = fx.run(&["action", "click", &link_idx]);
    assert_eq!(
        code(&frame_click),
        0,
        "click inside the iframe must resolve via the subframe bridge: {}",
        stdout(&frame_click)
    );
    // 2h-upload. Upload is NOT a viewport-coordinate action: it resolves the index
    //     in the ACTIVE frame's bridge world and sets the file on a frame-
    //     independent CDP objectId, so it works on a file input INSIDE a switched
    //     iframe and is deliberately not gated by require_main_frame. Prove it
    //     lands on the iframe's own #framefile (`src` from the main-frame upload
    //     test above is still in scope).
    let frame_file_idx = index_of(&frame_cap, "framefile");
    let frame_upload = fx.run(&["action", "upload", &frame_file_idx, src]);
    assert_eq!(
        code(&frame_upload),
        0,
        "upload to a file input inside a switched iframe must succeed (not gated): {}",
        stdout(&frame_upload)
    );
    let ff_count = fx.run(&["eval", "document.getElementById('framefile').files.length"]);
    let ffc: serde_json::Value = serde_json::from_str(&stdout(&ff_count)).expect("eval json");
    assert_eq!(
        ffc["result"].as_str(),
        Some("1"),
        "upload inside an iframe must place one file on the iframe's own #framefile: {}",
        stdout(&ff_count)
    );
    // 2h. While scoped to a frame, `--annotate` must fail loud: overlay
    //     coordinates are page-viewport relative, meaningful only on the main
    //     frame — drawing them here would misalign boxes onto a viewport
    //     screenshot. (Browser mode skips silently without this guard; both now
    //     return InvalidArgument.)
    let annotate_in_frame = fx.run(&["capture", "--include", "screenshot", "--annotate"]);
    assert_eq!(
        code(&annotate_in_frame),
        7,
        "capture --annotate while an iframe is active must fail InvalidArgument (7): {}",
        stdout(&annotate_in_frame)
    );
    // 2h-pdf. Likewise, `--include pdf` while scoped to a frame must fail loud:
    //     `Page.printToPDF` is top-level only (CDP has no frame-scoped print), so
    //     it would silently render the TOP page, not the iframe the agent
    //     switched into. Reject it like `--annotate` (both modes), so the agent
    //     switches back to main rather than receiving the wrong page.
    let pdf_in_frame = fx.run(&["capture", "--include", "pdf"]);
    assert_eq!(
        code(&pdf_in_frame),
        7,
        "capture --include pdf while an iframe is active must fail InvalidArgument (7), not render the top page: {}",
        stdout(&pdf_in_frame)
    );
    // 2h-shot. `--include screenshot` while an iframe is active DEGRADES: the
    //     shot is top-level only (it would be TOP-page pixels under an
    //     iframe-labelled header), but unlike pdf/annotate it is often an
    //     add-on to a frame-scoped DOM request — so the capture succeeds, the
    //     refusal rides in `screenshot_error` (the standing screenshot-failure
    //     channel), no image is produced, and a combined dom+screenshot request
    //     still returns the valid frame-scoped DOM.
    let shot_in_frame = fx.run(&["capture", "--include", "dom", "screenshot"]);
    assert_eq!(
        code(&shot_in_frame),
        0,
        "dom+screenshot while an iframe is active must still return the DOM: {}",
        stdout(&shot_in_frame)
    );
    let shot_json: serde_json::Value =
        serde_json::from_str(&stdout(&shot_in_frame)).expect("capture json");
    assert!(
        shot_json["screenshot_error"]
            .as_str()
            .is_some_and(|e| e.contains("main-frame only")),
        "the refused screenshot must surface in screenshot_error: {}",
        stdout(&shot_in_frame)
    );
    assert!(
        shot_json["screenshot_path"].is_null(),
        "no top-page image may be produced while an iframe is active: {}",
        stdout(&shot_in_frame)
    );
    assert!(
        shot_json["elements"].is_array(),
        "the frame-scoped DOM must still be captured alongside the refused screenshot: {}",
        stdout(&shot_in_frame)
    );
    // ...but a screenshot-ONLY request has no other output to return — success
    // with no artifact would be a lie, so it refuses loud instead.
    let shot_only = fx.run(&["capture", "--include", "screenshot"]);
    assert_eq!(
        code(&shot_only),
        7,
        "a screenshot-ONLY capture while an iframe is active must be InvalidArgument (7): {}",
        stdout(&shot_only)
    );

    // 2h-top. A `target="_top"` link clicked INSIDE the switched iframe navigates
    //         the TOP frame, not the active iframe — the bridge must report it as a
    //         top navigation (`navigates`, driving `url_changed` + the main settle),
    //         never a current-frame nav. Pre-fix the iframe-scoped hint mis-classed
    //         it (`navigates:false`, `frame_navigates:true`), so the click returned
    //         success with no `url_changed` and waited the wrong frame. The click
    //         must land the TOP on /second.
    let top_cap = fx.run(&["capture", "--include", "dom"]);
    let topnav_idx = index_of(&top_cap, "topnav");
    let topnav_click = fx.run(&["action", "click", &topnav_idx]);
    assert_eq!(
        code(&topnav_click),
        0,
        "_top link click inside an iframe failed: {}",
        stdout(&topnav_click)
    );
    let topnav_json: serde_json::Value =
        serde_json::from_str(&stdout(&topnav_click)).expect("topnav click json");
    assert!(
        topnav_json["url_changed"]
            .as_str()
            .is_some_and(|u| u.ends_with("/second")),
        "a _top link clicked inside a switched iframe must navigate the TOP frame \
         and report url_changed=/second, not settle the active frame: {}",
        stdout(&topnav_click)
    );
    // Restore: the _top nav left the top on /second with no iframe — return to base
    // and re-enter /frame so the iframe-internal nav test below runs in context.
    assert_eq!(code(&fx.run(&["action", "navigate", &base])), 0);
    let _ = fx.run(&["capture", "--include", "dom"]);
    assert_eq!(
        code(&fx.run(&["frame", "url", "/frame"])),
        0,
        "re-enter /frame after the _top nav"
    );

    // 2i. A click on a link that navigates ONLY the switched iframe (not the top
    //     URL) must settle the ACTIVE frame's own navigation: the auto-capture
    //     lands on the iframe's new document, never the pre-click one. The top
    //     URL never moves, so the main-frame settle can't see this — it is the
    //     `frame_navigates` + active-frame-context wait that catches it.
    let frame_cap2 = fx.run(&["capture", "--include", "dom"]);
    let framenav_idx = index_of(&frame_cap2, "framenav");
    let framenav_click = fx.run(&["action", "click", &framenav_idx, "--capture"]);
    assert_eq!(
        code(&framenav_click),
        0,
        "iframe-internal nav click failed: {}",
        stdout(&framenav_click)
    );
    assert!(
        stdout(&framenav_click).contains("framed2btn"),
        "auto-capture after an iframe-internal navigation must show the new frame \
         document (framed2btn), not the pre-click page: {}",
        stdout(&framenav_click)
    );

    // 2i-name. The same iframe-internal settle when the link addresses the frame
    //     by NAME: the iframe is `name="innerfr"` and `#selftarget` carries
    //     `target="innerfr"`, which HTML resolves to the frame itself — exactly
    //     `_self`. The bridge must map the matching name to a current-frame nav
    //     hint, not classify it as a popup (which would skip the settle and
    //     capture the pre-click document).
    assert_eq!(code(&fx.run(&["action", "navigate", &base])), 0);
    let _ = fx.run(&["capture", "--include", "dom"]);
    assert_eq!(
        code(&fx.run(&["frame", "url", "/frame"])),
        0,
        "re-enter /frame for the named-target nav"
    );
    let cap_name = fx.run(&["capture", "--include", "dom"]);
    let selftarget_idx = index_of(&cap_name, "selftarget");
    let selftarget_click = fx.run(&["action", "click", &selftarget_idx, "--capture"]);
    assert_eq!(
        code(&selftarget_click),
        0,
        "named-target iframe-internal nav click failed: {}",
        stdout(&selftarget_click)
    );
    assert!(
        stdout(&selftarget_click).contains("framed2btn"),
        "a link targeting the frame's own NAME must settle like _self and \
         capture the new frame document (framed2btn): {}",
        stdout(&selftarget_click)
    );

    let back = fx.run(&["frame", "main"]);
    assert_eq!(code(&back), 0, "frame main failed: {}", stdout(&back));

    // 2j. A persisted active frame that VANISHED between CLI invocations surfaces
    //     as FrameNotFound on the next scoped command (exit 4 → recapture), never a
    //     SILENT retarget to the main frame. Switch into /frame, schedule the page
    //     to drop that iframe, then a fresh process's `eval` must FrameNotFound —
    //     not run in main and return a value. `frame list` is the recovery path: it
    //     resets the stale scope and reports `active_frame_id: null`.
    assert_eq!(code(&fx.run(&["action", "navigate", &base])), 0);
    let _ = fx.run(&["capture", "--include", "dom"]);
    assert_eq!(
        code(&fx.run(&["frame", "url", "/frame"])),
        0,
        "switch into /frame for the vanish test"
    );
    let _ = fx.run(&[
        "eval",
        "setTimeout(function(){window.parent.document.querySelector('iframe').remove()},50); 'scheduled'",
    ]);
    std::thread::sleep(std::time::Duration::from_millis(400));
    let stale_eval = fx.run(&["eval", "1+1"]);
    assert_eq!(
        code(&stale_eval),
        4,
        "a scoped command on a vanished persisted frame must be FrameNotFound (exit 4), \
         not a silent main-frame run: {}",
        stdout(&stale_eval)
    );
    // A `session import` carrying STORAGE runs in the active frame's bridge, so a
    // vanished frame must FrameNotFound (exit 4) — AND, because the frame preflight
    // runs BEFORE the cookie loop, the import must NOT apply its cookies (atomicity:
    // no half-import behind a storage that can't land).
    let vanish_session = home.join("vanish-session.json");
    std::fs::write(
        &vanish_session,
        br#"{"version":1,"cookies":[{"name":"vanish_canary","value":"x","domain":"127.0.0.1","path":"/","same_site":"lax","host_only":true}],"local_storage":{"k":"v"}}"#,
    )
    .expect("write vanish session fixture");
    let vsi = fx.run(&["session", "import", vanish_session.to_str().unwrap()]);
    assert_eq!(
        code(&vsi),
        4,
        "session import (with storage) on a vanished active frame must be FrameNotFound (exit 4): {}",
        stdout(&vsi)
    );
    assert!(
        !stdout(&fx.run(&["cookie", "list", &base])).contains("vanish_canary"),
        "session import must NOT apply cookies when the active frame vanished — the preflight gates BEFORE the cookie loop (atomicity): {}",
        stdout(&fx.run(&["cookie", "list", &base]))
    );

    let recover = fx.run(&["frame"]);
    assert_eq!(
        code(&recover),
        0,
        "frame list recovery failed: {}",
        stdout(&recover)
    );
    let recover_json: serde_json::Value =
        serde_json::from_str(&stdout(&recover)).expect("frame list json");
    assert!(
        recover_json["active_frame_id"].is_null(),
        "frame list must reset a vanished active frame to main and report it: {}",
        stdout(&recover)
    );
    assert_eq!(
        code(&fx.run(&["eval", "2+2"])),
        0,
        "after frame-list recovery a scoped command runs in main again"
    );

    // 2h-amb. A pattern frame selector that matches MULTIPLE frames is
    //     ambiguous — switching into whichever comes first would silently scope
    //     later commands to a frame the agent may not mean. `/twoframes` embeds
    //     two iframes with the same URL, so `frame url /framed2` matches both
    //     and must fail InvalidArgument (7), naming the ambiguity, not silently
    //     pick one. (The `frame predicate` escape hatch stays first-match.)
    assert_eq!(
        code(&fx.run(&["action", "navigate", &format!("{base}/twoframes")])),
        0,
        "navigate to /twoframes failed"
    );
    // Both same-URL iframes must be loaded as http subframes before the switch,
    // so the selector genuinely matches two. The capture confirms it — and the
    // frame set is immutable between these separate processes, so the following
    // `frame url` resolves against the same two frames.
    let pre = fx.run(&["capture", "--include", "dom"]);
    let pre_json: serde_json::Value = serde_json::from_str(&stdout(&pre)).expect("capture json");
    assert_eq!(
        pre_json["subframes"],
        2,
        "/twoframes must expose two http subframes before the ambiguity check: {}",
        stdout(&pre)
    );
    let amb = fx.run(&["frame", "url", "/framed2"]);
    assert_eq!(
        code(&amb),
        7,
        "an ambiguous frame url (two frames match) must be InvalidArgument (7), not a silent first-match: {}",
        stdout(&amb)
    );
    assert!(
        stdout(&amb).contains("frames match"),
        "the ambiguity error must name the match count: {}",
        stdout(&amb)
    );

    assert_eq!(code(&fx.run(&["action", "navigate", &base])), 0);

    // 3. A link click that navigates reports `url_changed`, `--capture`
    //    returns the NEW document (settle: committed + parsed, never the dying
    //    page), and armed monitors keep recording across the navigation even
    //    though every step here is a separate process.
    let started = fx.run(&["console", "start"]);
    assert_eq!(code(&started), 0, "console start: {}", stdout(&started));
    let cap = fx.run(&["capture", "--include", "dom"]);
    assert_eq!(code(&cap), 0);
    let nav_index = index_of(&cap, "nav");
    let navd = fx.run(&["action", "click", &nav_index, "--capture"]);
    assert_eq!(code(&navd), 0, "nav click failed: {}", stdout(&navd));
    let navd_json: serde_json::Value = serde_json::from_str(&stdout(&navd)).expect("action json");
    assert!(
        navd_json["url_changed"]
            .as_str()
            .is_some_and(|u| u.ends_with("/second")),
        "link click must report url_changed: {}",
        stdout(&navd)
    );
    assert!(
        navd_json["page_url"]
            .as_str()
            .is_some_and(|u| u.ends_with("/second")),
        "--capture must return the document the click landed on: {}",
        stdout(&navd)
    );
    // ...with its actual ELEMENTS, not an empty snapshot. The new document's
    // isolated bridge world is a fresh execution context, and for a poll cycle the
    // context map can still hand back the transitional pre-commit document — a
    // capture through it returns the right `page_url` but no elements. Asserting
    // page_url alone let that race slip; assert the content too.
    assert!(
        navd_json["elements"]
            .as_array()
            .is_some_and(|els| !els.is_empty()),
        "--capture after a navigation must return the new page's DOM elements, not an empty pre-load snapshot: {}",
        stdout(&navd)
    );

    // A submit-button click navigates with no href, so the settle must catch it
    // via the `frame_navigates` hint, not just a link's. Go back to the form page,
    // click the submit button, and the auto-capture must land on the submitted
    // document (/second), not the pre-submit page.
    let _ = fx.run(&["action", "navigate", &base]);
    let form_cap = fx.run(&["capture", "--include", "dom"]);
    let submit_index = index_of(&form_cap, "formsubmit");
    let submitted = fx.run(&["action", "click", &submit_index, "--capture"]);
    assert_eq!(
        code(&submitted),
        0,
        "submit-button click failed: {}",
        stdout(&submitted)
    );
    let submitted_json: serde_json::Value =
        serde_json::from_str(&stdout(&submitted)).expect("action json");
    assert!(
        submitted_json["page_url"]
            .as_str()
            .is_some_and(|u| u.contains("/second")),
        "a submit-button click must settle on the submitted document: {}",
        stdout(&submitted)
    );

    // Enter pressed in a form's text input submits it — a QUEUED navigation the
    // settle must wait for via the native key_press nav hint (Enter is the only
    // native key that loads a document), or `url_changed` is silently dropped and
    // a following capture races the submitted page. Focus the input, press Enter,
    // and the submit must report url_changed=/second.
    let _ = fx.run(&["action", "navigate", &base]);
    let enter_cap = fx.run(&["capture", "--include", "dom"]);
    let forminput_idx = index_of(&enter_cap, "forminput");
    let _ = fx.run(&["action", "focus", &forminput_idx]);
    let entered = fx.run(&["action", "key-press", "Enter"]);
    assert_eq!(
        code(&entered),
        0,
        "key_press Enter failed: {}",
        stdout(&entered)
    );
    let entered_json: serde_json::Value =
        serde_json::from_str(&stdout(&entered)).expect("key_press json");
    assert!(
        entered_json["url_changed"]
            .as_str()
            .is_some_and(|u| u.contains("/second")),
        "Enter in a form input must submit and report url_changed=/second — the settle waits for the queued form-submit nav via the Enter nav hint: {}",
        stdout(&entered)
    );

    // A click whose handler navigates via `location.href` (a JS navigation with no
    // href attribute the bridge could hint) must STILL be settled: headless must
    // catch the queued nav like browser mode does (pinned by the browser e2e), or
    // url_changed is dropped and --capture races the slow target.
    let _ = fx.run(&["action", "navigate", &base]);
    let slow_cap = fx.run(&["capture", "--include", "dom"]);
    let slownav_idx = index_of(&slow_cap, "slownav");
    let slownav = fx.run(&["action", "click", &slownav_idx]);
    assert_eq!(
        code(&slownav),
        0,
        "slownav click failed: {}",
        stdout(&slownav)
    );
    let slownav_json: serde_json::Value =
        serde_json::from_str(&stdout(&slownav)).expect("slownav click json");
    assert!(
        slownav_json["url_changed"]
            .as_str()
            .is_some_and(|u| u.contains("/slow")),
        "a click whose handler navigates via location.href must report url_changed=/slow \
         (the settle must catch the queued JS nav, as browser mode does): {}",
        stdout(&slownav)
    );

    let _ = fx.run(&["action", "navigate", &base]);
    let logged = fx.run(&["eval", "console.log('e2e-monitor-marker')"]);
    assert_eq!(code(&logged), 0);
    let logs = fx.run(&["console", "read"]);
    assert!(
        stdout(&logs).contains("e2e-monitor-marker"),
        "monitors must stay armed across a link-click navigation: {}",
        stdout(&logs)
    );

    // 3a-clip. A runaway `console.log` is clipped like the DOM capture: a
    //     10000-char arg must come back capped (~4096) with a marker, never the
    //     whole string ballooning the buffer and the read's CDP payload.
    let _ = fx.run(&["console", "clear"]);
    let _ = fx.run(&["eval", "console.log('Z'.repeat(10000)); 'logged'"]);
    let clip_read = fx.run(&["console", "read"]);
    let cr: serde_json::Value = serde_json::from_str(&stdout(&clip_read)).expect("clip read json");
    let clipped_msg = cr["entries"]
        .as_array()
        .and_then(|a| {
            a.iter()
                .find(|e| e["message"].as_str().is_some_and(|m| m.starts_with('Z')))
        })
        .and_then(|e| e["message"].as_str())
        .expect("clipped entry");
    assert!(
        clipped_msg.chars().count() < 5000 && clipped_msg.contains("chars]"),
        "a 10000-char log must be clipped with a marker, not stored whole: len={}",
        clipped_msg.chars().count()
    );
    // ...and the clip is CODEPOINT-safe: a 5000-emoji log (each an astral pair)
    // must clip at the 4096-codepoint boundary WITHOUT splitting a surrogate
    // pair — a lone surrogate would break the entry's JSON serialization through
    // CDP returnByValue and the read would fail. A clean exit 0 + a marker proves
    // it round-tripped intact. (`Z` above is ASCII; this is the surrogate case.)
    let _ = fx.run(&["console", "clear"]);
    let _ = fx.run(&["eval", "console.log('😀'.repeat(5000)); 'logged'"]);
    let emoji_read = fx.run(&["console", "read"]);
    assert_eq!(
        code(&emoji_read),
        0,
        "an emoji-clip read must not break serialization with a lone surrogate: {}",
        stdout(&emoji_read)
    );
    let er: serde_json::Value =
        serde_json::from_str(&stdout(&emoji_read)).expect("emoji read json");
    assert!(
        er["entries"]
            .as_array()
            .and_then(|a| a
                .iter()
                .find(|e| e["message"].as_str().is_some_and(|m| m.contains("😀"))))
            .and_then(|e| e["message"].as_str())
            .is_some_and(|m| m.contains("chars]")),
        "the emoji log must be clipped with a marker and read back intact: {}",
        stdout(&emoji_read)
    );

    // 3a-fetchthrow. `fetch()` with no args throws SYNCHRONOUSLY (a TypeError,
    //     not a rejected promise). The recorded network entry must be stamped
    //     errored, not left dangling in-flight (duration 0, no status) forever —
    //     and the page must still see the exception.
    let _ = fx.run(&["network", "start"]);
    let _ = fx.run(&["eval", "try { fetch(); } catch (e) {} 'done'"]);
    let nthrow = fx.run(&["network", "read"]);
    let nt: serde_json::Value = serde_json::from_str(&stdout(&nthrow)).expect("net throw json");
    assert!(
        nt["entries"].as_array().is_some_and(|a| a
            .iter()
            .any(|e| e["type"] == "fetch" && e["error"].is_string())),
        "a synchronous fetch() throw must stamp the entry errored, not leave it dangling: {}",
        stdout(&nthrow)
    );
    let _ = fx.run(&["console", "clear"]);
    let _ = fx.run(&["network", "clear"]);

    // 3a-safe. A page that booby-traps Array.prototype.push must NOT break its
    //     OWN console.log: the monitor hook wraps recording in try/catch and
    //     calls the original unconditionally. `ok` is true only if console.log
    //     returned normally despite a throwing push (pre-fix it threw).
    let safe = fx.run(&[
        "eval",
        "const op = Array.prototype.push; Array.prototype.push = function(){ throw new Error('x'); }; \
         let ok = false; try { console.log('safe-probe'); ok = true; } catch (e) {} \
         Array.prototype.push = op; ok",
    ]);
    let sj: serde_json::Value = serde_json::from_str(&stdout(&safe)).expect("safe eval json");
    assert_eq!(
        sj["result"].as_str(),
        Some("true"),
        "a throwing Array.prototype.push must not break the page's own console.log: {}",
        stdout(&safe)
    );
    let _ = fx.run(&["console", "clear"]);

    // 3a-trunc. `truncated` reflects ACTUAL eviction, not `length >= cap`: at
    //     exactly the cap (500) nothing has been dropped yet, so truncated is
    //     false; one more entry evicts the oldest and flips it true.
    let _ = fx.run(&[
        "eval",
        "for (let i = 0; i < 500; i++) console.log('row' + i); 'filled'",
    ]);
    let at_cap = fx.run(&["console", "read"]);
    let ac: serde_json::Value = serde_json::from_str(&stdout(&at_cap)).expect("at-cap json");
    assert_eq!(
        ac["truncated"],
        false,
        "a buffer at exactly the cap with nothing evicted must not be truncated: {}",
        stdout(&at_cap)
    );
    let _ = fx.run(&["eval", "console.log('overflow'); 'over'"]);
    let over = fx.run(&["console", "read"]);
    let ov: serde_json::Value = serde_json::from_str(&stdout(&over)).expect("over json");
    assert_eq!(
        ov["truncated"],
        true,
        "one entry past the cap evicts the oldest and must report truncated: {}",
        stdout(&over)
    );
    let _ = fx.run(&["console", "clear"]);
    // ...and clear resets the eviction flag, so a fresh small buffer is not truncated.
    let _ = fx.run(&["eval", "console.log('after-clear'); 'x'"]);
    let post_clear = fx.run(&["console", "read"]);
    let pc: serde_json::Value =
        serde_json::from_str(&stdout(&post_clear)).expect("post-clear json");
    assert_eq!(
        pc["truncated"],
        false,
        "clear must reset the eviction flag — a fresh buffer is not truncated: {}",
        stdout(&post_clear)
    );

    // 3b. The `eval` gate covers monitor re-injection: a deny that lands AFTER
    //     `console start` must stop the MAIN-world hooks from re-arming on the
    //     next document — `reinstall_monitors` re-checks the gate (browser mode
    //     mirrors this via host-attached verdicts). First confirm a log IS
    //     captured while allowed, so the deny case can't pass on a timing miss.
    //     `navigate` awaits `reinstall_monitors`, so once it returns the armed
    //     hook is in place; drive the log via `eval` (not a page startup timer)
    //     so the check can't race the re-arm — a log a page fires during its own
    //     startup, before the hook re-installs, is by design NOT captured
    //     (extension.md), and timing that against a fixed sleep is flaky.
    let _ = fx.run(&["console", "clear"]);
    assert_eq!(code(&fx.run(&["action", "navigate", &base])), 0);
    let _ = fx.run(&["eval", "console.log('postnav-monitor-marker')"]);
    assert!(
        stdout(&fx.run(&["console", "read"])).contains("postnav-monitor-marker"),
        "an armed monitor must capture a log on the document navigated to"
    );
    let _ = fx.run(&["console", "clear"]);
    // Arm the network monitor too, so the suppressed-read signal below is
    // pinned for BOTH monitors (each has its own hook and its own read path).
    assert_eq!(
        code(&fx.run(&["network", "start"])),
        0,
        "network start (deny setup) failed"
    );
    let deny = fx.run(&["policy", "set", "--operation", "eval", "--verdict", "deny"]);
    assert_eq!(code(&deny), 0, "policy set eval deny: {}", stdout(&deny));
    let _ = fx.run(&["action", "navigate", &base]);
    assert_eq!(
        code(&fx.run(&["action", "navigate", &format!("{base}/log")])),
        0,
        "navigate is allowed (only eval was denied)"
    );
    std::thread::sleep(std::time::Duration::from_millis(700));
    // The suppressed monitor is EXPLICIT: the armed flag survived but this
    // document carries no hook, so `console read` is a typed InvalidArgument
    // naming the suppression — never an empty success the agent would read as
    // "the page logged nothing".
    let suppressed_read = fx.run(&["console", "read"]);
    assert_eq!(
        code(&suppressed_read),
        7,
        "console read on an eval-deny-suppressed monitor must be InvalidArgument (7), not empty success: {}",
        stdout(&suppressed_read)
    );
    assert!(
        stdout(&suppressed_read).contains("not installed"),
        "the suppressed-monitor error must name the cause: {}",
        stdout(&suppressed_read)
    );
    // The network monitor's read path carries the same explicit signal.
    let suppressed_net = fx.run(&["network", "read"]);
    assert_eq!(
        code(&suppressed_net),
        7,
        "network read on an eval-deny-suppressed monitor must be InvalidArgument (7): {}",
        stdout(&suppressed_net)
    );
    assert!(
        stdout(&suppressed_net).contains("not installed"),
        "the suppressed network-monitor error must name the cause: {}",
        stdout(&suppressed_net)
    );
    let _ = fx.run(&["network", "clear"]);
    let _ = fx.run(&["policy", "clear"]);
    // Restore the working page for the steps below (the deny test left us on /log).
    assert_eq!(code(&fx.run(&["action", "navigate", &base])), 0);

    // 3c. An armed monitor must survive a navigation WebPilot did NOT drive (a
    //     page-initiated `location.href` redirect). Between two CLI processes
    //     nothing watches for that nav, so its document swap wipes the MAIN-world
    //     hooks with no `reinstall_monitors` to fire — the monitor would silently
    //     go dead. `open` re-arms an armed monitor against the current document,
    //     so a log emitted after the out-of-band nav is still captured.
    let _ = fx.run(&["console", "start"]);
    let _ = fx.run(&["console", "clear"]);
    // A bare eval that sets location.href is an OUT-OF-BAND nav — not an `action
    // navigate`, so navigate_reconnect's post-nav reinstall never runs for it.
    let _ = fx.run(&[
        "eval",
        &format!("window.location.href='{base}/frame'; 'go'"),
    ]);
    // Settle on /frame deterministically: #topnav exists only in the FRAME doc,
    // so the wait cannot pass on the pre-nav page.
    assert_eq!(
        code(&fx.run(&["wait", "--timeout", "5", "selector", "#topnav"])),
        0,
        "out-of-band nav must reach the /frame document"
    );
    let _ = fx.run(&["eval", "console.log('oob-recovered')"]);
    assert!(
        stdout(&fx.run(&["console", "read"])).contains("oob-recovered"),
        "an armed monitor must re-arm on open after an out-of-band navigation and \
         capture a log emitted in the new document (open re-applies armed monitors)"
    );
    // Restore the working page for the steps below.
    assert_eq!(code(&fx.run(&["action", "navigate", &base])), 0);

    // 4. A click-opened tab (`rel=noopener`, so correlation cannot rely on
    //    `window.opener`) is reported as `new_tab` and becomes the active tab —
    //    the pin follows the agent's working tab.
    let cap = fx.run(&["capture", "--include", "dom"]);
    assert_eq!(code(&cap), 0);
    let pop_index = index_of(&cap, "pop");
    let popped = fx.run(&["action", "click", &pop_index]);
    assert_eq!(code(&popped), 0, "popup click failed: {}", stdout(&popped));
    let popped_json: serde_json::Value =
        serde_json::from_str(&stdout(&popped)).expect("action json");
    assert!(
        popped_json["new_tab"]["id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "popup must be reported as new_tab: {}",
        stdout(&popped)
    );
    let status = fx.run(&["status"]);
    let status_json: serde_json::Value =
        serde_json::from_str(&stdout(&status)).expect("status json");
    assert!(
        status_json["tab_url"]
            .as_str()
            .is_some_and(|u| u.ends_with("/second")),
        "the popup must be the active tab after adoption: {}",
        stdout(&status)
    );

    // 5. Zero-false-positive guard: a same-document navigation (pushState)
    //    changes location.href but keeps the DOM. An index from the prior
    //    capture must STILL resolve — the snapshot binds to node identity, not
    //    URL, so invalidating here would be a false positive (a regression if
    //    anyone "fixes" staleness by comparing URLs).
    let cap = fx.run(&["capture", "--include", "dom"]);
    assert_eq!(code(&cap), 0);
    let go_index = index_of(&cap, "go");
    let pushed = fx.run(&[
        "eval",
        "history.pushState({}, '', '/changed'); location.pathname",
    ]);
    assert_eq!(
        code(&pushed),
        0,
        "pushState eval failed: {}",
        stdout(&pushed)
    );
    let pj: serde_json::Value = serde_json::from_str(&stdout(&pushed)).expect("eval json");
    assert_eq!(
        pj["result"].as_str(),
        Some("\"/changed\""),
        "pushState must change the URL so the same-document guard is exercised: {}",
        stdout(&pushed)
    );
    let after_nav = fx.run(&["action", "click", &go_index]);
    assert_eq!(
        code(&after_nav),
        0,
        "a URL change with the element still live must NOT raise StaleSnapshot: {}",
        stdout(&after_nav)
    );

    // 6. Stale-snapshot guard: remove the button from the DOM (out of band, via
    //    eval — which never touches the bridge snapshot), then click its old
    //    index. It must fail typed, not silently click a different element.
    let recap = fx.run(&["capture", "--include", "dom"]);
    assert_eq!(code(&recap), 0);
    let go_index = index_of(&recap, "go");
    let _ = fx.run(&["eval", "document.getElementById('go').remove()"]);
    let stale = fx.run(&["action", "click", &go_index]);
    assert_eq!(
        code(&stale),
        4,
        "stale click must exit 4: {}",
        stdout(&stale)
    );
    assert!(
        stdout(&stale).contains("StaleSnapshot"),
        "stale click must be StaleSnapshot: {}",
        stdout(&stale)
    );

    // 6b. Opening a new tab rebinds the page session AND refreshes the cached
    //     main frame id, so a bridge op on the new tab's main frame resolves
    //     against the NEW tab's isolated world — not the previous tab's frame
    //     id. Without the refresh this capture/click would FrameNotFound: the
    //     bridge context is looked up by main frame id, which differs per tab.
    let newtab = fx.run(&["tab", "new", &base]);
    assert_eq!(code(&newtab), 0, "tab new failed: {}", stdout(&newtab));
    let nt_cap = fx.run(&["capture", "--include", "dom"]);
    assert_eq!(
        code(&nt_cap),
        0,
        "capture on the new tab's main frame failed: {}",
        stdout(&nt_cap)
    );
    let nt: serde_json::Value = serde_json::from_str(&stdout(&nt_cap)).expect("capture json");
    assert!(
        nt["elements"]
            .as_array()
            .is_some_and(|a| a.iter().any(|e| e["tag"] == "button")),
        "new-tab capture must resolve the new tab's bridge (refreshed main frame id): {}",
        stdout(&nt_cap)
    );
    let nt_go = index_of(&nt_cap, "go");
    let nt_click = fx.run(&["action", "click", &nt_go]);
    assert_eq!(
        code(&nt_click),
        0,
        "click on the new tab must resolve via its bridge: {}",
        stdout(&nt_click)
    );

    // 6c. Armed monitors follow the pin across a tab-new MOVE, not just a
    //     same-tab navigation. `tab new` routes through `do_tab_switch`, which
    //     DEFERS the monitor arm (the new tab is still about:blank there and the
    //     imminent load would wipe it) and re-arms after the document settles —
    //     and `do_tab_new` AWAITS that re-arm before returning, so once `tab new`
    //     returns the new tab's hooks are in place. Drive an explicit log via
    //     `eval` (not the page's own startup timer): a startup log can fire
    //     BEFORE the re-arm completes — `tab new` is slower than a same-tab
    //     navigate (create + switch + settle + re-arm), so its re-arm easily
    //     trails a +200ms timer — and such a startup log is by design not
    //     captured (extension.md), which would make the test race the re-arm. An
    //     eval after `tab new` returns cannot. Without the deferred re-arm the new
    //     tab carries no hooks and the log is lost. (Browser-mode mirror: the
    //     monitor-follow step in e2e_browser.)
    let _ = fx.run(&["console", "clear"]);
    let mtab = fx.run(&["tab", "new", &base]);
    assert_eq!(
        code(&mtab),
        0,
        "tab new for monitor-follow failed: {}",
        stdout(&mtab)
    );
    let _ = fx.run(&["eval", "console.log('postnav-monitor-marker')"]);
    assert!(
        stdout(&fx.run(&["console", "read"])).contains("postnav-monitor-marker"),
        "an armed console monitor must follow the pin onto a new tab: {}",
        stdout(&fx.run(&["console", "read"]))
    );
    assert_eq!(code(&fx.run(&["action", "navigate", &base])), 0);

    // 7. Policy: a deny rule is enforced at the transport boundary before the
    //    page is touched, in this (headless) mode. Re-capture first so the
    //    index is otherwise valid — proving policy, not staleness, blocks it.
    let recap = fx.run(&["capture", "--include", "dom"]);
    assert_eq!(code(&recap), 0);
    let set = fx.run(&["policy", "set", "--operation", "click", "--verdict", "deny"]);
    assert_eq!(code(&set), 0, "policy set failed: {}", stdout(&set));
    let denied = fx.run(&["action", "click", "1"]);
    assert_eq!(
        code(&denied),
        6,
        "denied click must exit 6: {}",
        stdout(&denied)
    );
    assert!(
        stdout(&denied).contains("PolicyDenied"),
        "{}",
        stdout(&denied)
    );
    let clear = fx.run(&["policy", "clear"]);
    assert_eq!(code(&clear), 0);

    // 7b. A CSP-strict iframe (`script-src 'self'`, no unsafe-eval) keeps the
    //     full frame surface working — switch by NAME, eval inside the frame,
    //     and a `frame find` predicate: CDP-routed evaluation is not subject
    //     to page CSP, so hardening a page must not cost the agent its eval.
    //     Locks the same contract the browser-mode suite asserts.
    let nav = fx.run(&["action", "navigate", &format!("{base}/csp")]);
    assert_eq!(code(&nav), 0, "csp navigate failed: {}", stdout(&nav));
    let sw = fx.run(&["frame", "switch", "cspframe"]);
    assert_eq!(
        code(&sw),
        0,
        "csp frame switch by name failed: {}",
        stdout(&sw)
    );
    // The switch response carries the frame's name, so the agent can re-address it
    // by `frame switch <name>` — and both modes now populate it identically.
    assert!(
        stdout(&sw).contains("cspframe"),
        "frame switch must report the matched frame's name: {}",
        stdout(&sw)
    );
    let title = fx.run(&["eval", "document.title"]);
    assert_eq!(code(&title), 0, "csp frame eval failed: {}", stdout(&title));
    assert!(
        stdout(&title).contains("cspframe"),
        "eval must run inside the CSP-strict frame: {}",
        stdout(&title)
    );
    let back_to_main = fx.run(&["frame", "main"]);
    assert_eq!(code(&back_to_main), 0);
    let found = fx.run(&["frame", "find", "document.title === 'cspframe'"]);
    assert_eq!(
        code(&found),
        0,
        "predicate find must work inside a CSP-strict frame: {}",
        stdout(&found)
    );
    let title = fx.run(&["eval", "document.title"]);
    assert!(
        stdout(&title).contains("cspframe"),
        "predicate match must scope eval to the found frame: {}",
        stdout(&title)
    );
    let back_to_main = fx.run(&["frame", "main"]);
    assert_eq!(code(&back_to_main), 0);

    // 7c. A STATEMENT-form predicate (not a bare expression) must find the frame
    //     too: `frame find` shares `eval`'s compile-then-evaluate form decision,
    //     so multi-statement predicates behave identically to browser mode's
    //     cdpEval — the parity the shared `eval_form` guarantees.
    let found_stmt = fx.run(&[
        "frame",
        "find",
        "const t = document.title; t === 'cspframe'",
    ]);
    assert_eq!(
        code(&found_stmt),
        0,
        "statement-form predicate must find the frame (eval_form parity): {}",
        stdout(&found_stmt)
    );
    // And a statement-form eval runs inside the CSP frame, returning its value.
    let stmt_eval = fx.run(&["eval", "const x = document.title; x"]);
    assert!(
        stdout(&stmt_eval).contains("cspframe"),
        "statement-form eval must return a value inside a CSP frame: {}",
        stdout(&stmt_eval)
    );
    let back_to_main = fx.run(&["frame", "main"]);
    assert_eq!(code(&back_to_main), 0);

    // 7c-ax. An AX-only capture against a DEAD frame pin is a typed
    //     FrameNotFound (exit 4 → recapture / `frame main`), not the generic
    //     Other a raw getFullAXTree surfaces — the pin is validated through
    //     the same resolver every bridge call uses (browser mode resolves the
    //     live context the same way). Switch into the fixture iframe, navigate
    //     the TOP page out-of-band (the pin is deliberately KEPT on an
    //     out-of-band nav), then ask for the AX tree.
    assert_eq!(
        code(&fx.run(&["capture", "--include", "dom", "--url", &base])),
        0,
        "navigate for the dead-pin AX test failed"
    );
    // A predicate true in MORE than one frame fails loud (the strict-selector
    // contract, completing it for predicates): base embeds /frame and its
    // /nested child, so `true` matches both — exit 7 naming the frames, and
    // the active scope is untouched.
    let amb_pred = fx.run(&["frame", "find", "true"]);
    assert_eq!(
        code(&amb_pred),
        7,
        "an ambiguous frame predicate must fail loud, not first-match: {}",
        stdout(&amb_pred)
    );
    assert!(
        stdout(&amb_pred).contains("frames match"),
        "the ambiguity error must name the frames: {}",
        stdout(&amb_pred)
    );
    assert_eq!(
        code(&fx.run(&["frame", "url", "/frame"])),
        0,
        "frame switch for the dead-pin AX test failed"
    );
    // `window.top`: eval runs in the SWITCHED frame, and it is the TOP page
    // that must navigate away (killing the pinned frame). Settle via the
    // pin-independent `tab` list — every frame-scoped read is dead by design
    // here, which is exactly what the test exercises.
    let _ = fx.run(&["eval", "window.top.location.href='/second'; 'go'"]);
    let mut settled = false;
    for _ in 0..40 {
        if stdout(&fx.run(&["tab"])).contains("/second") {
            settled = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    assert!(settled, "the out-of-band top nav must reach /second");
    let dead_ax = fx.run(&["capture", "--include", "accessibility"]);
    assert_eq!(
        code(&dead_ax),
        4,
        "an AX capture on a dead frame pin must be FrameNotFound (4), not Other: {}",
        stdout(&dead_ax)
    );
    assert_eq!(
        code(&fx.run(&["frame", "main"])),
        0,
        "frame main must clear the dead pin"
    );

    // 7d. Device emulation persists across CLI processes. A UA override does
    //     NOT survive a CDP client disconnect on its own, so `device set` must
    //     persist it and `open` must re-apply it — every WebPilot command is a
    //     fresh process re-attaching to the one Chrome. Set, then read UA +
    //     viewport in a SEPARATE invocation; reset must revert both.
    let dev = fx.run(&[
        "device",
        "set",
        "--width",
        "390",
        "--height",
        "844",
        "--scale",
        "3",
        "--user-agent",
        "WP-E2E-UA/1",
    ]);
    assert_eq!(code(&dev), 0, "device set failed: {}", stdout(&dev));
    // The result carries the COMPLETE applied state on the JSON surface —
    // `scale` (used to be dropped from JSON) AND the user-agent presence (the
    // human line used to drop it). Neither alone reflected what was emulated.
    let devj: serde_json::Value = serde_json::from_str(&stdout(&dev)).expect("device set json");
    assert_eq!(
        devj["scale"].as_f64(),
        Some(3.0),
        "device set JSON must carry the applied scale: {}",
        stdout(&dev)
    );
    assert_eq!(
        devj["user_agent"].as_str(),
        Some("WP-E2E-UA/1"),
        "device set JSON must carry the applied user agent: {}",
        stdout(&dev)
    );
    let ua = fx.run(&["eval", "navigator.userAgent"]);
    assert!(
        stdout(&ua).contains("WP-E2E-UA/1"),
        "device UA override must survive into the next process: {}",
        stdout(&ua)
    );
    let vw = fx.run(&["eval", "window.innerWidth"]);
    let vwj: serde_json::Value = serde_json::from_str(&stdout(&vw)).expect("eval json");
    assert_eq!(
        vwj["result"].as_str(),
        Some("390"),
        "device metrics must persist into the next process: {}",
        stdout(&vw)
    );
    let dreset = fx.run(&["device", "reset"]);
    assert_eq!(code(&dreset), 0, "device reset failed: {}", stdout(&dreset));
    let ua2 = fx.run(&["eval", "navigator.userAgent"]);
    assert_eq!(code(&ua2), 0, "UA eval failed: {}", stdout(&ua2));
    let ua2j: serde_json::Value = serde_json::from_str(&stdout(&ua2)).expect("eval json");
    assert!(
        !ua2j["result"]
            .as_str()
            .unwrap_or_default()
            .contains("WP-E2E-UA/1"),
        "device reset must clear the persisted UA override: {}",
        stdout(&ua2)
    );

    // 7e. Wait + drag on a clean page.
    let cap_w = fx.run(&["capture", "--include", "dom", "--url", &base]);
    assert_eq!(
        code(&cap_w),
        0,
        "wait-page capture failed: {}",
        stdout(&cap_w)
    );

    // A CDP "invalid params" rejection (here, a cookie URL with no scheme) is a
    // typed InvalidArgument (exit 7), not a leaked "CDP error" Other (exit 1).
    let bad_cookie = fx.run(&["cookie", "set", "not-a-url", "k", "v"]);
    assert_eq!(
        code(&bad_cookie),
        7,
        "a malformed cookie URL must be a typed InvalidArgument, not a leaked CDP error: {}",
        stdout(&bad_cookie)
    );

    // `cookie set` can specify SameSite and an expiry, and `cookie list` reports
    // them back — a faithful round-trip the read side already supported but the
    // write side could not. A `--expires` makes a persistent cookie (carries an
    // expiration); omitting it stays a session cookie (no expiration field).
    let cset = fx.run(&[
        "cookie",
        "set",
        &base,
        "sess",
        "tok",
        "--same-site",
        "lax",
        "--expires",
        "1900000000",
        "--httponly",
    ]);
    assert_eq!(
        code(&cset),
        0,
        "cookie set with attributes failed: {}",
        stdout(&cset)
    );
    let clist = fx.run(&["cookie", "list", &base]);
    let cj: serde_json::Value = serde_json::from_str(&stdout(&clist)).expect("cookie list json");
    let sess = cj
        .as_array()
        .expect("cookie list is a JSON array")
        .iter()
        .find(|c| c["name"] == "sess")
        .expect("the cookie just set is present");
    assert_eq!(
        sess["same_site"].as_str(),
        Some("lax"),
        "cookie set --same-site must round-trip through cookie list: {}",
        stdout(&clist)
    );
    assert!(
        sess["expiration"].is_number(),
        "a cookie set with --expires must be persistent (carry an expiration): {}",
        stdout(&clist)
    );
    assert_eq!(
        sess["http_only"].as_bool(),
        Some(true),
        "cookie set --httponly must round-trip: {}",
        stdout(&clist)
    );
    let cset2 = fx.run(&[
        "cookie",
        "set",
        &base,
        "ses2",
        "v2",
        "--same-site",
        "strict",
    ]);
    assert_eq!(
        code(&cset2),
        0,
        "session-cookie set failed: {}",
        stdout(&cset2)
    );
    let clist2 = fx.run(&["cookie", "list", &base]);
    let cj2: serde_json::Value = serde_json::from_str(&stdout(&clist2)).expect("cookie list json");
    let ses2 = cj2
        .as_array()
        .expect("cookie list is a JSON array")
        .iter()
        .find(|c| c["name"] == "ses2")
        .expect("the session cookie is present");
    assert_eq!(
        ses2["same_site"].as_str(),
        Some("strict"),
        "strict SameSite must round-trip: {}",
        stdout(&clist2)
    );
    assert!(
        ses2["expiration"].is_null(),
        "a cookie set without --expires must stay a session cookie (no expiration): {}",
        stdout(&clist2)
    );

    // A SameSite=None cookie REQUIRES Secure — Chrome silently refuses it
    // otherwise (`Network.setCookie` returns success:false). The set must
    // surface that refusal as InvalidArgument (exit 7), not a false success that
    // hides a cookie the agent's auth depends on, and the refused cookie must be
    // absent from the list.
    let none_no_secure = fx.run(&["cookie", "set", &base, "nsec", "v", "--same-site", "none"]);
    assert_eq!(
        code(&none_no_secure),
        7,
        "cookie set --same-site none without --secure must fail InvalidArgument (7), not falsely succeed: {}",
        stdout(&none_no_secure)
    );
    assert!(
        !stdout(&fx.run(&["cookie", "list", &base])).contains("nsec"),
        "a cookie Chrome refused must not appear in the list: {}",
        stdout(&fx.run(&["cookie", "list", &base]))
    );

    // `cookie delete` mirrors `cookie get`'s contract: deleting an ABSENT
    // cookie is the typed CookieNotFound (exit 4), never a silent success that
    // claims a deletion which removed nothing — and a real delete reports HOW
    // MANY scoped cookies it removed (same-name cookies can coexist across
    // domain/path scopes).
    let del_absent = fx.run(&["cookie", "delete", &base, "no_such_cookie"]);
    assert_eq!(
        code(&del_absent),
        4,
        "deleting an absent cookie must be CookieNotFound (4), not silent success: {}",
        stdout(&del_absent)
    );
    let del_real = fx.run(&["cookie", "delete", &base, "ses2"]);
    assert_eq!(
        code(&del_real),
        0,
        "deleting an existing cookie failed: {}",
        stdout(&del_real)
    );
    assert!(
        stdout(&del_real).contains("Deleted 1 cookie(s)"),
        "the delete must report the scoped count: {}",
        stdout(&del_real)
    );
    assert!(
        !stdout(&fx.run(&["cookie", "list", &base])).contains("ses2"),
        "the deleted cookie must be gone from the list"
    );

    // `cookie get NAME` of an ABSENT cookie is a typed not-found (exit 4), like
    // `find`/`click` on a missing target — not a "(0 cookies)" list reported as
    // success (exit 0), which an agent checking an auth cookie by exit code would
    // misread. A present cookie (`sess`, set above) still resolves.
    let miss = fx.run(&["cookie", "get", &base, "no_such_cookie"]);
    assert_eq!(
        code(&miss),
        4,
        "cookie get of an absent cookie must be a typed not-found (exit 4), not a 0-item success: {}",
        stdout(&miss)
    );
    assert_eq!(
        code(&fx.run(&["cookie", "get", &base, "sess"])),
        0,
        "cookie get of a present cookie must still succeed: {}",
        stdout(&fx.run(&["cookie", "get", &base, "sess"]))
    );

    // `session import` of a non-object JSON (here an array) is a typed
    // InvalidArgument (exit 7), never a false `success` reporting an import that
    // applied nothing.
    let bad_session = home.join("bad-session.json");
    std::fs::write(&bad_session, b"[]").expect("write bad session fixture");
    let bs = fx.run(&["session", "import", bad_session.to_str().unwrap()]);
    assert_eq!(
        code(&bs),
        7,
        "a non-object session file must be a typed InvalidArgument, not a false success: {}",
        stdout(&bs)
    );
    // A NEWER schema version is rejected even when written as a non-integer like
    // `1.5`: read as a number (not `as_u64`, which would see `1.5` as absent and
    // let it through), so a too-new file fails identically to browser mode, which
    // compares the version numerically.
    let newer_session = home.join("newer-session.json");
    std::fs::write(&newer_session, br#"{"version":1.5,"cookies":[]}"#)
        .expect("write newer session fixture");
    let ns = fx.run(&["session", "import", newer_session.to_str().unwrap()]);
    assert_eq!(
        code(&ns),
        7,
        "a newer (even non-integer) schema version must be rejected: {}",
        stdout(&ns)
    );
    // `session import` is ATOMIC across cookies and storage: a non-string
    // local_storage VALUE (Web Storage holds only strings) is rejected up front,
    // BEFORE any cookie is applied — so a malformed file never leaves cookies
    // mutated behind a storage reject. Import a valid host-only canary cookie
    // alongside a numeric storage value; the import must fail (exit 7) AND the
    // canary must not appear in the cookie store. (Pre-fix the cookie loop ran
    // first, so the canary leaked despite the error.)
    let atomic_session = home.join("atomic-session.json");
    std::fs::write(
        &atomic_session,
        br#"{"version":1,"cookies":[{"name":"atomic_canary","value":"x","domain":"127.0.0.1","path":"/","same_site":"lax","host_only":true}],"local_storage":{"k":1}}"#,
    )
    .expect("write atomic session fixture");
    let asy = fx.run(&["session", "import", atomic_session.to_str().unwrap()]);
    assert_eq!(
        code(&asy),
        7,
        "a non-string storage value must reject the whole import (exit 7): {}",
        stdout(&asy)
    );
    assert!(
        !stdout(&fx.run(&["cookie", "list", &base])).contains("atomic_canary"),
        "session import must NOT apply cookies when storage validation fails (atomicity): {}",
        stdout(&fx.run(&["cookie", "list", &base]))
    );

    // A cookie Chrome REFUSES during import (SameSite=None defaults to insecure,
    // which Chrome rejects) must be reported as not-imported, not slipped through
    // while the import claims full success — the cookie loop now counts a
    // `Network.setCookie` `success:false`, not only a transport error, so a
    // refused auth cookie surfaces instead of a session quietly missing it.
    let refused_session = home.join("refused-session.json");
    std::fs::write(
        &refused_session,
        br#"{"version":1,"cookies":[{"name":"refusedck","value":"v","domain":"127.0.0.1","path":"/","same_site":"none"}]}"#,
    )
    .expect("write refused-cookie session fixture");
    let ri = fx.run(&["session", "import", refused_session.to_str().unwrap()]);
    assert!(
        stdout(&ri).contains("refused by the browser"),
        "a session import whose cookie Chrome refuses must report it as not-imported, not silent success: {}",
        stdout(&ri)
    );

    // Storage is ORIGIN-scoped: an import whose recorded export origin differs
    // from the current page's is a typed InvalidArgument naming both origins,
    // BEFORE any write — never origin-A state silently written into origin B
    // under a success status. A matching origin imports as before. (Cookies
    // are unaffected either way — each carries its own domain.)
    assert_eq!(
        code(&fx.run(&["action", "navigate", &base])),
        0,
        "navigate for the origin-scope test failed"
    );
    // A localStorage key literally named `__proto__` round-trips intact. Export
    // used to drop it (a plain `{}` accumulator's `obj["__proto__"]=v` hits the
    // prototype setter, setting no own property), and the headless import
    // inlined the payload as an object literal — another `{"__proto__":…}`
    // setter. Both fixed (Object.create(null) export + JSON.parse import). Runs
    // on `base` (same origin → import allowed).
    let _ = fx.run(&[
        "eval",
        "localStorage.clear(); localStorage.setItem('__proto__','protoval'); 'set'",
    ]);
    let proto_session = home.join("proto-session.json");
    let pe = fx.run(&[
        "session",
        "export",
        "--output",
        proto_session.to_str().unwrap(),
    ]);
    assert_eq!(code(&pe), 0, "proto-key export failed: {}", stdout(&pe));
    assert!(
        std::fs::read_to_string(&proto_session)
            .unwrap()
            .contains("__proto__"),
        "export must carry a __proto__ localStorage key, not drop it via the prototype setter"
    );
    let _ = fx.run(&["eval", "localStorage.clear(); 'c'"]);
    let pi = fx.run(&["session", "import", proto_session.to_str().unwrap()]);
    assert_eq!(code(&pi), 0, "proto-key import failed: {}", stdout(&pi));
    let pr = fx.run(&["eval", "localStorage.getItem('__proto__')==='protoval'"]);
    let prj: serde_json::Value = serde_json::from_str(&stdout(&pr)).expect("proto eval json");
    assert_eq!(
        prj["result"].as_str(),
        Some("true"),
        "a __proto__ localStorage key must survive the session round-trip: {}",
        stdout(&pr)
    );
    let _ = fx.run(&["eval", "localStorage.clear(); 'c'"]);

    let foreign_session = home.join("foreign-origin-session.json");
    std::fs::write(
        &foreign_session,
        br#"{"version":1,"origin":"http://192.0.2.1:1","local_storage":{"wp_foreign":"1"}}"#,
    )
    .expect("write foreign-origin session fixture");
    let fo = fx.run(&["session", "import", foreign_session.to_str().unwrap()]);
    assert_eq!(
        code(&fo),
        7,
        "importing another origin's storage must be a typed InvalidArgument, not a silent cross-origin write: {}",
        stdout(&fo)
    );
    assert!(
        stdout(&fo).contains("navigate there before importing"),
        "the origin error must point at the remediation: {}",
        stdout(&fo)
    );
    assert!(
        stdout(&fx.run(&["eval", "localStorage.getItem('wp_foreign') === null"])).contains("true"),
        "a rejected cross-origin import must write NOTHING"
    );
    // An OPAQUE origin serializes as the string "null" — shared by every
    // file:// and sandboxed page while being same-origin with nothing, even
    // itself. The gate refuses it on either side rather than matching two
    // unrelated pages by their serialization.
    let opaque_session = home.join("opaque-origin-session.json");
    std::fs::write(
        &opaque_session,
        br#"{"version":1,"origin":"null","local_storage":{"wp_opaque":"1"}}"#,
    )
    .expect("write opaque-origin session fixture");
    let oq = fx.run(&["session", "import", opaque_session.to_str().unwrap()]);
    assert_eq!(
        code(&oq),
        7,
        "an opaque export origin must be refused, not string-matched: {}",
        stdout(&oq)
    );

    // CHIPS: a PARTITIONED cookie round-trips with its partition key — the key
    // is part of the cookie's IDENTITY, so an export that dropped it would
    // re-import an unpartitioned twin under a clean success (silently absent
    // from the embedded context that set it). 127.0.0.1 is a trustworthy
    // origin, so Secure+Partitioned is settable over plain http.
    let _ = fx.run(&[
        "eval",
        "document.cookie = 'wp_part=pv; Secure; Path=/; Partitioned'; 'set'",
    ]);
    let part_session = home.join("partitioned-session.json");
    let pexp = fx.run(&[
        "session",
        "export",
        "--output",
        part_session.to_str().unwrap(),
    ]);
    assert_eq!(
        code(&pexp),
        0,
        "session export (partitioned) failed: {}",
        stdout(&pexp)
    );
    let pfile = std::fs::read_to_string(&part_session).expect("read exported session");
    assert!(
        pfile.contains("wp_part") && pfile.contains("partition_key"),
        "the export must carry the partitioned cookie WITH its partition_key: {pfile}"
    );
    let pim = fx.run(&["session", "import", part_session.to_str().unwrap()]);
    assert_eq!(
        code(&pim),
        0,
        "re-importing a partitioned cookie must restore it into its partition, not refuse: {}",
        stdout(&pim)
    );
    // ...and the partitioned cookie is first-class in the COOKIE family too:
    // `cookie list` shows it with its partition (identity-distinguishing), and
    // `cookie delete` actually removes it from its partition — measured before
    // the fix: a partition-less `Network.deleteCookies` left the cookie alive
    // behind a clean "Deleted 1". The survival check below catches that lie.
    let plist = fx.run(&["cookie", "list", &base]);
    assert!(
        stdout(&plist).contains("wp_part") && stdout(&plist).contains("partition_key"),
        "cookie list must show the partitioned cookie WITH its partition: {}",
        stdout(&plist)
    );
    let pdel = fx.run(&["cookie", "delete", &base, "wp_part"]);
    assert_eq!(
        code(&pdel),
        0,
        "partitioned cookie delete failed: {}",
        stdout(&pdel)
    );
    assert!(
        stdout(&pdel).contains("Deleted 1"),
        "partitioned delete must report its true count: {}",
        stdout(&pdel)
    );
    assert!(
        !stdout(&fx.run(&["cookie", "list", &base])).contains("wp_part"),
        "the partitioned cookie must actually be GONE after delete — survival here \
         means the partition key was dropped on the delete path: {}",
        stdout(&fx.run(&["cookie", "list", &base]))
    );
    assert!(
        stdout(&oq).contains("opaque origin"),
        "the opaque-origin error must name the cause: {}",
        stdout(&oq)
    );
    let matching_session = home.join("matching-origin-session.json");
    std::fs::write(
        &matching_session,
        format!(r#"{{"version":1,"origin":"{base}","local_storage":{{"wp_origin_ok":"1"}}}}"#)
            .as_bytes(),
    )
    .expect("write matching-origin session fixture");
    assert_eq!(
        code(&fx.run(&["session", "import", matching_session.to_str().unwrap()])),
        0,
        "a matching-origin import must succeed"
    );
    assert!(
        stdout(&fx.run(&["eval", "localStorage.getItem('wp_origin_ok') === '1'"])).contains("true"),
        "the matching-origin import must actually write the storage"
    );

    // The `*`-new marker is suppressed for a fresh DOCUMENT (no baseline), but a
    // same-document `pushState`/hash change keeps the baseline — so an element it
    // inserts IS flagged new. Capture a baseline, then push a new URL state and
    // add a button, and the next capture must mark that button `is_new`.
    let star_base = fx.run(&["capture", "--include", "dom", "--url", &base]);
    assert_eq!(
        code(&star_base),
        0,
        "star baseline capture: {}",
        stdout(&star_base)
    );
    let _ = fx.run(&[
        "eval",
        "history.pushState({}, '', '#section'); \
         document.body.insertAdjacentHTML('beforeend', '<button id=\"freshbtn\">fresh</button>'); 'ok'",
    ]);
    let star_cap = fx.run(&["capture", "--include", "dom"]);
    let star_json: serde_json::Value =
        serde_json::from_str(&stdout(&star_cap)).expect("star capture json");
    let freshbtn = star_json["elements"]
        .as_array()
        .expect("elements")
        .iter()
        .find(|e| e["id"] == "freshbtn")
        .expect("the pushState-inserted button is indexed");
    assert_eq!(
        freshbtn["is_new"],
        true,
        "an element added after a same-document pushState must be flagged new (*): {}",
        stdout(&star_cap)
    );

    // A `wait text` value starting with `-` is the value, not a flag
    // (allow_hyphen_values) — it must reach the bridge and time out, not be
    // rejected by clap (exit 2).
    let wt = fx.run(&["wait", "--timeout", "1", "text", "-nomatch-"]);
    assert_eq!(
        code(&wt),
        5,
        "leading-dash wait text must parse and time out (5), not clap-reject (2): {}",
        stdout(&wt)
    );

    // A wait whose timeout exceeds the CDP-send timeout must run its full
    // in-page loop, not be truncated to a false CDP-level error. With a 1s
    // cdp-send and a 3s wait, the timeout that surfaces must be the bridge's
    // own (the condition-naming `wait selector …` kind), proving the loop ran
    // past the CDP-send bound — and the connection must survive (a response
    // timeout must not kill it).
    let lw = fx.run_env(
        &["wait", "--timeout", "3", "selector", "#never-exists"],
        &[("WEBPILOT_CDP_SEND_TIMEOUT_MS", "1000")],
    );
    assert_eq!(code(&lw), 5, "long wait must time out (5): {}", stdout(&lw));
    let lwj: serde_json::Value = serde_json::from_str(&stdout(&lw)).expect("wait json");
    assert_eq!(
        lwj["error"]["kind"].as_str(),
        Some(r##"wait selector "#never-exists""##),
        "the wait must run its own loop past the CDP-send bound, not be cut by it: {}",
        stdout(&lw)
    );
    let alive = fx.run(&["eval", "1+1"]);
    assert_eq!(
        code(&alive),
        0,
        "the CDP connection must survive a long wait: {}",
        stdout(&alive)
    );

    // A pathological `--timeout` must not overflow the deadline `Instant` — on
    // a release build (`panic = "abort"`) that overflow was a process-killing
    // panic. do_wait clamps the timeout at the in-page timer ceiling, and since
    // `body` exists immediately the wait satisfies at once rather than running
    // (or panicking). u64::MAX seconds saturating_mul(1000) reaches do_wait,
    // which clamps to i32::MAX ms.
    let huge = fx.run(&[
        "wait",
        "--timeout",
        "18446744073709551615",
        "selector",
        "body",
    ]);
    assert_eq!(
        code(&huge),
        0,
        "a pathological timeout must clamp and satisfy immediately, never panic: {}",
        stdout(&huge)
    );

    // Drag whose source and target can't share the viewport (far apart) fails
    // loud (`InvalidArgument`) instead of releasing into empty space and
    // reporting success.
    let inject = fx.run(&[
        "eval",
        "document.body.insertAdjacentHTML('beforeend','<div id=dsrc onclick=\"\" style=\"width:50px;height:50px\">S</div><div style=\"height:4000px\"></div><div id=dtgt onclick=\"\" style=\"width:50px;height:50px\">T</div>'); 'ok'",
    ]);
    assert_eq!(
        code(&inject),
        0,
        "drag-fixture inject failed: {}",
        stdout(&inject)
    );
    let cap_d = fx.run(&["capture", "--include", "dom"]);
    assert_eq!(code(&cap_d), 0, "drag capture failed: {}", stdout(&cap_d));
    let dsrc = index_of(&cap_d, "dsrc");
    let dtgt = index_of(&cap_d, "dtgt");
    let drag = fx.run(&["action", "drag", &dsrc, &dtgt]);
    assert_eq!(
        code(&drag),
        7,
        "a drag whose endpoints can't share the viewport must fail InvalidArgument (7), not falsely succeed: {}",
        stdout(&drag)
    );

    // 8. Context isolation: localStorage written in one context is invisible in
    //    another (separate CDP browser contexts = separate storage partitions).
    let a_url = base.clone();
    let null_result = |out: &Output| -> bool {
        let v: serde_json::Value = serde_json::from_str(&stdout(out)).expect("eval json");
        v["result"] == serde_json::Value::Null || v["result"] == "null"
    };

    let cap_a = fx.run(&[
        "--context",
        "ctx-a",
        "capture",
        "--include",
        "dom",
        "--url",
        &a_url,
    ]);
    assert_eq!(code(&cap_a), 0, "ctx-a capture failed: {}", stdout(&cap_a));
    let write_a = fx.run(&[
        "--context",
        "ctx-a",
        "eval",
        "localStorage.setItem('k','a')",
    ]);
    assert_eq!(
        code(&write_a),
        0,
        "ctx-a write failed: {}",
        stdout(&write_a)
    );

    // Confirm the write is visible WITHIN ctx-a first — otherwise ctx-b reading
    // null would prove nothing (the write could have silently failed).
    let read_a = fx.run(&["--context", "ctx-a", "eval", "localStorage.getItem('k')"]);
    assert_eq!(code(&read_a), 0, "ctx-a read failed: {}", stdout(&read_a));
    assert!(
        !null_result(&read_a),
        "ctx-a must see its own write before isolation can be proven: {}",
        stdout(&read_a)
    );

    let cap_b = fx.run(&[
        "--context",
        "ctx-b",
        "capture",
        "--include",
        "dom",
        "--url",
        &a_url,
    ]);
    assert_eq!(code(&cap_b), 0, "ctx-b capture failed: {}", stdout(&cap_b));
    let read_b = fx.run(&["--context", "ctx-b", "eval", "localStorage.getItem('k')"]);
    assert_eq!(code(&read_b), 0, "ctx-b eval failed: {}", stdout(&read_b));
    assert!(
        null_result(&read_b),
        "ctx-b must not see ctx-a's localStorage: {}",
        stdout(&read_b)
    );

    // 8b. Tab-level isolation: the default context's `tab` list and `tab switch`
    //     must never reach a tab opened under an isolated `--context`. The default
    //     scope is every target NOT in a created browser context, so ctx-a's tab is
    //     invisible to it and switching to that id from the default is TabNotFound —
    //     without it the default scope (`None`) matched every context's tabs.
    let ctx_a_tabs = fx.run(&["--context", "ctx-a", "tab"]);
    let ctx_a_json: serde_json::Value =
        serde_json::from_str(&stdout(&ctx_a_tabs)).expect("ctx-a tab json");
    let ctx_a_tab_id = ctx_a_json[0]["id"]
        .as_str()
        .expect("ctx-a tab id")
        .to_string();
    let default_tabs = fx.run(&["tab"]);
    assert!(
        !stdout(&default_tabs).contains(&ctx_a_tab_id),
        "the default context's tab list must not leak an isolated --context tab: {}",
        stdout(&default_tabs)
    );
    let cross_switch = fx.run(&["tab", "switch", &ctx_a_tab_id]);
    assert_eq!(
        code(&cross_switch),
        4,
        "switching from the default context to an isolated --context tab must be TabNotFound (4): {}",
        stdout(&cross_switch)
    );

    // 8c. `context close NAME --all` is a contradictory request — close one named
    //     context AND every context. clap now declares the two mutually
    //     exclusive (`conflicts_with`), so the PARSER rejects it (exit 2, before
    //     any transport opens — no Chrome side effect), never silently taking
    //     the destructive all-branch. ctx-a survives.
    let ambiguous_close = fx.run(&["context", "close", "ctx-a", "--all"]);
    assert_eq!(
        code(&ambiguous_close),
        2,
        "context close NAME --all must be rejected by the parser (2), not a silent close-all: {}",
        String::from_utf8_lossy(&ambiguous_close.stderr)
    );
    assert!(
        stdout(&fx.run(&["context", "list"])).contains("ctx-a"),
        "a rejected ambiguous close must not have disposed ctx-a: {}",
        stdout(&fx.run(&["context", "list"]))
    );

    // 8d. `record --frames N --duration M` names the same quantity (a frame
    //     count) two contradictory ways — the flags are documented as
    //     alternatives — so it must be rejected (InvalidArgument, exit 7) before
    //     any frame is captured, never silently honor one and drop the other.
    let ambiguous_record = fx.run(&["record", "--frames", "2", "--duration", "5"]);
    assert_eq!(
        code(&ambiguous_record),
        7,
        "record --frames N --duration M must be InvalidArgument (7), not a silent pick-one: {}",
        stdout(&ambiguous_record)
    );

    // 8e. A successful `Ok(msg)` command must carry its message into JSON, not
    //     just `{"success":true}` — the piped JSON path is how an agent reads
    //     output, so dropping the message would hide actionable detail (e.g. a
    //     partial `context close --all` reporting which contexts it kept). ctx-a
    //     survived the rejected 8c close; closing it now must echo the message.
    let closed = fx.run(&["context", "close", "ctx-a"]);
    assert_eq!(
        code(&closed),
        0,
        "context close ctx-a failed: {}",
        stdout(&closed)
    );
    let closed_json: serde_json::Value =
        serde_json::from_str(&stdout(&closed)).expect("close json");
    assert!(
        closed_json["message"]
            .as_str()
            .is_some_and(|m| m.contains("ctx-a")),
        "a successful Ok command's JSON must carry its message, not just success: {}",
        stdout(&closed)
    );

    // 8d. History traversal settles on the MAIN frame, not whichever frame
    //     fires first. The top fixture embeds an iframe, so going `back` to it
    //     reloads a subframe too; the wait must settle on the top document we
    //     navigated FROM (its `cardwrap`), never end early on the embedded
    //     frame's own load and leave the readyState probe reading the page we
    //     just left (`inner link` lives only on /frame's main document; once we
    //     are back on the top page it sits in an unshown subframe). Browser
    //     mode asserts the mirror at its navigation section. The subframe-vs-
    //     main ordering is a few-ms race verified by review (like the other
    //     navigation-settle races); this pins the deterministic real-back path
    //     and its main-frame filter.
    assert_eq!(
        code(&fx.run(&["action", "navigate", &base])),
        0,
        "navigate to base (history seed) failed"
    );
    assert_eq!(
        code(&fx.run(&["action", "navigate", &format!("{base}/frame")])),
        0,
        "navigate to /frame failed"
    );
    let on_frame = fx.run(&["capture", "--include", "dom"]);
    assert!(
        stdout(&on_frame).contains("inner link"),
        "must be on /frame's main document before going back: {}",
        stdout(&on_frame)
    );
    let back = fx.run(&["action", "back"]);
    assert_eq!(code(&back), 0, "action back failed: {}", stdout(&back));
    let after_back = fx.run(&["capture", "--include", "dom"]);
    assert!(
        stdout(&after_back).contains("cardwrap") && !stdout(&after_back).contains("inner link"),
        "back must settle on the top document we navigated from (main-frame \
         wait), not the embedded frame's load that would leave the capture on \
         /frame: {}",
        stdout(&after_back)
    );

    // 8d-frag. A `#fragment` navigation must NOT hang. `await_live_bridge_context`
    //     compares the bridge's fragment-bearing `location.href` against
    //     `bound_target_url` (also fragment-bearing), NOT the fragment-STRIPPED
    //     `Page.getFrameTree` url — which never matched a fragment URL, evicting
    //     the only bridge context every poll until the navigation timeout and then
    //     failing the capture as `FrameNotFound`. This must capture promptly with a
    //     real DOM (the fixture page; `#section` is just a fragment).
    let frag_cap = fx.run(&[
        "capture",
        "--include",
        "dom",
        "--url",
        &format!("{base}/#section"),
    ]);
    assert_eq!(
        code(&frag_cap),
        0,
        "capture of a #fragment URL must succeed, not hang/FrameNotFound: {}",
        stdout(&frag_cap)
    );
    assert!(
        stdout(&frag_cap).contains("cardwrap"),
        "a #fragment-URL capture must return the page DOM, not an empty/failed snapshot: {}",
        stdout(&frag_cap)
    );

    // 8d-204. A 204 (or any stay-put response: download/intercepted) ABORTS the
    //     navigation (ERR_ABORTED) without committing a new document — the previous
    //     page stays live. `navigate` must return success FAST (bounded by PROBE),
    //     not spin to the navigation timeout and report a false NavigationFailed,
    //     and the previous document must stay capturable.
    let nav204 = fx.run(&["action", "navigate", &format!("{base}/empty204")]);
    assert_eq!(
        code(&nav204),
        0,
        "navigate to a 204 stay-put response must succeed, not NavigationFailed/hang: {}",
        stdout(&nav204)
    );
    let after204 = fx.run(&["capture", "--include", "dom"]);
    assert!(
        stdout(&after204).contains("cardwrap"),
        "after a 204 the previous document must stay live and capturable: {}",
        stdout(&after204)
    );

    // 8d-image. `<input type=image>` is a submit button: it carries an implicit
    //     ARIA `button` role (its `alt` is the accessible name), so a semantic
    //     `find --role button` reaches it — pinning the implicit-role mapping end
    //     to end on a live page, not just the type-level unit test.
    let img_btn = fx.run(&["find", "--role", "button", "--tag", "input"]);
    assert!(
        stdout(&img_btn).contains("Image submit"),
        "find --role button must match the <input type=image> by its implicit \
         button role (alt as accessible name): {}",
        stdout(&img_btn)
    );

    // 8d-noop. A fresh tab has no back entry. `back`/`forward` are now decided by
    //     OUTCOME — the traversal fired a real main-frame navigation — having
    //     dropped the `navigation.canGoBack` probe that only saw the contiguous
    //     same-origin history run and so falsely denied a cross-origin entry (a
    //     valid traversal; pinned by review against a live two-origin run). A
    //     genuine no-op fires no navigation and the URL never moves, so it
    //     surfaces as a typed NavigationFailed (exit 8) after the window, never a
    //     false success. This pins that negative; browser mode asserts the mirror.
    let fresh = fx.run(&["tab", "new", &base]);
    assert_eq!(code(&fresh), 0, "tab new failed: {}", stdout(&fresh));
    let no_history = fx.run(&["action", "back"]);
    assert_eq!(
        code(&no_history),
        8,
        "back with no history must be a typed NavigationFailed: {}",
        stdout(&no_history)
    );

    // 8e. A budget-clipped shadow traversal cannot prove set-uniqueness: past
    //     the host budget the deep walker stops early, and "unique so far"
    //     could write the wrong element — the WRITE fails honest (the same
    //     truncation capture surfaces as `shadow_truncated`), while a READ
    //     keeps its deterministic light-first first match. Spawn 5001 empty
    //     shadow hosts to exhaust the budget; section 9 navigates away,
    //     restoring a clean page.
    assert_eq!(
        code(&fx.run(&[
            "eval",
            "for(let i=0;i<5001;i++){const d=document.createElement('div');d.attachShadow({mode:'open'});document.body.appendChild(d)} 'spawned'",
        ])),
        0,
        "spawning shadow hosts failed"
    );
    let trunc = fx.run(&["dom", "set-text", "#wsp", "y"]);
    assert_eq!(
        code(&trunc),
        7,
        "a budget-clipped traversal must refuse the write (uniqueness unproven): {}",
        stdout(&trunc)
    );
    assert!(
        stdout(&trunc).contains("budget"),
        "the truncation error must name the budget: {}",
        stdout(&trunc)
    );
    assert_eq!(
        code(&fx.run(&["dom", "get-text", "#wsp"])),
        0,
        "a read keeps serving its deterministic first match under truncation"
    );

    let active_id = |list: &Output| -> String {
        serde_json::from_str::<serde_json::Value>(&stdout(list))
            .expect("tab list json")
            .as_array()
            .expect("tab list array")
            .iter()
            .find(|t| t["active"] == serde_json::Value::Bool(true))
            .and_then(|t| t["id"].as_str())
            .expect("an active tab")
            .to_string()
    };

    // 8f. Headless `wait navigation` classifies a tab that CLOSES mid-wait as
    //     TabNotFound (exit 4), like browser mode's tabs.onRemoved arm — the
    //     page socket died but Chrome is fine, so neither an infra
    //     ConnectionLost nor a sat-out Timeout is the truth. The close runs
    //     from a second process while the wait is in flight.
    assert_eq!(
        code(&fx.run(&["tab", "new", &base])),
        0,
        "tab new (wait-vanish setup) failed"
    );
    let wt_id = active_id(&fx.run(&["tab"]));
    let wait_out = std::thread::scope(|s| {
        let waiter = s.spawn(|| fx.run(&["wait", "--timeout", "15", "navigation"]));
        std::thread::sleep(std::time::Duration::from_millis(1500));
        assert_eq!(
            code(&fx.run(&["tab", "close", &wt_id])),
            0,
            "closing the awaited tab failed"
        );
        waiter.join().expect("wait thread")
    });
    assert_eq!(
        code(&wait_out),
        4,
        "a tab closing mid-wait must be TabNotFound (4), not ConnectionLost/Timeout: {}",
        stdout(&wait_out)
    );
    assert!(
        stdout(&wait_out).contains("Tab not found"),
        "the wait error must name the gone tab: {}",
        stdout(&wait_out)
    );
    assert_eq!(
        code(&fx.run(&["tab", "new", &base])),
        0,
        "re-pin after the wait-vanish test failed"
    );

    // 8g. A `wait selector` whose document NAVIGATES mid-poll must survive:
    //     the condition's intent transfers to the new document (a redirect
    //     landing on the page the agent is waiting for), so the wait re-arms
    //     with the remaining budget instead of dying as an untyped "Execution
    //     context was destroyed" infra error (exit 1). /slowredir
    //     self-navigates to /redirtarget (#navgoal) ~800ms in — after the
    //     wait below is already polling in-page.
    assert_eq!(
        code(&fx.run(&["action", "navigate", &format!("{base}/slowredir")])),
        0,
        "navigate to /slowredir failed"
    );
    let surv = fx.run(&["wait", "--timeout", "10", "selector", "#navgoal"]);
    assert_eq!(
        code(&surv),
        0,
        "wait selector must survive a mid-poll document navigation and satisfy \
         against the NEW document: {}",
        stdout(&surv)
    );
    // ...and a timed-out wait names WHAT was waited for — a self-contained
    // error, not a bare "wait timed out" that forces a re-read of the call.
    let wto = fx.run(&["wait", "--timeout", "1", "selector", "#never_appears"]);
    assert_eq!(
        code(&wto),
        5,
        "a never-matching selector must time out (5): {}",
        stdout(&wto)
    );
    assert!(
        stdout(&wto).contains("wait selector") && stdout(&wto).contains("#never_appears"),
        "the wait timeout must name the condition it waited for: {}",
        stdout(&wto)
    );
    assert_eq!(
        code(&fx.run(&["action", "navigate", &base])),
        0,
        "re-home after the wait-survival test failed"
    );

    // 8g-shot. A screenshot reports its SAVED dimensions — and a full-page
    //     shot of this tall page (3000px spacer) exceeds the long-edge cap, so
    //     the downscale ratio must ride along too: pixel coordinates on the
    //     saved image map to page pixels via `÷ scale`, and a silent resize
    //     would break that math (pre-fix the dimensions died in a debug log).
    let vshot = fx.run(&["capture", "--include", "screenshot"]);
    assert_eq!(
        code(&vshot),
        0,
        "viewport screenshot failed: {}",
        stdout(&vshot)
    );
    let vs: serde_json::Value = serde_json::from_str(&stdout(&vshot)).expect("shot json");
    assert!(
        vs["screenshot_width"].as_u64().is_some() && vs["screenshot_height"].as_u64().is_some(),
        "a screenshot must report its saved dimensions: {}",
        stdout(&vshot)
    );
    assert!(
        vs["screenshot_scale"].is_null(),
        "an unscaled viewport shot must NOT claim a downscale: {}",
        stdout(&vshot)
    );
    let fshot = fx.run(&["capture", "--include", "screenshot", "--full-page"]);
    assert_eq!(
        code(&fshot),
        0,
        "full-page screenshot failed: {}",
        stdout(&fshot)
    );
    let fs: serde_json::Value = serde_json::from_str(&stdout(&fshot)).expect("full shot json");
    assert!(
        fs["screenshot_scale"]
            .as_f64()
            .is_some_and(|s| s > 0.0 && s < 1.0),
        "a tall full-page shot exceeds the long-edge cap and must report its \
         downscale ratio: {}",
        stdout(&fshot)
    );
    let cap_edge = fs["screenshot_width"]
        .as_u64()
        .unwrap_or(0)
        .max(fs["screenshot_height"].as_u64().unwrap_or(0));
    assert!(
        cap_edge > 0 && cap_edge <= 1568,
        "the reported dimensions must be the SAVED image's (long edge at the cap): {}",
        stdout(&fshot)
    );

    // 8h. The same mid-wait close while switched INTO AN IFRAME must also be
    //     the root-cause TabNotFound — not the FrameNotFound the dead re-arm's
    //     frame probe would otherwise collapse into (sending the agent
    //     recapturing frames on a tab that no longer exists), and not
    //     ConnectionLost (Chrome is fine).
    assert_eq!(
        code(&fx.run(&["tab", "new", &base])),
        0,
        "tab new (iframe wait-vanish setup) failed"
    );
    let _ = fx.run(&["capture", "--include", "dom"]);
    assert_eq!(
        code(&fx.run(&["frame", "url", "/frame"])),
        0,
        "frame switch (iframe wait-vanish setup) failed"
    );
    let fw_id = active_id(&fx.run(&["tab"]));
    let fwait_out = std::thread::scope(|s| {
        let waiter = s.spawn(|| fx.run(&["wait", "--timeout", "15", "selector", "#never_appears"]));
        std::thread::sleep(std::time::Duration::from_millis(1500));
        assert_eq!(
            code(&fx.run(&["tab", "close", &fw_id])),
            0,
            "closing the iframe-scoped awaited tab failed"
        );
        waiter.join().expect("iframe wait thread")
    });
    assert_eq!(
        code(&fwait_out),
        4,
        "a tab closing mid-wait while iframe-scoped must be TabNotFound (4), \
         not FrameNotFound/ConnectionLost: {}",
        stdout(&fwait_out)
    );
    assert!(
        stdout(&fwait_out).contains("Tab not found"),
        "the iframe-scoped wait error must name the gone TAB (root cause): {}",
        stdout(&fwait_out)
    );
    assert_eq!(
        code(&fx.run(&["tab", "new", &base])),
        0,
        "re-pin after the iframe wait-vanish test failed"
    );
    let _ = fx.run(&["frame", "main"]);

    // 8i. `tab new` to an unreachable URL must fail like `navigate` does — a
    //     refused/DNS load is a typed NavigationFailed (exit 8), not a success
    //     pinned to an error page. `tab new` opens the tab blank and drives the
    //     load through the very path `navigate` uses, so the two agree by
    //     construction and the failed open rolls back to the previous tab. Port
    //     1 on loopback refuses instantly (`Page.navigate`'s `errorText`), so
    //     this is fast and deterministic.
    let dead_new = fx.run(&["tab", "new", "http://127.0.0.1:1/"]);
    assert_eq!(
        code(&dead_new),
        8,
        "tab new to a refused port must be NavigationFailed (8), not a success \
         pinned to an error page: {}",
        stdout(&dead_new)
    );
    assert_eq!(
        code(&fx.run(&["tab", "new", &base])),
        0,
        "re-pin after the unreachable tab new failed"
    );

    // 9. Closing the ACTIVE tab leaves a dead pin. A pin-INDEPENDENT command
    //    (`tab` list) must still work so the agent can find a survivor and
    //    recover — not fail with a spurious TabNotFound. A page ACTION, by
    //    contrast, must fail loud rather than silently retarget onto a fallback
    //    survivor (the never-silent-retarget contract). The persisted pin is
    //    dropped after one resolve, so each case sets up its own dead pin. The
    //    new tab's id isn't in `tab new`'s output, so read it from the `active`
    //    flag in the list (`tab new` pins the tab it created). Runs LAST: it
    //    churns the active-tab pin, which would disturb earlier tab-bound steps.
    assert_eq!(code(&fx.run(&["action", "navigate", &base])), 0);
    assert_eq!(
        code(&fx.run(&["tab", "new", &base])),
        0,
        "tab new (vanish setup) failed"
    );
    assert_eq!(
        code(&fx.run(&["tab", "close", &active_id(&fx.run(&["tab"]))])),
        0,
        "closing the active tab failed"
    );
    assert_eq!(
        code(&fx.run(&["tab"])),
        0,
        "tab list after closing the active tab must work (pin-independent), not TabNotFound: {}",
        stdout(&fx.run(&["tab"]))
    );

    assert_eq!(
        code(&fx.run(&["tab", "new", &base])),
        0,
        "tab new (2) failed"
    );
    assert_eq!(
        code(&fx.run(&["tab", "close", &active_id(&fx.run(&["tab"]))])),
        0,
        "closing the active tab failed"
    );
    assert_eq!(
        code(&fx.run(&["eval", "1"])),
        4,
        "a page command after the active tab vanished must be TabNotFound (4), not a silent retarget: {}",
        stdout(&fx.run(&["eval", "1"]))
    );

    // A COOKIE-ONLY session import is browser-global (cookies land in the
    // shared jar through any target's session), so a dead pin must NOT block
    // it — only the storage half is page-bound. Set up a fresh dead pin, then
    // import cookies-only and confirm both the exit and the cookie landing.
    assert_eq!(
        code(&fx.run(&["tab", "new", &base])),
        0,
        "tab new (cookie-only import setup) failed"
    );
    assert_eq!(
        code(&fx.run(&["tab", "close", &active_id(&fx.run(&["tab"]))])),
        0,
        "closing the active tab failed"
    );
    let cookie_only = home.join("cookie-only-session.json");
    std::fs::write(
        &cookie_only,
        br#"{"version":1,"cookies":[{"name":"deadpin_ok","value":"1","domain":"127.0.0.1","path":"/","same_site":"lax","host_only":true}]}"#,
    )
    .expect("write cookie-only session fixture");
    let coi = fx.run(&["session", "import", cookie_only.to_str().unwrap()]);
    assert_eq!(
        code(&coi),
        0,
        "a cookie-only session import must succeed on a dead pin (cookies are browser-global): {}",
        stdout(&coi)
    );
    assert!(
        stdout(&fx.run(&["cookie", "list", &base])).contains("deadpin_ok"),
        "the cookie-only import must actually land its cookie: {}",
        stdout(&fx.run(&["cookie", "list", &base]))
    );

    // And the cookie commands themselves are browser-global: the FIRST command
    // after the pin dies (the one that observes the vanished pin before it is
    // dropped) must still list/set against the shared jar, not TabNotFound.
    assert_eq!(
        code(&fx.run(&["tab", "new", &base])),
        0,
        "tab new (cookie-on-dead-pin setup) failed"
    );
    assert_eq!(
        code(&fx.run(&["tab", "close", &active_id(&fx.run(&["tab"]))])),
        0,
        "closing the active tab failed"
    );
    let dead_pin_list = fx.run(&["cookie", "list", &base]);
    assert_eq!(
        code(&dead_pin_list),
        0,
        "cookie list as the first command on a dead pin must succeed (the jar is browser-global): {}",
        stdout(&dead_pin_list)
    );

    // 9b. `tab find --url` is strict about ambiguity, like `frame url`: one
    //     match switches, two matches fail loud with the list — never a silent
    //     switch to whichever tab listed first.
    assert_eq!(code(&fx.run(&["action", "navigate", &base])), 0);
    assert_eq!(
        code(&fx.run(&["tab", "new", &format!("{base}/second")])),
        0,
        "tab new /second failed"
    );
    assert_eq!(
        code(&fx.run(&["tab", "find", "--url", "/second"])),
        0,
        "a uniquely-matching tab find must switch: {}",
        stdout(&fx.run(&["tab", "find", "--url", "/second"]))
    );
    assert_eq!(
        code(&fx.run(&["tab", "new", &format!("{base}/second")])),
        0,
        "tab new /second (2) failed"
    );
    // The target list's URL field updates asynchronously (targetInfoChanged):
    // wait until BOTH /second tabs are visible to `tab` so the ambiguity below
    // is deterministic, not a race against the second tab's URL landing.
    let mut second_count = 0usize;
    for _ in 0..20 {
        second_count = serde_json::from_str::<serde_json::Value>(&stdout(&fx.run(&["tab"])))
            .ok()
            .and_then(|v| {
                v.as_array().map(|a| {
                    a.iter()
                        .filter(|t| t["url"].as_str().is_some_and(|u| u.contains("/second")))
                        .count()
                })
            })
            .unwrap_or(0);
        if second_count >= 2 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert_eq!(
        second_count, 2,
        "both /second tabs must be listed before the ambiguity check"
    );
    let ambiguous_find = fx.run(&["tab", "find", "--url", "/second"]);
    assert_eq!(
        code(&ambiguous_find),
        7,
        "an ambiguous tab find (two tabs match) must be InvalidArgument (7), not a silent first-switch: {}",
        stdout(&ambiguous_find)
    );
    assert!(
        stdout(&ambiguous_find).contains("tabs match"),
        "the ambiguity error must name the match count: {}",
        stdout(&ambiguous_find)
    );

    // 9c. Closing the LAST tab must not wedge the session: the attach creates a
    //     blank page to bind to (the state a fresh browser starts in), so `tab`
    //     still lists, the dead pin still fires its one loud TabNotFound on the
    //     first page action, and the recovery `navigate` then works. Before the
    //     fix every command — including the navigate that would fix it — failed
    //     NoPage forever.
    let all_ids: Vec<String> =
        serde_json::from_str::<serde_json::Value>(&stdout(&fx.run(&["tab"])))
            .expect("tab list json")
            .as_array()
            .expect("tab array")
            .iter()
            .filter_map(|t| t["id"].as_str().map(str::to_owned))
            .collect();
    for id in &all_ids {
        assert_eq!(
            code(&fx.run(&["tab", "close", id])),
            0,
            "closing tab {id} failed"
        );
    }
    // The first page action observes the dead pin once — loud, typed.
    assert_eq!(
        code(&fx.run(&["action", "navigate", &base])),
        4,
        "the first page action after the last tab closed must be the one loud TabNotFound"
    );
    // ...and the retry proceeds on the blank attach: the session recovered.
    assert_eq!(
        code(&fx.run(&["action", "navigate", &base])),
        0,
        "navigate must recover the session after the last tab closed"
    );
    assert!(
        stdout(&fx.run(&["capture", "--include", "dom"])).contains("cardwrap"),
        "the recovered session must capture the navigated page"
    );

    // 8f-cap. Every other axis of a capture is bounded — page text, element text,
    //     option lists, the shadow walk — but the index itself was not, and a
    //     content-heavy page reaches four figures of links on its own. The clip
    //     must announce itself: a short index read as the whole page is what makes
    //     an agent conclude a control does not exist.
    let many = fx.run(&[
        "capture",
        "--include",
        "dom",
        "--url",
        &format!("{base}/many"),
    ]);
    let many_json: serde_json::Value = serde_json::from_str(&stdout(&many)).expect("json");
    let listed = many_json["elements"].as_array().expect("elements").len();
    assert_eq!(listed, 1000, "the index must be capped: {listed}");
    assert_eq!(
        many_json["elements_truncated"],
        true,
        "a capped index must say so: {}",
        stdout(&many)
    );
    // The indices that WERE emitted still resolve — capping the list must not
    // desync it from the bridge's action-resolution snapshot.
    assert_eq!(
        code(&fx.run(&["action", "scroll-to", "1000"])),
        0,
        "the last emitted index must still resolve"
    );
    assert_eq!(
        code(&fx.run(&["action", "scroll-to", "1001"])),
        4,
        "an index past the cap must be a typed not-found, never a silent hit"
    );

    // 8e-dl. A navigation that resolves to an ATTACHMENT is a stay-put whose
    //     cause the agent could not previously see: the page never moves, so an
    //     unreported download reads as a command that did nothing and invites the
    //     retry that downloads the file again. The download must be reported, and
    //     the file must land under WebPilot's artifact root rather than the user's
    //     OS download directory, where nothing WebPilot owns would ever reclaim it.
    let dl_dir = fx.home.join("artifacts/downloads/default");
    let dl_nav = fx.run(&[
        "capture",
        "--include",
        "dom",
        "--url",
        &format!("{base}/attachment"),
    ]);
    let dl_nav_json: serde_json::Value = serde_json::from_str(&stdout(&dl_nav)).expect("json");
    let saved = &dl_nav_json["downloads"][0];
    assert_eq!(
        saved["state"],
        "saved",
        "a navigation that downloaded must report it: {}",
        stdout(&dl_nav)
    );
    assert_eq!(
        saved["suggested_filename"],
        "invoice.pdf",
        "the server's own name for the file must reach the agent: {}",
        stdout(&dl_nav)
    );
    let saved_path = PathBuf::from(saved["path"].as_str().expect("download path"));
    assert!(
        saved_path.starts_with(&dl_dir),
        "a download must land under the artifact root, not the OS download dir: {saved_path:?}"
    );
    assert_eq!(
        settled_file(&saved_path),
        "INVOICE-BODY",
        "the reported path must name the real bytes"
    );

    // 8e-dl-click. The same for a download a CLICK starts: it never reaches
    //     `Page.navigate`, so it is announced only as a browser event — and the
    //     announcement stream is browser-wide, so what lands here also proves the
    //     report is scoped to the page the click happened on.
    let cap_dl = fx.run(&["capture", "--include", "dom", "--url", &base]);
    let click_dl = fx.run(&["action", "click", &index_of(&cap_dl, "dlnav")]);
    let click_json: serde_json::Value = serde_json::from_str(&stdout(&click_dl)).expect("json");
    assert_eq!(
        click_json["downloads"][0]["state"],
        "saved",
        "a click that downloaded must report it: {}",
        stdout(&click_dl)
    );
    let click_path = PathBuf::from(click_json["downloads"][0]["path"].as_str().expect("path"));
    assert_eq!(
        settled_file(&click_path),
        "INVOICE-BODY",
        "the click's reported path must name the real bytes"
    );

    // 8e-dl-deny. `download deny` refuses the transfer in the browser, so nothing
    //     is written — and the refusal is REPORTED rather than dropped, or the
    //     agent would retry a click that can never succeed. A denied download
    //     carries no path: naming a file that was never written would be the same
    //     lie in the other direction.
    let before = std::fs::read_dir(&dl_dir).expect("downloads dir").count();
    fx.run(&[
        "policy",
        "set",
        "--operation",
        "download",
        "--verdict",
        "deny",
    ]);
    let cap_deny = fx.run(&["capture", "--include", "dom", "--url", &base]);
    let denied = fx.run(&["action", "click", &index_of(&cap_deny, "dlnav")]);
    let denied_json: serde_json::Value = serde_json::from_str(&stdout(&denied)).expect("json");
    assert_eq!(
        denied_json["downloads"][0]["state"],
        "denied",
        "a denied download must still be reported: {}",
        stdout(&denied)
    );
    assert!(
        denied_json["downloads"][0]["path"].is_null(),
        "a denied download must not name a file: {}",
        stdout(&denied)
    );
    assert_eq!(
        std::fs::read_dir(&dl_dir).expect("downloads dir").count(),
        before,
        "a denied download must write nothing"
    );
    fx.run(&["policy", "clear"]);

    // 2a-domset-tt. `dom set-html` on a Trusted-Types page (`require-trusted-types-
    //     for 'script'`) makes `innerHTML = <string>` THROW — surface it as a typed
    //     InvalidArgument (exit 7) carrying the page's reason, never an untyped
    //     Other with the bridge's raw V8 stack (in browser mode this throw also
    //     closed the message port and stalled the command for the full send
    //     timeout). Runs LAST — it navigates away from the fixture page.
    let tt_url = "data:text/html,<meta http-equiv=\"Content-Security-Policy\" content=\"require-trusted-types-for 'script'\"><div id=ttx>orig</div>";
    let _ = fx.run(&["capture", "--include", "dom", "--url", tt_url]);
    let tt_set = fx.run(&["dom", "set-html", "#ttx", "<b>x</b>"]);
    assert_eq!(
        code(&tt_set),
        7,
        "set-html on a Trusted-Types page must be a typed InvalidArgument (7), not Other: {}",
        stdout(&tt_set)
    );
    assert!(
        !stdout(&tt_set).contains("<anonymous>"),
        "the set-html Trusted-Types error must not leak the bridge's V8 stack: {}",
        stdout(&tt_set)
    );

    drop(fx);
}
