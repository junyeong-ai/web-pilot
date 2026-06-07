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
        Command::new(BIN)
            .args(args)
            .env("WEBPILOT_HOME", &self.home)
            // Force JSON regardless of how the test harness wires stdio.
            .arg("--json")
            .output()
            .expect("spawn webpilot")
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
        snapshot["subframes"], 1,
        "the one http iframe must be reported as a subframe: {dom}"
    );

    let button_index = elements
        .iter()
        .find(|e| e["tag"] == "button")
        .and_then(|e| e["index"].as_u64())
        .expect("button index") as u32;

    // 2. Click the button by its captured index; its onclick sets the title.
    let click = fx.run(&["action", "click", &button_index.to_string()]);
    assert_eq!(code(&click), 0, "click failed: {}", stdout(&click));
    let title = fx.run(&["eval", "document.title"]);
    assert!(
        stdout(&title).contains("clicked"),
        "click should have run the handler: {}",
        stdout(&title)
    );

    // 2a1. Free-text values that start with `-` are values, not flags
    //      (allow_hyphen_values): an agent evaluating a negative expression or
    //      typing a negative number must not hit a clap usage error — and a
    //      trailing flag after such a value must still parse.
    let neg = fx.run(&["eval", "-7 * 6"]);
    assert_eq!(code(&neg), 0, "leading-dash eval must not be a clap error: {}", stdout(&neg));
    let nj: serde_json::Value = serde_json::from_str(&stdout(&neg)).expect("eval json");
    assert_eq!(nj["result"].as_str(), Some("-42"), "leading-dash eval must evaluate: {}", stdout(&neg));
    let q_index = index_of(&cap, "q");
    let typed = fx.run(&["action", "type", &q_index, "-99", "--clear"]);
    assert_eq!(code(&typed), 0, "type of a leading-dash value with --clear failed: {}", stdout(&typed));
    let tv = fx.run(&["eval", "document.getElementById('q').value === '-99'"]);
    let tvj: serde_json::Value = serde_json::from_str(&stdout(&tv)).expect("eval json");
    assert_eq!(
        tvj["result"].as_str(),
        Some("true"),
        "the leading-dash value must land exactly and --clear must still apply: {}",
        stdout(&tv)
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
    let ar: serde_json::Value =
        serde_json::from_str(&stdout(&after_reload)).expect("capture json");
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

    // 2e. `fetch` runs as a debugger-routed MAIN-world eval in both modes (no
    //     contextId, CSP-exempt) and returns the response body — a same-origin
    //     GET against the fixture server must come back with the page markup.
    let fetched = fx.run(&["fetch", &base]);
    assert_eq!(code(&fetched), 0, "fetch failed: {}", stdout(&fetched));
    assert!(
        stdout(&fetched).contains("shadowhost") || stdout(&fetched).contains("<button"),
        "fetch must return the page body: {}",
        stdout(&fetched)
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
    assert_eq!(code(&cap_t), 0, "capture must survive MAIN tampering: {}", stdout(&cap_t));
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
    assert_eq!(code(&clk), 0, "click must survive MAIN tampering: {}", stdout(&clk));
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
    assert_eq!(code(&sw_frame), 0, "switch into child iframe failed: {}", stdout(&sw_frame));
    let frame_cap = fx.run(&["capture", "--include", "dom"]);
    assert_eq!(code(&frame_cap), 0, "capture inside iframe failed: {}", stdout(&frame_cap));
    let link_idx = index_of(&frame_cap, "link");
    let frame_click = fx.run(&["action", "click", &link_idx]);
    assert_eq!(
        code(&frame_click),
        0,
        "click inside the iframe must resolve via the subframe bridge: {}",
        stdout(&frame_click)
    );
    let back = fx.run(&["frame", "main"]);
    assert_eq!(code(&back), 0, "frame main failed: {}", stdout(&back));

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
    let logged = fx.run(&["eval", "console.log('e2e-monitor-marker')"]);
    assert_eq!(code(&logged), 0);
    let logs = fx.run(&["console", "read"]);
    assert!(
        stdout(&logs).contains("e2e-monitor-marker"),
        "monitors must stay armed across a link-click navigation: {}",
        stdout(&logs)
    );

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
    let pushed = fx.run(&["eval", "history.pushState({}, '', '/changed'); location.pathname"]);
    assert_eq!(code(&pushed), 0, "pushState eval failed: {}", stdout(&pushed));
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
    assert_eq!(code(&sw), 0, "csp frame switch by name failed: {}", stdout(&sw));
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

    // 7d. Device emulation persists across CLI processes. A UA override does
    //     NOT survive a CDP client disconnect on its own, so `device set` must
    //     persist it and `open` must re-apply it — every WebPilot command is a
    //     fresh process re-attaching to the one Chrome. Set, then read UA +
    //     viewport in a SEPARATE invocation; reset must revert both.
    let dev = fx.run(&[
        "device", "set", "--width", "390", "--height", "844", "--user-agent", "WP-E2E-UA/1",
    ]);
    assert_eq!(code(&dev), 0, "device set failed: {}", stdout(&dev));
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
        !ua2j["result"].as_str().unwrap_or_default().contains("WP-E2E-UA/1"),
        "device reset must clear the persisted UA override: {}",
        stdout(&ua2)
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

    drop(fx);
}
