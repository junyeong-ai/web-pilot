//! Shared end-to-end fixtures: the test page, a minimal HTTP server, and
//! process-output helpers. Used by the headless, browser-mode and MCP behavioral
//! suites so every surface is asserted against the same page.
//!
//! Each suite links its own copy and uses the subset it needs — a fixture only
//! one of them reaches for is not dead, so the module carries the allow rather
//! than every suite carrying an artificial reference to keep it quiet.
#![allow(dead_code)]

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
<input id="maxed" type="text" maxlength="3">
<input id="file" type="file">
<input id="hiddenfile" type="file" style="display:none">
<button id="slownav" onclick="location.href='/slow'">slow nav</button>
<a id="nav" href="/second">go second</a>
<a id="selfref" href="">selfref</a>
<a id="pop" href="/second" target="_blank" rel="noopener">open popup</a>
<form action="/second" method="get"><input id="forminput" type="text"><button type="submit" id="formsubmit">submit form</button></form>
<div id="cardwrap" style="cursor:pointer">clickable card<input id="hiddenchild" style="display:none"></div>
<button id="ptrbtn" style="cursor:pointer"><span id="ptrbtnlabel">Pointer label</span></button>
<a id="vishidewrap" href="/anchor" style="visibility:hidden"><span id="vischild" style="visibility:visible;cursor:pointer">visible child in hidden wrap</span></a>
<div id="presdiv" role="presentation" onclick="document.title='pres-clicked'">presentation click target</div>
<ul role="listbox"><li id="ariaopt" role="option" onclick="" style="cursor:pointer">aria option click</li><li id="ariaoptrove" role="option" tabindex="0">aria option roving</li></ul>
<div role="tree"><div id="ariatreeitem" role="treeitem" tabindex="0">tree item</div></div>
<div id="invalidrole" role="zzgarbage" tabindex="0">invalid role focusable</div>
<div role="listbox" aria-activedescendant="kbopt" aria-label="kb"><div id="kbopt" role="option">kb only option</div></div>
<div id="kbspin" role="spinbutton" aria-valuenow="1" aria-label="kbqty">kb spinbutton</div>
<p id="wsp">whitespace<br>collapse<br>marker</p>
<div id="slothost2"><button id="slottedbtn" slot="t">SB</button></div>
<nav><div id="shadowhost"><span slot="s">SL</span></div></nav>
<div role="navigation"><button id="arianavbtn" onclick="">in aria nav</button></div>
<article><header><button id="artheaderbtn" onclick="">in article header</button></header></article>
<select id="hidopt"><option value="vis">Visible</option><option value="hid" style="display:none">Hidden</option></select>
<form action="/second"><input type="image" id="imgbtn" alt="Image submit" src="data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw=="></form>
<iframe src="/frame" name="innerfr"></iframe>
<button id="dlg" onclick="window.__dlg=[confirm('c?'), prompt('p?', 'dv')]; document.title='dlg-done'">dialogs</button>
<button id="mkif" onclick="const f=document.createElement('iframe'); f.src='/latealert'; document.body.appendChild(f)">mk iframe</button>
<div style="height:3000px"></div>
<button id="deepbtn" onclick="document.title='deep-clicked'">deep button</button>
<a id="dlnav" href="/attachment">download invoice</a>
<a id="dldirect" download="direct.txt" href="data:text/plain,hello">direct download</a>
<a id="dlpopup" href="/attachment" target="_blank" rel="noopener">download in a new tab</a>
<a id="dlslow" href="/slowattachment">slow download</a>
<button id="dlblob" onclick="webpilotExport()">export blob</button>
<button id="dlpair" onclick="webpilotExport('one.csv'); webpilotExport('two.csv')">export pair</button>
<script>
  // An export button of the shape SPAs actually ship: build the file client-side
  // and click a hidden `<a download>`. The browser never navigates, so only the
  // Navigation API's `downloadRequest` reveals that a file was written.
  function webpilotExport(name) {
    const blob = new Blob(["a,b\n1,2\n"], { type: "text/csv" });
    const a = document.createElement("a");
    a.href = window.URL.createObjectURL(blob);
    a.download = name || "export.csv";
    document.body.appendChild(a);
    a.click();
    a.remove();
  }
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
  // `slotbtn` paints its visible content through a slotted LIGHT span — the
  // occlusion hit-test over its label lands on light DOM, which the composed
  // walk must relate back through the slot assignment, or the shadow button
  // would read occluded by its own label.
  sr.innerHTML = '<input id="shadowfile" type="file"><button id="shadowbtn">shadow</button><p>shadowonlyprose</p><button id="slotbtn"><slot name="s"></slot></button>';
  // A slotted LIGHT control rendering inside a SHADOW landmark: the flat tree
  // (which the a11y tree follows) places #slottedbtn under the shadow <aside>,
  // while its light ancestors carry no landmark at all — the landmark walk
  // must follow the slot assignment, not the light parent chain.
  const sr2 = document.getElementById('slothost2').attachShadow({ mode: 'open' });
  // The named slot carries an ASSIGNED light node; the second slot is
  // UNASSIGNED, so its FALLBACK text renders from the shadow side — the text
  // capture must include it (the base innerText never sees it) without
  // double-counting the assigned slot's light content.
  sr2.innerHTML = '<aside><slot name="t"></slot><slot name="empty">slotfallbackprose</slot></aside>';
