//! Behavioral end-to-end tests: drive the real `webpilot` binary against a
//! local fixture server in headless mode, asserting on exit codes and output.
//!
//! These launch a real Chrome, so they are opt-in: set `WEBPILOT_E2E=1` to run
//! them (CI provisions Chrome and sets it; a plain `cargo test` skips them so it
//! stays green on machines without a browser).
//!
//!   WEBPILOT_E2E=1 cargo test -p webpilot-cli --test e2e_headless -- --nocapture
//!
//! The whole flow runs as one test because it owns a single headless session
//! (keyed by an isolated `WEBPILOT_HOME`) and tears it down at the end.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_webpilot");

const PAGE: &str = r#"<!doctype html><html><head><title>fixture</title></head>
<body>
<button id="go" onclick="document.title='clicked'">Go</button>
<input id="q" type="text" placeholder="Search">
<iframe src="/frame"></iframe>
</body></html>"#;

const FRAME: &str = r##"<!doctype html><html><head><title>frame</title></head>
<body><a id="link" href="#">inner link</a></body></html>"##;

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

/// Minimal blocking HTTP server: serves the fixture page for `/` and the inner
/// document for `/frame`. Runs on a daemon thread for the test's lifetime.
fn spawn_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 1024];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let body = if req.starts_with("GET /frame") {
                FRAME
            } else {
                PAGE
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
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

    // 3. Zero-false-positive guard: a same-document navigation (pushState)
    //    changes location.href but keeps the DOM. An index from the prior
    //    capture must STILL resolve — the snapshot binds to node identity, not
    //    URL, so invalidating here would be a false positive (a regression if
    //    anyone "fixes" staleness by comparing URLs).
    let cap = fx.run(&["capture", "--include", "dom"]);
    assert_eq!(code(&cap), 0);
    let _ = fx.run(&["eval", "history.pushState({}, '', '/changed')"]);
    let after_nav = fx.run(&["action", "click", &button_index.to_string()]);
    assert_eq!(
        code(&after_nav),
        0,
        "a URL change with the element still live must NOT raise StaleSnapshot: {}",
        stdout(&after_nav)
    );

    // 4. Stale-snapshot guard: remove the button from the DOM (out of band, via
    //    eval — which never touches the bridge snapshot), then click its old
    //    index. It must fail typed, not silently click a different element.
    let recap = fx.run(&["capture", "--include", "dom"]);
    assert_eq!(code(&recap), 0);
    let _ = fx.run(&["eval", "document.getElementById('go').remove()"]);
    let stale = fx.run(&["action", "click", &button_index.to_string()]);
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

    // 5. Policy: a deny rule is enforced at the transport boundary before the
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

    // 6. Context isolation: localStorage written in one context is invisible in
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
