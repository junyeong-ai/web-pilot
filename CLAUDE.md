# WebPilot - Browser Control Tool for AI Agents

WebPilot lets AI agents control Chrome through a CLI.
It captures DOM state, takes screenshots, and executes actions (click, type, scroll, navigate).

## Build & Run

```bash
cargo build --workspace                              # Build
webpilot capture --dom --url "https://example.com"   # Headless (default, no setup)
webpilot --browser capture --dom                     # Browser mode (SSO sessions)
webpilot status                                      # Check connection
webpilot quit                                        # Stop headless Chrome
```

## Commands

Run `webpilot --help` or `webpilot <command> --help` for full reference.

Core: `capture`, `action`, `eval`, `wait`, `find`, `tabs`, `frames`, `dom`, `fetch`,
`cookies`, `session`, `network`, `console`, `policy`, `device`, `profile`, `record`,
`context`, `diff`, `status`, `install`, `quit`.

### DOM Output Format
```
*[1] input#query "Search" type=text autocomplete=search form=searchform @search
[2] button "Go" @search
--- Page: Example (https://example.com) ---
--- Scroll: 25% (0.5 above, 1.2 below) ---
--- 3 elements (from 120 nodes, 5ms) ---
```
Format: `[index] tag#id "text" attributes @landmark` — use index for `action click N`.

### Output Modes
- **Terminal** (stdout is TTY): human-readable text
- **Piped** (stdout is not TTY): JSON automatically
- **Force JSON**: add `--json` flag

## Architecture

```
Headless (default):  CLI → CDP WebSocket → Chrome for Testing → bridge.js (injected)
Browser (--browser): CLI → Unix Socket → NM Host → Extension → bridge.js (content script)
```

Single binary with auto-detected modes: CLI (default), Browser (`--browser`), Host (launched by Chrome).

## Project Structure

```
crates/
  webpilot/                    # Shared library
    src/
      protocol.rs              # Command/Response types (Request, Command, ResponseData)
      types.rs                 # Domain types (InteractiveElement, DomSnapshot, ErrorCode, ProtocolError)
      ipc.rs                   # Unix Socket IPC
      native_messaging.rs      # Chrome NM protocol
      screenshot.rs            # Image processing + resize

  webpilot-cli/                # CLI binary
    src/
      main.rs                  # Entry point + exit code classification
      cli.rs                   # CLI arg parsing + mode routing (headless vs --browser)
      cdp.rs                   # CDP WebSocket client (heartbeat, health monitoring, Drop)
      session.rs               # Chrome lifecycle (async, atomic writes)
      host.rs                  # NM Host bridge (pending cleanup, orphan reaper)
      output.rs                # OutputMode (Human/Json) + format_error(ProtocolError)
      timeouts.rs              # Centralized timeout constants (env var overrides)
      stitch.rs                # Full-page screenshot tile stitching
      commands/                # Browser-mode command handlers (IPC-based)
        mod.rs                 # Command enum + module registry
        ipc_helper.rs          # send_command() shared helper
        action.rs .. wait.rs   # 22 command modules
      headless/                # Headless-mode command handlers (CDP-based)
        mod.rs                 # HeadlessContext + run() dispatch + shared infra
        capture.rs .. wait.rs  # 19 command modules

extension/                     # Chrome Extension (browser mode only)
  manifest.json                # Manifest V3
  background/service-worker.js # Message routing, CDP operations
  content/bridge.js            # DOM extraction + actions (shared with headless via include_str!)
```

## Coding Conventions

### Naming
- Subcommand enums: `XCommand` (e.g., `ConsoleCommand`, `TabsCommand`, `FramesCommand`)
- Args structs: `XArgs` with subcommand field named `command` (not `action`)
- Headless handlers: `pub(crate) async fn run(...)` in each module
- Protocol commands: `Request::new(id, command)` — never struct literals

### Error Handling
- `ProtocolError { message: String, code: ErrorCode }` — used in all ResponseData variants
- `format_error(&ProtocolError)` for Human mode; `format_error_str(&str)` for raw strings
- Exit codes: 0=success, 1=general, 3=connection, 4=element-not-found, 5=timeout, 6=policy-denied

### Types
- `ErrorCode` enum: ElementNotFound, Timeout, PolicyDenied, NoPage, NavigationFailed, ...
- `ActionType`, `PolicyVerdict`, `ConsoleLevel`, `SameSite` — all typed enums (no raw strings)
- `InteractiveElement.expanded/selected`: `Option<bool>` with custom deserializer for string/"true" compat

### HeadlessContext
```rust
pub(crate) struct HeadlessContext {
    pub browser: CdpClient,  // browser-level CDP (targets, contexts)
    pub ws_url: String,       // for reconnection
    pub page: CdpClient,      // page-level CDP (DOM, JS eval)
}
```
- `capture`/`action`: take `&mut HeadlessContext` (may reconnect page on cross-origin navigation)
- `tabs`: uses `ctx.browser` (not ctx.page) for target management
- Most others: use `&ctx.page`
- `policy`: no CDP (file-based)

### Timeouts
All configurable via env vars (e.g., `WEBPILOT_CDP_SEND_TIMEOUT_MS`).
Defined in `timeouts.rs`: cdp_send(30s), navigation(15s), reload_wait(10s), back_forward(5s),
poll_interval(300ms), post_navigate(200ms), post_reconnect(500ms), ipc_response(60s),
chrome_launch(15s), heartbeat(10s).

### CDP Client (cdp.rs)
- `AtomicU64` for lock-free request IDs
- `alive: AtomicBool` — checked on every `send()`, set to false when reader dies
- Background heartbeat (10s interval, Browser.getVersion)
- `impl Drop` — aborts reader + heartbeat tasks

### bridge.js
Shared between headless (injected via `Runtime.evaluate`) and browser mode (content script).
- Element fields: index, tag, id, role, text, name, value, placeholder, href, input_type,
  disabled, focused, checked, expanded(bool), selected(bool), required, readonly, label,
  options, landmark, in_viewport, bounds, is_new, occluded, frame, description, form_id, autocomplete
- Text limit: 300 chars
- Error responses: `{ success: false, error: { message, code } }` — PascalCase codes (ElementNotFound, Timeout)

## Troubleshooting

**"Chrome not found"**: Set `WEBPILOT_CHROME=/path/to/chrome` or install Chrome for Testing.
**"CDP timeout"**: Run `webpilot quit` and retry.
**Session stuck**: PID file at `$XDG_RUNTIME_DIR/webpilot-<user>-headless.pid` (or `/tmp/`).
**"Not connected" (--browser)**: Run `webpilot install --extension-id <ID>`, reload extension.
