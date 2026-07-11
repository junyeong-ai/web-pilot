# CDP flat-session migration

Status: **Phase 1 shipped; Phase 2–3 planned.** The headless e2e suite is the
gate for every phase (`WEBPILOT_E2E=1 cargo test -p webpilot-cli --test
e2e_headless`).

## Why

Headless mode used to open **one WebSocket per page target** beside the
browser-endpoint
connection. CDP's Target domain is moving flat session mode (a single
connection, commands stamped with `sessionId`) to the default, with
non-flattened access slated for deprecation. Beyond deprecation tracking, the
per-target-socket design has three concrete costs:

1. **Cross-origin iframes (OOPIF) are unreachable.** An OOPIF lives in another
   renderer and never announces an execution context on the page target's
   connection, so `frame_contexts` never maps it and `frame switch` fails
   `FrameNotFound`. Reaching it requires `Target.setAutoAttach` with
   `flatten: true` — child sessions on one connection.
2. **Per-process override loss.** `Emulation.setUserAgentOverride` reverts when
   the client that set it disconnects; the device-persistence layer
   (`DeviceState`, re-applied on every `open`) exists to paper over exactly
   this. Fewer connections narrow the surface.
3. **A cross-site navigation forces a socket rebind.** The renderer swap kills
   the page-socket's usefulness, so `navigate_reconnect` re-connects and
   re-primes (`rebind_page_world`). A flat session attaches to the *target*,
   which survives renderer swaps — the rebind branch disappears.

## Architecture (Phase 1, shipped — `cdp.rs`)

- `CdpClient` — the one WebSocket per process: id→oneshot routing, heartbeat
  with activity-counter HOL detection, broadcast event ring with explicit
  `Lagged` handling. Holds the browser domain.
- `CdpSession` — `{ Arc<CdpClient>, session_id, session_alive }`, created by
  `CdpClient::attach` (`Target.attachToTarget { flatten: true }`). Mirrors the
  old page-client API (`send`, `send_with_timeout`, `evaluate`,
  `subscribe_events`, `wait_for_event_matching`, `wait_on_receiver`,
  `screenshot`, `spawn_dialog_responder`) with three differences: every send is
  stamped with `sessionId`; every event receiver is a `SessionEvents` **filtered
  to that `sessionId`** — reproducing the event isolation the dedicated socket
  gave for free (without it, one page's settle drain would consume another
  page's `Page.frameStartedLoading`, the false-positive class the
  event-ring-lag fixes in 0.6.22/0.6.23 exist to prevent); and a detach watcher
  flips `session_alive` on the session's `Target.detachedFromTarget`, which the
  in-flight response wait polls so a tab closing mid-`wait` ends the wait at
  once (typed `ConnectionLost`, reclassified to `TabNotFound`) instead of
  running to the full deadline. Chrome's `-32001` "session not found" is the
  backstop for a detach event lagged out of the ring.
- `LocalTransport.browser: Arc<CdpClient>`, `LocalTransport.page: CdpSession`.
  A cross-site navigation no longer rebinds a socket: the session is attached
  to the *target* and survives the renderer swap, so `navigate_reconnect` only
  resets document-scoped state. It DOES force a `reemit_execution_contexts`
  (Runtime disable/enable) after the swap commits — the surviving session's
  new-document `executionContextCreated` events fire once, and if that burst
  overflowed the ring the async listener would drop them and every later bridge
  command fail `FrameNotFound` on a live page; the re-emit re-fires them
  deterministically. This is the one piece of the deleted `rebind_page_world`
  recovery that a surviving session still needs. `connect_to_page`,
  `rebind_page_world`'s socket reconnect, and `wait_navigation_settled` are gone.
- `CdpSession::Drop` aborts every task the session spawned — its detach watcher,
  dialog responder, and frame-context listener (the transport hands the listener
  in via `CdpSession::track`). The responder holds an `Arc<CdpClient>` and cannot
  be relied on to exit by itself once Chrome is dead — its own Arc keeps the
  `events` sender alive so `recv` never returns `Closed` — so without the abort
  a long-lived `webpilot mcp` server would leak a `CdpClient` (and its reader +
  heartbeat tasks) on every Chrome-death→reopen cycle. The watcher and listener
  hold no such Arc (only broadcast receivers), but tracking all three on one
  teardown is uniform and avoids a lingering listener when a wedged-then-
  recovered connection drops the detach event the listener would otherwise wait
  for.
