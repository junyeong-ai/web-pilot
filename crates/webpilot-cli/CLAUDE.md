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
- `policy.rs` — single-file policy store (`policy/policies.json`, durable data
  root — not the evictable cache) +
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
    - `mod.rs` — struct (`browser: Arc<CdpClient>`, `page: CdpSession`), `open`,
      `Transport` impl, the **isolated-world bridge** (`install_bridge_world`
      auto-loads `bridge.js` into the `webpilot_bridge` world per document;
      `bridge_context_id` vs `active_context_id` route bridge calls vs page
      expressions), **navigation** (`navigate_reconnect` — a cross-site swap
      resets document state on the surviving session, no reconnect).
      `send` re-establishes per command what a transport must not have captured
      once — `reconcile_binding` (which page, resolved from the pin; `bind_target`
      is the single "make this page the bound one" step, shared with `tab switch`),
      `ensure_download_behavior`, `ensure_device_emulation`, `ensure_monitor_hooks`
      — because Chrome outlives every process and a session served by ONE transport
      (`webpilot mcp`, the NM host) would otherwise drift from it.
    - `action.rs` — page-mutating (click/type/scroll/drag, `do_action`).
      `require_main_frame` blocks viewport-coordinate actions while an iframe is
      active. `key_press` is a native CDP `Input.dispatchKeyEvent` (`do_key_press`)
      — real Tab/Backspace/arrow/text/Enter-submit behaviour a synthetic event
      can't produce — while every other page action runs via the bridge; both
      bracket a navigation + popup watch (events subscribed *before* the action
      runs): `settled_action_url` reports `url_changed` (bounded commit wait
      only when a main-frame load started and has not already stopped),
      `adopt_click_opened_target` moves the pin to a click-opened tab — the
      browser-mode `dispatchActionToPage` contract, mirrored. `--capture` runs
      after adoption and document readiness; a capture failure becomes
      `capture_error`, never a command failure (a retry would re-run the side
      effect).
    - `capture.rs` — DOM / screenshot / PDF / accessibility tree;
      `count_http_subframes` → `DomSnapshot.subframes`.
    - `query.rs` — eval (`do_eval`) / wait / dom get·set / fetch.
    - `state.rs` — cookies / console·network monitors / session. Monitors set an
      armed flag — persisted per context, so later CLI processes keep arming — and
      `ensure_monitor_hooks` reconciles each armed monitor's
      `addScriptToEvaluateOnNewDocument` registration before every command, so the
      hook enters a document ahead of the document's own scripts (a page's startup
      output is captured) and an `eval` deny removes it again. The recorders
      themselves are `extension/content/monitor-*.js`, embedded here and injected
      by the extension — one text for both modes.
    - `browser.rs` — tab / frame / status. The **active-tab pin**
      (`runtime/active_tab_<key>.json`) is the session's page identity: every
      target `pick_active_target` binds is written there, so separate CLI
      processes agree instead of each re-deriving from `Target.getTargets`
      (Chrome orders it by target GUID, so a re-derivation can land elsewhere).
      A pin whose tab closed STAYS on disk until the command that REPORTS it —
      `send`'s one loud `TabNotFound` — moves it onto the fallback, so a
      pin-independent command (`tab` list, the browser-global cookie/session
      ops) can never consume the signal silently; `tab list` marks no tab active
      meanwhile, and an explicit `tab switch`/`tab new` answers the dead pin
      instead. Browser mode's pin is sticky in the same way but never
      auto-moves: it drives the user's own Chrome, where the fallback would be a
      tab the user is browsing.
  - `local_context.rs` — per-user CDP browser-context store (multi-agent,
    `MAX_CONTEXTS`).
- `cdp.rs` — `CdpClient` (one tokio-tungstenite WebSocket to the browser
  endpoint) + `CdpSession` (a flat-protocol session on it, from
  `CdpClient::attach` → `Target.attachToTarget { flatten: true }`). id→oneshot
  routing; heartbeat tolerates up to `HEARTBEAT_MAX_MISSES` consecutive misses
  before declaring the connection dead; maps `ConnectionLost`/`Timeout`. Every
  session send is stamped with its `sessionId`; `SessionEvents` filters the
  shared event ring to one session (isolating each page's events); a detach
  watcher flips `session_alive` so an in-flight wait ends at once when the tab
  closes (backstopped by Chrome's `-32001`). The page is a `CdpSession` that
  survives a cross-site renderer swap — no socket rebind.
- `session.rs` — Chrome lifecycle + `flock` launch lock; `headless_viewport()`
  (settings).
- `host.rs` — NM host process (IPC ↔ stdin/stdout). The browser-mode policy sink:
  validates and gates via `policy::parse_and_enforce`, then forwards the
  **re-serialized parsed command** (stripping unmodeled fields). Version gate:
  compares the extension's Ping version to the bundled version, rejecting with
  `VersionMismatch`. The NM writer skips an oversized message instead of wedging.
  The host's lifetime is Chrome's: stdin EOF (Chrome gone) exits the process —
  a graceful teardown is unreachable (the blocking writer ends only when every
  channel sender drops, and detached per-connection tasks hold them).
- `output.rs` — `CommandOutput` → human/json `render()`. `to_agent_text()` reuses
  the same renderers to build MCP tool results.
- `mcp.rs` — `webpilot mcp`: a stdio JSON-RPC (MCP) server, hand-rolled with no
  protocol dependency (same philosophy as native messaging / CDP / IPC).
  `initialize` negotiates the protocol revision (`SUPPORTED_PROTOCOL_VERSIONS`,
  newest wins when the client's is unknown). Each tool builds a typed
  `Command`/`Action` and runs it through the existing handler over the shared
  `Transport`, inheriting mode, policy, and rendering. Action tools inject the
  wire `kind` to reuse `Action`'s deserialization and defaults; `browser_find`
  deserializes the CLI's own `FindArgs`. The surface is curated to page
  interaction (act/observe/orient/wait + `eval`) — environment management
  (cookies, session, device, policy, monitors) stays CLI-only.
  `Execution::Mcp` puts it in the compiler-checked topology.
- `assets.rs` — compile-time embedded skill + extension (`include_dir!`);
  `expected_extension_version()` (stale-install gate) and
  `expected_extension_id()` (the id derived from the manifest's pinned `key`, so
  `setup` registers the NM host without a copied id).

Timeouts are read directly via `webpilot::settings::timeouts().<field>` (one
pattern across both crates, no separate facade).

## Navigation (`mod.rs::navigate_reconnect`)

Completion is one predicate, `navigation_settled(page, loader_id, before_url)`:
- committed = (loader_id matches) **or** (frame URL ≠ before_url)
- ready = `readyState` is `interactive`/`complete`

A URL change can swap the renderer cross-site, but the flat-protocol `CdpSession`
is attached to the target and survives the swap — so instead of reconnecting a
socket, `navigate_reconnect` just resets document-scoped state (clears the active
frame, force-re-emits the new document's execution contexts so a dropped
`executionContextCreated` can't strand the bridge). Same URL =
same-site reload → the loaderId distinguishes the new document from the old. No
loaderId and no error = a same-document (fragment) navigation → complete
immediately (frame preserved). `net::ERR_ABORTED` is not an immediate failure but
pending — if it later settles, Ok; if not by the deadline, `NavigationFailed`.
Armed monitors need no settle step: their hooks are registered per document, so a
new one carries them from its first script.

Conventions: `.claude/rules/rust-conventions.md`. For the full command-addition
checklist and gating rules, see the root `CLAUDE.md`.
