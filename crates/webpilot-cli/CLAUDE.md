# crate: webpilot-cli

The single `webpilot` binary. `main.rs` branches by role at startup: **CLI**
(default) vs **NM Host** (only when `argv[1]` is `chrome-extension://<32-char
[a-p]>`, strict).

## Core structure

- `cli.rs` — clap entry. `Cmd::execution()` is the single source of command
  topology (`Local`/`Status`/`Quit`/`HeadlessOnly`/`TransportGeneric`/`Mcp`); the
  compiler forces a classification when a command is added. `policy` is `Local`
  (a local file command); `mcp` is `Mcp` (opens its own long-lived transport).
- `commands/` — one handler per command (see "Command handlers" below).
- `policy.rs` — the policy store and its gates: `enforce(&Command)`,
  `parse_and_enforce(&Value)` (parses the wire value as a typed `Command` first;
  parse failure = `InvalidArgument`) and `enforce_key(PolicyKey)` for an effect
  with no `Command`.
- `transport/` — the `Transport` trait (`send(Command) -> ResponseData`) is the
  only boundary between command logic and I/O.
  - `ipc.rs` — `IpcTransport` (browser): a plain socket writer.
  - `local/` — `LocalTransport` (headless), **split by domain**. `send` calls
    `policy::enforce` first:
    - `mod.rs` — struct (`browser: Arc<CdpClient>`, `page: CdpSession`), `open`,
      `Transport` impl, the **isolated-world bridge** (`install_bridge_world`
      auto-loads `bridge.js` into the `webpilot_bridge` world per document;
      `bridge_context_id` vs `active_context_id` route bridge calls vs page
      expressions), **navigation** (`navigate_reconnect` — a cross-site swap
      resets document state on the surviving session, no reconnect).
      `parse_bridge_response` lifts an `invoke_bridge` reply of
      `{success: false, error}` into a typed `WebPilotError`. The per-command re-establishment the root `CLAUDE.md`
      requires happens in `send`: `reconcile_binding` (which page, resolved from
      the pin; `bind_target` is the single "make this page the bound one" step,
      shared with `tab switch`), `ensure_download_behavior`,
      `ensure_device_emulation`, `ensure_monitor_hooks`.
    - `action.rs` — page-mutating (click/type/scroll/drag, `do_action`).
      `require_main_frame` blocks viewport-coordinate actions while an iframe is
      active. Type/scroll/focus/select run via the bridge; click uses native CDP
      pointer input with a bridge-resolved node and actionability checks;
      `key_press` (`do_key_press`), navigation, hover, drag, and upload have
      dedicated CDP paths. Input actions
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
      output is captured) and an `eval` deny removes it again. Each read returns
      `truncated` (the cap evicted entries) and `covers_load` (the recorder was in
      place before the document's first script). `MONITOR_SHAPE` is the whole
      recorder contract — entries AND the globals a read expects — and
      `monitor_shape_stamp_is_in_step` fails the build if the six stamp sites
      disagree.
    - `browser.rs` — tab / frame / status. The **active-tab pin**
      (`runtime/active_tab_<key>.json`) is the session's page identity: every
      target `pick_active_target` binds is written there, so separate CLI
      processes agree instead of each re-deriving from `Target.getTargets`
      (Chrome orders it by target GUID, so a re-derivation can land elsewhere).
      A pin whose tab closed STAYS on disk until the command that REPORTS it —
      `send`'s one loud `TabNotFound` — moves it onto the fallback, so a
      pin-independent command (`tab` list, `status`, the browser-global
      cookie/session ops) can never consume the signal silently: meanwhile `tab`
      marks no tab pinned and `status` reports `closed`, and an explicit
      `tab switch`/`tab new` answers the dead pin instead. Browser mode's pin
      never auto-moves, since the fallback would be a tab the user is browsing;
      its model is in `.claude/rules/extension.md`.
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
- `output.rs` — `CommandOutput` with `render()`, `to_json()` and `to_agent_text()`.
- `mcp.rs` — `webpilot mcp`: a stdio JSON-RPC (MCP) server, hand-rolled with no
  protocol dependency (same philosophy as native messaging / CDP / IPC).
  Dual-era per request: a request whose `_meta` names a revision is served
  statelessly (`MODERN_PROTOCOL_VERSIONS`, `server/discover`, `resultType`);
  otherwise `initialize` negotiates a `LEGACY_PROTOCOL_VERSIONS` revision.
  `INSTRUCTIONS` is the model guidance both entry points serve. The input reader
  stays active during a serialized command so ping and cancellation remain
  responsive; cancellation interrupts waits and lets mutations finish cleanup.
  Schemas validate inputs before opening a transport. Successes carry structured
  output alongside text. Action tools inject the wire `kind` to reuse `Action`'s
  deserialization and defaults; `browser_find` deserializes the CLI's own
  `FindArgs`. The surface is curated to page interaction
  (act/observe/orient/wait/diagnose + `eval`) — environment management (cookies,
  session, device, policy) stays CLI-only. `Execution::Mcp` puts it in the
  compiler-checked topology.
- `lockfile.rs` — the one `flock` implementation behind the launch lock and the
  policy and context stores.
- `test_support.rs` — an in-process mock CDP server, so unit tests drive the
  real `CdpClient`.
- `commands/setup/` — `setup` installs the skill, the extension and the NM host
  (one module each); `skill.rs` keeps a user's edits through its provenance digest.
- `assets.rs` — compile-time embedded skill + extension (`include_dir!`);
  `expected_extension_version()` (stale-install gate) and
  `expected_extension_id()` (the id derived from the manifest's pinned `key`, so
  `setup` registers the NM host without a copied id).

## Command handlers

Every handler is generic over `Transport`, so one body serves both modes:

```rust
pub async fn run<T: Transport>(transport: &mut T, args: FooArgs) -> Result<CommandOutput> {
    let result = transport.send(Command::Foo { ... }).await?;
    match result { /* destructure ResponseData */ }
}
```

Headless-only commands (`profile`/`record`/`device`/`context`) take
`&mut LocalTransport` to reach raw CDP through `local.page()` / `local.browser()`.
Pure-local commands (`policy`/`setup`/`diff`/`uninstall`/`self`) take no
transport and run in `run_cli` before any session opens.

A handler returns one `CommandOutput` variant: `Ok(String)`, `Data { json, human }`,
`Dom { snapshot, extra }`, `Content { stdout, json, note }` or
`List { items, human_lines, summary }`. `note` carries context such as an HTTP
status beside the content without altering stdout.

## Navigation (`mod.rs::navigate_reconnect`)

One `timeouts.navigation` budget runs from sending `Page.navigate` through its
reply, every settle probe and every poll. Completion is one predicate,
`navigation_settled(page, loader_id, before_url, until)`:
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
pending for `PROBE`: a commit in that window settles it, otherwise the previous
document is still live and the navigation succeeds on it. A load that outlasts the
budget is stopped (`abandon_navigation`) and reports `Timeout`: a load waiting on
its server holds every renderer-bound command, a later session's `Page.enable`
included.
Armed monitors need no settle step: their hooks are registered per document, so a
new one carries them from its first script.
