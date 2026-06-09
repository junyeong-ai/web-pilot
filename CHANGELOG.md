# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.69] - 2026-06-09

### Fixed

- **A transient `Target.getBrowserContexts` error no longer fails the common
  tab-resolve fast paths.** Failing context-scope reads closed (0.4.68) is correct,
  but the read was issued up front, so even resolving a still-pinned tab or the
  exact bound target — paths that never consult the created-context list — aborted
  on a CDP hiccup. The created-context list is now read only on the paths that
  actually scope by it (a fresh attach's first-page pick, the sole-page navigation
  fallback); the persisted-pin and exact-target fast paths resolve without it.

## [0.4.68] - 2026-06-09

### Fixed

- **Context isolation fails closed, not open.** The created-context list that
  scopes the default agent away from isolated `--context` tabs was read
  best-effort: a `Target.getBrowserContexts` error became an empty list, which
  made the default scope match every context's tabs again — the very leak the
  scope exists to prevent. The four lookups now propagate that error (abort, or
  resolve no target) rather than silently widen scope, reusing the existing
  `get_browser_contexts` (which already validates its response) instead of a
  swallow-and-default duplicate.
- **`frame find` surfaces a predicate evaluation fault instead of reporting no
  match.** A `cdpEval` rejection in the per-frame probe was caught into `null` and
  the frame silently skipped, so a faulting predicate looked like `FrameNotFound`
  (no frame matched) rather than the real error. The fault is now remembered and
  surfaced, matching the clean-`false` path and headless behaviour; only an
  unreachable frame is a silent skip.

## [0.4.67] - 2026-06-09

### Fixed

- **A default (no-`--context`) agent no longer sees or attaches to tabs opened by
  an isolated `--context` agent.** The default target scope matched EVERY browser
  context, so a plain `tab` list, `tab switch`, `capture`, and the pin resolver all
  reached an isolated context's tabs — a multi-agent isolation breach. The default
  scope is now every target NOT in a Chrome-created context (default-context
  targets do carry a browserContextId, just one Chrome doesn't list among the
  created ones), applied through one `target_in_context` helper at all four target
  lookups. Guarded by a tab-level isolation assertion in the headless e2e.
- **An accessibility capture in an unreachable cross-origin OOPIF fails loud
  instead of returning the root tree.** Scoping the AX tree to the active frame
  (0.4.66) left a worthless fallback: when the frame's CDP id couldn't be resolved
  (an OOPIF has no in-tab context), it returned the ROOT document's tree under an
  iframe-scoped envelope — coherent but factually wrong. It now returns
  `FrameNotFound`, the same boundary `eval`/`find` use.

## [0.4.66] - 2026-06-09

### Fixed

- **The accessibility tree follows the active frame.** With an iframe switched in,
  `capture --include accessibility` returned the ROOT document's AX tree while the
  footer/URL reported the iframe — the agent read accessibility for a frame it
  wasn't looking at, missing the iframe's own controls. `getFullAXTree` is now
  scoped to the active frame's CDP frame id (headless: `active_frame_id`; browser:
  resolved through the same nonce path eval uses, unambiguous for same-URL
  siblings), matching how DOM/screenshot/metadata already scope. Guarded in both
  e2e suites.

## [0.4.65] - 2026-06-09

### Fixed

- **`type` into a disabled or read-only field now fails loud instead of reporting a
  phantom success.** A `.value` write succeeds via JS even on a field a real user
  can't edit — and the page then never submits a disabled value and resets or
  ignores a read-only one. `type` now rejects `disabled` / `aria-disabled` / `readOnly`
  targets with `InvalidArgument`, so the agent learns the edit won't take rather
  than believing it did. Both modes.
- **A click-opened tab reports its settled URL and title, not `about:blank`.** The
  new tab's identity was read the instant it was created, so a slow or redirecting
  `target=_blank` popup was described as `about:blank` (and an empty title) while
  the agent was already pinned to it — and the auto-capture, which waited, showed a
  different page. The adopt step now waits for the popup to commit and parse, then
  reads its identity from the live target. Both modes.

## [0.4.64] - 2026-06-09

### Fixed

- **`scroll` reports the true landing position on a smooth-scrolling page.** The
  relative scroll used the bare `window.scrollBy(0, dy)`, which inherits a page's
  `scroll-behavior: smooth` and animates — returning before the scroll finishes, so
  the auto-capture footer reported a mid-animation `scroll_y` that didn't match
  where the page actually came to rest. It now forces `behavior: "instant"`, the
  same as `scroll_to`, so the position is final by the time the snapshot reads it.

## [0.4.63] - 2026-06-09

### Fixed

- **A click inside a switched iframe that navigates that iframe no longer captures
  the pre-click page.** The action settle watched only the main frame, but a click
  that navigates an embedded iframe (an internal link, a paginated widget) never
  moves the top URL — so the auto-capture, and the next command, read the iframe's
  OLD document. The bridge now reports a current-frame navigation hint
  (`frame_navigates`) beside the top-frame one, and the action waits for the
  switched frame's new document to commit and parse (a fresh execution context;
  documentId in browser mode) before capturing. Both modes, guarded by an
  iframe-internal-nav fixture in both e2e suites.
- **A same-document top-URL change from a switched iframe no longer drops a live
  frame.** The frame-scope reset keyed off the top URL changing, so a click inside
  an iframe that ran `history.pushState` on the top reset the still-live frame —
  every later command then resolved a dead scope. The reset now distinguishes a new
  main document (the iframe is gone — reset) from a same-document URL change (the
  iframe is intact — leave it), resetting only when the switched frame has actually
  vanished.

## [0.4.62] - 2026-06-09

### Fixed

- **`context list` no longer launches Chrome.** Listing the multi-agent context
  store is pure filesystem I/O, but it was classified alongside the context
  commands that need a live session, so running it with no session cold-started
  Chrome (or failed outright where Chrome is unavailable) just to read a directory.
  It now resolves before any transport opens. `context close` still binds the
  session it needs to dispose a live CDP context.
- **A `Capture` request with no `include` defaults to the DOM.** The wire field
  defaulted to an empty list, so a raw IPC caller (or the host parsing a bare
  `{"type":"Capture"}`) got back an empty capture — no DOM, no screenshot, no
  error. The wire default is now the DOM, matching the CLI surface; an explicit
  `include` is still respected.

## [0.4.61] - 2026-06-09

### Fixed

- **A same-URL main-frame navigation from a switched iframe no longer leaves the
  agent stuck on a dead frame.** Clicking a `target=_top` link to the current URL
  (or otherwise reloading the top) from inside a `frame switch`ed iframe destroys
  that iframe without changing the URL, so the `url_changed`-gated frame reset
  missed it: every later command then resolved a dead frame context
  (StaleSnapshot/FrameNotFound) until an explicit `frame switch`. The action now
  also resets to the main frame when the switched-into frame has vanished — checked
  only when a frame is actually switched and the URL didn't change, so the common
  path is unaffected. Both modes.

## [0.4.60] - 2026-06-09

### Fixed

- **`wait text` now catches text revealed by an attribute change.** It matched on
  visible `innerText` but observed only `childList`/`characterData`, so text that
  appears when an element loses `display:none` via a style/class change (a common
  reveal pattern) never fired the observer and the wait timed out. It now also
  observes `attributes` and polls — the same belt-and-suspenders the selector wait
  already used.
