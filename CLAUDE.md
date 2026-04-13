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

## Error Handling

- Exit codes: 0=success, 1=general, 3=connection, 4=not-found, 5=timeout, 6=security, 7=invalid-arg, 8=navigation
- `format_error(&ProtocolError)` provides AI-friendly guidance per error code
- All timeouts configurable via `WEBPILOT_*_TIMEOUT_MS` env vars (see `timeouts.rs`, `ipc.rs`)

## Troubleshooting

- **"Chrome not found"**: Set `WEBPILOT_CHROME=/path/to/chrome`
- **"CDP timeout"**: `webpilot quit` and retry
- **Session stuck**: PID file at `$XDG_RUNTIME_DIR/webpilot-<user>-headless.pid` (or `/tmp/`)
- **"Not connected" (--browser)**: `webpilot install --extension-id <ID>`, reload extension