</script>
<div id="ce" contenteditable>hello</div>
<input id="num" type="number">
<div id="dragme" draggable="true">drag me</div>
</body></html>"#;

pub const FRAME: &str = r##"<!doctype html><html><head><title>frame</title></head>
<body><a id="link" href="#">inner link</a>
<a id="framenav" href="/framed2">go framed2</a>
<a id="topnav" target="_top" href="/second">go top</a>
<a id="selftarget" target="innerfr" href="/framed2">self by name</a>
<input id="framefile" type="file">
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

/// Named-target resolution. A `target` that is not a keyword names a browsing
/// context, and the name that resolves is the context's LIVE one: an
/// `<iframe name>` attribute only seeds it, so a frame that has renamed itself
/// answers to the new name and nothing answers to the attribute's stale value.
pub const NAMED: &str = r##"<!doctype html><html><head><title>named</title></head>
<body>
<a id="livetarget" href="/framed2" target="livename">into the renamed frame</a>
<a id="staletarget" href="/framed2" target="attrname">by the stale attribute</a>
<a id="disownedtarget" href="/framed2" target="livename" rel="noopener">the same name, disowned</a>
<iframe src="/renamed" name="attrname"></iframe>
</body></html>"##;

/// The child of `/named`, which renames its own browsing context on load.
pub const RENAMED: &str = r##"<!doctype html><html><head><title>renamed</title></head>
<body><script>window.name = "livename";</script><p id="renamedmark">renamed</p></body></html>"##;

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

/// Logs from an inline script and fetches while the document is still parsing —
/// output no injection made after the load could ever see. It also carries an
/// iframe that logs its own marker, which must NOT reach the buffer: only the
/// main frame's is read, so hooking a subframe would put WebPilot's patch in a
/// third-party document for nothing.
pub const LOADLOG: &str = r##"<!doctype html><html><head><title>loadlog</title>
<script>console.log('loadwindow-console-marker')</script></head>
<body><script>fetch('/loadfetch')</script><iframe src="/loadframe"></iframe></body></html>"##;

/// Everything the page's console shows while the document is still parsing that
/// a plain `console.log` would not produce: an exception that reaches the top of
/// the stack, one the page CANCELS (the browser prints nothing for it, so the
/// monitor must not either), a rejection nothing handles, a failed assertion —
/// and a passing one plus a subresource that fails, neither of which the browser
/// prints.
pub const PAGE_ERROR: &str = r##"<!doctype html><html><head><title>pageerror</title>
<script>window.addEventListener("error", function (e) { if (e.message.indexOf("cancelled") >= 0) e.preventDefault(); });</script>
</head><body>
<script>null.pageErrorMarker;</script>
<script>throw new Error("cancelled-error-marker");</script>
<script>Promise.reject(new Error("rejection-marker"));</script>
<script>console.assert(true, "passing-assert-marker"); console.assert(false, "assert-marker");</script>
<img src="/notanimage">
</body></html>"##;

/// An exception held for its cancellation verdict while a `console.log` is
/// recorded in the meantime: the inner timer is queued before the throw unwinds,
/// so it runs during the hold, and it burns past a millisecond boundary so the
/// two entries cannot share a timestamp. The buffer's timestamps must still not
/// run backwards — they are the cursor `--since` reads from.
pub const HELD_ORDER: &str = r##"<!doctype html><html><head><title>heldorder</title></head>
<body><script>
setTimeout(function () {
  setTimeout(function () {
    var until = Date.now() + 5;
    while (Date.now() < until) {}
    console.log("held-order-marker");
  }, 0);
  null.heldOrderProbe;
}, 0);
</script></body></html>"##;

/// Navigates ITSELF, long enough after loading that the process which drove it
/// there has exited: the document it lands on is built with nothing attached, so
/// no recorder can be in it from the start. The one document a WebPilot process
/// cannot cover, and the reason a read reports whether it covered the load — an
/// empty buffer here is the recorder's absence, not the page's silence.
pub const SELF_NAV: &str = r##"<!doctype html><html><head><title>selfnav</title></head>
<body><script>setTimeout(function () { location.href = "/loadlog"; }, 1500);</script></body></html>"##;

/// The iframe of `/loadlog`.
pub const LOAD_FRAME: &str = r#"<!doctype html><html><head><title>loadframe</title></head>
<body><script>console.log('loadwindow-subframe-marker')</script></body></html>"#;

/// Fires an `alert` a beat after loading — the destination of the iframe a
/// click handler CREATES (`#mkif`). A dialog from a frame that did not exist
/// when the action started must still be intercepted (browser mode injects the
/// override per committed document; headless answers via the CDP responder),
/// or the native modal wedges every later command on the tab.
pub const LATE_ALERT: &str = r#"<!doctype html><html><head><title>late-alert</title></head>
<body><script>setTimeout(function(){alert('late')},400)</script></body></html>"#;

