//! Shared end-to-end fixtures: the test page, a minimal HTTP server, and
//! process-output helpers. Used by both the headless and the browser-mode
//! behavioral suites so the two modes are asserted against the same page.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Output;

pub const PAGE: &str = r#"<!doctype html><html><head><title>fixture</title></head>
<body>
<button id="go" onclick="document.title='clicked'">Go</button>
<input id="q" type="text" placeholder="Search">
<input id="file" type="file">
<a id="nav" href="/second">go second</a>
<a id="pop" href="/second" target="_blank" rel="noopener">open popup</a>
<div id="shadowhost"></div>
<iframe src="/frame"></iframe>
<script>
  const sr = document.getElementById('shadowhost').attachShadow({ mode: 'open' });
  sr.innerHTML = '<input id="shadowfile" type="file">';
</script>
</body></html>"#;

pub const FRAME: &str = r##"<!doctype html><html><head><title>frame</title></head>
<body><a id="link" href="#">inner link</a></body></html>"##;

/// A page whose `Content-Security-Policy` forbids dynamic evaluation
/// (`unsafe-eval`): the parity bar for `eval` inside a switched frame. Both
/// documents carry the header, because eval runs against the FRAME's policy.
pub const CSP_PAGE: &str = r#"<!doctype html><html><head><title>csp-outer</title></head>
<body><p>csp outer</p><iframe src="/cspframe" name="cspframe"></iframe></body></html>"#;

pub const CSP_FRAME: &str = r#"<!doctype html><html><head><title>cspframe</title></head>
<body><p id="m">csp inner</p></body></html>"#;

const CSP_HEADER: &str = "Content-Security-Policy: script-src 'self'\r\n";

/// Minimal blocking HTTP server: serves the fixture page for `/`, the inner
/// document for `/frame`, and a CSP-strict pair for `/csp` + `/cspframe`.
/// Runs on a daemon thread for the test's lifetime.
pub fn spawn_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 1024];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let (body, extra_headers) = if req.starts_with("GET /frame") {
                (FRAME, "")
            } else if req.starts_with("GET /cspframe") {
                (CSP_FRAME, CSP_HEADER)
            } else if req.starts_with("GET /csp") {
                (CSP_PAGE, CSP_HEADER)
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
