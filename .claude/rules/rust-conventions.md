---
paths:
  - "crates/**/*.rs"
---

# Rust Conventions

## Single source of truth
- `Action` (in `webpilot::action`) derives both `clap::Subcommand` and `serde::{Serialize,Deserialize}`. The same enum is the CLI surface and the wire protocol — no parallel `BrowserAction` / `ActionCommand` / `to_browser_action` mapping.
- `WebPilotError` (in `webpilot::error`) is variant-rich and carries the data needed to render guidance via `Display`. There is **no message-string parsing anywhere**. Wire error: `{code, message, ...data}` round-trips through `WireError`.
- `WaitCondition`, `CaptureField`, `CaptureOpts`, `FrameSelector`, `DomProperty` are typed enums, not boolean flag bags or stringly-typed fields.

## Naming
- Subcommand enums: singular `XCommand` (e.g., `TabCommand`, `FrameCommand`).
- Args structs: `XArgs` with subcommand field named `command` (or `action` / `condition` when more specific).
- Protocol commands: `NounVerb` (e.g., `TabList`, `CookieSet`, `FrameSwitch`).
- Initialism casing: `CspViolation` (Rust idiom), not `CSPViolation`.
- String parsing: `impl FromStr` — never bespoke `parse(s)` / `from_str_lossy(s)` helpers.

## Error handling
- All errors that escape command handlers are `WebPilotError`. External crate errors wrap into `WebPilotError::Other` at the boundary in `main.rs::into_webpilot_error`.
- Exit codes from `WebPilotError::exit_code()` — never inferred from message substrings.
- Structured fields per variant: `ElementNotFound { requested, available }`, `Timeout { kind, elapsed_ms }`, etc.

## Command handler pattern
Every command handler is **generic over `Transport`** — written once, run in both browser and headless modes.
```rust
pub async fn run<T: Transport>(transport: &mut T, args: FooArgs) -> Result<CommandOutput> {
    let result = transport.send(Command::Foo { ... }).await?;
    match result { /* destructure ResponseData */ }
}
```
Headless-only commands (`profile`, `record`, `device`, `context`) take `&mut LocalTransport` directly so they can reach the underlying CDP via `local.page()` / `local.browser()`.

Output variants: `Ok(String)`, `Data { json, human }`, `Dom { snapshot, extra }`, `Content { stdout, json }`, `List { items, human_lines, summary }`, `Silent`.

## Transport
- `Transport` trait — sole boundary between command logic and I/O. `send(Command) -> ResponseData`.
- `IpcTransport` — Unix socket → NM Host → Extension → bridge.js (browser mode).
- `LocalTransport` — direct CDP WebSocket → bridge.js (headless mode). Holds `browser`/`page` `CdpClient`s, `ws_url`, optional `browser_context_id`, `target_id`. Owns navigation reconnect logic.

## Context isolation
- `LocalTransport::open(Some("agent-1"))` resolves a named CDP browser context, creating one if absent. Per-user state under `dirs::contexts_dir()`.
- `quit_named_context()` disposes a single context; `quit_session()` terminates Chrome (all contexts).
- `ensure_session()` uses `libc::flock` to serialize concurrent Chrome launches.

## Paths
- All persistent state under `webpilot::dirs::root()` (per-user, mode 0700). Subdirs: `runtime/`, `contexts/`, `artifacts/`, `chrome-profile/`. **Never** use `/tmp/...` or `webpilot::OUTPUT_DIR` constants.

## Bridge calls
- `invoke_bridge(cdp, &serde_json::Value)` — pass a `Value`, not a string.
- `parse_bridge_response(raw)` extracts typed `WebPilotError` from `{success: false, error: ...}` responses.

## Forbidden
- `unwrap_or_default()` on `serde_json::to_string*` — use `expect("static")` or surface the error.
- `args.iter().any(|a| a.contains("..."))` heuristics for mode detection — use a strict, documented check.
- Re-export shims, deprecated aliases, "removed for X" comments. Delete cleanly.
