# crate: webpilot-cli

The single `webpilot` binary. `main.rs` branches by role at startup: **CLI**
(default) vs **NM Host** (only when `argv[1]` is `chrome-extension://<32-char
[a-p]>`, strict).

## Core structure

- `cli.rs` — clap entry. `Cmd::execution()` is the single source of command
  topology (`Local`/`Status`/`Quit`/`HeadlessOnly`/`TransportGeneric`/`Mcp`); the
  compiler forces a classification when a command is added. `policy` is `Local`
  (a local file command); `mcp` is `Mcp` (opens its own long-lived transport).
- `commands/` — one handler set. Each is written `run<T: Transport>` to serve
  both modes. Headless-only commands (`profile`/`record`/`device`/`context`) take
  `&mut LocalTransport` for raw CDP. Pure-local commands (`policy`/`setup`/`diff`/
  `uninstall`/`self`) take no transport.
- `policy.rs` — single-file policy store (`artifacts/policies.json`) +
  `enforce(&Command)` / `parse_and_enforce(&Value)`. Fail-closed: an unreadable
  or torn store denies. Writes are atomic (temp + rename). Enforced only at the
  **browser-reaching sink**: `LocalTransport::send` (headless) and the host
  (browser). `parse_and_enforce` validates the wire value as a typed `Command`
  (parse failure = `InvalidArgument`) before enforcing.
- `transport/` — the `Transport` trait (`send(Command) -> ResponseData`) is the
  only boundary between command logic and I/O.
  - `ipc.rs` — `IpcTransport` (browser). Not gated — a plain socket writer; the
    host gates.
  - `local/` — `LocalTransport` (headless), **split by domain**. `send` calls
    `policy::enforce` first:
    - `mod.rs` — struct, `open`, `Transport` impl, bridge injection, **navigation**
      (`navigate_reconnect`), monitor re-install after navigation.
    - `action.rs` — page-mutating (click/type/scroll/drag, `do_action`).
      `require_main_frame` blocks viewport-coordinate actions while an iframe is
      active.
    - `capture.rs` — DOM / screenshot / PDF / accessibility tree;
      `count_http_subframes` → `DomSnapshot.subframes`.
    - `query.rs` — eval (`do_eval`) / wait / dom get·set / fetch.
    - `state.rs` — cookies / console·network monitors / session. Monitors set an
      armed flag so `reinstall_monitors` re-injects them after a navigation wipes
      their `window` hooks.
    - `browser.rs` — tab / frame / status.
  - `local_context.rs` — per-user CDP browser-context store (multi-agent,
    `MAX_CONTEXTS`).
- `cdp.rs` — `CdpClient` (tokio-tungstenite WebSocket). id→oneshot routing;
  heartbeat tolerates up to `HEARTBEAT_MAX_MISSES` consecutive misses before
  declaring the connection dead; maps `ConnectionLost`/`Timeout`.
- `session.rs` — Chrome lifecycle + `flock` launch lock; `headless_viewport()`
  (settings).
- `host.rs` — NM host process (IPC ↔ stdin/stdout). The browser-mode policy sink:
  validates and gates via `policy::parse_and_enforce`, then forwards the
  **re-serialized parsed command** (stripping unmodeled fields). Version gate:
  compares the extension's Ping version to the bundled version, rejecting with
  `VersionMismatch`. The NM writer skips an oversized message instead of wedging.
- `output.rs` — `CommandOutput` → human/json `render()`. `to_agent_text()` reuses
  the same renderers to build MCP tool results.
- `mcp.rs` — `webpilot mcp`: a stdio JSON-RPC (MCP) server, hand-rolled with no
  protocol dependency (same philosophy as native messaging / CDP / IPC). Each
  tool builds a typed `Command`/`Action` and runs it through the existing handler
  over the shared `Transport`, inheriting mode, policy, and rendering. Action
  tools inject the wire `kind` to reuse `Action`'s deserialization and defaults.
  `Execution::Mcp` puts it in the compiler-checked topology.
- `assets.rs` — compile-time embedded skill + extension (`include_dir!`);
  `expected_extension_version()`.

Timeouts are read directly via `webpilot::settings::timeouts().<field>` (one
pattern across both crates, no separate facade).

## Navigation (`mod.rs::navigate_reconnect`)

Completion is one predicate, `navigation_settled(page, loader_id, before_url)`:
- committed = (loader_id matches) **or** (frame URL ≠ before_url)
- ready = `readyState` is `interactive`/`complete`

A URL change can swap the renderer cross-site → rebind a fresh session. Same URL
= same-site reload → reuse the session (the loaderId distinguishes the new
document from the old). No loaderId and no error = a same-document (fragment)
navigation → complete immediately (frame preserved). `net::ERR_ABORTED` is not an
immediate failure but pending — if it later settles, Ok; if not by the deadline,
`NavigationFailed`. After a navigation that built a new document, armed
console/network monitors are re-installed.

Conventions: `.claude/rules/rust-conventions.md`. For the full command-addition
checklist and gating rules, see the root `CLAUDE.md`.
