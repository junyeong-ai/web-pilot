# WebPilot

AI agent용 Chrome 브라우저 제어 CLI. DOM 캡처, 스크린샷, 액션 실행(click, type, scroll, navigate).

## Build & Run

```bash
cargo build --workspace
webpilot capture --dom --url "https://example.com"   # Headless (default)
webpilot --browser capture --dom                     # Browser mode (SSO)
webpilot --context agent-1 capture --dom             # Multi-agent isolation
webpilot status                                      # Connection check
webpilot quit                                        # Stop Chrome
```

## Architecture

```
Headless (default):  CLI → CDP WebSocket → Chrome for Testing → bridge.js (injected)
Browser (--browser): CLI → Unix Socket → NM Host → Extension → bridge.js (content script)
```

Single binary, auto-detected modes: CLI (default), Browser (`--browser`), Host (launched by Chrome).
`--context NAME` creates isolated CDP BrowserContexts for multi-agent use.

## DOM Output Format

```
*[1] input#query "Search" type=text autocomplete=search @search
[2] button "Go" @search
--- Page: Example (https://example.com) ---
--- Scroll: 25% (0.5 above, 1.2 below) ---
```

`[index]` is used for `action click N`. `*` = new since last capture. `@landmark` = semantic context.

## Output Modes

- **Terminal** (stdout is TTY): human-readable to stderr
- **Piped** (stdout is not TTY): JSON automatically
- **Force JSON**: `--json` flag

All command handlers return `CommandOutput` enum → rendered by `output::render()`.
Handlers never see `OutputMode` — dispatch layer handles formatting.

## Coding Conventions

### Naming
- Subcommand enums: singular `XCommand` (e.g., `TabCommand`, `FrameCommand`, `CookieCommand`)
- Args structs: `XArgs` with subcommand field named `command`
- Protocol commands: NounVerb pattern (e.g., `TabList`, `CookieSet`, `FrameSwitch`)
- Bridge calls: `invoke_bridge()` + `parse_bridge_response()` for standardized error handling

### Error Handling
- `WebPilotError { code: ErrorCode, message }` for structured exit codes
- `ErrorCode` has `category()`, `is_retryable()`, `exit_code()` methods
- Exit codes: 0=success, 1=general, 3=connection, 4=not-found, 5=timeout, 6=security, 7=invalid-arg, 8=navigation
- `format_error(&ProtocolError)` provides AI-friendly guidance per error code

### Command Handler Pattern
```rust
// All handlers follow this pattern — no OutputMode parameter
pub async fn run(cdp: &CdpClient, args: FooArgs) -> Result<CommandOutput> {
    // ... do work ...
    Ok(CommandOutput::Ok("OK".into()))
}
```
Variants: `Ok(String)`, `Data { json, human }`, `Dom { snapshot, extra }`, `Content { stdout, json }`, `List { items, human_lines, summary }`, `Silent`

### Context Isolation
- `HeadlessContext` carries `browser_context_id` and `target_id` for multi-agent isolation
- `navigate_reconnect()` filters targets by `browserContextId` via `find_page_target()`
- `quit_context()` disposes CDP BrowserContext; `quit_session()` kills Chrome process
- `ensure_session()` uses `libc::flock` to prevent concurrent Chrome launch race

### Timeouts
All configurable via env vars (e.g., `WEBPILOT_CDP_SEND_TIMEOUT_MS`).
Defaults in `timeouts.rs`: cdp_send(30s), navigation(15s), reload_wait(10s), back_forward(5s),
poll_interval(300ms), post_navigate(200ms), post_reconnect(500ms), ipc_response(60s),
chrome_launch(15s), heartbeat(10s).

### bridge.js
Shared between headless (`include_str!` → `Runtime.evaluate`) and browser mode (content script).
- Error responses: `{ success: false, error: { message, code } }` with PascalCase codes
- Text limit: 300 chars per element
- `keyToCode()` maps special keys (Enter, Tab, Space, Arrow, etc.)

## Troubleshooting

- **"Chrome not found"**: Set `WEBPILOT_CHROME=/path/to/chrome`
- **"CDP timeout"**: `webpilot quit` and retry
- **Session stuck**: PID file at `$XDG_RUNTIME_DIR/webpilot-<user>-headless.pid` (or `/tmp/`)
- **"Not connected" (--browser)**: `webpilot install --extension-id <ID>`, reload extension