- **An aborted or timed-out XHR is labeled accurately, not "Network error".** The
  network monitor mapped every `status===0` loadend to "Network error", so a
  request the page itself cancelled (`xhr.abort()`) or one that timed out was
  reported as a network failure. It now reads the actual terminal event
  (`abort`/`timeout`/`error`). Both modes.

## [0.4.59] - 2026-06-09

### Fixed

- **A click that opens a popup now reports the new tab on the text/MCP channel,
  not only in JSON.** `new_tab` is an object (the adopted popup), which the
  string-keyed extras table skipped, so terminal/MCP output showed the new page's
  URL via `page_url` but never that the working tab had MOVED to a freshly opened
  one. Its URL is now rendered as a `New tab:` line.
- **MCP tool errors carry the typed error code.** A failed tool returned only the
  guidance text with `isError:true`, dropping the `{code, …data}` the CLI exposes
  via `--json`. The wire error is now in `structuredContent`, so a client can
  branch on `ElementNotFound` vs `Timeout` vs `PolicyDenied` instead of parsing prose.
- **A drag coordinate exactly on the right/bottom viewport edge is rejected, not
  dispatched off-target.** The in-viewport check used inclusive bounds, but the
  viewport is half-open `[0, innerWidth)` — a centre on `innerWidth`/`innerHeight`
  (an element straddling the edge) would dispatch onto nothing. Now strict bounds.

## [0.4.58] - 2026-06-09

### Fixed

- **`upload` no longer silently sets a file on a detached input.** A node the page
  removed between `prepareUpload` and `DOM.setFileInputFiles` keeps a live CDP
  objectId, so the existing null check passed it through and the file-set hit an
  orphaned node with no effect. The target is now resolved through an
  `isConnected` recheck, so a removed input becomes a typed `StaleSnapshot`. Both
  modes.
- **A navigation no longer silently rebinds to an unrelated tab if the navigated
  tab is closed mid-flight.** When the bound tab vanished (closed by another
  process) and exactly one sibling remained, `bound_target`'s sole-page fallback
  resolved that sibling and the navigation rebound to it. A same-tab navigation
  keeps its target id, so the rebind path now fails loud when the resolved target
  id is not the one it set out to navigate.

## [0.4.57] - 2026-06-09

### Fixed

- **A cursor:pointer-only click target with a hidden interactive child is no longer
  dropped.** The `cursor:pointer` pass surfaces only the innermost such element,
  skipping any that wrap an already-collected control. But a `display:none`
  interactive descendant — collected as a candidate, then dropped by the visibility
  filter — still counted as "wrapped", so a clickable card containing e.g. a hidden
  input was skipped AND its child dropped, leaving the card unindexed and
  unaddressable. Only a VISIBLE collected descendant now marks an element a wrapper.

## [0.4.56] - 2026-06-09

### Changed

- **Consolidated three duplicated naming/structure conventions to a single
  source**, so the read and write sides can't drift:
  - The `ctx-<hash>.json` context-file pattern, checked inline in five sweeps over
    the contexts dir, is now one `is_context_file` predicate beside the
    `context_file_path` writer.
  - The Native Messaging host name and manifest path, rebuilt inline in `status`
    (the whole Chrome path) and `uninstall`, are now `NM_HOST_NAME` +
    `nm_manifest_path()` shared with `setup`.
  - The frame-tree walker existed twice (one per output type); `FrameRecord` was a
    strict subset of the wire `FrameInfo`, so it and its walker are removed and
    `frame find` uses `FrameInfo` and the one `collect_frames`.

## [0.4.55] - 2026-06-09

### Fixed

- **Browser-mode `fetch` surfaces a rejected request instead of "no result".** A
  failed fetch (DNS, connection refused, CORS) arrives as a JS eval exception,
  which the handler ignored, returning the misleading "No fetch result". It now
  raises the exception, matching headless (`page.evaluate(...)?` propagates it).
- **`self update` aborts if the new binary can't be code-signed**, keeping the old
  binary, instead of reporting success while installing an unrunnable unsigned
  binary. Signing happens before the atomic swap.
- **A multi-agent context whose target-id write fails no longer leaks a target.**
  `resolve_context_target` swallowed the persist of the resolved/created target,
  so a failed write left the next process to create another target against a stale
  record. The write is propagated, and a target created in the failing resolve is
  closed so the command fails atomically with nothing leaked.

### Changed

- Removed historical "used to / replaced / previously" notes from comments (tab
  find, settings, capture frame validation) so the code reads as designed.

## [0.4.54] - 2026-06-09

### Fixed

- **A `console start` / `network start` whose armed-state marker fails to persist
  now fails the command, instead of reporting success while the next process runs
  with no monitor.** The marker file is what makes later CLI invocations re-arm the
  monitor; its write was discarded with `let _ =`, so a failed write left the agent
  believing monitoring was on when a separate `console read` later would silently
  see an empty buffer. The write is now propagated — the same correctness the pin
  writes got in 0.4.53. (The marker stays a plain presence file: an empty file
  needs no atomic write, only an un-swallowed one.)

## [0.4.53] - 2026-06-09

### Fixed

- **A pin/frame/device persistence write that fails now fails the command, instead
  of silently reporting success with a stale pin on disk.** `tab switch`,
  `tab close`, `frame switch`, and `device set` exist to persist state for the
  NEXT process (each CLI invocation re-attaches); the atomic write of that state
  was discarded with `let _ =`, so a failed write (e.g. a full disk) left the old
  pin in place and the next process silently attached to the wrong tab. The writes
  now return their result and the commands propagate it — matching session export.
  A click-opened popup adoption, which routes through `tab switch`, already treats
  a failure as "not adopted", so it degrades cleanly.

## [0.4.52] - 2026-06-09

### Fixed

- **Browser mode now honours an `eval` deny when re-arming console/network
  monitors across a navigation, matching headless.** The service worker re-injected
  the MAIN-world monitor hooks after every navigation purely on the armed flag,
  ignoring policy — so a deny that landed after `console start` kept capturing
  across the next navigation, while headless `reinstall_monitors` re-checks the
  `eval` gate and stops. The host now forwards the current `console`/`network`
  policy verdicts with each command and `rearmMonitors` skips a denied injector
  (the armed flag is kept, so re-allowing `eval` re-arms). Guarded by a deny-re-arm
  assertion in both e2e suites.

## [0.4.51] - 2026-06-09

### Fixed

- **A `<select>` / listbox with more than 50 options now flags the cut.** The
  bridge caps an element's option list at 50 to bound tokens, but the snapshot
  carried no signal — so a 250-country dropdown rendered `options(50)` as if
  complete and the agent could conclude an unlisted option did not exist. The
  element now carries `options_truncated`, rendered as `options(50+)` and
  serialized in JSON, matching the `text_truncated` / `shadow_truncated` markers.

## [0.4.50] - 2026-06-09

### Fixed

- **`capture --include text` now flags when the page text was clipped.** The
  bridge caps extracted text at 50,000 codepoints; previously a longer page was
  silently truncated, so the visible prefix read as the whole page. The snapshot
  now carries `text_truncated`, rendered as a footer (`--- page text clipped … ---`)
  in human/MCP output and serialized in JSON — matching the existing
  `shadow_truncated` / console / network truncation markers. Both modes.

