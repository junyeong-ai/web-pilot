//! Behavioral end-to-end tests for the LONG-LIVED transport: drive the real
//! `webpilot mcp` server over stdio JSON-RPC against a local fixture server,
//! asserting the contract a transport that outlives one command must hold.
//!
//! The CLI is one process per command, so it re-establishes everything at `open`
//! and the e2e headless suite exercises that half. `webpilot mcp` (and the NM
//! host) serve a whole session over one transport, and Chrome outlives them
//! both: state captured at open — which page is bound, whether its tab is still
//! there — goes stale under them. That is the half this suite covers.
//!
//! These launch a real Chrome, so they are opt-in: set `WEBPILOT_E2E=1` to run
//! them, the same flag the headless suite uses.
//!
//!   WEBPILOT_E2E=1 cargo test -p webpilot-cli --test e2e_mcp -- --nocapture

mod common;

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Output, Stdio};

use common::{code, spawn_server, stdout};
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_webpilot");

/// The emulated user agent: distinctive enough that a real Chrome UA can never
/// contain it, so the assertions read the override and not a coincidence.
const UA: &str = "WebPilotProbe/1.0";

/// An isolated headless session plus the CLI processes that share it — the
/// second party every long-lived-transport case needs, since the interesting
/// staleness comes from something ELSE changing the browser.
struct Session {
    home: PathBuf,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = Command::new(BIN)
            .arg("quit")
            .env("WEBPILOT_HOME", &self.home)
            .output();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

impl Session {
    fn cli(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .arg("--json")
            .env("WEBPILOT_HOME", &self.home)
            .output()
            .expect("spawn webpilot")
    }

    /// Close a tab from a SEPARATE process, asserting the close itself landed —
    /// the setup for every case where the staleness comes from outside the
    /// long-lived session.
    fn close_tab_externally(&self, tab_id: &str) {
        let out = self.cli(&["tab", "close", tab_id]);
        assert_eq!(
            code(&out),
            0,
            "the external close must succeed: {}",
            stdout(&out)
        );
    }

    /// The id of the tab the session is pinned to, read from the `active` flag a
    /// `tab` listing carries.
    fn pinned_tab(&self) -> Option<String> {
        serde_json::from_str::<Value>(&stdout(&self.cli(&["tab"])))
            .ok()?
            .as_array()?
            .iter()
            .find(|t| t["active"] == Value::Bool(true))
            .and_then(|t| t["id"].as_str().map(str::to_owned))
    }

    fn tab_ids(&self) -> Vec<String> {
        serde_json::from_str::<Value>(&stdout(&self.cli(&["tab"])))
            .expect("tab json")
            .as_array()
            .expect("tab array")
            .iter()
            .filter_map(|t| t["id"].as_str().map(str::to_owned))
            .collect()
    }
}

/// A running `webpilot mcp` server, spoken to over its stdio JSON-RPC.
struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Mcp {
    fn start(session: &Session) -> Self {
        let mut child = Command::new(BIN)
            .arg("mcp")
            .env("WEBPILOT_HOME", &session.home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn webpilot mcp");
        let stdin = child.stdin.take().expect("mcp stdin");
        let stdout = BufReader::new(child.stdout.take().expect("mcp stdout"));
        let mut mcp = Self {
            child,
            stdin,
            stdout,
            next_id: 0,
        };
        let init = mcp.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "e2e", "version": "0"},
            }),
        );
        assert!(
            init["result"]["serverInfo"].is_object(),
            "initialize must answer with serverInfo: {init}"
        );
        mcp.notify("notifications/initialized", json!({}));
        mcp
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let line = json!({
            "jsonrpc": "2.0",
            "id": self.next_id,
            "method": method,
            "params": params,
        });
        writeln!(self.stdin, "{line}").expect("write request");
        self.stdin.flush().expect("flush request");
        let mut reply = String::new();
        self.stdout
            .read_line(&mut reply)
            .expect("read reply from the mcp server");
        serde_json::from_str(&reply).unwrap_or_else(|e| panic!("reply is not json ({e}): {reply}"))
    }

    fn notify(&mut self, method: &str, params: Value) {
        let line = json!({"jsonrpc": "2.0", "method": method, "params": params});
        writeln!(self.stdin, "{line}").expect("write notification");
        self.stdin.flush().expect("flush notification");
    }

    fn tool(&mut self, name: &str, args: Value) -> Tool {
        let reply = self.request("tools/call", json!({"name": name, "arguments": args}));
        Tool { reply }
    }
}

