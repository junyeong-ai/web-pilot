//! Browser-mode end-to-end: drive the real `webpilot --browser` pipeline —
//! CLI → Unix socket → NM host → extension service worker → bridge.js —
//! against a real Chrome with the extension loaded, asserting on exit codes
//! and output. The same fixture page as the headless suite, so the two modes
//! are held to the same behavior.
//!
//! Opt-in: set `WEBPILOT_E2E_BROWSER=1`. Requires a **Chrome for Testing**
//! binary (`WEBPILOT_CHROME`, or the agent-browser layout that `find_chrome`
//! prefers): branded Chrome silently ignores `--load-extension` (removed in
//! 137), which this harness detects by the missing extension target and
//! reports precisely instead of timing out.
//!
//! Isolation — nothing global is touched:
//! - Chrome runs in a temp `--user-data-dir`, and the NM host manifest lives in
//!   `<user-data-dir>/NativeMessagingHosts/` (Chrome resolves user-level NM
//!   manifests against the active user data dir — verified on macOS + Linux).
//! - Chrome is launched with `WEBPILOT_HOME` pointing at a temp dir. The NM
//!   host it spawns inherits that environment, so the IPC socket, policies,
//!   and artifacts all land in the same isolated home the CLI probes.

mod common;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use common::{code, spawn_server, stdout};

const BIN: &str = env!("CARGO_BIN_EXE_webpilot");

struct BrowserFixture {
    home: PathBuf,
    user_data_dir: PathBuf,
    chrome: Child,
}

impl Drop for BrowserFixture {
    fn drop(&mut self) {
        let _ = self.chrome.kill();
        let _ = self.chrome.wait();
        let _ = std::fs::remove_dir_all(&self.home);
        let _ = std::fs::remove_dir_all(&self.user_data_dir);
    }
}

impl BrowserFixture {
    /// Run `webpilot --browser <args>` against the isolated home.
    fn run(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .arg("--browser")
            .args(args)
            .arg("--json")
            .env("WEBPILOT_HOME", &self.home)
            .output()
            .expect("spawn webpilot")
    }
}

/// The Chrome for Testing binary for this run. Branded Chrome is useless here
/// (it ignores `--load-extension`), so only explicit/CfT sources qualify.
fn chrome_binary() -> PathBuf {
    if let Ok(p) = std::env::var("WEBPILOT_CHROME") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").expect("HOME");
    let browsers = PathBuf::from(home).join(".agent-browser/browsers");
    let mut versions: Vec<_> = std::fs::read_dir(&browsers)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    // Natural version order, matching production `find_chrome`: lexicographic
    // ranks "99" above "120".
    versions.sort_by_key(|p| {
        p.file_name()
            .map(|n| {
                n.to_string_lossy()
                    .split('.')
                    .map(|c| c.parse::<u64>().unwrap_or(0))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    });
    for dir in versions.into_iter().rev() {
        for rel in [
            "chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
            "chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
            "chrome-linux64/chrome",
        ] {
            let p = dir.join(rel);
            if p.exists() {
                return p;
            }
        }
    }
    panic!(
        "no Chrome for Testing found: set WEBPILOT_CHROME to a CfT binary \
         (branded Chrome cannot load the extension — `--load-extension` was removed)"
    );
}

/// Minimal HTTP GET against the DevTools endpoint (no client dependency).
/// HTTP/1.1 with `Connection: close` — the DevTools server silently drops
/// HTTP/1.0 requests (measured), and honors close so EOF delimits the body.
/// Every I/O step is bounded: this runs inside `wait_for` probes, and a probe
/// that can block forever would defeat the deadline that calls it (measured:
/// the server occasionally accepts and never responds during warmup).
fn http_get(port: u16, path: &str) -> String {
    let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", port)) else {
        return String::new();
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
    let req = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    if stream.write_all(req.as_bytes()).is_err() {
        return String::new();
    }
    let mut raw = Vec::new();
    let mut buf = [0u8; 65536];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => raw.extend_from_slice(&buf[..n]),
            Err(_) => break, // timeout/reset: parse whatever arrived
        }
    }
    let raw = String::from_utf8_lossy(&raw);
    raw.split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default()
}