## [0.4.49] - 2026-06-09

### Fixed

- **Browser-mode session export is written atomically too**, matching headless.
  The NM host wrote the exported session JSON with `std::fs::write`; it now uses
  `atomic_write` (temp + rename) like the headless path, so the two modes are
  consistent and an interrupted export never leaves a partial file behind.

## [0.4.48] - 2026-06-09

### Fixed

- **Persisted pin/frame/device state is written atomically, so a concurrent
  process can't read a torn pin and retarget silently.** `std::fs::write`
  truncates-then-writes, so a second WebPilot process resolving the active tab
  could read it mid-write — empty or torn — parse it as "no pin", and fall through
  to a DIFFERENT tab than the one the agent pinned. These writes now use the
  policy store's `atomic_write` (temp + rename); session export is atomic too.

## [0.4.47] - 2026-06-09

### Fixed

- **A capture taken immediately after a navigation no longer races to an empty
  DOM.** After a link click, `action navigate`, or `capture --url` committed a new
  document, the snapshot could come back with the correct `page_url` but ZERO
  elements: the new document's isolated bridge world is a fresh execution context,
  and for a poll cycle the context map still handed back the transitional
  pre-commit document, which extracts empty. The transport now waits until the
  bridge context names the live, committed, parsed document — verifying the
  context's own `location.href` and `readyState` — before reading through it. The
  headless e2e now asserts the post-navigation capture's ELEMENTS, not just its
  URL, so the race can't silently return.

## [0.4.46] - 2026-06-09

### Fixed

- **Closing the active tab is `TabNotFound` next even before a pin was persisted.**
  The v0.4.45 fix relied on a persisted pin going dead, but a fresh session acts on
  its implicit target with no pin written — so closing it skipped the dead-pin path
  and the next command silently rebound to an arbitrary tab. `do_tab_close` now
  records the active target as the pin before closing it, so the next command fails
  loud in that case too.
- **Browser `cookie set` validates the URL scheme** like headless (where CDP
  enforces http/https), returning a typed `InvalidArgument` instead of a less
  specific exception with a different code.

## [0.4.45] - 2026-06-09

### Fixed

- **Closing the active tab is `TabNotFound` on the next command, not a silent
  retarget (headless).** Headless `tab close` of the active tab cleared the pin, so
  the next command silently landed on a different tab — the page the agent never
  saw. It now leaves the dead pin in place so the next command fails loud (matching
  browser mode and the "a vanished pin is TabNotFound, never a silent retarget"
  rule); the command after that recovers on a live tab.

## [0.4.44] - 2026-06-09

### Fixed

- **A subframe's load-stop no longer ends the post-click navigation wait early.**
  After a link click, the live wait returned on any `frameStoppedLoading`, so a
  cross-origin iframe reloading on its own could end the wait before the main-frame
  commit — `--capture` then snapshotted the pre-click page and the agent saw a
  success at the old URL. The wait now ignores subframe start/stop, like its
  buffered-replay path already did.
- **key_press of an emoji works in both modes.** Headless accepted a single code
  point; the browser used the UTF-16 length and rejected an astral character as
  InvalidArgument. The browser now counts code points, so both modes agree.
- **capture `annotate` + `full_page` is rejected in both modes.** Headless refused
  the pair (annotations are viewport-only); the browser had no guard. It now
  rejects it with the same message.

## [0.4.43] - 2026-06-09

### Fixed

- **Device emulation drives touch, so a mobile preset is actually touch-capable.**
  `device preset iphone-15` set metrics and UA but never touch emulation, leaving
  `navigator.maxTouchPoints === 0` — a page using touch detection to serve its
  mobile UI saw a desktop client. Apply now enables touch for mobile devices (and
  disables it for desktop), and `device reset` turns it back off.

## [0.4.42] - 2026-06-09

### Fixed

- **`scroll` with amount 0 is rejected instead of a no-op success.** It ran
  `scrollBy(0, 0)` and returned success though the tool schema declares
  `minimum: 1`. The shared bridge now rejects an explicit 0 with `InvalidArgument`;
  an absent amount still defaults to 600.
- **`action --capture` onto a non-http pin reports the missing snapshot.** When a
  click-opened popup stayed about:blank (a non-http pin now resolves to null), the
  auto-capture was skipped with no `capture_error`, so the agent got a clean
  success and no DOM. It now sets `capture_error` (NoPage) like any capture failure.

## [0.4.41] - 2026-06-09

### Fixed

- **`frame list` is `NoPage` on a non-http page, like `frame switch`.** It returned
  an empty `{ frames: [] }` success on a chrome:// pin — read as "this page has no
  iframes" rather than "there is no page". An http page with no iframes still
  returns the empty list.
- **A misspelled key-press modifier is rejected, not silently dropped.** The
  `Modifiers` struct (every field optional) accepted unknown keys, so an MCP caller
  sending `control`/`command` instead of `ctrl`/`meta` had the chord sent as a bare
  key with no error. It now returns `InvalidArgument` naming the valid modifiers.

## [0.4.40] - 2026-06-09

### Fixed

