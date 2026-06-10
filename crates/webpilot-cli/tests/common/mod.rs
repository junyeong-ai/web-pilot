//! Shared end-to-end fixtures: the test page, a minimal HTTP server, and
//! process-output helpers. Used by both the headless and the browser-mode
//! behavioral suites so the two modes are asserted against the same page.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Output;

pub const PAGE: &str = r#"<!doctype html><html><head><title>fixture</title></head>
<body>
<button id="go" onclick="document.title='clicked'">Go</button>
<button id="disabledbtn" disabled onclick="document.title='SHOULD-NOT-FIRE'">disabled</button>
<select id="dsel" disabled><option value="x">x</option></select>
<fieldset disabled><input id="fsfield" type="text"></fieldset>
<select id="ogsel" oninput="window.__oginput=(window.__oginput||0)+1" onchange="window.__ogchange=(window.__ogchange||0)+1"><optgroup label="g" disabled><option value="ogx">ogx</option></optgroup><option value="ogy">ogy</option></select>
<select id="multisel" multiple><option value="ma">ma</option><option value="mb">mb</option><option value="mc">mc</option></select>
<input id="q" type="text" placeholder="Search">
<input id="file" type="file">
<button id="slownav" onclick="location.href='/slow'">slow nav</button>
<a id="nav" href="/second">go second</a>
<a id="pop" href="/second" target="_blank" rel="noopener">open popup</a>
<form action="/second" method="get"><input id="forminput" type="text"><button type="submit" id="formsubmit">submit form</button></form>
<div id="cardwrap" style="cursor:pointer">clickable card<input id="hiddenchild" style="display:none"></div>
<div id="presdiv" role="presentation" onclick="document.title='pres-clicked'">presentation click target</div>
<p id="wsp">whitespace<br>collapse<br>marker</p>
<nav><div id="shadowhost"></div></nav>
<iframe src="/frame"></iframe>
<script>
  // A DOCUMENT-level delegated click listener: it fires for a click inside the
  // shadow root only if the synthetic event is `composed` (crosses the boundary).
  // `e.target` is retargeted to the host once it leaves the shadow root, so the
  // real button is found via `composedPath()` — which is populated only for a
  // composed event that actually crossed the boundary.
  document.addEventListener('click', function (e) {
    if (e.composedPath().some((n) => n && n.id === 'shadowbtn')) {
      document.title = 'shadow-delegated';
    }
  });
  const sr = document.getElementById('shadowhost').attachShadow({ mode: 'open' });
  sr.innerHTML = '<input id="shadowfile" type="file"><button id="shadowbtn">shadow</button><p>shadowonlyprose</p>';
</script>
<div id="ce" contenteditable>hello</div>
<input id="num" type="number">
</body></html>"#;

pub const FRAME: &str = r##"<!doctype html><html><head><title>frame</title></head>
<body><a id="link" href="#">inner link</a>
<a id="framenav" href="/framed2">go framed2</a>
<a id="topnav" target="_top" href="/second">go top</a>
<iframe src="/nested"></iframe></body></html>"##;

/// An HTTP iframe nested INSIDE `/frame`. Switching into `/frame` and capturing
/// must report `subframes: 1` for this — a scoped capture's subframe count is
/// the active frame's own descendants, not just the main frame's.
pub const NESTED: &str = r##"<!doctype html><html><head><title>nested</title></head>
<body><p id="deep">deepest frame</p></body></html>"##;

/// The destination of the iframe-internal `#framenav` link: a click on it while
/// switched INTO the iframe navigates only that iframe, never the top URL — so a
/// post-click capture that lands here (button `framed2btn`) proves the action
/// settled the active frame's own navigation, not just the main frame's.
pub const FRAMED2: &str = r##"<!doctype html><html><head><title>framed2</title></head>
<body><button id="framed2btn">on framed2</button></body></html>"##;

/// Two iframes with the SAME URL — `frame url /framed2` matches both, so the
/// switch must fail loud as an ambiguous selector rather than silently pick the
/// first. The disambiguation surface (`frame predicate`) stays first-match.
pub const TWOFRAMES: &str = r##"<!doctype html><html><head><title>twoframes</title></head>
<body><iframe src="/framed2"></iframe><iframe src="/framed2"></iframe></body></html>"##;

/// A page whose `Content-Security-Policy` forbids dynamic evaluation
/// (`unsafe-eval`): the parity bar for `eval` inside a switched frame. Both
/// documents carry the header, because eval runs against the FRAME's policy.
pub const CSP_PAGE: &str = r#"<!doctype html><html><head><title>csp-outer</title></head>
<body><p>csp outer</p><iframe src="/cspframe" name="cspframe"></iframe></body></html>"#;

pub const CSP_FRAME: &str = r#"<!doctype html><html><head><title>cspframe</title></head>
<body><p id="m">csp inner</p></body></html>"#;

/// The destination of `#slownav` — served with a deliberate delay so the
/// navigation is still in flight when the action returns. A capture that
/// settles on this page (title `slow-final`) proves the action waited for the
/// click-triggered navigation rather than snapshotting the dying page.
pub const SLOW: &str = r#"<!doctype html><html><head><title>slow-final</title></head>
<body><h1 id="arrived">arrived</h1></body></html>"#;

/// Emits a console log a beat AFTER it loads, so the message lands only if a
/// monitor is (re)armed on the new document — the signal the
/// eval-deny-stops-monitor-rearm assertion keys on, in both modes.
pub const LOG: &str = r#"<!doctype html><html><head><title>log-page</title></head>
<body><script>setTimeout(function(){console.log('postnav-monitor-marker')},200)</script></body></html>"#;

const CSP_HEADER: &str = "Content-Security-Policy: script-src 'self'\r\n";

/// Minimal HTTP server: serves the fixture page for `/`, the inner document for
/// `/frame`, a CSP-strict pair for `/csp` + `/cspframe`, and a deliberately
/// delayed `/slow`. Each connection is handled on its own thread so the delayed
/// route never blocks a concurrent request (e.g. the iframe load).
pub fn spawn_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            std::thread::spawn(move || {
                let mut buf = [0u8; 1024];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                // A deliberately non-UTF8 body: `fetch` must fail loud on a
                // binary response, never hand back a lossy-decoded string under
                // a success status. Raw bytes — it cannot be a &str.
                if req.starts_with("GET /binary") {
                    let bytes = [0xFFu8, 0xFE, 0x00, 0x01, 0xFF];
                    let header = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        bytes.len()
                    );
                    let _ = stream.write_all(header.as_bytes());
                    let _ = stream.write_all(&bytes);
                    return;
                }
                let (body, extra_headers) = if req.starts_with("GET /twoframes") {
                    (TWOFRAMES, "")
                } else if req.starts_with("GET /framed2") {
                    (FRAMED2, "")
                } else if req.starts_with("GET /nested") {
                    (NESTED, "")
                } else if req.starts_with("GET /frame") {
                    (FRAME, "")
                } else if req.starts_with("GET /cspframe") {
                    (CSP_FRAME, CSP_HEADER)
                } else if req.starts_with("GET /csp") {
                    (CSP_PAGE, CSP_HEADER)
                } else if req.starts_with("GET /slow") {
                    std::thread::sleep(std::time::Duration::from_millis(800));
                    (SLOW, "")
                } else if req.starts_with("GET /log") {
                    (LOG, "")
                } else {
                    (PAGE, "")
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n{}",
                    body.len(),
                    extra_headers,
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

pub fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

pub fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}