/// One tool result, read the way a client reads it: the typed wire error rather
/// than the prose, so a case asserts on `TabNotFound` instead of on wording.
struct Tool {
    reply: Value,
}

impl Tool {
    fn text(&self) -> String {
        self.reply["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    fn ok(&self) -> bool {
        self.reply["result"]["isError"] == Value::Bool(false)
    }

    /// The `structuredContent` error code, absent on success.
    fn code(&self) -> Option<String> {
        self.reply["result"]["structuredContent"]["code"]
            .as_str()
            .map(str::to_owned)
    }
}

#[test]
fn mcp_long_lived_transport_flow() {
    if std::env::var("WEBPILOT_E2E").is_err() {
        eprintln!("skipping e2e (set WEBPILOT_E2E=1 to run)");
        return;
    }

    let base = spawn_server();
    let session = Session {
        home: std::env::temp_dir().join(format!("webpilot-e2e-mcp-{}", std::process::id())),
    };
    let mut mcp = Mcp::start(&session);

    // 1. The transport opens lazily on the first tool call and then serves the
    //    whole session — so the surface has to work at all before any of the
    //    staleness cases mean anything.
    let tools = mcp.request("tools/list", json!({}));
    assert!(
        tools["result"]["tools"]
            .as_array()
            .is_some_and(|t| !t.is_empty()),
        "tools/list must advertise the curated surface: {tools}"
    );
    let nav = mcp.tool("browser_navigate", json!({"url": base}));
    assert!(nav.ok(), "navigate failed: {}", nav.text());
    let here = mcp.tool("browser_eval", json!({"code": "location.pathname"}));
    assert!(
        here.ok(),
        "eval on the navigated page failed: {}",
        here.text()
    );

    // 2. The session closes the tab it is itself bound to. Chrome announces that
    //    close, but the announcement lands after the `Target.closeTarget`
    //    response, so a transport that waited for it would run one more command
    //    against a session it already knows is gone — and answer an infra
    //    `ConnectionLost` where the truth is a typed `TabNotFound`.
    let second = mcp.tool("browser_tabs", json!({"action": "new", "url": base}));
    assert!(second.ok(), "tab new failed: {}", second.text());
    let own = session.pinned_tab().expect("a pinned tab");
    let closed = mcp.tool("browser_tabs", json!({"action": "close", "tab_id": own}));
    assert!(closed.ok(), "closing its own tab failed: {}", closed.text());
    let after_self_close = mcp.tool("browser_eval", json!({"code": "location.pathname"}));
    assert_eq!(
        after_self_close.code().as_deref(),
        Some("TabNotFound"),
        "a page command after the session closed its own tab must be a typed TabNotFound: {}",
        after_self_close.text()
    );
    // ...and the report is the end of it: the fallback it announced is the active
    // page, so the session keeps working instead of repeating the error.
    let recovered = mcp.tool("browser_eval", json!({"code": "location.pathname"}));
    assert!(
        recovered.ok(),
        "the announced fallback must be the active page: {}",
        recovered.text()
    );

    // 3. The same staleness from the other direction: a SEPARATE process closes
    //    the tab this session is pinned to. Nothing in the session did it, so the
    //    detach announcement is the only signal — and it must still be a typed
    //    TabNotFound rather than a failed send.
    assert!(
        mcp.tool("browser_tabs", json!({"action": "new", "url": base}))
            .ok(),
        "tab new (external-close setup) failed"
    );
    let pinned = session.pinned_tab().expect("a pinned tab");
    session.close_tab_externally(&pinned);
    let after_external_close = mcp.tool("browser_eval", json!({"code": "location.pathname"}));
    assert_eq!(
        after_external_close.code().as_deref(),
        Some("TabNotFound"),
        "a page command after another process closed the pinned tab must be a typed TabNotFound: {}",
        after_external_close.text()
    );
    assert!(
        mcp.tool("browser_eval", json!({"code": "location.pathname"}))
            .ok(),
        "the session must keep working on the announced fallback"
    );

    // 4. An explicit re-pin ANSWERS a dead pin: the agent has chosen a tab, so
    //    the session must not still be holding the vanished signal from the tab
    //    it left behind and refuse the page command that follows.
    assert!(
        mcp.tool("browser_tabs", json!({"action": "new", "url": base}))
            .ok(),
        "tab new (re-pin setup) failed"
    );
    let doomed = session.pinned_tab().expect("a pinned tab");
    session.close_tab_externally(&doomed);
    let survivor = session
        .tab_ids()
        .into_iter()
        .next()
        .expect("a surviving tab to re-pin");
    let switched = mcp.tool(
        "browser_tabs",
        json!({"action": "switch", "tab_id": survivor}),
    );
    assert!(
        switched.ok(),
        "tab switch must resolve a dead pin: {}",
        switched.text()
    );
    let after_repin = mcp.tool("browser_eval", json!({"code": "location.pathname"}));
    assert!(
        after_repin.ok(),
        "a page command after an explicit re-pin must not report the tab the agent left: {}",
        after_repin.text()
    );

    // 5. Device emulation is one record with two lifetimes: the metrics override
    //    outlives the CDP client that set it while the user-agent override reverts
    //    with it. A session that established the device once would drift into a
    //    mobile viewport behind a desktop identity — so the emulation answers to
    //    the persisted record on every command, and follows a bind. Runs last: it
    //    reshapes the viewport every case above reads.
    let set = session.cli(&[
        "device",
        "set",
        "--width",
        "400",
        "--height",
        "800",
        "--user-agent",
        UA,
    ]);
    assert_eq!(code(&set), 0, "device set failed: {}", stdout(&set));
    let ua = mcp.tool("browser_eval", json!({"code": "navigator.userAgent"}));
    assert!(
        ua.text().contains(UA),
        "a device set from another process must reach the live session's user agent, not only its viewport: {}",
        ua.text()
    );
    let width = mcp.tool("browser_eval", json!({"code": "innerWidth"}));
    assert!(
        width.text().contains("400"),
        "the emulated viewport must be live too: {}",
        width.text()
    );
    assert!(
        mcp.tool("browser_tabs", json!({"action": "new", "url": base}))
            .ok(),
        "tab new (emulation setup) failed"
    );
    let ua_after_bind = mcp.tool("browser_eval", json!({"code": "navigator.userAgent"}));
    assert!(
        ua_after_bind.text().contains(UA),
        "binding another tab must not drop the emulation the agent set: {}",
        ua_after_bind.text()
    );
    let width_after_bind = mcp.tool("browser_eval", json!({"code": "innerWidth"}));
    assert!(
        width_after_bind.text().contains("400"),
        "the emulated viewport must follow the bind: {}",
        width_after_bind.text()
    );
    let reset = session.cli(&["device", "reset"]);
    assert_eq!(code(&reset), 0, "device reset failed: {}", stdout(&reset));
    let ua_after_reset = mcp.tool("browser_eval", json!({"code": "navigator.userAgent"}));
    assert!(
        !ua_after_reset.text().contains(UA),
        "a device reset from another process must clear the user-agent override this session owns: {}",
        ua_after_reset.text()
    );
}
