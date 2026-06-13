# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.210] - 2026-06-13

### Fixed

- **`tab new` is honest about navigation failure.** A URL that doesn't parse
  is now a typed `InvalidArgument` (exit 7) in browser mode too — it was a raw
  `chrome.tabs.create` throw read as `Other` (exit 1). A URL that loads onto
  Chrome's error page (refused connection, DNS) is `NavigationFailed` (exit 8)
  in both modes — `tab new` reported success on the error page, while
  `navigate` (the same effect under the same gate) already failed loud.
  Headless reads the main frame's `unreachableUrl`; browser buffers
  `webNavigation.onErrorOccurred` from before the tab exists. A tab that
  CLOSES during the settle is the root-cause `TabNotFound` (exit 4) in both
  modes.
- **Headless `tab switch`/`tab close` reclassify a post-lookup race as
  `TabNotFound`.** The existence check runs once, then `Target.activateTarget`
  / `connect_to_page` / `Target.closeTarget` could still fail with a raw CDP
  error (exit 1) if the tab closed in the gap — now re-queried against the
  live targets and surfaced as `TabNotFound` (exit 4), matching browser mode's
  catch arms.
- **A screenshot whose tab vanished mid-capture is `TabNotFound`, not a
  degraded `screenshot_error` / `Other`.** The `screenshot_error` note is for
  a live page whose image pipeline failed; a gone tab now surfaces typed
  (exit 4) in both modes instead of "success, no image" for a page that no
  longer exists.

### Changed

- Browser `adoptedDocumentReady` settles on a main-frame load error (not just
  a commit), so a `tab new` / popup to an unreachable URL returns immediately
  instead of waiting out the full navigation timeout — headless already
  settled fast on the same failure.

## [0.4.209] - 2026-06-13

### Fixed

- **An explicitly-set `WEBPILOT_CONFIG` pointing at a missing file fails
  loud.** The default config path being absent is the all-default state, but
  an operator-set path that doesn't exist silently ran on built-in defaults —
  ignoring every setting they intended to apply. Now a typed InvalidArgument
  naming the missing path; non-TOML and directory paths already failed loud.
- **Zero viewport dimensions are refused at startup.**
  `WEBPILOT_VIEWPORT_WIDTH=0` (or height, or the `[chrome]` keys) reached
  Chrome as `--window-size=0,0` and CDP as a 0×0 emulation override — a
  degraded session instead of a typed refusal. Joins the existing
  zero-breaks-downstream validator set; pinned in `config_validation`.
- A modifier click through the MCP `build_action` path is unit-pinned (the
  schema advertises `modifiers`; a silent drop would make it a lie).

## [0.4.208] - 2026-06-12

### Fixed

- **The contenteditable input-probe survives a rich editor's
  `stopImmediatePropagation()`.** The v0.4.207 probe listened on the editing
  host itself — an editor's own capture listener (registered earlier, common
  in rich editors) calling `stopImmediatePropagation()` starved the probe
  into a false "native input never fired" and brought the double dispatch
  back. The probe now listens on document capture (which no target-phase
  handler can cut off) scoped by `composedPath()`, so it sees exactly what
  fired for this element. Pinned in e2e with a stopImmediatePropagation
  editor whose own counter must read 1.
- **`browser_click` (MCP) advertises `modifiers` in its schema.** The
  deserializer accepted them (same `Action` path as the CLI) but the
  inputSchema didn't say so, while `browser_press_key` already advertises the
  identical block — schema drift, not curation: a client generating calls
  from the schema had no way to know modifier clicks exist.
- **A failed device-emulation re-apply on reconnect warns instead of
  vanishing.** Cross-invocation reconnect re-applies persisted emulation; a
  CDP rejection was `let _ =`-swallowed, leaving the session running with the
  REAL user agent/viewport while the agent believed the spoof was active.
  Failing the open would block even the `device reset` that recovers, so the
  honest middle is a stderr warning naming the failure and the recovery.

## [0.4.207] - 2026-06-12

### Added

- **`action click` takes modifier flags.** `--ctrl/--shift/--alt/--meta` ride
  every event of the synthetic click sequence (pointerdown → click), so the
  page's own handlers see them — an app-level ctrl multi-select, a shift
  range-select. Browser-level defaults (open-in-new-tab) intentionally don't
  apply to a synthetic click; that path is `tab new URL`, and the help text
  says so. Same `Modifiers` definition `key-press` already uses — a misspelled
  modifier is still a typed rejection. Pre-fix no surface could express click
  modifiers at all.

### Fixed

- **One `type` into a contenteditable fires exactly one `input` event.**
  `execCommand("insertText")` fires `input` natively on success, and the
  bridge unconditionally dispatched a second one — a raw `oninput` counter or
  an append-per-input editor saw a phantom second edit on every insert. The
  bridge now probes whether the native event fired (one-shot capture listener)
  and dispatches the fallback only when it did not — the fallback still covers
  an empty text, an unsupported command, and a framework that swallowed the
  edit. The synthetic `change` (contenteditable never fires one natively) is
  unchanged.
- **`action drag` documents its boundary**: mouse-event sequence only —
  sliders and mouse-based sortables work; an HTML5 dragstart/drop-API sortable
  won't react (no silent expectation gap in the SKILL).

## [0.4.206] - 2026-06-11

### Fixed

- **Screenshot output reports the saved dimensions — and the downscale ratio
  when one was applied.** A capture whose long edge exceeds the cap (default
  1568px; any full-page shot of a tall page) was silently downscaled: the
  dimensions died in a debug log and only the path reached the agent, so any
  pixel-coordinate math on the image (a vision model picking a click target,
  an `eval` click by coordinates) was wrong by an unknowable factor. Both
  modes now return `screenshot_width`/`screenshot_height` (always) and
  `screenshot_scale` (only when downscaled; page px = image px ÷ scale), in
  the JSON, the human render, and the MCP text block.

## [0.4.205] - 2026-06-11

### Fixed

- **The cookie family is partition-aware, like the session round-trip
  (v0.4.204).** Browser `cookie list`/`get` used a bare `getAll({url})`, which
  omits CHIPS partitioned cookies entirely — the same page listed a
  partitioned auth cookie in headless and reported it absent (a false
  `CookieNotFound`) in browser mode. Worse, `cookie delete` was dishonest in
  BOTH modes: the match came from a partition-spanning read but the delete ran
  partition-less, so the partitioned cookie survived behind a clean
  "Deleted 1" (measured in headless — `Network.deleteCookies` without the key
  leaves it alive). List/get/delete now span partitions
  (`getAll({partitionKey:{}})`), and every delete threads the matched cookie's
  own partition key (`Network.deleteCookies` / `chrome.cookies.remove`). Both
  suites pin the survival check: delete then re-list — the cookie must be
  gone.
- **`cookie list`/`get` rows name the partition.** `partitioned=<top-level
  site>` (plus `,xsite` for a cross-site-ancestor key) — the partition is part
  of the cookie's identity, so a partitioned `sid` and an unpartitioned `sid`
  no longer render as identical rows.

## [0.4.204] - 2026-06-11

### Fixed

- **Partitioned (CHIPS) cookies round-trip through session export/import.**
  The partition key is part of a cookie's identity: dropping it re-imported an
  unpartitioned twin — one the partitioned (embedded) context never sends —
  under a clean success, silently losing partitioned auth state. Browser mode
  was worse: a bare `chrome.cookies.getAll({})` never even saw partitioned
  cookies, so they vanished from the export entirely. `CookieInfo` now carries
  `partition_key` (`top_level_site` + `has_cross_site_ancestor`, the shape CDP
  and `chrome.cookies` share), populated by both readers, forwarded by both
  setters, and exported via `getAll({partitionKey: {}})`; a malformed
  `partition_key` row counts as malformed in both modes — it never imports the
  cookie unpartitioned. Older session files (no field) import unchanged.
- **A tab closing mid-wait while a frame is switched is `TabNotFound`, not
  `FrameNotFound`.** Both modes' frame probes collapsed "the tab can't be
  probed at all" into a frame answer, sending the agent recapturing frames on
  a tab that no longer exists. `frameVanishedError` (browser) now probes the
  tab first — the same tab-first split `ensureBridge` already makes — and
  headless `do_wait` reclassifies a ConnectionLost/FrameNotFound poll failure
  against the browser client's live target list, the same check its
  `wait navigation` arm already ran.

## [0.4.203] - 2026-06-11

### Fixed