- **`session import` fails `NoPage` when Web Storage can't be applied, matching
  export.** The export-side guard had no import sibling: with no active http page
  (e.g. a chrome:// pin), import silently skipped the file's local/sessionStorage
  yet reported success. Import now refuses up front when the file carries storage
  but no page is active, instead of dropping it quietly.

## [0.4.39] - 2026-06-09

### Fixed

- **A completed network entry is re-stamped so `--since` polling still sees it.**
  The v0.4.38 in-flight recording stamped each entry's timestamp at request start
  and left it there, so a request that started before a `--since` cursor but
  finished after it was filtered out — an incremental poller saw the request in
  flight but never its resolution. Entries are now re-stamped at completion,
  restoring the at-completion `--since` semantics while a plain read still shows
  in-flight requests.

## [0.4.38] - 2026-06-09

### Fixed

- **A command on a pin left on a non-http page reports `NoPage`, not
  `BridgeUnavailable`.** `resolveActiveTab` applied its http(s) check only to the
  focused-tab fallback, so a pin on chrome://newtab reached the bridge inject and
  failed with a confusing infra error. The pinned path now also returns "no page",
  so every bridge-needing command (including `session export`) says "navigate
  first".
- **`network read` shows an in-flight request** instead of an empty buffer. The
  monitor recorded a request only on completion, so a read during a slow request
  read as "no network activity". Both fetch and XHR now record the request at start
  (no status yet) and fill it in on completion.

## [0.4.37] - 2026-06-09

### Fixed

- **A capture scoped to a since-removed frame is `FrameNotFound`, not a stale
  success.** After `frame switch` into an iframe the page later removed, a
  screenshot/PDF/accessibility-only capture returned success — the dead-frame check
  ran only in the DOM pass (browser) and the metadata read swallowed the failure
  into empty URL/title (headless). Both modes now validate the active frame for
  every capture mode and surface `FrameNotFound`.

## [0.4.36] - 2026-06-09

### Fixed

- **`action focus` accepts a shadow-DOM control instead of falsely rejecting it.**
  The v0.4.35 focus guard checked `document.activeElement`, which only names the
  outermost shadow host — so focusing an `<input>` inside a web component's open
  shadow root reported `InvalidArgument` though the focus landed. The guard now
  also descends the shadow-active chain, accepting a focused shadow child while
  still rejecting a genuinely non-focusable element.

## [0.4.35] - 2026-06-09

### Fixed

A sweep of the "action reports success while doing nothing" class:

- **`action focus` rejects a non-focusable element.** It always returned success,
  but `focus()` on a static div/span silently doesn't land — the agent then sent a
  key-press to the wrong place. It now verifies focus actually landed.
- **`action select` requires a native `<select>`.** It only checked `.options`,
  which a `<datalist>` also exposes, so a select on one reported success while
  selecting nothing. It now guards `instanceof HTMLSelectElement`.
- **Browser `session export` fails when no http page is focused** (e.g. only
  chrome://newtab) instead of writing a session with silently empty Web Storage —
  it returns `NoPage`, matching headless.

## [0.4.34] - 2026-06-09

### Fixed

- **`action type` rejects a non-text element instead of a silent wrong-success.**
  It dispatched to the typing path for any element; on a link, button, checkbox,
  or div the native value-setter threw and a fallback stamped a meaningless expando
  `.value` plus synthetic events — returning OK while changing nothing, so the
  agent believed its text landed. It now verifies the target is genuinely
  text-editable (contenteditable, textarea, or a text-admitting `<input>`) and
  returns `InvalidArgument` otherwise, pointing at `action click`/`action select`.

## [0.4.33] - 2026-06-09

### Fixed

- **Browser `console read` coerces a non-numeric timestamp to 0, matching
  headless.** The sanitizer validated `level`/`message` but forwarded `timestamp`
  as-is, so a page-injected numeric-string timestamp reached the CLI as a string
  that wouldn't deserialize — a tampered entry became a malformed-reply error
  rather than clean output. It now coerces like headless's `as_u64().unwrap_or(0)`,
  keeping the entry; the two modes stay identical.

## [0.4.32] - 2026-06-09

### Fixed

- **`wait selector` with an invalid CSS selector returns `InvalidArgument`, not a
  false `OK`.** The invalid-selector guard resolved the wait with the wrong error
  envelope (`{ error: … }` instead of the bare error object the timeout path uses),
  so the Rust side parsed it as success — `wait selector "["` reported the wait
  satisfied. It now matches the timeout path, so an invalid selector is the typed
  error a valid-but-unmatched one already was.

## [0.4.31] - 2026-06-09

### Fixed

- **A screenshot/PDF/accessibility-only capture reports which page it captured.**
  The handler dropped `page_url`/`page_title`; a DOM capture still showed them in
  its footer, but an artifact-only `capture --include screenshot`/`pdf` returned
  just the path — so after a redirected `--url`, or with an iframe as the active
  frame, the agent couldn't tell what page the artifact reflected. The no-DOM path
  now surfaces the page URL/title in both the JSON and the human/MCP text.

## [0.4.30] - 2026-06-09

### Fixed

- **`find --click`/`--fill` surfaces the navigation or popup it caused.** The
  chained action's `url_changed`/`new_tab` were discarded, so the `find --click`
  shortcut on a link that navigates or opens a tab told the agent nothing changed —
  while a direct `action click` reports both. `find` now appends them to the JSON
  and human output, exactly like `action`.
- **`webpilot --browser status` gives a specific connection diagnosis again.** It
  went through a transport that flattened every `IpcError` into `ConnectionLost`,
  so the diagnostic's `downcast` to `IpcError` could never match and every failure
  read as a generic "Status query failed". It now keeps the typed error: with no
  host the agent gets "Host not running" plus the `setup nm-host` / manifest hint.

## [0.4.29] - 2026-06-09

### Fixed

- **A CDP event-buffer overflow during a wait is reported as event loss, not a
  Timeout.** `wait_for_event` silently swallowed a broadcast `Lagged` (a burst
  larger than `cdp.event_buffer`); if the awaited event was among the dropped
  messages, the wait ran to a generic Timeout that implied the page/event never
  happened. It now reclassifies such a timeout as a typed `ConnectionLost` naming
  the overflow ("retry, or raise cdp.event_buffer"), while still waiting through a
  recoverable lag so a transient burst is not turned into a spurious failure.

## [0.4.28] - 2026-06-09

### Fixed

- **`console read`/`network read` flag a truncated buffer.** The MAIN-world monitor
  buffers cap at 500 and silently evict the oldest, so a read after 500+ events
  looked complete — a startup error before entry 501 read as a confident "no
  error". A `truncated` flag (conservatively true when the buffer is at capacity)
  now rides in both the JSON and the human/MCP text, so neither surface mistakes an
  incomplete buffer for the whole story — matching the existing shadow-DOM-clip
  warning pattern.

## [0.4.27] - 2026-06-08

### Fixed

- **`capture --include text` shows the page text in the terminal and to MCP, not
  only in `--json`.** `DomSnapshot::to_text` — the renderer behind both the human
  output and the MCP `to_agent_text` — emitted the element index and footers but
  never the captured `text_content`, so the requested page text was serialized to
  JSON yet silently dropped from every agent-facing text path. It now renders a
  `--- Page text ---` block when text was captured.

## [0.4.26] - 2026-06-08

### Fixed

- **A button/image input is findable by its visible label.** `<input type=submit
  value="Search">` (and `type=button`/`reset`, and `type=image` whose label is its
  `alt`) carried its label only in `value`, but `find --text` searches `text`/`name` —
  so the button was in the snapshot yet `find --text "Search"` matched nothing. The
  snapshot now puts each input type's real label in `text`.
- **Detail-carrying wire errors no longer double their Display prefix.**
  `InvalidArgument`/`ConnectionLost`/`Session` round-tripped through the host
  rebuilt `detail` from the already-prefixed `message`, yielding "Invalid argument:
  Invalid argument: …" (reachable via the NM host's oversized-command guard). The
  raw `detail` now travels as a structured field, so the prefix applies once.

## [0.4.25] - 2026-06-08

### Fixed

- **`wait selector` resolves on a state pseudo-class change a MutationObserver
  can't see.** It re-checked only from mutations, but `el.checked = true`,
  `el.disabled = false`, or a live `.value` edit fire none — so
  `wait selector 'input:checked'` (or `:disabled`/`:valid`/`:focus`) ran to its
  full timeout though the element already matched. A bounded 100ms poll now runs
  alongside the observer (the observer keeps instant response to structural and
  attribute changes), the same approach `waitForSelector` takes elsewhere. Both
  modes get it (shared bridge path).

## [0.4.24] - 2026-06-08

### Fixed

- **Context GC deletes a record only on confirmed disposal.** The idle-context
  sweep treated a FAILED `get_browser_contexts` re-list (CDP socket dropped
  mid-sweep) as proof the context was gone and deleted its metadata, orphaning a
  possibly-live Chrome context that leaked until Chrome quit. It now keeps the
  record on an unknown result and retries next sweep.
- **`label[for]` resolves through the element's shadow root**, completing the IDREF
  sweep (with aria-labelledby/describedby): a custom labelable control whose label
  lives in its own shadow root is no longer returned unlabeled.
- **Browser `console`/`network read` sanitize entries to the headless wire shape** —
  dropping a buffer entry with an unknown console level or an incomplete network
  shape (the MAIN-world buffer is page-reachable) and coercing a console message to
  a string, so both modes deserialize an identical typed result and a tampered
  entry can't break the read.

## [0.4.23] - 2026-06-08

### Fixed

- **`aria-describedby` resolves through the element's root**, like `aria-labelledby`
  does, so a control inside an open shadow root whose description element lives in
  that same root now contributes its help/constraint/error text instead of an
  empty `description`.
- **Browser `session import` reports malformed cookie rows** instead of silently
  dropping them and returning success — it now counts skipped rows and returns the
  same partial-failure result (and message) as headless, so an import that lost a
  cookie can't read as a full success.

## [0.4.22] - 2026-06-08

### Fixed

- **Browser/MCP mode resets the frame scope when an action navigates the tab.**
  After `frame switch` into an iframe, a click or key that navigated the main tab
  (a `_top` link or form submit) destroyed that iframe but left `activeFrameId`
  pointing at it, so the `--capture` auto-snapshot and every later command
  targeted a dead frame (`capture_error`/no fresh DOM). The settled-navigation
  branch now drops to the main frame on `url_changed`, matching the explicit
  navigate/back/reload cases and headless `clear_active_frame`.

## [0.4.21] - 2026-06-08

### Fixed

- **`tab find --url` matches with the same `*` glob as `frame url`**, not the
  star-stripping substring the latter just shed. `tab find --url '*'` reduced to
  an empty needle that matched everything, silently switching to the first listed
  tab, and a middle `*` matched the wrong URL. Both URL selectors now route
  through one shared matcher (`webpilot::url_glob`), so they can't drift, and an
  empty or all-wildcard pattern is rejected with `InvalidArgument`.

## [0.4.20] - 2026-06-08

### Fixed

- **The policy store lives in the durable data root, not the evictable cache.** It
  was `artifacts/policies.json` under the cache tree (`~/Library/Caches`,
  `$XDG_CACHE_HOME`, `$XDG_RUNTIME_DIR`) — which the OS evicts under disk pressure
  and cache cleaners wipe. Losing it silently reset every deny rule to the default,
  so a `policy default deny` + allowlist guardrail would fail OPEN with no error.
  The store moves to `policy/policies.json` under the durable data root
  (Application Support / `$XDG_DATA_HOME` / `~/.local/share`), still honoring
  `$WEBPILOT_HOME`. Fail-closed-on-corruption is unchanged; this closes the
  fail-open-on-eviction gap beneath it.

## [0.4.19] - 2026-06-08

### Fixed

- **`tab list` marks the agent's pinned tab active, not every CDP-attached one.**
  It read the CDP `attached` flag (true for any debugger client), so an open
  DevTools window or a second tool made tabs the agent never pinned read as active.
  Active is now the tab this transport actually acts on.
- **`wait navigation` preserves a typed infrastructure error** instead of mapping
  every failure to `Timeout` — a dropped CDP socket now surfaces as
  `ConnectionLost` (exit 3), not a misleading navigation `Timeout` (exit 5).
- **`diff --screenshot` counts a size change as changed.** Two differently-sized
  images with an identical overlapping region used to read as unchanged; the
  `changed` flag now also trips when the dimensions differ.

## [0.4.18] - 2026-06-08

### Fixed

- **`frame find` surfaces a predicate's evaluation error** instead of disguising
  it as `FrameNotFound`. Both modes swallowed a thrown predicate (a typo, a
  reference error) and treated it as "didn't match"; the error is now returned
  when nothing matches, so a broken predicate is distinguishable from one that
  cleanly matched no frame.
- **`frame url` matches with a real `*` glob, not star-stripping.** It removed all
  `*` then did a substring check, so `foo*bar` searched for "foobar" and an empty
  or all-`*` pattern silently matched the first frame. A shared `*`-glob replaces
  it in both modes (`/auth/` stays a plain contains-match), and an empty or
  all-wildcard pattern is rejected with `InvalidArgument`.

## [0.4.17] - 2026-06-08

### Fixed

- **`tab new` settles on a ready page and reports its real URL/title**, like
  `navigate`. It echoed the requested URL and returned before the tab loaded, so
  a redirect made the reported URL wrong and the agent's next action could race
  the load. Both modes now wait for the new tab to leave about:blank and parse,
  then read the landed URL/title.
- **`diff` reports an explicit `changed` boolean** (DOM and image output) so a
  caller checks one field instead of inferring change from the counts. The exit
  code stays 0 on success — a WebPilot exit code names an error class, not a
  domain result.
- **An unparseable boolean env var falls through instead of silently reading as
  `false`.** `WEBPILOT_*=tru` (a typo) used to become `false` and override a
  correct `config.toml`; only `1/true/yes/on` and `0/false/no/off` are recognized,
  anything else is treated as unset.

## [0.4.16] - 2026-06-08

### Fixed

- **`wait idle` watches attribute and text mutations**, not just node insertion,
  so a page still toggling a class or updating a live counter is no longer
  declared idle after the first 500ms quiet window.
- **`find --role` matches an ARIA role only, never a raw tag name** — `--role nav`
  no longer matches `<nav>` (its role is `navigation`) and `--role div` no longer
  matches every `<div>`. Use `find --tag` for tag matching.

### Added

- **`console read --since <ts>`** — an incremental cursor for the console buffer,
  matching `network read`, so polling no longer needs a destructive `console
  clear` to advance.

## [0.4.15] - 2026-06-08

### Fixed

- **An unknown `key_press` key is a typed error, not a silent no-op success.** A
  typo like "Entr" or an out-of-range "F13" was dispatched with no native effect
  while reporting success; an unrecognized multi-character key now returns
  `InvalidArgument` (a single character still types via its text). Both modes.
- **MCP rejects misplaced/unknown action arguments.** `Action` deserialized
  permissively, so e.g. `ctrl` placed at the top level instead of inside
  `modifiers` was silently dropped, turning an intended chord into a plain key
  press. `Action` now denies unknown fields and a misaligned call fails clearly.
- **A closed pinned tab is `TabNotFound`, not a silent retarget.** Headless fell
  through to the first page when the persisted pin's tab had closed, landing the
  next command on a different tab; it now fails typed, matching browser mode (a
  genuine Chrome restart already clears the pin, so a fresh attach is unaffected).

## [0.4.14] - 2026-06-08

### Fixed

- **An invalid CSS selector is a typed `InvalidArgument`**, not a silent
  page-response timeout (browser mode) — `wait selector` and any selector
  resolution now catch the `SyntaxError` and name the bad selector.
- **The network monitor logs a `Request`-object `fetch` correctly** — it read
  `String(resource)` (logging "[object Request]") and lost the method; it now
  reads the `Request`'s own url/method, in both modes.
- **`session import` surfaces malformed cookie rows** instead of silently dropping
  them and reporting success — they are counted and reported as a partial failure.

## [0.4.13] - 2026-06-08

### Fixed

- **`eval` distinguishes `undefined`/`NaN`/`Infinity` from `null`.** CDP omits a
  result's `value` for anything JSON can't carry (`undefined`, `NaN`, `±Infinity`,
  `-0`, `BigInt`, functions, symbols), and both modes had coerced that to `null` —
  so `eval "el.onclick"` on a handler-less element returned `null`,
  indistinguishable from a handler set to `null`, and `eval "1/0"` returned `null`
  rather than `Infinity`. A shared decode now renders such results faithfully (the
  `unserializableValue` literal, the bare `undefined`, or the object description),
  while a genuine `null`/`0`/`false`/`""` is preserved.

## [0.4.12] - 2026-06-08

A design-quality pass removing two non-principled patterns.

### Fixed

- **`fetch` no longer forces `Content-Type: application/json`.** Both transports
  hardcoded that header on every request — meaningless on a GET and silently
  mislabeling any non-JSON body (a form or multipart body still went out as JSON).
  `fetch` now sends only the headers the caller gives it via a repeatable
  `--header NAME:VALUE`; a JSON body needs an explicit
  `--header content-type:application/json`. `credentials: include` (the
  authenticated-session contract) is unchanged.
- **The browser annotation-paint delay is the configured setting**, not a
  hardcoded 300ms that disagreed with headless's 200ms default. The host now
  streams `annotation_paint_ms` over the settings handshake, so one setting tunes
  both modes.

## [0.4.11] - 2026-06-08

A design-quality pass: a side-effect-prone heuristic removed, monitor re-arm
brought to parity and given a single responsibility, and the last text/AX capture
parities closed.

### Fixed

- **`frame switch NAME` matches the frame name only — no silent fallback.** It
  used to fall back to matching `NAME` as a URL substring, so a misspelled or
  absent name quietly entered an unrelated iframe whose URL happened to contain
  the string. `name` and `url` are now disjoint: a name with no exact match
  returns a typed `FrameNotFound`, and URL matching is `frame url PATTERN`.
- **Browser monitors re-arm the moment a navigation settles**, matching headless,
  instead of only at the `load`-time `webNavigation.onCompleted`. A `fetch` or
  `console` the new page emitted after DOMContentLoaded but before a slow `load`
  was previously lost from the buffer.
- **`capture --include text` of a text-empty page returns the empty string** (with
  a snapshot shell), as headless does, instead of dropping the result; and a
  text/AX-only snapshot shell now carries the resolved page URL/title.

### Changed

- `rearmMonitors` is now a single-responsibility helper (re-arm the MAIN-world
  console/network hooks, a no-op when the tab is unmonitored); the bridge
  re-inject a bfcache restore needs is its own explicit step in the navigation
  listener, no longer entangled with monitor re-arm.

## [0.4.10] - 2026-06-08

Completes the capture browser↔headless parity sweep the 0.4.9 annotate fix
started: three more fields where the browser handler diverged from headless.

### Fixed

- **`capture --include accessibility` returns the same shape in both modes.**
  Headless serializes the whole `Accessibility.getFullAXTree` response
  (`{ nodes: [...] }`, pretty-printed); browser had stringified only the inner
  `nodes` array, compact. Browser now serializes the full response, pretty —
  an agent parsing the tree sees one shape regardless of mode.
- **A text- or accessibility-only capture reports the subframe count.** Headless
  sets `subframes` on the snapshot shell even with no DOM pass, so the agent
  still gets the "N iframe(s) not shown" hint; browser set it only on a full DOM
  pass and now sets it on the shell too.
- **A no-DOM capture scoped to an iframe reports the frame's title**, not the top
  tab's. Headless reads the active frame's `document.title`; browser had used the
  tab title, mislabeling a frame-scoped screenshot/PDF/AX capture.

## [0.4.9] - 2026-06-08

A seam-level convergence pass (cross-mode parity, policy, error codes, resource
leaks) plus a regression audit of the campaign's own changes — which came back
clean. One reachable parity gap fixed.

### Fixed

- **`webpilot --browser capture --annotate` now returns the annotated
  screenshot.** `--annotate` draws overlay boxes and captures them; headless
  forces the DOM, bounds, and screenshot passes it needs when `--annotate` is
  set, but the browser handler keyed each on `--include`. So `--annotate` without
  an explicit `--include dom,screenshot` drew nothing and returned no image,
  while headless returned the shot. The browser handler now forces all three for
  `--annotate`, matching headless; the browser e2e exercises `--annotate` alone
  and asserts a screenshot comes back.

## [0.4.8] - 2026-06-08

A final convergence sweep across the wire types, command handlers, and a repo-wide
falsy-collapse class sweep. Several small agent-facing correctness fixes; no
behaviour change for the common path.

### Fixed

- **`status` keeps an empty tab title as `""`** in browser mode (a page with no
  `<title>`), instead of collapsing it to `null` via `||` — matching headless,
  which maps `document.title` straight through.
- **A custom-select option with `data-value=""` reports its real empty value**,
  not the visible text. `getAttribute("data-value") || clip(text)` discarded the
  empty string; now `?? clip(text)`, so `action select` targets the right option.
- **`action select` on a missing option lists the valid values in the error
  message.** They were carried only in a structured field the Rust
  `InvalidArgument` variant drops before JSON/MCP; the message is now
  self-contained, so the retry guidance survives every surface.
- **The DOM iframe footer points to `webpilot frame url <pattern>`**, the entry
  path that resolves against the URL-listed subframes, rather than `frame switch`,
  which matches a frame `name` an iframe usually lacks.

## [0.4.7] - 2026-06-08

A convergence sweep of the last under-audited surfaces — cookie/session state and
the embedded-asset version gate — closed a session-cookie data-loss bug and a
silently-inert stale-extension gate.

### Fixed

- **`session export` → `session import` no longer drops session cookies.** CDP
  marks a session cookie (no expiry) with the sentinel `expires: -1`, which was
  stored verbatim and forwarded back on import as `expires: -1` — an absolute
  timestamp one second before the epoch, already expired, so Chrome silently
  dropped it. Exporting a logged-in session and importing it lost exactly the
  cookies carrying the login. The sentinel now maps to "no expiry", so a session
  cookie round-trips as one and `cookie list` shows it as a session cookie rather
  than a 1969 expiry.
- **The stale-extension version gate works again.** `extension/manifest.json` had
  frozen at `1.0.0` while the binary advanced and the extension's own code changed
  across releases, so the host's `VersionMismatch` check — which compares the
  installed extension's version to the bundled one — always compared equal and
  could never fire. A browser-mode user who upgraded the binary without reloading
  the extension ran stale extension code silently. The manifest now tracks the
  workspace version, enforced by a test that fails the build if the two drift, so
  an outdated extension is caught at connect time as designed.

## [0.4.6] - 2026-06-08

A comprehensive parallel sweep of the previously least-audited surfaces — the
browser-mode service worker, the MCP server, the CDP client, and the action
settle logic — turned up five reachable defects (one of them the intermittent CI
flake). No wire or CLI change.

### Fixed

- **A link click reliably reports `url_changed`.** `settled_action_url` concluded
  "no navigation" whenever its one-shot drain of buffered CDP events was empty,
  on the optimistic assumption that a link click's `frameStartedLoading` is always
  buffered by then. But following a hyperlink is a queued task, so the event can
  arrive after the bridge click response — leaving `url_changed` (and the
  `--capture` of the new page) intermittently missing. The bridge now derives a
  deterministic `navigates` hint at click time (a non-prevented self-targeting
  `a[href]` to a different http(s)/file document); when hinted, the settle waits
  for the commit instead of reporting nothing. A non-navigating click — a button,
  a `preventDefault`'d SPA link, a fragment link — sets no hint and still pays
  zero. (This was the CI-only `headless_behavioral_flow` flake.)
- **Browser mode adopts a click-opened tab after settle**, not by a single
  pre-settle check. A `target=_blank` / `window.open` popup whose `tabs.onCreated`
  arrived during the settle window was missed — `new_tab` absent, the pin silently
  left on the opener. Adoption now runs after `settledActionUrl` (whose awaits
  yield the event loop), matching headless. Adds the missing browser-mode popup
  regression test.
- **Browser `dom get` preserves an empty string value.** `r.value || null`
  collapsed a legitimately empty value (`getText` on an empty element, `getAttr`
  on `disabled=""`) to `null`, diverging from headless, which keeps `""`. Now
  `?? null`, so present-but-empty stays distinct from absent.
- **The MCP server answers a malformed request with -32600** rather than silently
  dropping it: a `{}` with no `jsonrpc`/`method` was treated as a no-id
  notification before the envelope was validated, so a client awaiting a reply
  would hang. Validation now precedes the notification check.
- **A CDP wait surfaces `ConnectionLost` immediately.** `wait_for_event` blocked
  until its full deadline and then returned `Timeout` when Chrome died mid-wait —
  the broadcast channel never closes while the struct holds the sender. It now
  polls the liveness flag and returns a typed `ConnectionLost` at once, like
  `send`.

## [0.4.5] - 2026-06-08

A deep review of `bridge.js` — the content script shared by both modes and, until
now, the least-audited critical file — turned up six reachable DOM-extraction and
action defects. Each affects what an agent sees and where its actions land, in
headless and browser mode alike. No wire or CLI change.

### Fixed — DOM extraction & actions (both modes)

- **A control under an `opacity:0` ancestor is no longer reported as actionable.**
  `isVisible` checked only the element's own opacity, but opacity is not inherited
  — a transparent ancestor (a faded-out modal/dropdown/animation) hides its whole
  subtree while each child keeps opacity 1. The check now walks the element and
  its ancestors, across open shadow boundaries, so an invisible control is no
  longer emitted for the agent to click.
- **`wait selector` now also fires on an attribute change**, not only on node
  insertion. A selector that starts matching when an existing element gains a
  class/attribute (`.active`, `[aria-expanded=true]`) was missed and the wait
  timed out; the observer now watches `attributes` too.
- **Every editable host is captured, not only `contenteditable="true"`.** A bare
  `<div contenteditable>` and `contenteditable="plaintext-only"` are editable but
  were dropped unless they carried another marker — comment boxes and rich-text
  editors were invisible to the agent. The allowlist now matches any
  `[contenteditable]` except an explicit `false`.
- **Typing into a contenteditable fires `input`/`change`**, so a framework-bound
  editor (Draft/Slate/ProseMirror, a React `onChange`) sees the edit instead of
  going out of sync with the visible text.
- **Snapshot indices follow document (reading) order**, not the order the three
  collection passes ran. A clickable `<div onclick>` above a `<button>` was
  indexed after it, breaking the agent's top-to-bottom spatial reasoning; the
  deduped candidate set is now sorted by `compareDocumentPosition` before
  indexing, and `state.snapshot` is reordered to match.
- **`aria-labelledby` resolves within the element's shadow root**, not only the
  document. A control inside a shadow root is labelled by a node in that same
  root, where a document lookup returns null — the accessible name came back empty
  and `find --label` couldn't match the component.

### Documentation

- Four stale code references in the rule docs are corrected to match the source:
  `isVisible`'s real bare-`checkVisibility` predicate (not the option-dictionary
  form), the `keyDescriptor`/`key_descriptor` key map (was `keyToCode`), the
  `close_contexts` single-context dispose (was `quit_named_context`), and the
  dropped `webpilot::OUTPUT_DIR` constant. A sweep confirms no doc references a
  function absent from the code.

## [0.4.4] - 2026-06-08

Completes the 0.4.3 hardening across the pure-local commands a transport-scoped
sweep had under-covered, found by re-auditing each class across the whole
codebase and by a full live exercise of every tool in both modes.

### Fixed

- `diff --screenshot`, `record`, and `profile` wrote their artifacts with
  pid-less, collision-prone names — the three artifact writers outside the 0.4.3
  consolidation (they are pure-local / headless-only, not transport handlers).
  All three now flow through the single `dirs::artifact_path` authority, so every
  artifact name carries the pid and two concurrent processes can't overwrite each
  other. (An exhaustive every-write audit confirms no writer is left outside it.)
- `record --dom` matched only a present DOM snapshot and silently dropped any
  frame that produced none, reporting success with a `dom_files` list shorter than
  the frame count. A frame with no DOM is now a hard error.

### Documentation

- The skill's `diff --screenshot` note no longer claims a fixed `diff.png`; it
  writes a timestamped image whose path is in the output (which the skill already
  teaches agents to trust over a guessed filename).

## [0.4.3] - 2026-06-08

A deep concurrency, lifecycle, isolation, and headless↔browser parity hardening
pass. No behaviour change for a cooperative page and a correctly-configured
agent — these close edge cases an adversarial page or a multi-agent / long-lived
session could hit.

### Security

- **Agent-view newline injection is closed everywhere.** Completing the
  line-safety rule: `status` (tab title/url), the action result (`url_changed`
  and the click-opened `new_tab` url), `frame switch`, `tab new`, and `find` now
  collapse control characters in page/server-controlled strings, so none can
  embed a newline and forge an agent-visible line. `--json` stays exact.

### Fixed — headless concurrency & lifecycle

- The context GC no longer disposes a context that a live session is still using.
  A live transport holds a *shared* liveness lock for its lifetime; the GC probes
  it with a non-blocking exclusive attempt and skips while any session is alive —
  with no resolve hang, deadlock, or read-to-dispose race (serialization and
  liveness are now separate locks).
- A `--context` resolve returns the browser-context id it resolved directly,
  instead of re-reading metadata that could `.ok()`-degrade and bind a page
  *outside* the requested context.
- `context list` / `context close` no longer auto-create the `--context` they
  were invoked with (and `close` stays reachable at the context cap).
- Artifact filenames (screenshot/pdf/accessibility/session) carry the pid, so
  two agents capturing at once can't mint the same name and overwrite — one
  `dirs::artifact_path` authority.

### Fixed — stale frame contexts (both modes)

- A MAIN-world eval through an active frame whose context was just destroyed by a
  navigation retries once against the fresh context, mirroring the bridge path —
  covering `frame switch` into a since-navigated frame, frame self-navigation,
  and `frame find`.
- `frame find` settles and re-observes candidate contexts before judging them, so
  a frame whose context lags a navigation is evaluated, not skipped.
- A switched frame that was removed surfaces as `FrameNotFound` in browser mode
  too, not a generic error.

### Fixed — headless ↔ browser parity

- `--capture` is honoured after every headless action — navigate, back, forward,
  reload, drag, hover, upload — not only the bridge-routed ones, matching browser
  mode's auto-capture.
- `frame switch` targets only HTTP(S) subframes in both modes (the set the
  subframe count surfaces).
- A malformed tab id is a typed `TabNotFound` in browser mode, never a lenient
  `parseInt` that closes the wrong tab.
- A zero `wait` timeout stays nonblocking in browser mode (`??`, not `||`).

## [0.4.2] - 2026-06-08

Security and robustness hardening from a deep multi-round adversarial review. No
behaviour change for a cooperative page and a correctly-configured agent.

### Security

- **Newline injection into the agent's view is closed everywhere.** Every
  renderer that turns a page- or server-controlled string into agent-facing
  lines — cookie list/get, tab list, frame list, console read, network read, and
  `find` — now collapses control characters through `line_safe`, not just the DOM
  snapshot. A crafted cookie value, document title, or `console.log` can no longer
  forge a row or inject text the agent reads as its own. (`--json` was already
  safe via JSON escaping.)
- A custom `device set --user-agent` with control characters (CR/LF/NUL) is
  rejected rather than passed toward a request header.

### Fixed

- Browser-mode infrastructure failures (the Native Messaging host not running, a
  closed socket, a timeout, a malformed reply) now surface as the typed
  `ConnectionLost` (exit 3) instead of a generic `Other` (exit 1).
- A `--context` resolve that created the CDP browser context, then failed
  creating its page or persisting metadata, no longer orphans the context in
  Chrome — it is disposed on any post-create error.
- A vanished bound target no longer rebinds the session to an ambiguous sibling
  tab: the fallback is taken only when the context has exactly one page.
- The MCP server answers a JSON-RPC batch (a top-level array) with a -32600
  invalid request instead of silently dropping it and hanging the client.
- The frame-tree walk is depth-bounded, so a pathological nesting degrades to an
  undercount rather than overflowing the stack.

### Documentation

- The console/network monitors' honest boundaries are documented: they are
  MAIN-world and therefore evadable by a hostile page, and they capture from when
  they are armed (a navigation's load-time events, before re-arm, are not
  captured) — an agent must not read an empty buffer as "nothing happened."

## [0.4.1] - 2026-06-08

Hardening follow-up to 0.4.0. No behaviour change for a correctly-configured
agent — internal robustness and a settings-precedence consistency fix.

### Fixed

- The post-navigation context retry (a capture or eval right after a renderer
  swap) is keyed on a typed CDP error instead of matching the protocol error's
  text, so an unrelated message can never be mistaken for it.
- A boolean env override set to an empty string (e.g.
  `WEBPILOT_CHROME_NO_SANDBOX=""`) now falls through to `config.toml` and the
  default instead of forcing `false`, consistent with every other env tunable.
- The release workflow publishes idempotently: a retried or re-pointed release
  rebuilds the GitHub release rather than failing on "release already exists".

## [0.4.0] - 2026-06-08

The release that makes browser mode a first-class peer of headless and hardens
both against adversarial input across many review rounds. Headless and browser
now share one command surface, one set of semantics, and one parity test that
fails the build if they drift.

### Added

- **MCP server** (`webpilot mcp`): a stdio JSON-RPC server exposing a curated
  subset of commands as `browser_*` tools, built over the same `Transport` and
  handlers as the CLI — same mode, policy, and rendering, no second engine.
- **Default-deny policy mode**: `webpilot policy default deny` plus an allowlist
  (`policy set <effect> allow`) gives least-privilege control. Policy is keyed by
  **effect** (`eval`, `fetch`, `cookie_list`, …), enforced at the one sink that
  reaches the browser in each mode, and fails closed on a torn store.
- **Browser-mode parity for the full surface**: deterministic tab binding,
  navigation-settle detection, native CDP key input, full-page CDP screenshots,
  debugger-routed frame `eval` that is not subject to page CSP, and a settings
  handshake — all reaching headless's behaviour, enforced by `browser_parity.rs`.
- **Shadow-DOM clip signal**: a capture whose shadow-host traversal exhausts its
  budget now reports `--- shadow DOM clipped … ---` (`DomSnapshot.shadow_truncated`)
  so an agent knows the index may be incomplete rather than acting on a short list.
- **Host-only cookie scope** is preserved across `session export`/`import`
  (`CookieInfo.host_only`), so a round-trip can't silently widen a host-scoped
  auth cookie to its subdomains.
- **`scripts/uninstall.sh`**: a curl-able one-shot symmetric to `install.sh`,
  delegating to `webpilot uninstall` (the single source of truth for artefact paths).
- **`--no-sandbox` opt-in** for running headless Chrome in an unprivileged
  container (Docker, CI, many cloud sandboxes), where Chrome's setuid sandbox
  can't initialise and it otherwise never reports a DevTools port. Off by default
  (it weakens the sandbox); enable with `WEBPILOT_CHROME_NO_SANDBOX=1` or
  `[chrome] no_sandbox = true`.

### Changed

- The headless bridge runs in a dedicated **isolated world**, mirroring the
  browser content script, so page JS can't tamper with how an index resolves or
  where an action lands.
- `wait` honours its full `--timeout` in headless mode (no longer cut short by
  the generic CDP send timeout), and the MCP `browser_wait` waits the exact
  milliseconds requested (no second-rounding).
- A CDP "invalid params" failure (e.g. a malformed cookie URL) surfaces as a
  typed `InvalidArgument` (exit 7) instead of a leaked `CDP error` string.
- Settings validation rejects zero-valued timeouts (`cdp_send`, `poll_interval`,
  `heartbeat`, `navigation`) at startup rather than degrading to a broken session.
- `diff --dom` parses both inputs as JSON and re-emits canonically — a malformed
  snapshot fails loud, and whitespace/key-order differences no longer read as changes.

### Fixed

- **Off-screen drag** and **`--annotate` outside the main frame** now fail loud
  (typed `InvalidArgument`) instead of releasing into empty space / drawing
  misaligned overlays.
- **Free-text CLI values starting with `-`** (`eval -1`, `type N -5`, `wait text
  -50%`) are accepted as values, not rejected as flags.
- **`quit` serialises against a concurrent launch**, and concurrent `policy set`
  calls can no longer lose an update (a dropped `deny` would leave an effect open).
- The context GC never disposes a context another process is actively using; an
  orphaned Chrome from a crash window is reaped before relaunch.
- A capture/eval immediately after a navigation no longer fails with CDP's
  "Cannot find context with specified id": the bridge drops the stale
  isolated-world context and retries against the new document's context (the
  renderer-swap race, more likely on slower/loaded machines).
- Upload paths are resolved against the CLI's working directory and existence-checked
  before the wire (correct in both modes; a missing file is a typed error).
- Many headless↔browser parity fixes across cookies, monitors, navigation settle,
  history nav, click-opened-tab adoption, and session import error codes.

### Documentation

- README (current to the implementation), the `webpilot` skill, the project and
  per-crate `CLAUDE.md` files, and `.claude/rules` aligned with the live CLI
  surface, the effect-keyed policy model, the DOM output footers (including the
  shadow-clip line), and the install/uninstall one-shots.

## [0.3.0] and earlier

Tagged releases `v0.1.0`–`v0.3.0` predate this changelog. See the Git history
(`git log v0.3.0`) for their contents.
