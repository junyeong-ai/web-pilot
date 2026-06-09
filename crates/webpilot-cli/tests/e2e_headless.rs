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
    assert!(
        !elements.iter().any(|e| e["id"] == "hiddenchild"),
        "a display:none element must never be indexed: {dom}"
    );
    // A `role="presentation"` element carrying a click marker is a real click
    // target: ARIA none/presentation STRIP the implicit role, so the marker
    // heuristic must treat it like a plain div, not skip it as a semantic control.
    assert!(
        elements.iter().any(|e| e["id"] == "presdiv"),
        "a role=presentation element with onclick must be indexed (none/presentation = no role): {dom}"
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

    let logged = fx.run(&["eval", "console.log('e2e-monitor-marker')"]);
    assert_eq!(code(&logged), 0);
    let logs = fx.run(&["console", "read"]);
    assert!(
        stdout(&logs).contains("e2e-monitor-marker"),
        "monitors must stay armed across a link-click navigation: {}",
        stdout(&logs)
    );

    // 3b. The `eval` gate covers monitor re-injection: a deny that lands AFTER
    //     `console start` must stop the MAIN-world hooks from re-arming on the
    //     next document — `reinstall_monitors` re-checks the gate (browser mode
    //     mirrors this via host-attached verdicts). First confirm the
    //     self-logging page IS captured while allowed, so the deny case can't
    //     pass on a timing miss.
    let _ = fx.run(&["console", "clear"]);
    assert_eq!(
        code(&fx.run(&["action", "navigate", &format!("{base}/log")])),
        0
    );
    std::thread::sleep(std::time::Duration::from_millis(700));
    assert!(
        stdout(&fx.run(&["console", "read"])).contains("postnav-monitor-marker"),
        "a self-logging page must be captured while the monitor is armed"
    );
    let _ = fx.run(&["console", "clear"]);
    let deny = fx.run(&["policy", "set", "--operation", "eval", "--verdict", "deny"]);
    assert_eq!(code(&deny), 0, "policy set eval deny: {}", stdout(&deny));
    let _ = fx.run(&["action", "navigate", &base]);
    assert_eq!(
        code(&fx.run(&["action", "navigate", &format!("{base}/log")])),
        0,
        "navigate is allowed (only eval was denied)"
    );
    std::thread::sleep(std::time::Duration::from_millis(700));
    assert!(
        !stdout(&fx.run(&["console", "read"])).contains("postnav-monitor-marker"),
        "eval-deny must stop the monitor re-arming on the new document"
    );
    let _ = fx.run(&["policy", "clear"]);
    // Restore the working page for the steps below (the deny test left us on /log).
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
    //     so the /log marker (fired +200ms, after the arm) is captured. Without
    //     the deferred re-arm the new tab would carry no hooks and `console read`
    //     would silently miss its logs. The console monitor armed at step 3 is
    //     still running. (Browser-mode mirror: the monitor-follow step in
    //     e2e_browser.) Restore the pin to a base page for the policy step below.
    let _ = fx.run(&["console", "clear"]);
    let mtab = fx.run(&["tab", "new", &format!("{base}/log")]);
    assert_eq!(
        code(&mtab),
        0,
        "tab new for monitor-follow failed: {}",
        stdout(&mtab)
    );
    std::thread::sleep(std::time::Duration::from_millis(700));
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
        "--user-agent",
        "WP-E2E-UA/1",
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
    // own (`kind: "wait"`), proving the loop ran past the CDP-send bound — and
    // the connection must survive (a response timeout must not kill it).
    let lw = fx.run_env(
        &["wait", "--timeout", "3", "selector", "#never-exists"],
        &[("WEBPILOT_CDP_SEND_TIMEOUT_MS", "1000")],
    );
    assert_eq!(code(&lw), 5, "long wait must time out (5): {}", stdout(&lw));
    let lwj: serde_json::Value = serde_json::from_str(&stdout(&lw)).expect("wait json");
    assert_eq!(
        lwj["error"]["kind"].as_str(),
        Some("wait"),
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

    drop(fx);
}