- **`wait selector`/`text`/`idle` survive document navigations.** A page
  redirecting mid-wait used to kill the in-flight poll — headless died with an
  untyped "Execution context was destroyed" infra error (exit 1); browser mode
  happened to survive exactly one navigation via the generic send retry, with
  the full timeout silently reset. Both modes now re-arm the wait against the
  new document with the **remaining** budget (Playwright-validated semantics:
  the condition's intent transfers to the document a redirect lands on), so
  `wait selector` for an element expected only after a redirect just works.
  Headless types the in-flight context teardown (`ContextDestroyedMidFlight`,
  distinct from the never-started `ContextGone` so non-idempotent calls are
  never blindly re-issued); the browser worker loops with deadline accounting
  instead of leaning on the full-budget send retry. A frame removed mid-wait
  still ends as a typed `FrameNotFound`, and a vanished tab as `TabNotFound`.
- **Wait timeouts name the condition.** `wait selector "#results" timed out
  after 10000ms` instead of the bare "wait timed out" — the error is
  self-contained in an agent transcript, in both modes, and a re-armed wait
  reports the full requested budget, never the residual round's.

## [0.4.202] - 2026-06-11

### Fixed

- **`console clear` / `network clear` are sentinel-preserving.** An
  unconditional `= []` CREATED the buffer in a document whose hook was never
  installed — and the read's hook-absent guard keys on the `undefined`
  sentinel, so after `start` → an `eval` policy deny → a navigation (re-arm
  suppressed) → `clear`, a later `read` returned an empty success while the
  monitor was in fact off: exactly the lie the guard exists to prevent. Both
  modes now clear only an existing buffer; an absent one is the same typed
  not-installed error the read gives, and `clear` before `start` joins the
  typed not-active contract (pinned in e2e).

## [0.4.201] - 2026-06-11

### Fixed

- **`cookie delete` deletes every matching scope, reports the count, and a
  missing cookie is `CookieNotFound`.** Deleting an absent cookie reported
  success while removing nothing (the silent empty-success class `cookie get`
  shed in 0.4.136), and same-name cookies coexisting across scopes (a
  `.domain` legacy cookie beside a host-only one, different paths) were only
  partially removed by the bare url+name delete while the command claimed
  completion. Both modes now list first — absent → typed `CookieNotFound`
  (exit 4) — then delete each matching scope precisely and report
  `Deleted N cookie(s)`; the wire carries the count (`deleted`).

- **Browser-mode capture metadata never substitutes the synthesized tab title
  (r99).** The no-DOM fill-in's main-frame branch fell back to `tab.title` —
  for an untitled page Chrome synthesizes one (≈ the URL), papering over the
  honest `""` headless reports. One probe now serves every frame (the main
  frame included), reading `location.href` / `document.title` from the
  document itself; `tab.title` is never consulted.

## [0.4.200] - 2026-06-11

### Changed

- The screenshot-diff threshold's doc comment names the fields that exist
  (`pixels_above_noise` / `percent_above_noise`) and states that the `changed`
  verdict keys on exact inequality, never the threshold — 0.4.199's rename had
  left the old `changed_percent` name in the comment.

## [0.4.199] - 2026-06-11

### Fixed

Three verdicts overturned by an adversarial re-audit of earlier "by design"
rulings:

- **`diff --screenshot`'s `changed` verdict is exact.** Every mitigating field
  (`changed_pixels`, the percent, the red overlay) derived from the same noise
  threshold, so a pair whose every pixel shifted subtly read
  `changed: false, 0/total` — an identity claim, not coarse reporting.
  `changed` now keys on exact pixel inequality (or a size change);
  `changed_pixels` counts exact mismatches, and the threshold remains a
  reporting aid as `pixels_above_noise` / `percent_above_noise` with the
  overlay unchanged.

- **An ambiguous `frame find` predicate fails loud**, completing the
  strict-selector contract for the last selector kind: a predicate true in
  more than one frame silently scoped every later command to whichever
  matched first. Both modes now evaluate all candidates and reject >1 match
  naming the frame URLs; one match switches, zero keeps the existing
  typed-error paths.

- **The browser-mode NM frame-limit error names the remedy.** Export persists
  through an asymmetric (larger) read path, so a big exported session was
  importable only outside `--browser` — and the error didn't say so. It now
  reads "retry without --browser (headless reads the file directly)".

(The fourth re-audited ruling — page-clock monitor stamps — stands: an
install-time clock capture still inherits a pre-install mock, so the
documented best-effort boundary is unchanged.)

## [0.4.198] - 2026-06-11

### Fixed

- **0.4.197's slot-fallback descent actually fires.** A `<slot>` is
  `display:contents` — no box, so `checkVisibility()` is false *by nature*,
  and the general per-child visibility gate skipped the unassigned slot before
  the fallback descent could run (0.4.197's own e2e caught it). The slot
  branch now precedes the gate: assigned → skip (light content the base
  carries), unassigned → descend its fallback unless an author explicitly set
  `display:none`; each fallback child still runs its own visibility check.

## [0.4.197] - 2026-06-11

### Fixed

- **Headless `wait navigation` classifies a tab that closes mid-wait as
  `TabNotFound`, matching browser mode.** The page socket dying took the
  `ConnectionLost` path (exit 3, "infra — retry") even though Chrome itself
  was fine and the truth was tab-gone (exit 4 — recover via `tab`). On a
  `ConnectionLost` inside the navigation wait, the still-alive browser client
  now checks whether the pinned target exists; absent → the typed
  `TabNotFound` browser mode's `tabs.onRemoved` arm reports, while a genuinely
  dead Chrome keeps `ConnectionLost`. Pinned by a concurrent e2e: a second
  process closes the awaited tab mid-wait and the wait must exit 4 naming the
  gone tab.

- **Slot fallback text reaches the text capture and `wait text`.** The
  shadow-text walk skipped every `<slot>` to avoid double-counting assigned
  light content — but an UNASSIGNED slot renders its own fallback children,
  shadow-side text the base `innerText` never sees, so visible fallback prose
  (a "Loading…" placeholder) was invisible to `capture --include text` and
  `wait text` timed out on text that was on screen. Only an assigned slot is
  skipped now; an unassigned one is descended like any shadow node. Pinned by
  e2e: fallback prose must appear, and assigned slotted content still appears
  exactly once.

## [0.4.196] - 2026-06-11

### Fixed

- **A slotted control inherits its flat-tree landmark (and rendered
  visibility).** `flatTreeParent` walked element parents and hopped shadow
  hosts but never followed `assignedSlot` — a light-DOM control slotted into a
  shadow tree read its LIGHT ancestors' landmark (usually none) instead of the
  shadow-side one the accessibility tree actually places it under, and the
  opacity-inheritance visibility walk missed a transparent shadow wrapper
  above the slot for the same reason. The flat-tree parent of a slotted node
  is now its `<slot>`, fixing both consumers at the single helper. Pinned by
  e2e: a slotted button whose only landmark is the shadow `<aside>` must
  report `@aside`.

## [0.4.195] - 2026-06-11

### Fixed

- **Browser-mode `wait navigation` reports a closed pinned tab as
  `TabNotFound`, not a sat-out `Timeout`.** If the tab closed during the wait
  (a `window.close()` in a load handler, the user closing it), the awaited
  navigation could never complete — yet the wait ran its full timeout and then
  claimed "navigation didn't finish" (exit 5), misfiring the agent's
  error-handling branch. A `tabs.onRemoved` arm now ends the wait immediately
  with the typed `TabNotFound` (exit 4 → recover via `tab`).

- **Docs de-drifted against the code** (a maintainer-contract audit): the
  iframe trailer is quoted as actually rendered (`enter: webpilot frame url
  <pattern>` — also correcting the wrong "alignment" 0.4.194 introduced in the
  skill), `CookieNotFound` joins the exit-4 table, the extension rules state
  the agent-level monitor-flag model, and the host's exit-with-Chrome
  lifecycle is documented.

## [0.4.194] - 2026-06-11

### Changed

- **The skill's contract caught up with the code** (a claims-vs-behavior
  audit): the policy-key list now includes `device` and `context_close` (an
  agent building a `default deny` allowlist would have missed them); the
  session section states the storage origin gate (export records the origin,
  import writes storage only on that same origin — cookies import regardless);
  the iframe trailer is quoted exactly as rendered.

## [0.4.193] - 2026-06-11

### Fixed

- **An accessibility capture against a dead frame pin is `FrameNotFound`
  (headless).** The AX path sent `Accessibility.getFullAXTree` with the raw
  pinned frame id, so a pin whose frame had navigated away out-of-band
  surfaced the generic CDP error as `Other` (exit 1) instead of the
  recoverable `FrameNotFound` (exit 4) every other frame-scoped command
  returns. The pin is now validated through the same resolver every bridge
  call uses — browser mode already resolves the live context this way. Pinned
  by a deterministic e2e: switch into the fixture iframe, navigate the top
  page away out-of-band, and the AX capture must exit 4.

- **The NM host exits when Chrome disconnects instead of leaking an orphan
  process per Chrome restart.** Its graceful teardown was unreachable: the NM
  writer is a blocking task that ends only when every channel sender drops,
  and the detached per-connection IPC tasks each hold one — the accept task
  alone kept the await pending forever, so every Chrome exit left a live
  orphan host (dozens accumulated across sessions, verified empirically).
  Chrome's death is the host's end of life: it now exits the process after
  the reader observes EOF. The socket handling is unchanged (a successor
  rebinds the fixed path at bind time), and a CLI mid-request observes the
  close as `ConnectionLost` — exactly "Chrome died mid-command". Verified
  empirically: a host with closed stdin now exits immediately.

## [0.4.192] - 2026-06-11

### Fixed

- **`aria-selected="false"` survives the wire as the tri-state it is.** A
  selectable-but-unselected widget (a tab) is distinct from "not a selectable
  widget", exactly like the `checked`/`expanded` tri-states already on the
  keep-list — the bridge now maps the explicit `"false"` and keeps it through
  the payload cleanup, so the JSON channel distinguishes the two.

- **An untitled page's empty title is reported honestly (browser mode).** The
  text-capture path papered an empty `document.title` over with the prior
  value (`||`), and a `tab new` whose settled document is untitled resurrected
  the transient creation-time title. Both now respect the honest `""` —
  matching headless, which reports `document.title` as-is.

## [0.4.191] - 2026-06-11

### Fixed

- **An empty-`href` anchor keeps its implicit `link` role.** `href=""` is a
  real link to the current page (focusable, ARIA role `link`), but the bridge
  collapsed the empty string with `||`, so the Rust side saw no `href` and
  granted no implicit role — `find --role link` missed such anchors. The wire
  now preserves the empty string (`?? undefined`); only a genuinely absent
  attribute is omitted. Shared bridge, both modes; pinned by a `find --role
  link` e2e against a bare-href fixture anchor.

- **Browser-mode `cookie list` / `delete` reject a malformed URL as
  `InvalidArgument`, like `cookie set` and headless.** Only the set handler
  validated the URL; list and delete let the raw `chrome.cookies` throw read
  as `Other` (exit 1), diverging from the rest of the cookie family and from
  headless CDP's typed rejection (exit 7). One guard now covers all three,
  pinned by browser e2e.

## [0.4.190] - 2026-06-11

### Changed

- **The skill names the window-vs-container scroll split.** `action scroll`
  moves the window scroller; on an app-shell page whose content pane is an
  inner scroll container the window has nothing to scroll (the capture's
  Scroll line shows `0 below`), and `scroll-to N` — which works through inner
  containers natively — is the right primitive. The agent now learns the
  recovery up front instead of reading a no-op scroll as page exhaustion.

## [0.4.189] - 2026-06-11

### Changed

- **The skill documents the dialog auto-answer contract.** Agents now learn up
  front that javascript dialogs never block automation — `alert` dismissed,
  `confirm` → true, `prompt` → its default, identically in both modes — and
  that a flow needing the cancel/false branch must drive the page another way
  (e.g. `eval`).

## [0.4.188] - 2026-06-11

### Fixed

- **A dialog from a frame created mid-action is intercepted too (browser
  mode).** The per-action override covers only the frames that exist when the
  action starts — an iframe a click handler creates, whose script then calls
  `alert()`, still raised a native modal that wedged the pinned tab (every
  later command timing out as `BridgeUnavailable`). The pinned tab's
  `webNavigation.onCommitted` now installs the override into each newly
  committed document — including non-http child documents, exactly where
  handler-spawned dialogs live — while staying scoped to the pinned tab: the
  user's other tabs keep their native dialogs. The override moved to a shared
  helper (`installDialogOverride`), one definition for the per-action and
  per-commit paths. Headless needs nothing: its CDP responder is
  page-session-wide. Pinned by a browser e2e (a handler-spawned iframe alerts
  at +400ms; the next capture must succeed, not time out).

## [0.4.187] - 2026-06-11

### Fixed

- **A page javascript dialog can no longer wedge a headless session, and both
  modes share accept-with-default dialog semantics.** With a CDP client
  holding `Page` enabled, Chrome stops its headless auto-dismiss and *waits*
  for `Page.handleJavaScriptDialog` — WebPilot never answered, so a bare
  `alert()` blocked the renderer and every later command timed out; and where
  auto-dismiss did apply it *cancels*, so `if (confirm(...))` took the
  opposite branch headless vs browser. Every page connection now spawns a
  dialog responder answering accept-with-default (confirm → true, prompt →
  its default), the exact contract the browser-mode override implements —
  page flows branching on a dialog behave identically in both modes, pinned
  by parity e2e in both suites.

- **The browser-mode dialog override covers every frame.** It injected only
  into the main and active frames, so a dialog fired from a third frame (a
  third-party iframe's handler) still raised a native modal that wedged the
  user's tab. The override now injects `allFrames`.

- **`prompt(msg, null)` returns `"null"`, matching WebIDL.** A `DOMString`
  parameter stringifies an explicit `null` (like `alert(null)`); only a
  missing argument takes the parameter default `""`.

## [0.4.186] - 2026-06-11

### Fixed

- **The browser-mode `prompt` interception returns the page's default
  faithfully.** The dialog override (which keeps a page dialog from wedging
  the user's Chrome during an action) returned `def || ""`, coercing a falsy
  default — `prompt(msg, 0)`, `prompt(msg, false)` — to the empty string,
  while a real accepted prompt returns the default *stringified* (`"0"`).
  Page logic branching on that return value misfired. It now returns
  `def == null ? "" : String(def)`, matching the spec's behavior.

## [0.4.185] - 2026-06-11

### Fixed

- **`capture --bounds` coordinates reach the text channel.** The requested
  `bounds` rendered only into JSON; the terminal and MCP text render (the
  same `to_text`) silently dropped the x/y/w/h the agent explicitly asked
  for. An element line now carries `bounds=(x,y,w,h)` exactly when `--bounds`
  was requested — no default noise, since the field is only populated on
  request.

## [0.4.184] - 2026-06-11

### Fixed

- **A pinned tab that closes mid-operation is `TabNotFound`, not
  `BridgeUnavailable` (browser mode)** — completing the 0.4.183 typed split
  symmetrically: callers resolve the tab before injecting the bridge, but the
  tab can close in the same async gap the subframe case had, and the agent got
  exit 3 ("infra, retry" — a retry loop) instead of exit 4 ("gone, recover").
  `ensureBridge`'s failure path now probes the tab first (a gone tab makes any
  frame answer moot), then the frame, then falls to `BridgeUnavailable` only
  for a page that exists but will not answer.

- **`ElementNotFound` on an empty page says so instead of rendering the
  nonsensical range `[1]-[0]`.** With zero interactive elements captured, the
  guidance now reads "the page has no interactive elements" (both the Rust
  `Display` both modes re-render from, and the bridge's advisory message).

## [0.4.183] - 2026-06-11

### Fixed

- **A subframe that vanishes mid-operation is `FrameNotFound`, not
  `BridgeUnavailable` (browser mode).** Callers probe the frame before
  injecting the bridge, but an iframe can navigate away in the async gap —
  every inject/ping then failed for a reason that is not infrastructure, yet
  the agent got exit 3 ("connection lost, retry infra") instead of exit 4
  ("stale frame, re-capture"). `ensureBridge` now re-probes the frame at its
  failure point and throws the typed `FrameNotFound` when it is gone,
  reserving `BridgeUnavailable` for a frame that exists but will not answer —
  the same split headless `bridge_context_id` makes. One choke point, so
  every bridge-routed command (capture, wait, dom, action, session storage)
  inherits the correct semantics. The `sendToContent` retry path completes
  the contract: a TYPED re-inject failure (the root cause) now outranks the
  untyped first send error, which would have collapsed it to `Other`.

## [0.4.182] - 2026-06-11

### Fixed

- **The occlusion composed-walk follows slot assignment, so a shadow control
  is never "occluded by its own slotted label".** A shadow button whose
  visible content is a slotted light element (`<my-button><span>Go</span>
  </my-button>` — the standard design-system pattern) had its sampled hits
  land on the light span, which the host-hopping walk alone could not relate
  back to the shadow button: the control read `occluded:true` under
  `--occlusion`. The composed walk now hops through `assignedSlot` (a slotted
  ancestor continues at the slot that renders it) as well as hosts. Shared
  bridge, both modes; the fixture pins a slot-labelled shadow button reading
  not-occluded.

- **`fetch`'s human output no longer fabricates `HTTP 0` when the status is
  absent.** `status.unwrap_or(0)` invented a value that reads as the XHR
  network-error convention; an absent status now renders "HTTP status
  unknown" (the JSON channel already carried the honest `status: null`).

## [0.4.181] - 2026-06-11

### Fixed

- **`--occlusion` no longer mislabels every shadow-root control as occluded.**
  `document.elementFromPoint` retargets a shadow-interior hit to its HOST, and
  tree-scoped `contains()` cannot relate the host to the element inside its
  shadow — so an uncovered component control read `occluded:true` at every
  sampled point. The hit-test now descends through open shadow roots (each
  root's own `elementFromPoint`), and containment walks the composed tree
  (host-hopping), which also keeps a closed-shadow control honest via its
  host. A genuinely covered shadow element (a sibling overlay in the same
  root) still reads occluded. Shared bridge, both modes.

- **A budget-clipped shadow traversal refuses a `dom set-*` instead of
  trusting a partial uniqueness check.** Past the shadow-host budget the deep
  walker stops early; "unique so far" could write the wrong element while an
  unseen twin sits beyond the cap. The write now fails typed naming the
  budget — the same truncation honesty the capture's `shadow_truncated`
  already has — while reads keep their deterministic light-first first match.

## [0.4.180] - 2026-06-11

### Fixed

- **`dom get-*` / `set-*` selectors pierce open shadow roots, and the set
  uniqueness check counts shadow matches.** The selector surface was light-DOM
  only while the element index, `wait selector`, and the text capture all
  pierce — so a web component's field was unreadable/unwritable without eval,
  and worse, the 0.4.178 uniqueness check could see one light-DOM match where
  a shadow twin also existed, judge it "unique", and silently write the wrong
  element. Both helpers now run the same budget-bounded deep traversal the
  capture uses (light DOM first, then each open shadow root in document
  order): get reads the first deep match, set requires a unique match across
  shadow boundaries — a light element and a shadow twin are two candidates,
  never a silent light-only write. Shared bridge, both modes.

## [0.4.179] - 2026-06-11

### Fixed

- **The `[offscreen]` marker works again: `in_viewport:false` survives the
  wire.** The bridge's payload cleanup strips `false` fields that mean mere
  property absence, but `in_viewport` is a genuine boolean whose *false* is
  the signal — a below-fold element — exactly like the `checked`/`expanded`
  tri-states already on the keep-list. Stripping it left the Rust side reading
  `None`, so no element ever rendered `[offscreen]` and the agent had no
  signal that a control sits outside the viewport (the annotation overlay's
  in-viewport filter kept working only because `true` survived). One keep-list
  entry; shared bridge, both modes. The e2e fixture now carries a below-fold
  control pinning the flag end-to-end.

## [0.4.178] - 2026-06-11

### Fixed

- **`action type` respects `maxlength` instead of silently setting a value the
  UI can never produce.** A programmatic value set sails past the cap a real
  keyboard stops at, so typing 4 characters into `maxlength="3"` reported
  success while the field held over-cap content — invalid-by-construction form
  state. It is now a typed `InvalidArgument` naming the cap and the requested
  length, before any mutation. Enforced only where the browser itself enforces
  maxlength (textarea + the textual input types — `type=number` ignores the
  attribute, so rejecting there would invent a constraint the page doesn't
  have). Shared bridge, both modes.

- **An ambiguous `dom set-text` / `set-html` / `set-attr` fails loud instead of
  mutating whichever element matched first.** A selector matching 100 elements
  wrote one of them and returned a bare success — silent mutation of an
  unintended element with no signal the others existed. The write paths now
  require a unique match (typed `InvalidArgument` naming the count, pointing at
  `#id` / `:nth-of-type`), completing the strict-selector contract for writes
  (`frame url` 0.4.152, `tab find` 0.4.169, `find --click` 0.4.176).
  `dom get-*` keeps standard first-match read semantics — a read is
  recoverable and its value identifies what was read. Shared bridge, both
  modes.

- **The session-storage origin gate refuses opaque origins instead of
  string-matching them.** An opaque origin (a `file://` or sandboxed page)
  serializes as `"null"` — shared by every such page while being same-origin
  with nothing, even itself — so the 0.4.177 equality check would have written
  storage across two genuinely unrelated pages that merely share the
  serialization. Either side being opaque is now a typed `InvalidArgument`
  naming the cause.

## [0.4.177] - 2026-06-11

### Fixed

- **`session import` refuses to write another origin's storage into the current
  page.** The export carried no origin, so importing a session taken on
  `https://A.com` while sitting on `https://B.com` wrote A's
  localStorage/sessionStorage into B and reported "Session imported" — silent
  state corruption of the wrong origin, with the right one getting nothing.
  The export now records `origin` (read by the bridge from the same frame the
  storage came from, both modes), and the import — enforced in the shared
  bridge at the moment it is about to write — rejects a mismatch as a typed
  `InvalidArgument` naming both origins and the remediation (navigate there
  first), before anything lands. A matching origin imports unchanged; cookies
  are unaffected either way (each carries its own domain and applies through
  the cookie API); a hand-written file may omit `origin` to skip the check —
  the same explicit opt-out the `version` field already has.

## [0.4.176] - 2026-06-11

### Fixed

- **An ambiguous `find --click` / `--fill` fails loud instead of silently
  acting on the first match**, completing the strict-selector contract
  (`frame url` 0.4.152, `tab find` 0.4.169) for chained element actions: a
  filter matching more than one element is a typed `InvalidArgument` naming
  the count and the matches (line-safed, capped at five), so the agent narrows
  the filter or acts by index — never a side-effecting guess that may have
  submitted the wrong form or filled the wrong field with no signal the other
  matches existed. A unique match still chains; a bare `find` (no action)
  still lists every match — that is its job. The handler is mode-generic, so
  both modes get the contract.

## [0.4.175] - 2026-06-11

### Fixed

- **A key-press chord can no longer leave a modifier latched after a mid-chord
  failure.** The modifier keys went down (rawKeyDown) before the main key, but
  any send failing after that — a transient CDP timeout on a still-live
  connection — returned before the releases ran, leaving Control/Shift/Alt/Meta
  held in the renderer: every subsequent click became a ctrl-click. Both modes
  now record what actually went down and always release it in reverse before
  any error propagates — the chord's own error first; a release failure
  surfaces when the chord succeeded (a stuck key must never be silent), and one
  failed release still tries the rest.

- **The browser-mode armed-monitor intent is agent-level, matching headless —
  closing the pinned tab no longer silently disarms it.** The armed state was a
  per-tab set, pruned when its tab closed: after `network start` → the pinned
  tab closes → re-pin, `network read` failed with "monitoring is not active" —
  a lie about the agent's own state, which it never stopped. The intent is now
  one flag per kind (exactly the headless persisted-flag model), re-armed on
  the pinned tab at every pin move and navigation settle, so it survives tab
  churn; the per-tab carry/prune bookkeeping is gone. The `load`-time re-arm
  backstop is scoped to the pinned tab, so the agent's monitor never injects
  MAIN-world hooks into an unrelated tab the user is browsing.

- `context close` no longer carries an unreachable handler-level duplicate of
  the name-XOR-`--all` rule the parser already enforces.

## [0.4.174] - 2026-06-11

### Fixed

- **Modifier chords press the modifier keys for real.** `key-press --shift/--ctrl/…`
  set only the CDP `modifiers` bitmask on the main key's events; the modifier
  key itself was never pressed, so renderer editing commands keyed off real
  modifier state did nothing — empirically, `shift+ArrowLeft` left the selection
  untouched. Each held modifier now goes down (`rawKeyDown`, accumulating the
  mask like a physical keyboard) before the main key and comes up in reverse
  after it: `shift+Arrow` extends the selection (verified live and pinned by
  e2e), and page-level shortcut listeners see the modifier keys themselves.
  Both modes. Note: browser-LEVEL shortcuts (ctrl/cmd+A select-all, copy/paste)
  have no UI layer headless and still do nothing — now documented in the skill
  with the working alternatives (`type --clear`, eval `el.select()`).
- **`context close` argument errors are parser-level.** A bare `context close`
  (no name, no `--all`) launched Chrome on its way to the handler's rejection;
  `NAME --all` was rejected in the handler too. clap now declares the contract
  (`required_unless_present`, `conflicts_with`), so both are refused before any
  transport opens.

## [0.4.173] - 2026-06-11

### Fixed

- **A denied `context close` / `device` command no longer launches Chrome on
  its way to the refusal.** These directly-gated headless-only commands
  enforced their policy key inside the handler — after `LocalTransport::open`
  had already started a session — so under `policy default deny` the refused
  command still left a live Chrome behind. Their keys are static, so the
  verdict is now decided before the transport opens (the handler's enforce
  stays as the sink backstop; both call the same `enforce_key`, so they cannot
  disagree). Transport-routed commands keep sink enforcement unchanged —
  pre-resolving their keys would duplicate `Command::policy_key`. A
  Chrome-free integration test pins it: under default-deny both commands exit
  `PolicyDenied` and `status` confirms no session was started.

## [0.4.172] - 2026-06-11

### Fixed

- **Closing the last tab no longer wedges the headless session.** With zero
  pages in scope the attach failed `NoPage` — and since every command (including
  the `tab new` and `navigate` that would fix it) needs that attach first, the
  session was permanently unrecoverable, with the NoPage guidance pointing at a
  command that failed the same way (empirically confirmed). The attach now
  creates a blank page to bind to — the state a fresh browser starts in — so
  the recovery commands work; the dead-pin signal still fires its one loud
  `TabNotFound` on the first page action, so nothing acts on the blank
  silently. An e2e closes every tab and asserts the loud signal then the
  recovery.
- **The vanished-pin signal is consumed by its one loud failure.** A long-lived
  transport (the MCP server) kept the flag forever, so after a dead pin every
  page tool — including `browser_navigate` — repeated `TabNotFound` with no way
  to clear it. One announced failure, then the fallback is the active page —
  matching what separate CLI invocations already did across their process
  boundary.
- `Target.createTarget` calls consolidate into one `CdpClient::create_target`
  (optional browser context), shared by `tab new`, the zero-page attach, and
  the context store.

## [0.4.171] - 2026-06-11

### Fixed

- **A `draggable="true"` element is indexed.** The `drag` action addresses
  elements by snapshot index, but a declared drag source carrying no other
  marker/semantic tag was invisible to the capture — the action existed with no
  way to name its target. The explicit attribute joins the interaction-marker
  set (the selector matches only the literal attribute, never the implicit
  draggable default of images/links). Empirically verified end-to-end: dragging
  a draggable source drives the real HTML5 DnD session (dragstart → drag →
  dragend; mouseup is correctly absent per spec), and dragging a plain element
  delivers the full mouse gesture (mousedown → moves → mouseup on the target).
- **The drag gesture carries the `buttons` bitmask on every event** (pressed/
  moved: 1, released: 0), aligning with how CDP tracks held buttons across a
  gesture (and with Puppeteer/Playwright). Both modes.

## [0.4.170] - 2026-06-11

### Fixed

- **Reading a monitor whose re-arm was suppressed is a typed error, not an
  empty success.** After `console start` → `policy set eval deny` → a
  navigation, the armed flag survives but the new document carries no hook (the
  deny stops monitor injection, by design) — and `console read` /
  `network read` returned an empty list the agent would read as "the page was
  quiet". The read now distinguishes a missing hook (`undefined`) from an empty
  buffer and fails with a typed `InvalidArgument` naming the suppression and
  the recovery (`policy list`, then `console start`). Both modes, identical
  messages; both e2e suites assert the explicit signal where they previously
  asserted only the marker's absence.

## [0.4.169] - 2026-06-11

### Fixed

- **An ambiguous `tab find --url` fails loud instead of silently switching to
  the first match**, completing the strict-selector contract `frame url`
  adopted in 0.4.152: a pattern matching more than one tab is a typed
  `InvalidArgument` naming the count and the matching URLs, so the agent
  refines the pattern or picks a tab id directly. A unique match still
  switches; zero matches stay `TabNotFound`. Mode-generic (the handler runs the
  shared `TabList`→`TabSwitch` path); an e2e pins both the unique-switch and
  the ambiguity rejection.

## [0.4.168] - 2026-06-11

### Fixed

- **`cookie list`/`set`/`delete` survive a vanished tab pin**, completing the
  browser-global classification 0.4.167 started for cookie-only session
  imports: the jar is shared, the commands take their scope from the URL
  argument (the Network calls ride whatever target session is attached — same
  jar either way), and browser mode never resolved a tab for them at all. The
  first command after the pinned tab closes can now be a cookie command and it
  proceeds against the shared jar instead of failing `TabNotFound`. An e2e pins
  it.

## [0.4.167] - 2026-06-11

### Fixed

- **A cookie-only `session import` succeeds on a vanished tab pin.** The
  headless dead-pin guard classified `SessionImport` as page-bound
  unconditionally, so importing a session that carries only cookies — which are
  browser-global and land in the shared jar through any target's session —
  failed `TabNotFound` after the active tab closed. The classification is now
  payload-based: the import is page-bound exactly when the payload carries
  storage (the half that writes into the active page's origin), read through a
  predicate shared with the import itself so the two can never disagree — and
  matching the browser worker's `hasStorage` gate, which already behaved this
  way. An e2e imports a cookie-only session over a dead pin and asserts both
  the success and the cookie landing.
- **An unknown MCP tool name is a protocol-level `-32602` error, per spec, not
  an `isError` tool result.** A typo'd tool name previously opened the
  transport (launching Chrome for nothing) and came back as a success-shaped
  response that spec-conformant clients read as a succeeded call. The name is
  now checked against the same `tool_specs` that `tools/list` serves — one
  source, no drift — before any transport opens.

## [0.4.166] - 2026-06-11

### Fixed

- **`status` renders the context label through `line_safe`**, closing the last
  raw interpolation of a context name (list and close were fixed in
  0.4.158/0.4.159): a name carrying a newline can no longer forge an extra
  status line such as a fake `URL:` field. The page-controlled tab title/URL
  fields were already wrapped.

## [0.4.165] - 2026-06-11

### Fixed

- **A combined `dom`+`screenshot` capture while an iframe is active keeps the
  frame-scoped DOM instead of failing whole.** 0.4.160's screenshot guard
  refused the entire capture up front, which threw away the valid frame-scoped
  DOM/text/AX the agent also asked for. The semantics are now split by what the
  request actually wants: a screenshot-**only** capture still refuses loud with
  the typed `InvalidArgument` (success with no artifact would be a lie), while
  a screenshot riding along frame-scoped outputs degrades through the standing
  `screenshot_error` channel — no image is produced, the refusal and its
  switch-back guidance ride in the error field, and the valid outputs return.
  Both modes; both e2e suites pin both cases.
- **`_unfencedTop` joins `_blank` as a reserved target keyword** in the bridge's
  named-target resolution: a frame named after either can no longer capture a
  special-target click into a current-frame navigation hint.

## [0.4.164] - 2026-06-11

### Fixed

- **A text/AX-only capture no longer claims "entire page visible".** The shell
  snapshot those captures ride carried zeroed scroll metrics, and the renderer
  read all-zeroes as a page that fits the viewport — so `capture --include
  text` on a long, scrolled page told the agent there was nothing more to
  scroll. Scroll metrics are now `Option`al: a capture that never measured
  layout carries `None` and the rendered text omits the Scroll line entirely
  (only a DOM pass, which measures, may speak about scroll). Both modes — the
  browser shell drops the zeroed struct, headless's `empty_snapshot` is `None`,
  and a unit test pins the omission.

## [0.4.163] - 2026-06-11

### Fixed

- **`_blank` can never be captured by the frame-name lookup.** The named-target
  resolution added in 0.4.162 checked only the three frame keywords before
  consulting frame names, so a page that set `window.name = "_blank"` could
  trick a `target="_blank"` click into being classified as a current-frame
  navigation. `_blank` is reserved per spec — always a new context — and is now
  short-circuited before any name matching, in both the link and form paths.
- **Browser `frame switch` on a tab that died mid-command is a typed
  `TabNotFound`, not `FrameNotFound`.** When the pinned tab closed between
  resolving it and reading its frames, `getAllFrames`' null/rejection collapsed
  to an empty candidate list and surfaced as `FrameNotFound` — which reads as
  "bad selector, retry the frame search" when the real recovery is re-pinning a
  live tab. The same null-guard `frame list` already uses now applies, so the
  dead tab surfaces as the `TabNotFound` the agent re-pins from.
- The parity test's enum anchor now includes the opening brace, so `Action` can
  never prefix-match `ActionKind` regardless of declaration order.

## [0.4.162] - 2026-06-11

### Fixed

- **A link or form targeting an existing frame's NAME now settles like the
  keyword it resolves to, instead of being classified as a popup.** HTML
  resolves a non-keyword `target`/`formtarget` name against the existing
  browsing contexts — most commonly the frame itself (a named iframe whose own
  links target its own name, the classic frameset-era pattern). The bridge
  treated every non-keyword name as a popup, so such a click reported success
  with no navigation hint, skipped the settle, and the following capture showed
  the pre-click document. The bridge now maps a name matching `window.name` to
  `_self`, a same-origin parent's name to `_parent`, and a same-origin top's
  name to `_top` (case-sensitive, per spec); an unreadable cross-origin
  ancestor or an unmatched name stays a popup, the pre-existing conservative
  behaviour. Shared bridge; an e2e clicks a `target="innerfr"` link inside the
  `name="innerfr"` iframe and asserts the auto-capture lands on the new frame
  document.

## [0.4.161] - 2026-06-11

### Changed

- **The browser-parity build gate now checks both directions and covers action
  kinds.** `browser_parity.rs` asserted only that every wire `Command` variant
  has a service-worker router case; a dead router arm (a removed command's
  forgotten JS case) passed silently, and the `Action` *kinds* under the single
  `Action` command had no static check at all — a new action added in Rust
  without its `bridge.js` `executeAction` case compiled, passed headless tests,
  and failed at runtime in whichever mode hit it first. The test now requires
  set equality for the Command/router pair and adds the action-level twin:
  `Action` kinds (snake_case wire tags) must equal the bridge's `case` set,
  which handles every kind explicitly (page actions run; CDP-native kinds are
  explicit mis-route rejections). A new action now fails the build until its
  bridge arm exists — the same guarantee the command check already gave.

## [0.4.160] - 2026-06-10

### Fixed

- **`capture --include screenshot` while an iframe is active fails loud instead
  of shooting the top page under an iframe-labelled header.** `Page.captureScreenshot`
  is a top-level operation (CDP has no frame-scoped capture), so a screenshot
  taken after `frame switch` was TOP-page pixels while the capture header and
  DOM described the iframe — the wrong image with correct-looking metadata. It
  is now refused with the same typed `InvalidArgument` that `--annotate` and
  `--include pdf` already use for exactly this reason, completing that guard
  family. Both modes; both e2e suites pin the rejection.

## [0.4.159] - 2026-06-10

### Fixed

- **`context close` renders the closed name through `line_safe` too**, finishing
  the convention pass that `context list` started: every agent-facing
  interpolation of a context name now collapses control characters. The
  remaining renderers were swept and are clean — frame list already wraps its
  page-derived URL/name, `find`'s filter echo quotes user input, and `diff`
  deliberately preserves file content verbatim (that is a diff's purpose).

## [0.4.158] - 2026-06-10

### Fixed

- **`context list` renders names and cwds through `line_safe`, like every other
  agent-facing field.** A context name or working directory carrying a newline
  could forge an extra row in the list output (the filesystem side was already
  safe — names are hashed into fixed paths). The render now collapses control
  characters, closing the one renderer that bypassed the codebase-wide
  `line_safe` convention.

## [0.4.157] - 2026-06-10

### Fixed

- **`diff --dom --screenshot` is rejected at parse instead of silently picking
  DOM mode.** The two mode flags were independent booleans with `--dom` taking
  precedence, so asking for both ran a DOM diff (against PNG bytes, a confusing
  JSON decode error) with no signal that the flags conflict. They are now
  declared mutually exclusive (`conflicts_with`, the same convention `find`
  uses), so clap rejects the combination up front naming both flags.

## [0.4.156] - 2026-06-10

### Fixed

- **`session import` applies storage before cookies, so a storage failure never
  leaves cookies committed behind it.** The import set every cookie first, then
  wrote `localStorage`/`sessionStorage` — so a write the page rejected (a
  `localStorage` quota overflow) failed *after* the auth cookies were already
  applied, leaving the agent an authenticated session sitting on inconsistent
  app state, subtly-wrong page behaviour it could not see. The import already
  resolved the storage frame up front to avoid exactly this for a vanished
  frame; the bulk write itself now runs there too. Storage is applied first and
  bails on any rejection before a single cookie is set, so the same failure
  leaves the page merely logged-out, not authenticated-but-inconsistent. A
  successful import lands both halves regardless of order. Both modes.

## [0.4.155] - 2026-06-10

### Fixed

- **`wait selector`'s 100 ms poll also pierces open shadow roots now.** The
  previous release made the initial check and the MutationObserver callback
  shadow-aware but left the poll on a bare `document.querySelector`, so a
  selector whose target appeared inside an open shadow root *after* the wait
  began still timed out — the light-tree observer can't see a shadow mutation,
  and the poll was the only path that could catch it. The poll now uses the same
  `matchesDeep` walk, completing the shadow-piercing `wait selector`. A headless
  e2e schedules a button into an open shadow root after the wait starts and
  asserts the wait is satisfied (it would time out under the previous poll).

## [0.4.154] - 2026-06-10

### Fixed

- **`capture --annotate` index labels stay on-screen at the viewport edges.**
  Each box's index label was pinned at a fixed `top:-16px; left:-2px` offset, so
  an element flush against the viewport top (a header or top nav — very common)
  rendered its number above the visible area and lost it, leaving a box the
  agent couldn't map back to an index. The label position is now clamped per
  axis: it flips down into the box at the top edge, in from the left, and back
  left when it would overflow the right (the index width is estimated from the
  monospace glyph advance plus padding). Shared bridge path; both modes.

## [0.4.153] - 2026-06-10

### Fixed

- **`wait selector` now pierces open shadow roots, like `capture`, `find`, and
  `wait text`.** The selector poll used a plain `document.querySelector`, which
  stops at the shadow boundary — so waiting for an element that lives inside a
  web component's open shadow root timed out even though `capture` already
  indexed it (a guaranteed timeout for `capture` → `wait selector
  <shadow-hosted-element>`). It now falls back to the same shadow-piercing walk
  the capture uses (`queryAllDeepMulti`) when the light-DOM query misses, so the
  light-DOM common case still pays nothing extra. The MutationObserver stays on
  the light tree, with the 100 ms poll covering shadow mutations — the same
  belt-and-suspenders `wait text` uses. One shared bridge path for both modes.

## [0.4.152] - 2026-06-10

### Fixed

- **An ambiguous `frame switch`/`frame url` selector now fails loud instead of
  silently picking the first match.** A `name` or `url` pattern that matched more
  than one frame switched into whichever came first in document order, so every
  later command was silently scoped to a frame the agent may not have meant. A
  pattern selector that matches multiple frames is now a typed `InvalidArgument`
  that names the count and lists the matching URLs, so the agent refines the
  pattern or reaches for a `frame predicate` (the precise escape hatch, which
  stays first-match by design — it is what the error points to). Both modes
  share the message; a `/twoframes` fixture (two iframes with the same URL) pins
  it in the headless e2e.

## [0.4.151] - 2026-06-10

### Fixed

- **`console read` and `network read` text rows now lead with each entry's
  timestamp.** The entries carried a millisecond timestamp (the JSON already
  exposed it), but the human/MCP rows showed only level+message and
  type/method/url/status/duration — so an agent reading via text could neither
  correlate entries to wall-clock events nor, more importantly, learn the value
  to pass to `--since` for an incremental read (the feature was effectively
  unusable from the text surface). Each row now leads with `[<ms>]`. Rendering
  moves to unit-tested `console_row` / `network_row` helpers; the JSON is
  unchanged.

## [0.4.150] - 2026-06-10

### Fixed

- **`type` into a typed input that rejects the value now fails loud instead of
  reporting success over an empty field.** A `<input type=number>` (and
  `date`/`time`/`month`/`week`/`datetime-local`) silently sanitizes a value it
  can't parse to the empty string — typing `abc` into a number field leaves it
  blank — but the bridge still fired `input`/`change` and returned success, so
  the agent believed the value landed when the field was empty. After setting
  the value it now checks for that rejection (a non-empty target the control
  blanked) and returns a typed `InvalidArgument`. A control that merely
  normalizes a valid value (`3.0` → `3`, keeping a non-empty value) is left
  alone, so legitimate input still succeeds. Shared bridge path; one fix for
  both modes.

## [0.4.149] - 2026-06-10

### Fixed

- **`type` into a `contenteditable` now appends at the end instead of inserting
  at a stale caret.** The bridge focused the element and called
  `insertText` straight away, but after a programmatic `focus()` the caret sits
  at a stale or start position — so typing into a contenteditable that already
  held text prepended or spliced into the middle rather than extending it (the
  default `type`, without `--clear`, is an append). An `<input>`/`<textarea>`
  appended correctly because that path concatenates onto the existing value; the
  contenteditable path now matches it by collapsing the selection to the end of
  the element's contents before inserting. The `--clear` path (select-all +
  delete) is unchanged. A headless e2e types into a contenteditable seeded with
  "hello" and asserts the result is "hellomore", not "morehello".

## [0.4.148] - 2026-06-10

### Fixed

- **`cookie list` now renders every scope and security attribute, not just
  `secure`/`httpOnly`.** The human/MCP row carried the name, value, domain, and
  those two flags but silently dropped `path`, `sameSite`, the expiry, and the
  host-only flag — all of which `CookieInfo` already held and the JSON already
  exposed. An agent reading cookies as text (an MCP tool result, or the
  terminal) could not see a cookie's path scope, its cross-site mode, whether it
  was host-only, or whether it was a session or persistent cookie. The row now
  shows the domain+path scope (`[example.com/admin]`) and the full flag set
  (`secure,httpOnly,hostOnly,sameSite=strict,expires=<unix>` / `session`),
  omitting only an unspecified `sameSite`. Rendering is extracted to a
  unit-tested `cookie_row` helper.

## [0.4.147] - 2026-06-10

### Fixed

- **`action select` on a `<select multiple>` now adds to the selection instead
  of replacing it.** The bridge set the choice with `el.value = ...`, which on a
  multi-select deselects every other chosen option — so an agent calling
  `select` twice silently clobbered the first choice on the second call and
  could never build a multi-option selection, while each call still reported
  success. It now sets the matched option's `selected` flag on a multi-select
  (additive, leaving other choices intact) and keeps `el.value =` for a
  single-select (where only one option can be chosen). The existing
  option-exists / disabled / hidden guards and the `input`+`change` dispatch are
  unchanged. Shared by both modes (one bridge path).

## [0.4.146] - 2026-06-10

### Fixed

- **A `<select multiple>` now renders every selected option, not just the
  first.** The DOM text showed `selected="<first>"` via a `.find()` that
  short-circuited on the first chosen option, so an agent inspecting a
  multi-select saw incomplete form state and could not tell which values were
  actually selected — even though the bridge already captured every option's
  `selected` flag. The renderer now collects all selected options
  (`selected="B, C"`); a single-select renders its one value unchanged and an
  empty selection renders empty. Shared by both modes (one rendering path).

## [0.4.145] - 2026-06-10

### Fixed

- **`fetch` no longer hands back a binary response as mojibake under a success
  status.** The body was decoded with a non-fatal `TextDecoder`, so a binary or
  otherwise non-UTF8 response (an image, a PDF, an `application/octet-stream`
  endpoint) came back as replacement-character garbage with `status: 200` — the
  agent had no way to tell corrupt text from the real body. It now decodes
  strictly (`{ fatal: true }`); a body that is not valid UTF-8 fails loud with
  its byte count ("response body is not valid UTF-8 (N bytes); fetch returns
  text, not binary"), mirroring the existing oversize guard. Valid text bodies
  (JSON/HTML/text) are unaffected. Both modes share the identical typed error,
  and a `/binary` fixture route pins it in the headless and browser e2e suites.

## [0.4.144] - 2026-06-10

### Fixed

- **The headless CDP heartbeat no longer declares a live-but-busy connection
  dead.** It probed with `Browser.getVersion` and counted a miss whenever its
  own pong did not return within the timeout; three consecutive misses tore the
  whole connection down (every in-flight and subsequent command failing with
  `ConnectionLost`). But the shared reader processes one message at a time, so a
  sustained burst of events — or a large response queued ahead of the pong —
  could delay three beats in a row on a perfectly healthy socket. The heartbeat
  now keys liveness on **socket activity**, not just its own pong: the reader
  counts every frame it pulls off the socket, and a missed beat only counts
  toward death when *no* frame at all arrived during the wait (genuine silence).
  Traffic still flowing means alive — head-of-line blocking, not death. A truly
  half-open socket (no frames) is still detected after the same bound, and a
  clean close/read-error is still caught by the reader as before.

## [0.4.143] - 2026-06-10

### Fixed

- **A CDP event-buffer overflow during a headless wait no longer masquerades as
  `ConnectionLost`.** When the event broadcast overflowed mid-wait (a burst
  larger than `cdp.event_buffer`, default 256), the wait returned a typed
  `ConnectionLost` (exit 3) even though the socket was still alive — the
  alive-flag check already covers a real drop. An agent that branches on the
  exit code would tear down and re-attach a live session when the right recovery
  is simply to retry the operation. It is now a `Timeout` (exit 5) whose
  free-form `kind` carries the loss ("event buffer overflowed; events were
  dropped, so the wait is inconclusive — retry, or raise `cdp.event_buffer`"),
  so the deadline stays honest (the awaited event may have fired and been
  discarded — never a confident "it never happened") without mislabelling a live
  connection as lost. `do_wait navigation` preserves the typed overflow `Timeout`
  as-is. Browser mode is unaffected (its navigation settle rides `chrome.*`
  listeners, not a bounded broadcast channel).

## [0.4.142] - 2026-06-10

### Fixed

- **A malformed `config.toml` no longer blocks the local commands that read no
  settings.** `settings::init()` ran unconditionally at CLI startup, so a typo
  in `[timeouts]` aborted `policy`, `setup`, `uninstall`, `diff`, and `self`
  before their handlers ran — even though none of them touch settings. That
  held the recovery and security surfaces hostage to an unrelated config error:
  you could not `policy default deny` to lock the tool down, nor `setup` /
  `uninstall` to repair the install, when `config.toml` was the very thing that
  was broken. Validation now runs after the local-command early-return, so the
  settings-backed paths (transport / headless / browser / MCP) still fail
  loudly with a clear `InvalidArgument`, while the settings-free commands stay
  usable. The host keeps its own up-front validation.

## [0.4.141] - 2026-06-10

### Fixed

- **Headless `action back` / `forward` now settles on the *main* frame's
  navigation, not any frame's.** The traversal waited for a bare
  `Page.frameNavigated` of any frame; a subframe that reloaded during the
  settle window (an ad iframe, a meta-refresh, a JS-driven iframe nav) could
  end the wait early, and the readyState probe that follows — finding the
  *old* main document still `complete` — returned at once. A following capture
  then showed the page we navigated away from while the command reported
  success. The wait is now filtered to the top frame (no `parentId`), matching
  the click-settle path and browser mode, which already filter for this exact
  reason. A single `main_frame_navigated` helper is now the one definition of
  "did the main frame navigate?", shared by both settle paths, and
  `CdpClient::wait_for_event_matching` adds the predicate-based wait the filter
  needs.

## [0.4.140] - 2026-06-10

### Fixed

- **Browser `frame list` surfaces a closed pinned tab as `TabNotFound` instead of
  an empty list or a raw TypeError.** If the pinned tab closed between resolving
  it and reading its frames, `getAllFrames` resolved null (or rejected): the old
  `.catch(() => [])` turned a rejection into a successful empty list (an agent
  reads "this page has no iframes"), and a null-resolve hit `null.map(...)`,
  throwing a raw `TypeError` surfaced as a generic error. It now returns the typed
  `TabNotFound` the agent recovers from — matching headless `do_frame_list`, which
  propagates a `Page.getFrameTree` failure, and the `frameVanishedError` pattern
  used elsewhere. (`frame list` is the documented recovery path, so a clean typed
  error there matters.)

### Fixed

- **`console read` / `network read` before the corresponding `start` now fail
  with a typed not-active error instead of a misleading empty success.** Reading
  a monitor that was never armed returned `{entries: [], truncated: false}` (exit
  0) — indistinguishable from "the page logged nothing / made no requests" — so
  an agent that forgot to `console start` (or read on a fresh tab) would
  confidently conclude there was no output when the monitor simply wasn't
  installed. Both reads now check the armed flag (headless `console_monitoring` /
  `network_monitoring`, browser `monitoringState`, both restored across
  processes/SW restarts before the read) and return `InvalidArgument` (exit 7)
  with "run `webpilot console start` first" when inactive. `clear` stays a no-op.
  Both modes.

## [0.4.138] - 2026-06-10

### Fixed

- **`wait --until text` now collapses whitespace, completing its alignment with
  `find --text`.** After the case/shadow fix, one normalization gap remained:
  `find --text` matches against element text with runs of whitespace collapsed to
  a single space, but `wait --until text` matched raw `innerText` — so
  `wait --until text "pay now"` still timed out on a `<button>Pay<br>now</button>`
  (whose innerText is `"Pay\nnow"`) that `find --text "pay now"` matches. The wait
  haystack is now collapsed the same way, so the three text-matching paths
  (find, `--include text` capture, wait) behave identically. Shared `bridge.js`.

## [0.4.137] - 2026-06-10

### Fixed

- **`wait --until text` is now case-insensitive and pierces open shadow roots,
  matching `find --text` and the `--include text` capture.** It compared raw
  `document.body.innerText` case-sensitively and stopped at the shadow boundary —
  so `wait --until text submit` never matched a `Submit` button, and text living
  only inside a web component's shadow root never unblocked the wait, even though
  `find --text` matches both and the text capture now surfaces shadow text. It
  now lowercases both sides and falls back to the shadow-aware text walk (fast
  light-DOM check first, so a page without shadow-only text pays nothing extra).
  Shared `bridge.js`, both modes.

## [0.4.136] - 2026-06-10

### Fixed

- **`cookie get NAME` of an absent cookie is now a typed not-found (exit 4)
  instead of a `(0 cookies)` success.** Asking for a specific cookie that doesn't
  exist returned an empty list with exit 0 — indistinguishable from a successful
  read, so an agent checking an auth cookie's presence by exit code would misread
  the absence as success. It now returns `CookieNotFound` (exit 4), matching how
  `find`/`action click` report a missing target. `cookie list` (no name) is
  unchanged — listing zero cookies is a valid result, not a miss. Both modes
  (shared handler).

### Fixed

- **`tab list` (and `tab switch`/`tab new`/`status`) no longer fail with a
  spurious `TabNotFound` right after the active tab is closed (headless).** When
  the pinned tab closed, every command re-opening a transport hit a hard
  `TabNotFound` at the active-target resolver — even `tab list`, which only needs
  the browser connection, blocking the agent from discovering a survivor to
  switch to. The resolver now drops the dead pin, attaches to a fallback
  survivor, and marks the transport `pin_vanished`; a command that ACTS on the
  active page still fails loud (`TabNotFound`) so it never silently retargets onto
  the survivor — the never-silent-retarget contract — while tab management and
  status proceed so the agent can re-pin. This matches browser mode, where the
  persistent worker reads the active tab directly and never tripped this.

### Fixed

- **`cookie set` and `session import` now detect a cookie Chrome refused,
  instead of reporting a false success.** Both paths ignored the result of the
  set — headless dropped `Network.setCookie`'s `success` field, browser dropped
  `chrome.cookies.set`'s return — so a cookie Chrome rejected (a `SameSite=None`
  cookie without `--secure`, a `__Host-`/`__Secure-` name that breaks the prefix
  rules, an invalid domain/value) was reported as set while it silently was not.
  - `cookie set` now returns `InvalidArgument` (exit 7) with the likely cause.
  - `session import`'s cookie loop counted only transport errors, so a refused
    cookie slipped through and the import claimed full success while the restored
    session was quietly missing auth cookies; it now counts a refusal too and
    reports it in the not-imported tally.
  An agent restoring an auth cookie would otherwise believe it succeeded and then
  fail to authenticate with no signal. Verified empirically: `cookie set
  --same-site none` without `--secure` leaves no cookie and now reports it. Both
  modes.

## [0.4.133] - 2026-06-10

### Fixed

- **The Native Messaging host no longer deletes a successor host's live socket
  on exit.** The host unconditionally unlinked the fixed per-user socket path
  when Chrome disconnected. If a new host had already started and rebound that
  path to its own listener (an extension reload / service-worker restart while
  the old host was still shutting down), the old host's exit deleted the live
  socket — leaving `--browser` commands reporting the host unreachable while one
  was actually running. The bind-time unlink in `ipc::start_server` is now the
  single cleanup point (run where socket ownership is established); a stale
  socket a clean exit leaves behind is harmless — a connect to it fails as
  `ConnectionLost`, the same bucket as an absent socket, and the next host's
  bind clears it.

## [0.4.132] - 2026-06-10

### Fixed

- **Skill docs: corrected the policy store path.** The embedded `SKILL.md` told
  the agent policies live at `artifacts/policies.json`, but the store is
  `policy/policies.json` under the durable data root — `artifacts/` is the
  evictable cache for screenshots/PDFs, exactly where a security config must NOT
  live (OS cache eviction would silently reset deny rules to allow). An agent
  following the doc would look in the wrong directory.

## [0.4.131] - 2026-06-10

### Fixed

- **`capture --include text` now includes text inside open shadow roots.** The
  text dump used `document.body.innerText`, which stops at the shadow boundary —
  so a web component's own labels/prose were silently dropped (with no
  `truncated` signal), even though the DOM snapshot already pierces shadow for
  interactive elements. `innerText` is kept as the fast, well-formatted base for
  the light tree and the text owned by each open shadow root is appended; a
  `<slot>`'s projected content already lives in the light tree, so the shadow
  walk skips slots — no double-counting, and the light-only common case is byte
  for byte unchanged. Shared `bridge.js`, so both modes are fixed at once.

## [0.4.130] - 2026-06-10

### Fixed

- **`capture --include pdf` while scoped to an iframe is now rejected instead of
  silently returning a PDF of the wrong page.** `Page.printToPDF` is inherently a
  top-level operation — CDP has no frame-scoped print — so after `frame switch`
  into an iframe, a PDF capture rendered the TOP page while the DOM/header
  described the iframe the agent switched into. It now fails with `InvalidArgument`
  (exit 7) telling the agent to switch back to main first, exactly like the
  existing `--annotate` guard. Both modes.

## [0.4.129] - 2026-06-10

### Fixed

- **`key-press <letter> --shift` now produces the uppercase character.** The
  shift modifier set the event's shiftKey flag but left the injected `key`/`text`
  unchanged, so `key-press a --shift` delivered lowercase `a` to a focused field
  (and a `e.key === "A"` listener never matched) even though shift is otherwise
  treated as a text-producing modifier. A shifted ASCII letter — uppercase on
  every Latin layout — is now emitted as its uppercase form for both the event
  `key` and the inserted text, in both modes. Shifted digits/punctuation are
  layout-specific (US `1`→`!`, others differ), so those are deliberately left
  unchanged rather than assume a keyboard layout.

## [0.4.128] - 2026-06-10

### Fixed

- **A click now focuses its target, like a real click.** `reliableClick`
  dispatched `pointerdown → mousedown → pointerup → mouseup → click` but never
  moved focus, which a real click does as mousedown's default action. So a click
  fired no `focus`/`focusin` event, left `document.activeElement` unchanged, and
  — most importantly — did not establish the browser focus a following native
  `key_press` lands on, silently breaking the documented click-then-type
  contract (a click on a field followed by a keypress went nowhere). The click
  now focuses the target after a non-cancelled `mousedown` (respecting a page
  that cancels mousedown to prevent focus theft); `focus()` no-ops on a
  non-focusable target. Shared `bridge.js`, so both modes are fixed at once.

## [0.4.127] - 2026-06-10

### Fixed

- **`action select` now fires an `input` event as well as `change`, matching a
  real user selection.** A real selection in a `<select>` fires `input` then
  `change`; the bridge dispatched only `change`, so a `<select>` wired to
  `oninput` — or a framework that observes the `input` event — silently ignored
  the agent's choice while the command still reported success. It now fires both
  (bubbling), the same way `reliableType` does for text fields. Shared
  `bridge.js`, so both modes are fixed at once.

## [0.4.126] - 2026-06-10

### Fixed

- **A successful command's message is now carried into JSON output, not dropped.**
  `CommandOutput::Ok(msg)` rendered as a bare `{"success":true}` on the piped
  JSON path — the very path an agent reads — while the human and MCP renders both
  emitted the message. So `context close --all` reporting `"Closed 3 context(s);
  1 kept (failed to dispose — retry)"` reached a human but an agent saw only
  `success:true`, reading a partial failure as a clean sweep. JSON now includes
  the `message` field, matching the human and MCP output.

## [0.4.125] - 2026-06-10

### Fixed

- **An armed console/network monitor now survives a navigation WebPilot did not
  drive (headless).** The monitor hooks live on `window` and are wiped by every
  full-document navigation; `reinstall_monitors` re-injected them after a
  WebPilot-driven navigation, but a page-initiated redirect (`location.href`)
  that happened between two CLI processes had no watcher to re-arm it, so the
  monitor silently went dead until the next WebPilot-driven navigation.
  `LocalTransport::open` now re-applies armed monitors against the current
  document — mirroring how it already re-applies device emulation — so a monitor
  follows the page across out-of-band navigations too. The install JS is
  idempotent and buffer-preserving, and re-arming re-checks the `eval` policy
  gate. (Browser mode already re-arms on every navigation via the persistent
  service worker, so this gap was headless-only.)

## [0.4.124] - 2026-06-10

### Fixed

- **`landmark` now pierces open shadow roots, like `focused` and the other
  per-element flags.** `findLandmark` walked `parentElement`, which returns null
  at the shadow boundary — so a control inside a web component's shadow root
  always reported no landmark, stripping semantic context from the DOM output and
  breaking `find --landmark` for shadow components, even when the element
  genuinely sits inside a `<nav>`/`<dialog>` in the outer tree. It now walks the
  flat tree, crossing to the shadow host (where the accessibility tree flattens
  the content). The shadow-crossing parent step that `isVisible`'s opacity walk
  already used is extracted into one `flatTreeParent` helper. Shared `bridge.js`,
  so both modes are fixed at once.

## [0.4.123] - 2026-06-10

### Fixed

- **A focused control inside an open shadow root is now reported as
  `focused:true` instead of `false`.** The per-element `focused` flag compared
  against `document.activeElement`, which names only the outermost shadow host —
  so after focusing an `<input>`/`<button>` inside a web component's shadow root,
  a capture silently reported it unfocused, and an agent could wrongly conclude
  the focus did not land or the element was non-interactive. It now resolves
  through `deepActiveElement` (which pierces shadow roots), the same focus
  handling the key-press path already used. Shared `bridge.js`, so both modes are
  fixed at once.

## [0.4.122] - 2026-06-10

### Fixed

- **`setup` (extension/skill) is now a clean replace — files a previous version
  left behind are pruned, not accumulated.** `write_dir` only ever wrote the
  embedded tree's own entries and never removed on-disk files the tree no longer
  carries, so a file dropped or renamed between releases lingered in the
  deployed directory. With `self update` now re-materializing the extension over
  an existing install on every upgrade, that drift would compound across
  versions. The materialised tree is now a pure function of the binary version:
  each level writes its embedded entries and prunes anything else (best-effort —
  an inert leftover that can't be removed never fails an otherwise-successful
  install).

## [0.4.121] - 2026-06-10

### Fixed

- **`webpilot self update` now refreshes the deployed Chrome extension to the
  new version instead of leaving it stale.** The on-disk unpacked extension is
  version-locked to the binary (browser mode's host rejects any drift with
  `VersionMismatch`), but the updater swapped only the binary — so the very next
  `--browser` command failed with an infra error after an apparently successful
  upgrade, and the user had to run `setup extension` by hand. The update now
  re-materializes the extension via the freshly-installed binary (the running
  process still holds the old embedded assets, so the new binary must write
  them), but only when the extension was already deployed — a headless-only
  install is left untouched. A running Chrome must still reload the extension to
  pick up the new version; the success output now says so.

## [0.4.120] - 2026-06-10

### Fixed

- **`record --frames N --duration M` is now rejected as `InvalidArgument`
  (exit 7) instead of silently honoring `--frames` and dropping `--duration`.**
  The two flags are documented as alternatives — each names the same quantity
  (a frame count) a different way — so supplying both is a contradictory
  request. The match took `--frames` unconditionally and discarded the
  `--duration` the agent also asked for; the four argument combinations are now
  exhaustive and the both-supplied case is an explicit rejection.

## [0.4.119] - 2026-06-10

### Fixed

- **`context close NAME --all` is now rejected as `InvalidArgument` (exit 7)
  instead of silently destroying every context.** The handler took the `--all`
  branch whenever the flag was set, ignoring the `name` — so a contradictory
  `context close mycontext --all` wiped EVERY context (and every agent's tabs)
  while the agent believed it closed one named context. The two are now
  mutually exclusive: specify a name or `--all`, not both.

## [0.4.118] - 2026-06-10

### Fixed

- **`key_press Enter` that submits a form now settles the navigation, so the
  click reports `url_changed` and `--capture` snapshots the submitted page.** A
  form submit via Enter is a QUEUED navigation (HTML spec), so its start event can
  land after the native key-dispatch response — and `key_press` hard-coded "no
  navigation", letting the settle conclude nothing happened and return the
  pre-submit document. Enter now carries a conservative nav hint in both modes
  (the only native key that loads a document), so the settle waits PROBE-bound for
  the commit, exactly as a link click's `navigates` hint does; a non-submitting
  Enter pays only that short probe, and other keys never navigate.

## [0.4.117] - 2026-06-10

### Fixed

- **A browser-mode `status` / keepalive now refreshes the service worker's
  console/network policy cache.** These bypass the command queue (a health check
  must not block on a busy worker), and the queue was where the host's pushed
  `eval` verdict was applied — so a `status` carrying a fresh deny left the cache
  stale, and a page-initiated navigation between commands could re-arm the
  MAIN-world monitor hooks under the old verdict. The verdict is now applied for
  queue-exempt messages too, so a re-arm between commands sees the current policy
  (matching headless, which reads the live store on every re-install); queued
  commands still apply their own verdict in order.

## [0.4.116] - 2026-06-10

### Fixed

- **Headless `session import` is now atomic on a vanished active frame, matching
  browser mode.** Storage imports through the active frame's bridge, but headless
  applied cookies FIRST and only hit the gone frame at the storage step — leaving
  cookies mutated behind a `FrameNotFound`. It now resolves the active frame's
  bridge context BEFORE the cookie loop (only when non-empty storage is present),
  so a gone frame fails up front and never half-imports. Empty/absent storage is
  a no-op that touches neither the bridge nor the frame (matching browser's
  `hasStorage` gate).
- **`network read` human output now `line_safe`s every page-derived field.** The
  formatter guarded `method` and `url` but rendered `req_type` and the error-branch
  status string raw — a crafted (or tampered-buffer) value could inject a forged
  line into the agent-facing output. Both now pass through `line_safe`, like the
  other fields and the console formatter.

## [0.4.115] - 2026-06-10

### Fixed

- **`session export` / `session import` on a vanished active iframe now return
  `FrameNotFound` (exit 4 → recapture), not `BridgeUnavailable` (exit 3 → infra).**
  Both read/write storage through the active frame's bridge but were the last two
  sites missing the frame-existence precheck that `wait` / `dom` / `capture` /
  `action` already run — completing the v0.4.107 frame-vanish class across every
  browser bridge-call site. The import checks the frame BEFORE applying any
  cookie, so a gone frame never half-imports.

## [0.4.114] - 2026-06-10

### Fixed

- **A browser-mode ACTION on a vanished active iframe now returns `FrameNotFound`
  (exit 4 → recapture), not `BridgeUnavailable` (exit 3 → infra).** The
  frame-existence precheck added for `wait` / `dom` / `capture` was missing from
  the action dispatch, so a `click` / `type` / etc. on a frame that had vanished
  since the capture surfaced a generic "page not responding" instead of the typed
  recapture signal headless and the other browser commands return. The dispatch
  now runs the shared `frameVanishedError` guard before injecting (reusing the
  frame tree it already fetches), excluding `key_press`, which targets browser
  focus rather than the active frame's bridge.

## [0.4.113] - 2026-06-10

### Fixed

- **A malformed entry in the page-reachable network buffer no longer breaks
  browser-mode `network read`.** The sanitizing filter checked the required
  `NetworkEntry` fields but not the OPTIONAL `status`/`error` types, so a
  tampered or quirky `status: "200"` (string, or an out-of-`u32` value) passed
  through and failed the CLI's `Option<u32>` decode — surfacing as a misleading
  `ConnectionLost` (exit 3) instead of the clean read headless returns. The
  filter now type-checks `status` (null or a `u32`-range integer) and `error`
  (null or string) and drops a bad entry, matching headless's per-entry `.ok()`.

## [0.4.112] - 2026-06-10

### Fixed

- **A `role="presentation"` / `role="none"` element with a click marker is now
  indexed.** The marker and cursor:pointer heuristic passes skipped any element
  carrying a `role` attribute (assuming the semantic pass covers it), but ARIA
  `none`/`presentation` explicitly STRIP the implicit role — so a `<div
  role="presentation" onclick>` is a real click target that was silently dropped
  from the snapshot, unclickable by the agent. Those two roles (and the first
  token of a multi-token role) are now treated as role-less; a genuine semantic
  role is still deferred to the semantic pass.
- **Browser-mode `--capture` after a click that triggers a QUEUED top navigation
  no longer snapshots the pre-click page.** A link click queues its navigation
  (HTML spec), so its `onBeforeNavigate`/`onCommitted` can land after the bridge
  click response — but `settledActionUrl` ignored the bridge's `navigates` hint
  and concluded "nothing navigated", returning immediately. It now takes the hint
  (e.g. a `target=_top` link clicked inside a switched iframe) and polls briefly
  for the start, PROBE-bounded, mirroring headless's `nav_hint` fall-through; a
  mis-hint costs only the probe, never the full navigation timeout.

## [0.4.111] - 2026-06-10

### Fixed

- **A vanished persisted active frame now surfaces as `FrameNotFound` (exit 4),
  not a silent retarget to the main frame, in headless too.** `LocalTransport::open`
  validated the restored active frame against the live tree and silently cleared
  it when the page had dropped it — so the next scoped command (`eval`, `dom`,
  `capture`) ran in the main frame with no signal, while browser mode kept the
  scope and returned `FrameNotFound`. Open now restores the id verbatim; the clear
  moves to the recovery paths that REPORT it — `frame list` (now resets a stale
  scope and returns `active_frame_id: null`, mirroring browser) and `frame main`.
- **`find_chrome` discovers a Linux Chrome-for-Testing install.** The agent-browser
  candidate list carried only the macOS layouts, so a Linux box with only a
  `chrome-linux64/chrome` CfT install (the exact path the browser e2e harness
  resolves) fell through to "Chrome not found". Added the Linux CfT path plus the
  standard Linux system-Chrome locations.

## [0.4.110] - 2026-06-10

### Fixed

- **A `target="_top"` (or `_parent`) link/form clicked inside a switched iframe is
  now classified as the top navigation it is, in both modes.** The bridge's two
  nav hints disagreed: `frameNavigates` treated `_top`/`_parent` as a same-frame
  load while `clickNavigates` only fired in the top window — so a `_top` click in
  an iframe reported `navigates:false`, `frame_navigates:true`, and the settle
  waited the wrong (active) frame, returning success with no `url_changed`. Both
  hints now derive from one `navTargetKeyword` helper that resolves which ancestor
  the click targets, so they can never disagree about whether — and where — a
  click navigates.
- **Page text (`capture --include text`) can no longer inject a fake `[index]`
  action row.** It was the one page-controlled string rendered into the
  agent-facing snapshot without `line_safe`, so a crafted body containing
  `\n[7] button "Pay"` (or a `\r` cursor-return) surfaced as a forged DOM index
  line. Each line is now indented (no leading `[` at column 0) and `line_safe`d,
  while staying multi-line.
- **Browser-mode monitor re-arm now fails closed on a service-worker restart.**
  `monitorPolicy` defaulted to allow, so a navigation's `onCompleted` firing after
  an MV3 relaunch but before the host pushed a fresh verdict would re-inject the
  MAIN-world console/network hooks even under an `eval` deny. It now defaults to
  deny — re-arm stays blocked until the first command carries the real verdict,
  matching headless, which reads the live policy store on every re-install.

## [0.4.109] - 2026-06-10

### Fixed

- **`session import` is now atomic across cookies and storage in both modes.** A
  non-string `local_storage` / `session_storage` VALUE (Web Storage holds only
  strings) was caught only at the bridge sink — after the cookie loop had already
  run — so a malformed file left cookies mutated behind a storage reject. The
  storage value types are now validated up front, before any cookie is applied,
  alongside the existing shape check; the bridge keeps its own check as the sink.
- **Browser-mode same-document frame preservation is now race-free.** v0.4.108
  decided cross- vs same-document by whether `onCommitted` fired, but a
  cross-document navigation that settled through the settle loop's URL-change
  fallback before that event was processed left the flag false and wrongly
  preserved a stale frame scope. `navigateBoundTab` now snapshots the main-frame
  `documentId` before the navigation and compares it after settle — the browser's
  loaderId equivalent — resetting the frame scope unless both ids are known and
  equal.

## [0.4.108] - 2026-06-10

### Fixed

- **Browser-mode top navigation now preserves a switched frame across a
  same-document (`#fragment` / `pushState`) navigation, matching headless.**
  `navigateBoundTab` unconditionally reset the active frame to main after every
  settle; a fragment or history navigation leaves the document and its frame tree
  intact, so a frame the agent had switched into stayed valid in headless but was
  silently dropped in browser mode. The commit watch now distinguishes a
  cross-document commit (`onCommitted`) from a same-document one
  (`onHistoryStateUpdated` / `onReferenceFragmentUpdated`) and only resets the
  frame scope for the former.
- **Browser-mode navigation that starts but never finishes parsing now returns
  `Timeout` (exit 5 → retry), not `NavigationFailed` (exit 8), matching
  headless.** The settle-deadline path always threw `NavigationFailed`; it now
  reserves that for a recorded navigation error (a hard `onErrorOccurred`, or an
  `ERR_ABORTED` that never settled) and returns a typed `Timeout` for a clean
  start that simply didn't parse in time. Hard navigation errors also now fail
  fast, as headless does off the `Page.navigate` response.
- **`frameVanishedError` is null-safe.** `chrome.webNavigation.getAllFrames`
  resolves `null` (not a rejection) when the tab is gone, which the `.catch`
  alone did not guard — a non-main active frame would have thrown a `TypeError`
  (surfacing as `Other`, exit 1) instead of `FrameNotFound` (exit 4). Any
  non-array result is now treated as an unconfirmable frame.

## [0.4.107] - 2026-06-10

### Fixed

- **Headless `tab new` / popup adoption now re-arm console/network monitors on
  the settled document, not the throwaway `about:blank`.** Both routed through
  `do_tab_switch`, which re-armed window-level monitor hooks immediately — but
  the new tab's real document had not loaded yet, so the imminent load wiped the
  hooks and the agent's `console read` / `network read` missed the adopted page's
  load-time activity. `do_tab_switch` now takes `reinstall_now`: a plain `tab
  switch` (already-loaded target) re-arms at once, while `tab new` and popup
  adoption defer and re-arm after the document settles, matching browser mode.
- **Browser-mode `wait` / `dom get` / `dom set` against a vanished active iframe
  now return `FrameNotFound` (exit 4 → recapture), not `BridgeUnavailable` (exit
  3 → infra).** They called `ensureBridge` without first checking the pinned
  frame still exists, so a failed injection into a removed frame surfaced as a
  generic "page not responding". A shared `frameVanishedError` guard (also now
  backing `capture`'s check) probes the frame tree first, matching headless
  `bridge_context_id`.

## [0.4.106] - 2026-06-10

### Fixed

- **`total_nodes` now counts shadow-DOM nodes, matching the indexed elements.**
  The capture indexes interactive elements through open shadow roots, but the
  `from N nodes` footer counted only the light DOM (`querySelectorAll("*")`), so
  a shadow-heavy page (web components) reported a node total smaller than the
  tree it actually scanned. The deep-traversal that finds the elements already
  scans every visited root, so it now sums those into the node total at no extra
  cost — light DOM plus every open shadow root walked — keeping the count
  consistent with the elements it reports.

## [0.4.105] - 2026-06-10

### Fixed

- **The `*`-new marker now appears after a same-document navigation.** It was
  suppressed on *any* `location.href` change, so an element added by a
  `pushState` or hash change — an SPA route, an in-page section — came back as
  `[N]` instead of `*[N]`, hiding that it was new. The suppression existed to
  keep a fresh page from reading as "all new", but a new document already starts
  with no snapshot baseline, so the URL gate was redundant for real navigations
  and wrong for same-document ones. It is removed: the baseline is simply the
  previous snapshot when one exists, so same-document insertions are flagged and
  a fresh document still is not.
- **Headless `session import` validates storage shape before applying cookies.**
  Browser mode (0.4.104) rejects a non-object `local_storage`/`session_storage`
  up front, but headless rejected it only later via the bridge — after the cookie
  loop had already set the cookies, so a malformed file left different state in
  the two modes (cookies applied in headless, not in browser). Headless now runs
  the same shape check before the cookie loop, so a malformed import fails up
  front and leaves identical state in both modes.

## [0.4.104] - 2026-06-10

### Fixed

- **Browser mode: an empty storage section no longer blocks the cookie import.**
  0.4.103 gated storage handling on field *presence*, so a present-but-empty
  `local_storage: {}` (the common shape when the exported page had no storage)
  triggered the no-page guard on a non-http pin and returned before importing the
  cookies — even though cookies are browser-global and need no page. Storage
  shape is now validated up front (a non-object is `InvalidArgument` on any pin,
  page-independently), and the no-page guard fires only for *non-empty* storage
  that actually needs a page to import. An empty or absent storage section is a
  no-op that never blocks the cookies.
- **Browser mode: `session import` keeps a cookie's `expiration: 0`.** The
  expiry was assigned with `c.expiration || undefined`, so a legitimate `0` (the
  epoch — an already-expired cookie) was dropped and the cookie was imported as a
  session cookie instead, where headless forwards `expires: 0` and lets it
  expire. The zero is now preserved (`== null` guard), so an expired cookie
  expires identically in both modes.

## [0.4.103] - 2026-06-10

### Fixed

- **Browser mode: `session import` no longer silently drops a malformed storage
  field.** Storage import was gated on `Object.keys(local_storage).length > 0`,
  so a present but non-object value — `"local_storage": 1`, or a falsy `""` that
  the `|| {}` fallback then masked — produced zero keys, skipped the bridge, and
  reported success while importing nothing. Headless forwards any present
  `local_storage`/`session_storage` to the bridge, which rejects a non-object as
  `InvalidArgument`. Browser now gates on field *presence* and forwards the
  actual value (no `|| {}` coercion), so the same validator runs in both modes:
  a non-object storage field is a typed error, an object (even empty) imports,
  and an absent field is a no-op.

## [0.4.102] - 2026-06-10

### Fixed

- **`session import` validates the schema version and cookie field types
  identically in both modes.** Headless read the version with `as_u64`, so a
  non-integer like `1.5` looked *absent* and a too-new file slipped through —
  while browser mode's numeric `>` rejected it; the headless check now reads the
  version as a number so both reject any version above what this binary supports.
  Conversely, browser mode validated only a cookie row's string fields, so a
  truthy non-boolean like `"host_only":"false"` coerced a domain cookie into a
  host-only one (dropping `domain`) and reported success — where headless's
  `CookieInfo` deserialization rejects it; browser now type-checks
  `secure`/`http_only`/`host_only` (boolean) and `expiration` (number) to match.
- **Browser mode: `frame list` no longer reports a stale active frame.** A
  persisted active-frame id could outlive its frame across a service-worker
  suspend/restart; `frame list` reported it even when the frame was gone from the
  live tree. It is now validated against the current frames and dropped back to
  main if absent, mirroring headless, which validates the persisted active frame
  on open.
- **MCP server: the over-cap line drain is no longer bounded.** The 0.4.99 bound
  (32× the cap) could leave the tail of a line longer than that to be misparsed
  as a fresh request. The drain again runs to the terminating newline: it awaits
  I/O and discards in capped chunks, so it neither busy-spins nor grows memory,
  and a never-terminating line is the client monopolizing the single stdin stream
  (drained until EOF) — there is nothing else to process meanwhile, so no tail is
  ever left to misframe.

## [0.4.101] - 2026-06-10

### Fixed

- **Browser mode: armed console/network monitors follow the working tab.** A
  `console start` armed the monitor on the pinned tab, but a later `tab switch`,
  `tab new`, or popup adoption left the armed flags on the old tab — so a
  `console read` on the new tab silently missed its logs. Headless re-installs
  its monitor on every pin move (each routes through the tab-switch path);
  browser now carries the armed kinds onto the new tab and injects their hooks at
  each pin move, so monitoring follows the agent's working tab in both modes.
- **Browser mode: a tab that vanishes mid-navigation is a typed TabNotFound.**
  If the bound tab closed (or a page `window.close()`d) while `navigate` /
  `capture --url` / `back` / `forward` / `reload` was waiting for the navigation
  to settle, the wait polled to its full timeout and then reported
  `NavigationFailed` — masking a gone pin as a navigation problem for 15s. The
  settle now detects the vanished tab and fails fast with `TabNotFound` (exit 4),
  matching `resolveActiveTab`, which already types a vanished pin that way.

## [0.4.100] - 2026-06-10

### Fixed

- **Browser mode: `action navigate` works from a non-http tab.** Navigate went
  through the same http-required tab resolution as click/type, so issuing it
  while the bound (or focused) tab was a fresh `about:blank` / `chrome://newtab`
  returned `No web page open` — even though navigate is precisely how an agent
  *reaches* an http page (headless navigates its bound `about:blank` directly).
  Navigate now resolves its own target through the same shared `navigateBoundTab`
  path `capture --url` uses: it reuses the bound http tab or pins a fresh one,
  whatever the current scheme, so it never false-fails with NoPage. The two
  navigation entry points now share one implementation, so they cannot drift.

## [0.4.99] - 2026-06-10

### Fixed

- **Concurrent captures no longer collide on an artifact filename.** The
  browser-mode host serves each agent's request in its own spawned task, so two
  `capture --include screenshot` calls can run at once in a single process — yet
  the artifact name was `prefix_<pid>_<nanos>`, and a `SystemTime` stamp is
  coarser than a nanosecond, so the two could mint the same name and one
  overwrite the other, handing an agent the other's screenshot. The name now
  carries a process-wide atomic counter (`prefix_<pid>_<nanos>_<seq>`),
  guaranteeing uniqueness within a process as well as across them.
- **MCP server: a never-terminating request line can no longer spin the read
  loop.** The over-cap drain (added in 0.4.98) looped until it found a newline;
  an unbounded non-newline stream would loop forever. The drain is now bounded —
  far beyond any real frame, so a genuinely over-cap-but-finite line still drains
  in one call — and past the bound it returns and lets the next read continue, so
  the loop stays responsive instead of hanging.
- **MCP server: a structurally malformed `tools/call` is a JSON-RPC error, not a
  tool result.** A non-string `name` or a non-object `arguments` was passed
  through to a tool and surfaced as a misleading "missing <field>" `isError`
  result. Such a request is now rejected up front with `-32602 invalid params`,
  matching the server's documented contract that only tool *execution* failures
  use `isError` (an unknown but well-formed tool name stays an `isError` result,
  per the MCP spec).

## [0.4.98] - 2026-06-10

### Fixed

- **MCP server: an over-size request line no longer desyncs the JSON-RPC
  stream.** The stdio read loop capped each line at 8 MiB but, on an over-cap
  line, answered with a parse error and left the line's unread tail in the
  buffer — so the next read parsed that residue as a fresh request, and every
  frame after it was misaligned (a giant line followed by a valid `ping` could
  return two replies, the second correlated to the wrong request). An over-cap
  line is now drained through its terminating newline so the stream resyncs at
  the next clean frame, matching the length-framed native-messaging and IPC
  paths that never had this hazard. The framing logic moved into a testable
  `read_frame` helper covered by unit tests.

## [0.4.97] - 2026-06-10

### Fixed

- **A page-controlled title can no longer inject a fake element line into the
  capture output.** The `Label: value` lines that report page identity and
  artifact paths (`Page:`, `Title:`, `Screenshot:`, …) — the only place a
  screenshot/PDF/accessibility-only capture shows the agent what page it sees —
  rendered each value verbatim. A page setting `document.title` to a string
  containing a newline and a forged `[999] button "Pay"` line could thus fabricate
  an index line in the snapshot an agent reads, even though `DomSnapshot::to_text`
  already neutralized the same value in the DOM footer. Every value now passes
  through `line_safe`, closing the inconsistency so control characters can never
  split one labelled line into two.

## [0.4.96] - 2026-06-10

### Fixed

- **A control disabled by an ancestor `<fieldset disabled>` is now captured as
  disabled.** The snapshot read the `.disabled` IDL property, which reflects only
  an element's own attribute — not the disabled state it inherits from a
  `<fieldset disabled>` ancestor — so such a control was indexed without the
  `[disabled]` marker, and an agent that acted on it hit a confusing rejection
  from the action guard (which correctly uses `:disabled`). Capture now uses the
  same `isDisabled` helper as the action guards, so the snapshot and the
  enforcement agree. The same `:disabled` fix covers selecting an `<option>`
  inside a disabled `<optgroup>`, which is now rejected rather than silently set.
- **Every session-breaking timeout is validated at startup, not just some.**
  Settings validation rejected a zero `navigation`/`cdp_send`/`poll_interval`/
  `heartbeat` but silently accepted a zero `ipc_response`, `chrome_launch`,
  `reload_wait`, `back_forward`, or `version_handshake` — each of which makes its
  operation fail instantly. All deadline/interval timeouts are now checked
  uniformly (a paint-delay tuning value that is legitimately zero is excluded),
  so a misconfiguration fails loudly at load instead of degrading the session.

## [0.4.95] - 2026-06-10

### Fixed

- **Headless: a non-canonical function-key name is now rejected, matching
  browser mode.** The headless `key-press` parser stripped the `F` and parsed
  the rest as a number, so `F01` (a leading zero) or `F007` were silently
  normalized to `F1`/`F7` and dispatched as a success — while browser mode's
  strict `^F([1-9]|1[0-2])$` rejected the same name as `InvalidArgument`. An
  agent that validated a key name against headless would then break in browser
  mode. The headless parser now requires the canonical `F1`–`F12` (no leading
  zeros or extra digits), so a malformed function key fails identically in both
  modes instead of being normalized in one.

## [0.4.94] - 2026-06-10

### Fixed

- **Browser mode: an action's `--capture` while switched into an iframe now
  reports nested iframes.** The post-action snapshot only set
  `DomSnapshot.subframes` when on the main frame, so after a `frame switch` an
  `action … --capture` always reported `subframes: 0` — hiding any HTTP iframe
  nested inside the active frame, so the agent's "N iframe(s) not shown" footer
  vanished and a deeper frame became undiscoverable after an action. The count is
  now computed unconditionally and scoped to the active frame via the shared
  `countHttpSubframes`, matching the standalone `capture` path and headless
  `capture_action_snapshot` (which was already unconditional).
- **Browser mode: a malformed cookie URL is now a typed `InvalidArgument`.**
  `cookie set` validated the URL with a scheme-prefix regex, so a well-prefixed
  but malformed URL (`http://` with no host) slipped through to
  `chrome.cookies.set` and surfaced as a generic `Other` (exit 1) — while
  headless rejected the same URL at the CDP sink as `InvalidArgument` (exit 7).
  The URL is now parsed (`new URL`) and required to be http(s), so every
  malformed URL is rejected with the same code in both modes, not just a missing
  scheme.

## [0.4.93] - 2026-06-10

### Added

- **`cookie set` can now set `SameSite` and an expiry.** The read side
  (`cookie list`) reports a cookie's `SameSite` and expiration, and session
  import applies them, but the manual `cookie set` could only ever write a
  default-SameSite session cookie — so an agent could not faithfully re-set a
  cookie it had just read. `cookie set` gains `--same-site <strict|lax|none>`
  and `--expires <unix-epoch-seconds>`, mapped to `Network.setCookie`
  (headless) and `chrome.cookies.set` (browser) through the same SameSite
  spelling used everywhere else. Omitting a flag preserves the prior behaviour
  (no SameSite attribute, a session cookie).

### Fixed

- **`session import` of a non-object JSON no longer reports a false success.**
  An array, string, or number parses as valid JSON but reaches every field read
  as absent, so the import fell straight through to `success: true` — telling the
  agent the session restored while nothing was applied (in browser mode a `null`
  root instead threw a `TypeError` mislabeled `Other`). Import now rejects a
  non-object root up front with `InvalidArgument` (exit 7), identically in both
  modes, alongside the existing version and `cookies`-array guards.

## [0.4.92] - 2026-06-09

### Fixed

- **Browser mode: a dialog from a switched iframe no longer wedges the session.**
  The `alert`/`confirm`/`prompt` override that keeps a native modal from blocking
  the page was injected only into the main frame. After `frame switch`, an action
  runs in the active iframe — and a `confirm()` its click handler fires opened a
  real modal that blocked the page thread until the action timed out, with no
  recovery. The override is now installed into the active frame as well as the main
  frame before each action, so a dialog in the frame the agent is acting on is
  suppressed too. (Headless needs no override — headless Chrome auto-dismisses
  dialogs in every frame.)

## [0.4.91] - 2026-06-09

### Fixed

- **`context close` respects `policy default deny`.** Disposing a browser context
  destroys it and every tab in it — and `context close --all` can wipe OTHER
  agents' contexts — yet it reached CDP directly, bypassing the policy gate, while
  the strictly-less-destructive `tab_close` was gated. A steered agent under
  `default deny` could still nuke contexts. A new `PolicyKey::ContextClose`
  (headless-only, like `device`) is now enforced at the command, so `default deny`
  forbids it and `policy set --operation context_close --verdict allow` re-permits
  it. `context list` (a read) stays ungated — completing the policy model's
  "gate effects, not observations" rule across the headless-only commands
  (`device`/`context close` gated; `profile`/`record`/`context list` are read-only).
  Verified: `context close` is PolicyDenied (6) under deny, allowed when permitted;
  `context list` stays exit 0.

## [0.4.90] - 2026-06-09

### Fixed

- **The cross-process device re-apply now respects the device policy gate too.**
  `device` emulation persists across CLI invocations by re-applying the stored
  `DeviceState` at `LocalTransport::open`. That re-apply bypassed the gate added in
  0.4.89, so a device set while allowed (a spoofed UA especially) would be restored
  into a later process even after `policy ... device deny` — leaving the emulation
  active under a policy that forbids it. `open` now skips the re-apply when `device`
  is denied; the persisted state stays on disk and is restored once `device` is
  re-allowed. Verified with a device-only deny (eval still readable): the UA is not
  restored under deny, restored again when re-allowed.

## [0.4.89] - 2026-06-09

### Fixed

- **`device` emulation now respects `policy default deny`.** The headless `device`
  command reaches CDP directly, bypassing `LocalTransport::send` (the usual policy
  sink), so a locked-down agent could still change the viewport and — notably —
  spoof the user agent, the exact steered-agent threat the policy guards against,
  contradicting the documented "default deny = least-privilege" mode. A new
  `PolicyKey::Device` is now enforced at the command's CDP sink
  (`policy::enforce_key`), so `default deny` forbids it and `policy set --operation
  device --verdict allow` re-permits it. Verified: device set is PolicyDenied (6)
  under default-deny, allowed once permitted.

### Changed

- Refreshed a stale `frameNavigates` comment that described only link clicks after
  it grew to also hint form-submit navigations.

## [0.4.88] - 2026-06-09

### Fixed

- **A click on a form's submit button settles its navigation instead of racing
  it.** The bridge's navigation hint only recognized `<a href>` clicks, so a
  `<button type=submit>` / `<input type=submit>` click — which submits the form and
  loads a new document with no href — got no hint, and the settle relied on the
  buffered load event alone (a race the link hint exists to close). A submit
  control now produces the same `frame_navigates` / `navigates` hint, so the
  post-submit auto-capture lands on the submitted document, not the pre-submit page.
  Verified 5/5 on a GET form. (Enter-triggered implicit submission stays on the
  buffered-event path: detecting it precisely is the spec's ambiguous heuristic, so
  it's left to the documented best-effort rather than a guess.)

## [0.4.87] - 2026-06-09

### Fixed

- **Browser-mode `key-press` with an unknown key returns `InvalidArgument`, not
  `ConnectionLost`.** `dispatchKeyPress` returned a bare error object for an
  unrecognized key; spread into the Action response it became
  `{type:"Action", code, message}` with no `success`/`error` field, which the Rust
  side couldn't parse and mislabeled as exit 3. It now returns the wrapped
  `{success:false, error}` shape, so the exit code matches headless (7). Guarded in
  the browser e2e.
- **`type --clear` on a contenteditable no longer clobbers a rich editor's
  structure.** It cleared via `innerHTML = ""`, a raw DOM wipe that destroys nested
  `<p>`/`<span>` structure and desyncs a framework managing its own DOM
  (Draft/Slate/ProseMirror). It now clears through the editing pipeline
  (select-all + delete), which fires the `beforeinput`/`input` events the framework
  observes; a plain contenteditable is emptied just the same.

## [0.4.86] - 2026-06-09

### Fixed

- **`select` rejects a disabled `<select>` or a disabled/hidden `<option>`.**
  Completing the disabled-control consistency (click/type already reject one),
  `action select` set `.value` and fired `change` on a disabled select, or picked a
  disabled/hidden option a real user can't choose — reporting a selection the page
  forbids. It now rejects with `InvalidArgument`, sharing the `isDisabled`
  predicate. Both modes.
- **`session import` rejects a file from a newer schema instead of silently
  dropping fields.** The export stamps a `version`; import ignored it, so a session
  written by a future incompatible WebPilot would import as success while quietly
  losing the fields this binary doesn't understand. Import now rejects a version
  above what it supports (`InvalidArgument`); a missing version is still accepted as
  the current schema. Both modes, from one `SESSION_SCHEMA_VERSION` per side.

## [0.4.85] - 2026-06-09

### Fixed

- **`click` on a disabled control fails loud instead of firing its handler.** A
  synthetic `dispatchEvent` click runs a disabled control's listeners that a real
  user (or `el.click()`) never could, so `action click` on a `:disabled` /
  `aria-disabled` element reported success while mutating page state in a way the
  page disallows. It now rejects with `InvalidArgument`, sharing one `isDisabled`
  predicate with `type` (which also now catches a control disabled by an ancestor
  `<fieldset disabled>`). Both modes.
- **A second `device set` without `--user-agent` resets the UA instead of leaving a
  stale one.** `device set` resets viewport, DPR, and touch unconditionally but
  only sent a UA override when one was given — so going from a UA-bearing device
  back to one without it kept the old UA active, contradicting the new device. The
  UA override is now always applied (cleared with `""` when absent, as `device
  reset` does), so a `device set` fully defines the device.

## [0.4.84] - 2026-06-09

### Fixed

- **The subframe-count walk's depth bound now fires at the same level as every
  other frame walk.** `count_http` was entered at depth 1 for the active frame's
  children, so its guard fired one level later (257) than the sibling walks (256).
  Each child is the entry of its own count, so it now starts at depth 0 like every
  other walk — the depth only bounds the stack and never affects the count, which
  is unchanged.

## [0.4.83] - 2026-06-09

### Fixed

- **Every CDP frame-tree walk is depth-bounded from one shared limit.** A
  systematic audit after the per-walk fixes found two more unbounded recursions —
  `frame_exists`'s `walk` and `active_frame_still_present`'s `contains` — that could
  overflow the stack on a pathological or corrupted browser-supplied tree. All five
  frame walks (`find`, `count_http`, `collect_frames`, `contains`, `walk`) now
  derive their cap from a single `MAX_FRAME_DEPTH` in the module (replacing three
  scattered copies), so the bound can't drift and no walk is left unguarded.

## [0.4.82] - 2026-06-09

### Fixed

- **`tab new` no longer echoes the requested URL as the landed one when the tab is
  missing.** It reports the new tab's settled URL (redirect-resolved) from the
  response — but on a success with no `new_tab` it fell back to the requested URL,
  the exact "blind echo" its own contract forbids: a redirect (or a missing tab)
  would be reported as the requested address. Since the handler always populates
  `new_tab` on success in both modes, that case is a protocol violation and now
  fails honestly rather than returning a plausible lie.

## [0.4.81] - 2026-06-09

### Fixed

- **`frame list` / `frame switch` frame-tree walk is depth-bounded.** `collect_frames`
  recursed through `childFrames` with no depth cap (unlike `count_http_subframes`,
  bounded in 0.4.79), so a pathologically deep or corrupted browser-supplied frame
  tree could overflow the stack while building the frame list. It now stops at the
  same `MAX_FRAME_DEPTH`, degrading to a shorter list rather than a crash.

## [0.4.80] - 2026-06-09

### Fixed

- **After a click that navigates, the console/network monitors re-arm onto the new
  document, not a transitional one.** The url_changed reset re-installed the monitor
  hooks BEFORE waiting for the live committed document, so the `window` hooks could
  land on the about-to-be-replaced pre-commit context and be lost — leaving the
  monitor silently dead for the new page until the next re-arm, so an agent reading
  console/network after the navigation saw an empty buffer and wrongly concluded the
  page was quiet. The live-document wait now runs FIRST, then the re-arm installs
  onto the real document. (The other re-arm sites already waited for readiness —
  reconnect via `rebind_page_world`, history via `await_document_ready`, reload via
  the load event; browser mode uses `chrome.scripting` which targets the
  current document, so neither needed the swap.)

## [0.4.79] - 2026-06-09

### Fixed

- **The active-frame lookup in the subframe count is depth-bounded.** The `find`
  helper that locates the active frame's node in the CDP frame tree (added in
  0.4.77) recursed without the depth cap its sibling counting walk already had — a
  pathological or corrupted browser-supplied tree could overflow the stack. It now
  degrades to "not found" past the same `MAX_FRAME_DEPTH`, never a crash.

## [0.4.78] - 2026-06-09

### Changed

- **Removed two dead exports.** `injectConsoleMonitoring` / `injectNetworkMonitoring`
  were in `state.js`'s export list but are only ever called within `state.js`
  itself — no other module imports them. Dropped them from the export so the
  module's public surface lists only what's actually consumed elsewhere.

## [0.4.77] - 2026-06-09

### Fixed

- **A capture scoped to a switched iframe now reports the iframes nested inside
  it.** `DomSnapshot.subframes` — the "N iframe(s) not shown" count — was only
  populated from the main frame, so after `frame switch` into an iframe that itself
  contained HTTP sub-iframes, the count was silently zero and the agent never
  learned it could go deeper. The count is now scoped to the active frame's own
  HTTP descendants (the whole page from the main frame, the frame's subtree from a
  switched one), in both modes. Guarded by a nested-iframe fixture in both e2e
  suites.

### Changed

- **Docs:** the project guide said `config.toml` is read from the repo root; it is
  actually resolved under the cache root (`dirs::config_file_path()`, override the
  path with `WEBPILOT_CONFIG`) — a repo-root/cwd-relative config would be fragile
  for a globally-invoked CLI. Corrected the doc to match the code.

## [0.4.76] - 2026-06-09

### Fixed

- **A failed session-state restore no longer silently retargets the agent's pin.**
  After a service-worker restart, the persisted active-tab/frame pins are restored
  before any command runs. The restore swallowed a storage-read error and resolved
  with default (empty) state — indistinguishable from "no session" — so a failure
  made the next command re-pin to the focused tab's main frame, acting on the wrong
  page. The restore now throws on an unreadable store; the command fails loud
  (`ConnectionLost`, retryable) instead of dispatching against a guessed pin, and
  the attempt is not cached so the next command retries (no worker wedge on a
  transient hiccup). The `load`-time monitor re-arm likewise skips rather than
  consult empty state.

## [0.4.75] - 2026-06-09

### Fixed

- **`setup nm-host` returns `InvalidArgument` (exit 7) for a malformed extension
  ID.** A bad `--extension-id` used `anyhow::bail!`, surfacing as a generic exit 1
  instead of the user-error exit code the rest of the CLI uses for bad input —
  exit codes name the error class, never inferred from a message.

## [0.4.74] - 2026-06-09

### Fixed

- **A switched frame's name is now reported, and identically in both modes.** The
  frame name was collected on both the `frame` list and `frame switch` (headless)
  but rendered nowhere, and browser mode omitted it from the switch response
  entirely — so `frame switch <name>`, a real addressing mode, was undiscoverable
  (the agent could switch by a name it had no way to see) and the two modes
  diverged. `frame` and `frame switch` now surface a frame's name when it has one
  (`name=<value>`, JSON `name`), and browser mode resolves and returns it on switch
  for full parity.
## [0.4.73] - 2026-06-09

### Fixed

- **A click on a control inside an open shadow root now crosses the shadow
  boundary.** The synthetic click events lacked `composed: true`, so a click on a
  shadow-DOM button (which capture indexes via `queryAllDeep`) stopped at its
  shadow root and never reached a host/document delegated `click` listener — a
  silent no-op for the common web-component delegation pattern. Both modes.
- **MCP `browser_screenshot` reports the page it captured.** It returned only the
  image and `Screenshot: {path}`, dropping the `page_url`/`page_title` the CLI adds
  for a DOM-less capture — so an MCP client couldn't tell which page (after a
  redirect or in a switched iframe) the image shows. It now carries the same
  `Page:`/`Title:` lines.
- **Browser-mode `status` no longer shows a healthy focused tab when the pinned
  tab has died.** A dead pin fell through to the focused window's active tab, while
  every other command fails that pin with `TabNotFound` — so status disagreed with
  what the agent's commands would do. Status now reports the pinned tab or nothing
  if it is gone; the focused-tab fallback applies only when no tab is pinned.
- **`diff` returns `InvalidArgument` (exit 7) for a bad file pairing.** Mismatched
  or undetectable input kinds used `anyhow::bail!`, surfacing as a generic exit 1
  instead of the user-error exit code the rest of the CLI uses.

## [0.4.72] - 2026-06-09

### Fixed

- **A concurrent launcher no longer reaps a Chrome another agent just spawned.**
  `get_existing_session` deleted the pid/ws files when it read a dead pid — but it
  runs BEFORE the launch lock, so between reading a stale dead pid and deleting,
  another process could write fresh pid/ws under the lock; the unlocked delete then
  clobbered them, orphaning the just-launched Chrome and surfacing as a random
  connection loss or a needless relaunch. The dead-pid path is now read-only; the
  stale files are reaped under the launch lock in `ensure_session`, where no write
  can race the delete.

## [0.4.71] - 2026-06-09

### Fixed

- **Reconnecting after a transient Chrome teardown now uses the relaunched
  session, not the dead one.** When the first CDP connect failed (Chrome exited
  between the liveness check and connect), `open` relaunched and reconnected the
  browser client to a fresh URL — but that fresh URL was bound inside the match arm
  and shadowed, so the rest of `open` (the page connection and the URL stored on
  the transport for every later page attach) kept using the dead session's URL. The
  recovery the path exists to provide therefore failed at the next step. The match
  now carries the connected URL out, so a relaunch is fully adopted.

## [0.4.70] - 2026-06-09

### Fixed

- **The console/network monitor buffer cap is one source, so eviction and the
  `truncated` flag cannot drift.** The page-side ring-buffer eviction hard-coded
  `500` while the read path that sets `truncated` used the named cap constant — a
  latent split where raising the constant would evict at 500 yet report the buffer
  complete, hiding the silent drops from the agent. Both are now derived from the
  single cap: browser passes it into the injected monitor as a `chrome.scripting`
  argument; headless substitutes it into the install script. No literal `500`
  remains in either eviction path.

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
