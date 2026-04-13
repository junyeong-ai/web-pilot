---
paths:
  - "crates/**/*.rs"
---

# Rust Conventions

## Naming
- Subcommand enums: singular `XCommand` (e.g., `TabCommand`, `FrameCommand`)
- Args structs: `XArgs` with subcommand field named `command`
- Protocol commands: NounVerb pattern (e.g., `TabList`, `CookieSet`, `FrameSwitch`)
- Bridge calls: `invoke_bridge()` + `parse_bridge_response()` for standardized error handling

## Error Handling
- `WebPilotError { code: ErrorCode, message }` for structured exit codes
- `ErrorCode` has `category()`, `is_retryable()`, `exit_code()` methods

## Command Handler Pattern
```rust
// All handlers follow this pattern — no OutputMode parameter
pub async fn run(cdp: &CdpClient, args: FooArgs) -> Result<CommandOutput> {
    Ok(CommandOutput::Ok("OK".into()))
}
```
Variants: `Ok(String)`, `Data { json, human }`, `Dom { snapshot, extra }`, `Content { stdout, json }`, `List { items, human_lines, summary }`, `Silent`

## Context Isolation
- `HeadlessContext` carries `browser_context_id` and `target_id` for multi-agent isolation
- `navigate_reconnect()` filters targets by `browserContextId` via `find_page_target()`
- `quit_context()` disposes CDP BrowserContext; `quit_session()` kills Chrome process
- `ensure_session()` uses `libc::flock` to prevent concurrent Chrome launch race