- Reconnect-across-CLI-invocations is unchanged in shape: each process connects
  to the stored browser endpoint and `attach`es a session to the persisted pin.
- `cdp.event_buffer` default raised 256 → 512: one ring now carries the browser
  domain plus every session's events.

## Invariants preserved (verified behaviours, not preferences)

- Event-lag semantics per consumer: `Lagged` → inconclusive-Timeout carrying
  the loss (`wait_for_event_matching`), authoritative re-reads on lagged
  drains (`settled_action_url`, `adopt_click_opened_target`, `nav_history_index`).
- One heartbeat per connection (not per session); connection liveness marks
  the whole connection dead and drains all pending.
- `Page.enable` + dialog responder + `Runtime.discardConsoleEntries`-before-
  `Runtime.enable` are per session (`attach_to_page`).
- The pin/fallback contract (`pin_vanished`, `command_needs_active_page`) and
  browser-context isolation (`target_in_context`) are session-independent.
- `navigation_settled`'s predicate (committed via loaderId-or-URL, parsed via
  readyState) is unchanged.

## Phase 2 (OOPIF adoption) — REJECTED after empirical testing

The intended Phase 2 payoff was cross-origin-iframe reachability via
`Target.setAutoAttach { flatten: true }` child sessions feeding `frame_contexts`.
**Measurement killed it: headless Chrome does not expose a cross-site iframe as
an attachable target, so there is no child session to route through.**

Probe (Chrome for Testing 150.0.7871.115, webpilot's launch flags; scripts were
run ad hoc, not committed): a parent on `http://127.0.0.1:PORT` embeds an
`<iframe src="http://localhost:PORT/child">` (a distinct site). Across all of —
per-page-session `setAutoAttach`, browser-level `setAutoAttach` (the
Puppeteer/Playwright approach, set before navigation), and each of those with
`--site-per-process` added — **no `Target.attachedToTarget` ever fired for the
iframe, and `Target.getTargets` never listed an `iframe`/`page` target for it.**
The child appears in the parent session's `Page.getFrameTree` as a placeholder
with an EMPTY url (the cross-process-proxy signature), which is exactly why
`count_http_subframes` counts it as 0 and `frame switch` returns `FrameNotFound`
— the documented boundary in `browser.rs` is correct and unavoidable in this
configuration, not a routing bug Phase 2 could fix.

Consequences:
- Phase 2's sole mechanism (child sessions) has nothing to attach to in
  headless, so it would deliver zero reachability and **cannot be verified with
  a local fixture** — the precondition (a child session) never materialises.
  Building it would violate "verify before shipping".
- OOPIFs that DO exist in a real logged-in Chrome are a **browser-mode** concern,
  and browser mode reaches the page over `IpcTransport` → the extension, whose
  frame handling is entirely separate from this headless CDP code. A headless
  CDP change cannot help browser-mode OOPIFs.

If a future Chrome starts exposing headless cross-site iframes as attachable
targets (re-run the probe to check), revisit with the child-session design
sketched in git history for this file. Until then, the `frame switch` →
`FrameNotFound` boundary stands as the honest, correct behaviour.

## Related decision: network/console monitors stay in-page (JS hooks)

`Network.enable` events flow only to the connection that enabled them. The
armed-monitor contract is **cross-process** — `network start` in one CLI
process, navigation in a second, `network read` in a third — and no persistent
headless collector process exists to hold CDP events across those boundaries.
The in-page buffer (`window.__webpilot_network`) *is* the persistence, which is
why the JS-hook design is the architecturally forced choice, not a shortcut;
its blind spots (subresources, service-worker fetches, WebSockets) are the
accepted trade-off. Revisit only if a persistent per-context collector ever
exists (e.g. a long-lived MCP server opting into richer capture for its own
lifetime).

## Rejected: WebDriver BiDi

Chrome-only tool; CDP remains the feature-complete default for Chrome in both
Puppeteer and Playwright, and no capability WebPilot needs is BiDi-only.
