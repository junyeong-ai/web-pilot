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
    let mut cmd = Command::new(chrome);
    cmd.args([
        "--headless=new",
        &format!("--user-data-dir={}", user_data_dir.display()),
        &format!("--load-extension={}", extension_dir.display()),
        "--remote-debugging-port=0",
        "--no-first-run",
    ]);
    // Mirror the binary's WEBPILOT_CHROME_NO_SANDBOX opt-in: this harness spawns
    // Chrome directly (not via the CLI), so it must add --no-sandbox itself to
    // run in an unprivileged CI container.
    if matches!(
        std::env::var("WEBPILOT_CHROME_NO_SANDBOX")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    ) {
        cmd.arg("--no-sandbox");
    }
    cmd.arg("about:blank")
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

    let chrome = launch(
        &home,
        &user_data_dir,
        &extension_dir.canonicalize().expect("ext dir"),
    );
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
    // The child iframe registers in webNavigation a beat after the main frame
    // settles, so a capture right after `tab new` may not count it yet —
    // re-capture until it appears, bounded. (Headless reads the CDP frame tree,
    // populated synchronously, so it has no such lag.)
    let mut cap = fx.run(&["capture", "--include", "dom"]);
    let mut snapshot: serde_json::Value =
        serde_json::from_str(&stdout(&cap)).expect("capture json");
    for _ in 0..15 {
        if snapshot["subframes"] == 2 {
            break;
        }
        cap = fx.run(&["capture", "--include", "dom"]);
        snapshot = serde_json::from_str(&stdout(&cap)).expect("capture json");
    }
    assert_eq!(code(&cap), 0, "capture failed: {}", stdout(&cap));
    let elements = snapshot["elements"].as_array().expect("elements array");
    assert!(
        elements.iter().any(|e| e["tag"] == "button"),
        "button must be indexed: {}",
        stdout(&cap)
    );
    assert!(
        elements.iter().any(|e| e["tag"] == "input"),
        "input must be indexed: {}",
        stdout(&cap)
    );
    assert_eq!(
        snapshot["subframes"],
        2,
        "from the main frame, subframes counts every nested http iframe — the /frame iframe and the /nested iframe inside it: {}",
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

    // 2a-key. An unknown key must return the typed InvalidArgument (exit 7), like
    //     headless — not ConnectionLost. The worker's error must be the wrapped
    //     `{success:false, error}` Action shape; a bare error object would parse as
    //     a malformed Action and mislabel exit 3.
    let badkey = fx.run(&["action", "key-press", "NotARealKey123"]);
    assert_eq!(
        code(&badkey),
        7,
        "an unknown key-press must be InvalidArgument (7), not ConnectionLost: {}",
        stdout(&badkey)
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

    // 2f. A click that triggers a SAME-TAB navigation (here `location.href` to a
    //     deliberately-slow `/slow`) must be detected and waited out: the action
    //     registers a commit watch before dispatching, settles on the new
    //     document, and `--capture` snapshots the destination — not the dying
    //     pre-click page. Without the watch the immediate URL check would miss
    //     the still-in-flight navigation and capture the stale page.
    let recap = fx.run(&["capture", "--include", "dom"]);
    assert_eq!(code(&recap), 0);
    let slow_snapshot: serde_json::Value =
        serde_json::from_str(&stdout(&recap)).expect("capture json");
    let slow_index = slow_snapshot["elements"]
        .as_array()
        .expect("elements")
        .iter()
        .find(|e| e["id"] == "slownav")
        .and_then(|e| e["index"].as_u64())
        .expect("slownav index")
        .to_string();
    let navd = fx.run(&["action", "click", &slow_index, "--capture"]);
    assert_eq!(code(&navd), 0, "slow-nav click failed: {}", stdout(&navd));
    let navd_json: serde_json::Value = serde_json::from_str(&stdout(&navd)).expect("action json");
    assert!(
        navd_json["url_changed"]
            .as_str()
            .is_some_and(|u| u.ends_with("/slow")),
        "a click-triggered same-tab navigation must report url_changed=/slow: {}",
        stdout(&navd)
    );
    assert_eq!(
        navd_json["page_title"].as_str(),
        Some("slow-final"),
        "--capture must snapshot the settled destination, not the dying page: {}",
        stdout(&navd)
    );
    let back_to_base = fx.run(&["action", "navigate", &base]);
    assert_eq!(
        code(&back_to_base),
        0,
        "re-navigate failed: {}",
        stdout(&back_to_base)
    );

    // 3. Stale-snapshot guard (the bridge is shared, so the typed error must
    //    hold in this mode too).
    let recap = fx.run(&["capture", "--include", "dom"]);
    assert_eq!(code(&recap), 0);
    let _ = fx.run(&["eval", "document.getElementById('go').remove()"]);
    let stale = fx.run(&["action", "click", &button_index]);
    assert_eq!(
        code(&stale),
        4,
        "stale click must exit 4: {}",
        stdout(&stale)
    );
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

    // 4b. State-keeping commands (the state.js module): a console monitor
    //     records entries the agent produces, and a cookie set is readable
    //     back — covering the monitor-injection and cookie paths end to end.
    let started = fx.run(&["console", "start"]);
    assert_eq!(
        code(&started),
        0,
        "console start failed: {}",
        stdout(&started)
    );
    let logged = fx.run(&["eval", "console.log('e2e-browser-console-marker')"]);
    assert_eq!(code(&logged), 0);
    let logs = fx.run(&["console", "read"]);
    assert!(
        stdout(&logs).contains("e2e-browser-console-marker"),
        "console monitor must record entries: {}",
        stdout(&logs)
    );

    // 4c. The eval gate covers monitor RE-injection in browser mode too: a deny
    //     landing after `console start` must stop the service worker re-arming the
    //     MAIN-world hooks on the next document (the host attaches the verdict;
    //     `rearmMonitors` honours it), matching headless `reinstall_monitors`.
    //     Confirm the self-logging page IS captured while allowed first, so the
    //     deny case can't pass on a timing miss.
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
    let bdeny = fx.run(&["policy", "set", "--operation", "eval", "--verdict", "deny"]);
    assert_eq!(code(&bdeny), 0, "policy set eval deny: {}", stdout(&bdeny));
    let _ = fx.run(&["action", "navigate", &base]);
    assert_eq!(
        code(&fx.run(&["action", "navigate", &format!("{base}/log")])),
        0,
        "navigate is allowed (only eval was denied)"
    );
    std::thread::sleep(std::time::Duration::from_millis(700));
    assert!(
        !stdout(&fx.run(&["console", "read"])).contains("postnav-monitor-marker"),
        "eval-deny must stop the service worker re-arming the monitor"
    );
    let _ = fx.run(&["policy", "clear"]);
    let _ = fx.run(&["action", "navigate", &base]);

    let cset = fx.run(&["cookie", "set", &base, "wp_e2e", "v1"]);
    assert_eq!(code(&cset), 0, "cookie set failed: {}", stdout(&cset));
    let clist = fx.run(&["cookie", "list", &base]);
    assert!(
        stdout(&clist).contains("wp_e2e"),
        "cookie list must show the set cookie: {}",
        stdout(&clist)
    );

    // 4d. Browser-mode parity for the cookie attributes: `cookie set` carries
    //     SameSite and an expiry through chrome.cookies.set, and cookie list
    //     reports them back — the same round-trip the headless path covers. A
    //     `--expires` makes a persistent cookie (carries an expiration).
    let cset_attr = fx.run(&[
        "cookie",
        "set",
        &base,
        "wp_attr",
        "v2",
        "--same-site",
        "lax",
        "--expires",
        "1900000000",
    ]);
    assert_eq!(
        code(&cset_attr),
        0,
        "cookie set with attributes failed: {}",
        stdout(&cset_attr)
    );
    let clist_attr = fx.run(&["cookie", "list", &base]);
    let caj: serde_json::Value =
        serde_json::from_str(&stdout(&clist_attr)).expect("cookie list json");
    let attr = caj
        .as_array()
        .expect("cookie list is a JSON array")
        .iter()
        .find(|c| c["name"] == "wp_attr")
        .expect("the attribute cookie is present");
    assert_eq!(
        attr["same_site"].as_str(),
        Some("lax"),
        "browser cookie set --same-site must round-trip: {}",
        stdout(&clist_attr)
    );
    assert!(
        attr["expiration"].is_number(),
        "browser cookie set --expires must be persistent (carry an expiration): {}",
        stdout(&clist_attr)
    );

    // 4e. Browser-mode parity for the session-import guard: a non-object JSON is
    //     a typed InvalidArgument (exit 7), not a false success reporting an
    //     import that applied nothing.
    let bad_session = fx.home.join("bad-session.json");
    std::fs::write(&bad_session, b"[]").expect("write bad session fixture");
    let bs = fx.run(&["session", "import", bad_session.to_str().unwrap()]);
    assert_eq!(
        code(&bs),
        7,
        "a non-object session file must be InvalidArgument in browser mode too: {}",
        stdout(&bs)
    );

    // 4f. A malformed cookie URL with a valid scheme prefix but no host
    //     (`http://`) is a typed InvalidArgument (exit 7) — matching headless,
    //     which rejects it at the CDP sink — not the generic chrome.cookies.set
    //     exception (Other, exit 1) a prefix-only scheme check used to let pass.
    let bad_cookie_url = fx.run(&["cookie", "set", "http://", "k", "v"]);
    assert_eq!(
        code(&bad_cookie_url),
        7,
        "a malformed cookie URL must be InvalidArgument in browser mode too, not Other: {}",
        stdout(&bad_cookie_url)
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
        .and_then(|t| {
            t["id"]
                .as_str()
                .map(str::to_string)
                .or_else(|| t["id"].as_u64().map(|n| n.to_string()))
        })
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
    assert_eq!(
        code(&recap),
        0,
        "capture after re-pin failed: {}",
        stdout(&recap)
    );

    // 6. Navigation settles via the predicate, not sleeps: a capture issued
    //    immediately after navigate must see the new document. And history
    //    nav is honest — `back` with no earlier entry is a typed
    //    NavigationFailed (exit 8), never a success that did nothing.
    let nav = fx.run(&["action", "navigate", &format!("{base}/frame")]);
    assert_eq!(code(&nav), 0, "navigate failed: {}", stdout(&nav));
    let cap = fx.run(&["capture", "--include", "dom"]);
    assert_eq!(
        code(&cap),
        0,
        "capture after navigate failed: {}",
        stdout(&cap)
    );
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
    // The subframe count is scoped to the ACTIVE frame (headless parity): switched
    // into /frame, it must report the one http iframe nested inside it (/nested),
    // not 0. Poll for the nested frame to register in webNavigation.
    let mut sf_cap = fx.run(&["capture", "--include", "dom"]);
    let mut sf_snap: serde_json::Value =
        serde_json::from_str(&stdout(&sf_cap)).expect("frame capture json");
    for _ in 0..15 {
        if sf_snap["subframes"] == 1 {
            break;
        }
        sf_cap = fx.run(&["capture", "--include", "dom"]);
        sf_snap = serde_json::from_str(&stdout(&sf_cap)).expect("frame capture json");
    }
    assert_eq!(
        sf_snap["subframes"],
        1,
        "a switched frame's capture must count its OWN nested http iframes: {}",
        stdout(&sf_cap)
    );
    // The accessibility tree must follow the active frame (headless parity): while
    // switched into the iframe it must describe the iframe's own controls, scoping
    // getFullAXTree to the active frame's CDP id rather than returning the root.
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
    // An ACTION's own --capture while switched into the iframe must scope the
    // subframe count to the active frame too — not just a standalone `capture`.
    // A click that stays in the frame (a same-document `#` fragment link) must
    // still report the nested /nested iframe (subframes: 1); a main-frame-only
    // gate would drop it to 0 and hide the nested iframe from the agent. Headless
    // `capture_action_snapshot` counts unconditionally; this holds browser to it.
    let link_cap = fx.run(&["capture", "--include", "dom"]);
    let link_snap: serde_json::Value =
        serde_json::from_str(&stdout(&link_cap)).expect("link capture json");
    let link_index = link_snap["elements"]
        .as_array()
        .expect("elements")
        .iter()
        .find(|e| e["id"] == "link")
        .and_then(|e| e["index"].as_u64())
        .expect("inner fragment link indexed")
        .to_string();
    let act_cap = fx.run(&["action", "click", &link_index, "--capture"]);
    assert_eq!(
        code(&act_cap),
        0,
        "in-frame fragment click --capture failed: {}",
        stdout(&act_cap)
    );
    let act_snap: serde_json::Value =
        serde_json::from_str(&stdout(&act_cap)).expect("action capture json");
    assert_eq!(
        act_snap["subframes"],
        1,
        "an action's --capture while switched into the iframe must scope subframes to it (count /nested), not 0: {}",
        stdout(&act_cap)
    );
    // A click on a link that navigates ONLY the switched iframe (not the top URL)
    // must settle the ACTIVE frame's own navigation: the auto-capture lands on the
    // iframe's new document, never the pre-click page. The top URL never moves, so
    // the main-frame settle can't see it — `frame_navigates` + waitActiveFrameSettled
    // do. (Headless parity: the same fixture + assertion in e2e_headless.)
    let frame_cap = fx.run(&["capture", "--include", "dom"]);
    let frame_snap: serde_json::Value =
        serde_json::from_str(&stdout(&frame_cap)).expect("frame capture json");
    let framenav_index = frame_snap["elements"]
        .as_array()
        .expect("elements")
        .iter()
        .find(|e| e["id"] == "framenav")
        .and_then(|e| e["index"].as_u64())
        .expect("framenav index")
        .to_string();
    let framenav_click = fx.run(&["action", "click", &framenav_index, "--capture"]);
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
    // A `--url` capture navigates to a fresh document, which drops the frame
    // scope — so `--annotate` here must succeed on the new main frame, not
    // false-fail against the stale switched-frame id. It also leaves us back on
    // the main frame, which the rest of this step then re-confirms.
    //
    // `--annotate` is given WITHOUT `--include screenshot` on purpose: it must
    // force the DOM+screenshot pass itself, exactly as headless does. This is the
    // regression guard for the parity gap where browser mode drew the overlay but
    // returned no image because the screenshot branch keyed only on `include`.
    let reannotate = fx.run(&["capture", "--annotate", "--url", &base]);
    assert_eq!(
        code(&reannotate),
        0,
        "capture --annotate --url after a frame switch must reset to main and succeed: {}",
        stdout(&reannotate)
    );
    assert!(
        stdout(&reannotate).contains("screenshot_path"),
        "capture --annotate alone must still produce a screenshot (headless parity): {}",
        stdout(&reannotate)
    );
    let main = fx.run(&["frame", "main"]);
    assert_eq!(code(&main), 0);
    let href_main = fx.run(&["eval", "location.href"]);
    assert_eq!(
        code(&href_main),
        0,
        "main-frame eval failed: {}",
        stdout(&href_main)
    );
    let hmj: serde_json::Value = serde_json::from_str(&stdout(&href_main)).expect("eval json");
    assert!(
        !hmj["result"]
            .as_str()
            .unwrap_or_default()
            .contains("/frame"),
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
    assert_eq!(
        code(&sw),
        0,
        "csp frame switch by name failed: {}",
        stdout(&sw)
    );
    // Parity with headless: the browser switch response must also carry the
    // matched frame's name (resolved from the live document).
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
    assert_eq!(
        code(&shot),
        0,
        "viewport screenshot failed: {}",
        stdout(&shot)
    );
    assert!(
        stdout(&shot).contains("screenshot_path"),
        "viewport screenshot must be persisted to a path: {}",
        stdout(&shot)
    );
    let full = fx.run(&["capture", "--include", "screenshot", "--full-page"]);
    assert_eq!(
        code(&full),
        0,
        "full-page screenshot failed: {}",
        stdout(&full)
    );
    assert!(
        stdout(&full).contains("screenshot_path"),
        "full-page screenshot must be persisted to a path: {}",
        stdout(&full)
    );

    // 9. A click-opened tab (`rel=noopener`, so correlation can't lean on
    //    `window.opener`) is reported as `new_tab` and the pin follows it —
    //    mirroring headless. This is the regression guard for the adoption
    //    race: the create event can be delivered during `settledActionUrl`, so
    //    the adoption is read AFTER settle (whose awaits yield the event loop),
    //    never by a single pre-settle check that would leave the pin on the
    //    opener. Last in the flow: it intentionally leaves an extra tab, and
    //    `drop(fx)` tears the session down.
    let pop_nav = fx.run(&["action", "navigate", &base]);
    assert_eq!(
        code(&pop_nav),
        0,
        "navigate to base failed: {}",
        stdout(&pop_nav)
    );
    let pop_cap = fx.run(&["capture", "--include", "dom"]);
    assert_eq!(code(&pop_cap), 0);
    let pop_snapshot: serde_json::Value =
        serde_json::from_str(&stdout(&pop_cap)).expect("capture json");
    let pop_index = pop_snapshot["elements"]
        .as_array()
        .expect("elements")
        .iter()
        .find(|e| e["id"] == "pop")
        .and_then(|e| e["index"].as_u64())
        .expect("pop index")
        .to_string();
    let popped = fx.run(&["action", "click", &pop_index]);
    assert_eq!(code(&popped), 0, "popup click failed: {}", stdout(&popped));
    let popped_json: serde_json::Value =
        serde_json::from_str(&stdout(&popped)).expect("action json");
    assert!(
        popped_json["new_tab"]["id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "browser popup must be reported as new_tab: {}",
        stdout(&popped)
    );
    let pop_status = fx.run(&["status"]);
    let pop_status_json: serde_json::Value =
        serde_json::from_str(&stdout(&pop_status)).expect("status json");
    assert!(
        pop_status_json["tab_url"]
            .as_str()
            .is_some_and(|u| u.ends_with("/second")),
        "the popup must be the active tab after adoption: {}",
        stdout(&pop_status)
    );

    drop(fx);
}