fn wait_for<T>(deadline: Duration, what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = probe() {
            return v;
        }
        assert!(start.elapsed() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn spawn_chrome(chrome: &Path, home: &Path, user_data_dir: &Path, extension_dir: &Path) -> Child {
    Command::new(chrome)
        .args([
            "--headless=new",
            &format!("--user-data-dir={}", user_data_dir.display()),
            &format!("--load-extension={}", extension_dir.display()),
            "--remote-debugging-port=0",
            "--no-first-run",
            "about:blank",
        ])
        .env("WEBPILOT_HOME", home)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn chrome")
}

/// Bring up Chrome with the extension and the NM manifest registered.
///
/// Two launches by design. The manifest must exist BEFORE the service worker's
/// startup `connectNative` — a manifest written later relies on the SW's retry
/// timers, which MV3 may have suspended by then. But the manifest needs the
/// extension ID, which Chrome derives from the (stable) extension path — so a
/// throwaway first launch reads the ID from the DevTools target list, and the
/// real launch then connects on startup, deterministically.
fn launch(home: &Path, user_data_dir: &Path, extension_dir: &Path) -> Child {
    let chrome = chrome_binary();

    let mut discovery = spawn_chrome(&chrome, home, user_data_dir, extension_dir);
    let port_file = user_data_dir.join("DevToolsActivePort");
    let port: u16 = wait_for(Duration::from_secs(30), "DevToolsActivePort", || {
        std::fs::read_to_string(&port_file)
            .ok()
            .and_then(|s| s.lines().next().and_then(|l| l.parse().ok()))
    });
    let extension_id = wait_for(Duration::from_secs(15), "extension target", || {
        let targets: serde_json::Value =
            serde_json::from_str(&http_get(port, "/json")).unwrap_or(serde_json::Value::Null);
        targets.as_array().and_then(|ts| {
            ts.iter().find_map(|t| {
                let url = t["url"].as_str()?;
                url.strip_prefix("chrome-extension://")?
                    .split_once("/background/service-worker.js")
                    .map(|(id, _)| id.trim_end_matches('/').to_string())
            })
        })
    });
    let _ = discovery.kill();
    let _ = discovery.wait();
    let _ = std::fs::remove_file(&port_file);

    let nm_dir = user_data_dir.join("NativeMessagingHosts");
    std::fs::create_dir_all(&nm_dir).expect("nm dir");
    let manifest = serde_json::json!({
        "name": "com.webpilot.host",
        "description": "WebPilot e2e",
        "path": BIN,
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{extension_id}/")],
    });
    std::fs::write(
        nm_dir.join("com.webpilot.host.json"),
        serde_json::to_string_pretty(&manifest).expect("static manifest"),
    )
    .expect("write nm manifest");

    spawn_chrome(&chrome, home, user_data_dir, extension_dir)
}

#[test]
fn browser_behavioral_flow() {
    if std::env::var("WEBPILOT_E2E_BROWSER").is_err() {
        eprintln!("skipping browser e2e (set WEBPILOT_E2E_BROWSER=1 to run)");
        return;
    }

    let base = spawn_server();
    // PID + nanos: a recycled PID must never resurrect a crashed run's profile
    // (a stale DevToolsActivePort would mis-route the discovery probe).
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let stamp = format!("{}-{nanos}", std::process::id());
    let home = std::env::temp_dir().join(format!("webpilot-e2e-browser-{stamp}"));
    let user_data_dir = std::env::temp_dir().join(format!("webpilot-e2e-profile-{stamp}"));
    std::fs::create_dir_all(&home).expect("home");
    std::fs::create_dir_all(&user_data_dir).expect("user data dir");
    let extension_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../extension");

    let chrome = launch(&home, &user_data_dir, &extension_dir.canonicalize().expect("ext dir"));
    let fx = BrowserFixture {
        home,
        user_data_dir,
        chrome,
    };

    // 0. The whole chain comes up: SW retry → host spawn → socket → status.
    wait_for(Duration::from_secs(30), "host connection", || {
        let out = fx.run(&["status"]);
        let v: serde_json::Value = serde_json::from_str(&stdout(&out)).ok()?;
        (v["connected"] == true).then_some(())
    });

    // 1. Open the fixture and capture: same assertions as headless step 1.
    let tab = fx.run(&["tab", "new", &base]);
    assert_eq!(code(&tab), 0, "tab new failed: {}", stdout(&tab));
    let cap = fx.run(&["capture", "--include", "dom"]);
    assert_eq!(code(&cap), 0, "capture failed: {}", stdout(&cap));
    let snapshot: serde_json::Value =
        serde_json::from_str(&stdout(&cap)).expect("capture json");
    let elements = snapshot["elements"].as_array().expect("elements array");
    assert!(
        elements.iter().any(|e| e["tag"] == "button"),
        "button must be indexed: {}",
        stdout(&cap)
    );
    let button_index = elements
        .iter()
        .find(|e| e["tag"] == "button")
        .and_then(|e| e["index"].as_u64())
        .expect("button index")
        .to_string();

    // 2. Click by captured index; the handler sets the title.
    let click = fx.run(&["action", "click", &button_index]);
    assert_eq!(code(&click), 0, "click failed: {}", stdout(&click));
    let title = fx.run(&["eval", "document.title"]);
    assert!(
        stdout(&title).contains("clicked"),
        "click should have run the handler: {}",
        stdout(&title)
    );

    // 2b. Hover rides the CDP input path in this mode too (bridge resolves the
    //     element centre, the worker moves the real cursor) — exercising the
    //     coordinate handoff end-to-end, same as headless `do_hover`.
    let hover = fx.run(&["action", "hover", &button_index]);
    assert_eq!(code(&hover), 0, "hover failed: {}", stdout(&hover));

    // 2c. Upload sets a file on the input the index addressed — snapshot
    //     identity handed to CDP as an object reference (the objectId resolved
    //     in the content-script's ISOLATED world), parity with headless. Prove
    //     the file landed on #file; uploading onto a non-file element is a typed
    //     InvalidArgument (exit 7) caught at the bridge before any CDP sink.
    let cap_up = fx.run(&["capture", "--include", "dom"]);
    assert_eq!(code(&cap_up), 0);
    let up_snapshot: serde_json::Value =
        serde_json::from_str(&stdout(&cap_up)).expect("capture json");
    let index_by_id = |id: &str| -> String {
        up_snapshot["elements"]
            .as_array()
            .expect("elements")
            .iter()
            .find(|e| e["id"] == id)
            .and_then(|e| e["index"].as_u64())
            .unwrap_or_else(|| panic!("element #{id} not captured: {}", stdout(&cap_up)))
            .to_string()
    };
    let file_index = index_by_id("file");
    let upload_src = fx.home.join("upload-src.txt");
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
    let bad = fx.run(&["action", "upload", &button_index, src]);
    assert_eq!(
        code(&bad),
        7,
        "upload onto a non-file element must be InvalidArgument: {}",
        stdout(&bad)
    );

    // 2d. The object handoff reaches a file input inside an OPEN SHADOW ROOT —
    //     the snapshot pierces shadow and the ISOLATED-world objectId crosses
    //     the boundary a document-root selector cannot. Parity with headless.
    let shadow_index = index_by_id("shadowfile");
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

    // 2e. `fetch` runs as a debugger-routed MAIN-world eval (no contextId,
    //     CSP-exempt) and returns the response body — a same-origin GET against
    //     the fixture server comes back with the page markup, parity with the
    //     headless suite.
    let fetched = fx.run(&["fetch", &base]);
    assert_eq!(code(&fetched), 0, "fetch failed: {}", stdout(&fetched));
    assert!(
        stdout(&fetched).contains("shadowhost") || stdout(&fetched).contains("<button"),
        "fetch must return the page body: {}",
        stdout(&fetched)
    );

    // 3. Stale-snapshot guard (the bridge is shared, so the typed error must
    //    hold in this mode too).
    let recap = fx.run(&["capture", "--include", "dom"]);
    assert_eq!(code(&recap), 0);
    let _ = fx.run(&["eval", "document.getElementById('go').remove()"]);
    let stale = fx.run(&["action", "click", &button_index]);
    assert_eq!(code(&stale), 4, "stale click must exit 4: {}", stdout(&stale));
    assert!(
        stdout(&stale).contains("StaleSnapshot"),
        "{}",
        stdout(&stale)
    );

    // 4. Policy is enforced at the NM host — the browser-mode privileged sink.
    //    The host and the CLI share the isolated WEBPILOT_HOME (the host
    //    inherits it from Chrome), so they read the same store.
    let recap = fx.run(&["capture", "--include", "dom"]);
    assert_eq!(code(&recap), 0);
    let set = fx.run(&["policy", "set", "--operation", "click", "--verdict", "deny"]);
    assert_eq!(code(&set), 0, "policy set failed: {}", stdout(&set));
    let denied = fx.run(&["action", "click", "1"]);
    assert_eq!(code(&denied), 6, "denied click must exit 6: {}", stdout(&denied));
    assert!(
        stdout(&denied).contains("PolicyDenied"),
        "{}",
        stdout(&denied)
    );
    let clear = fx.run(&["policy", "clear"]);
    assert_eq!(code(&clear), 0);

    // 4b. State-keeping commands (the state.js module): a console monitor
    //     records entries the agent produces, and a cookie set is readable
    //     back — covering the monitor-injection and cookie paths end to end.
    let started = fx.run(&["console", "start"]);
    assert_eq!(code(&started), 0, "console start failed: {}", stdout(&started));
    let logged = fx.run(&["eval", "console.log('e2e-browser-console-marker')"]);
    assert_eq!(code(&logged), 0);
    let logs = fx.run(&["console", "read"]);
    assert!(
        stdout(&logs).contains("e2e-browser-console-marker"),
        "console monitor must record entries: {}",
        stdout(&logs)
    );
    let cset = fx.run(&["cookie", "set", &base, "wp_e2e", "v1"]);
    assert_eq!(code(&cset), 0, "cookie set failed: {}", stdout(&cset));
    let clist = fx.run(&["cookie", "list", &base]);
    assert!(
        stdout(&clist).contains("wp_e2e"),
        "cookie list must show the set cookie: {}",
        stdout(&clist)
    );

    // 5. Deterministic tab binding: commands act on the pinned tab, and a
    //    vanished pin is a typed TabNotFound — never a silent retarget to
    //    whatever tab happens to be focused. `tab new` re-pins.
    let tabs = fx.run(&["tab"]);
    assert_eq!(code(&tabs), 0, "tab list failed: {}", stdout(&tabs));
    let tabs_json: serde_json::Value = serde_json::from_str(&stdout(&tabs)).expect("tabs json");
    let fixture_tab = tabs_json
        .as_array()
        .expect("tabs array")
        .iter()
        .find(|t| t["url"].as_str().is_some_and(|u| u.starts_with(&base)))
        .and_then(|t| t["id"].as_str().map(str::to_string).or_else(|| t["id"].as_u64().map(|n| n.to_string())))
        .expect("fixture tab id");
    let closed = fx.run(&["tab", "close", &fixture_tab]);
    assert_eq!(code(&closed), 0, "tab close failed: {}", stdout(&closed));
    let orphaned = fx.run(&["capture", "--include", "dom"]);
    assert_eq!(
        code(&orphaned),
        4,
        "a command against a closed pin must be typed TabNotFound, not retargeted: {}",
        stdout(&orphaned)
    );
    assert!(
        stdout(&orphaned).contains("TabNotFound"),
        "{}",
        stdout(&orphaned)
    );
    let repin = fx.run(&["tab", "new", &base]);
    assert_eq!(code(&repin), 0, "re-pin failed: {}", stdout(&repin));
    let recap = fx.run(&["capture", "--include", "dom"]);
    assert_eq!(code(&recap), 0, "capture after re-pin failed: {}", stdout(&recap));

    // 6. Navigation settles via the predicate, not sleeps: a capture issued
    //    immediately after navigate must see the new document. And history
    //    nav is honest — `back` with no earlier entry is a typed
    //    NavigationFailed (exit 8), never a success that did nothing.
    let nav = fx.run(&["action", "navigate", &format!("{base}/frame")]);
    assert_eq!(code(&nav), 0, "navigate failed: {}", stdout(&nav));
    let cap = fx.run(&["capture", "--include", "dom"]);
    assert_eq!(code(&cap), 0, "capture after navigate failed: {}", stdout(&cap));
    assert!(
        stdout(&cap).contains("inner link"),
        "the navigated-to document must be captured immediately: {}",
        stdout(&cap)
    );
    let back = fx.run(&["action", "back"]);
    assert_eq!(code(&back), 0, "real back failed: {}", stdout(&back));
    let fresh = fx.run(&["tab", "new", &base]);
    assert_eq!(code(&fresh), 0);
    let no_history = fx.run(&["action", "back"]);
    assert_eq!(
        code(&no_history),
        8,
        "back with no history must be a typed NavigationFailed: {}",
        stdout(&no_history)
    );

    // 7. Frame-scoped eval: after switching into the iframe, eval runs THERE
    //    (headless parity) — not silently in the main frame.
    let cap = fx.run(&["capture", "--include", "dom"]);
    assert_eq!(code(&cap), 0);
    let sw = fx.run(&["frame", "url", "/frame"]);
    assert_eq!(code(&sw), 0, "frame switch failed: {}", stdout(&sw));
    let href = fx.run(&["eval", "location.href"]);
    assert_eq!(code(&href), 0, "frame eval failed: {}", stdout(&href));
    assert!(
        stdout(&href).contains("/frame"),
        "eval must run in the switched frame: {}",
        stdout(&href)
    );
    let main = fx.run(&["frame", "main"]);
    assert_eq!(code(&main), 0);
    let href_main = fx.run(&["eval", "location.href"]);
    assert!(
        !stdout(&href_main).contains("/frame"),
        "after frame main, eval must run in the main frame again: {}",
        stdout(&href_main)
    );

    // 7b. A CSP-strict iframe (`script-src 'self'`, no unsafe-eval) keeps the
    //     full frame surface working: switching by NAME (a precompiled
    //     `window.name` read no CSP can refuse), eval inside the frame, and a
    //     `frame find` predicate all ride debugger-routed evaluation, which is
    //     not subject to page CSP — exactly like headless. This is the
    //     regression test for the retired scripting-injection eval path.
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
    let main = fx.run(&["frame", "main"]);
    assert_eq!(code(&main), 0);
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
    let main = fx.run(&["frame", "main"]);
    assert_eq!(code(&main), 0);

    // 7c. A STATEMENT-form predicate and a statement-form eval must work inside
    //     the CSP frame too — cdpEval's compile-then-evaluate, the parity bar the
    //     headless suite asserts against its shared eval_form.
    let found_stmt = fx.run(&[
        "frame",
        "find",
        "const t = document.title; t === 'cspframe'",
    ]);
    assert_eq!(
        code(&found_stmt),
        0,
        "statement-form predicate must find the CSP frame: {}",
        stdout(&found_stmt)
    );
    let stmt_eval = fx.run(&["eval", "const x = document.title; x"]);
    assert!(
        stdout(&stmt_eval).contains("cspframe"),
        "statement-form eval must return a value inside a CSP frame: {}",
        stdout(&stmt_eval)
    );
    let main = fx.run(&["frame", "main"]);
    assert_eq!(code(&main), 0);

    // 8. Both screenshot paths ride CDP, not `captureVisibleTab`: they capture
    //    the tab's own surface through the debugger, so they need neither the
    //    window to be OS-foreground nor an `<all_urls>` grant. The viewport
    //    shot is the common case; the full-page shot adds captureBeyondViewport.
    let shot = fx.run(&["capture", "--include", "screenshot"]);
    assert_eq!(code(&shot), 0, "viewport screenshot failed: {}", stdout(&shot));
    assert!(
        stdout(&shot).contains("screenshot_path"),
        "viewport screenshot must be persisted to a path: {}",
        stdout(&shot)
    );
    let full = fx.run(&["capture", "--include", "screenshot", "--full-page"]);
    assert_eq!(code(&full), 0, "full-page screenshot failed: {}", stdout(&full));
    assert!(
        stdout(&full).contains("screenshot_path"),
        "full-page screenshot must be persisted to a path: {}",
        stdout(&full)
    );

    drop(fx);
}