/// Self-navigates to `/redirtarget` a beat after loading — long enough that a
/// `wait selector` issued right after the navigate is already polling in-page
/// when the document is torn down. The wait must survive the navigation and
/// satisfy against the NEW document (`#navgoal`), in both modes.
pub const SLOW_REDIR: &str = r#"<!doctype html><html><head><title>slow-redir</title></head>
<body><p>leaving soon</p><script>setTimeout(function(){location.href='/redirtarget'},800)</script></body></html>"#;

/// Where `/slowredir` lands — carries the element the surviving wait keys on.
pub const REDIR_TARGET: &str = r#"<!doctype html><html><head><title>redir-target</title></head>
<body><div id="navgoal">navgoal arrived</div></body></html>"#;

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
                // A 204 No Content: the browser ABORTS the navigation (ERR_ABORTED)
                // and keeps the previous document. `navigate` must return success
                // fast on the still-live page, not spin to the navigation timeout
                // and report a false NavigationFailed.
                // Resolves to a FILE, not a document: the navigation aborts and the
                // browser downloads instead. The body is checked byte-for-byte, so
                // the test can tell a real transfer from a placeholder.
                // More interactive elements than the bridge's index cap, so the
                // clip is exercised against a real extraction rather than a stub.
                if req.starts_with("GET /many") {
                    let mut body = String::from("<!doctype html><html><body>");
                    for i in 0..1200 {
                        body.push_str(&format!("<a href=\"/n{i}\">link {i}</a>"));
                    }
                    body.push_str("</body></html>");
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    return;
                }
                // Bytes that arrive over several seconds, so a command cannot
                // observe a terminal state within its budget: the outcome must be
                // reported as still running rather than as finished.
                if req.starts_with("GET /slowattachment") {
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=\"slow.bin\"\r\nContent-Length: 60000\r\nConnection: close\r\n\r\n",
                    );
                    for _ in 0..6 {
                        if stream.write_all(&[b'Z'; 10_000]).is_err() {
                            return;
                        }
                        let _ = stream.flush();
                        std::thread::sleep(std::time::Duration::from_millis(700));
                    }
                    return;
                }
                if req.starts_with("GET /attachment") {
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/pdf\r\nContent-Disposition: attachment; filename=\"invoice.pdf\"\r\nContent-Length: 12\r\nConnection: close\r\n\r\nINVOICE-BODY",
                    );
                    return;
                }
                if req.starts_with("GET /empty204") {
                    let _ =
                        stream.write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n");
                    return;
                }
                let (body, extra_headers) = if req.starts_with("GET /twoframes") {
                    (TWOFRAMES, "")
                } else if req.starts_with("GET /framed2") {
                    (FRAMED2, "")
                } else if req.starts_with("GET /nested") {
                    (NESTED, "")
                } else if req.starts_with("GET /named") {
                    (NAMED, "")
                } else if req.starts_with("GET /renamed") {
                    (RENAMED, "")
                } else if req.starts_with("GET /frame") {
                    (FRAME, "")
                } else if req.starts_with("GET /cspframe") {
                    (CSP_FRAME, CSP_HEADER)
                } else if req.starts_with("GET /csp") {
                    (CSP_PAGE, CSP_HEADER)
                } else if req.starts_with("GET /slowredir") {
                    // Before `/slow`: `starts_with("GET /slow")` would swallow
                    // this route and serve the delayed page instead.
                    (SLOW_REDIR, "")
                } else if req.starts_with("GET /slow") {
                    std::thread::sleep(std::time::Duration::from_millis(800));
                    (SLOW, "")
                } else if req.starts_with("GET /heldorder") {
                    (HELD_ORDER, "")
                } else if req.starts_with("GET /pageerror") {
                    (PAGE_ERROR, "")
                } else if req.starts_with("GET /loadlog") {
                    (LOADLOG, "")
                } else if req.starts_with("GET /selfnav") {
                    (SELF_NAV, "")
                } else if req.starts_with("GET /loadframe") {
                    (LOAD_FRAME, "")
                } else if req.starts_with("GET /log") {
                    (LOG, "")
                } else if req.starts_with("GET /latealert") {
                    (LATE_ALERT, "")
                } else if req.starts_with("GET /redirtarget") {
                    (REDIR_TARGET, "")
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

/// Poll `probe` until it yields, or fail naming what never arrived. Browser and
/// page state settles on the browser's clock, not the test's, so a bare read can
/// only ever be a race; every wait in both suites goes through this.
pub fn wait_for<T>(
    deadline: std::time::Duration,
    what: &str,
    mut probe: impl FnMut() -> Option<T>,
) -> T {
    let start = std::time::Instant::now();
    loop {
        if let Some(v) = probe() {
            return v;
        }
        assert!(start.elapsed() < deadline, "timed out waiting for {what}");
        // Each probe typically spawns a `webpilot` process, so the interval is
        // the browser suite's original 250 ms rather than a tighter one — a
        // faster poll would only add process churn to the same deadline.
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

pub fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

pub fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}
