# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.6.17] - 2026-06-15

### Fixed

- **Headless: `action back` after the first navigation falsely succeeded**,
  hopping to a blank page instead of reporting a typed `NavigationFailed`.
  Headless opens every target blank (`Target.createTarget("about:blank")`) and
  drives the load through `Page.navigate`, which APPENDS — so the first load
  landed session history `[about:blank, url]`, and a `back` traversed to the
  synthetic `about:blank` the agent never requested. Browser mode
  (`chrome.tabs.create({url})`) has no such entry, so its `back` after the first
  load is a typed `NavigationFailed`. The first real load on a freshly-created
  target now prunes the synthetic entry (`Page.resetNavigationHistory`, which
  keeps the current document), bringing headless `back`/`forward` history to
  parity with browser mode. Affected every headless session — `capture --url`
  and `tab new <url>` alike.
- **Headless: `cookie set` / `session import` of a Secure cookie partitioned
  under a first-party non-`https` top-level site** (e.g. CHIPS-partitioned under
  `http://localhost`) was refused by Chrome. The cookie was set via an
  `https://…` URL (secure-implied), but Chrome validates a first-party partition
  schemefully, so the set URL's scheme must match the partition's
  `top_level_site` scheme. Headless now derives the scheme from the partition
  key (falling back to secure-implied), mirroring browser-mode `state.js`.
- **Browser mode: `navigate` from a tab pinned to a non-`http` page**
  (`about:blank` / `chrome://`) orphaned the pin and opened a SECOND tab, while
  headless navigates its bound target in place. `navigate` needs no injectable
  bridge — it is REPLACING the URL — so it now reuses the pinned tab whatever its
  scheme (a new navigate-specific resolver), matching headless and the command's
  own documented contract. A first navigate with no pin still defers to the
  focused-http-or-create path, so the user's non-http focused tab is never
  hijacked.
- **Headless `action reload --capture`** now settles at the same point as
  `navigate` and browser-mode reload — committed then parsed (`readyState` past
  `loading`, the DOMContentLoaded point a capture acts on) — instead of
  `Page.loadEventFired`. The old full-load wait blocked on trailing subresources
  that add no interactive elements and lagged browser mode; the commit gate
  keeps a same-URL reload from reading the previous document's `readyState`.
- **Headless `capture --annotate`** no longer emits a degenerate `0×0`
  annotation box; it applies the same `w > 0 && h > 0` keep-filter browser mode
  already uses, so the two modes annotate the identical element set.

## [0.6.16] - 2026-06-14

### Fixed

- **Browser mode: a navigation to a page whose renderer is wedged before
  DOMContentLoaded** — a parser-blocking `<script>while(true){}</script>` — no
  longer hangs the command forever. `waitNavigationSettled` and `documentReady`
  read `document.readyState` via `chrome.scripting.executeScript` with no
  per-probe timeout, so a wedged renderer queued the injected probe indefinitely
  and the navigation deadline — re-checked only between probes — never fired.
  Both probes are now bounded by the same `PROBE` timeout headless uses, so a
  stuck renderer times out the probe and the navigation reports a typed Timeout.
- **`dom set-html` on a Trusted-Types page** (`require-trusted-types-for
  'script'`, deployed by GitHub/Google and others) now reports a typed
  `InvalidArgument` with the page's reason, instead of leaking the bridge's raw
  V8 exception as an untyped `Other` (headless) or — in browser mode — letting
  the uncaught throw close the message port and stall the command for the full
  send timeout. The `innerHTML` assignment is now guarded exactly as `set-attr`
  already was; locked by an e2e test on a Trusted-Types page.

### Removed

- The unreachable `navigation` case in the bridge's wait handler (both modes
  settle a navigation wait outside the bridge — headless via `Page.loadEventFired`,
  the SW via `webNavigation.onCompleted`), along with the comment that
  misdescribed it.

## [0.6.15] - 2026-06-14

### Fixed

- `action key-press <key>` accepts a hyphen-led key name (e.g. `-`, the minus
  key), matching `type`/`select` which already accept hyphen-led values — it was
  rejected as an unknown flag before.
- `action upload` now works on a file input inside a switched iframe. The
  `require_main_frame` constraint that refused it was unnecessary — upload
  resolves the index in the active frame's own bridge world and sets the file on
  a frame-independent CDP objectId (`DOM.setFileInputFiles`), with no viewport
  coordinate or main-document lookup — and the guard's rationale was false for
  upload (it holds for `drag`/`hover`, which stay gated). Lifted in both modes,
  with an e2e test uploading to a file input inside a switched iframe.

## [0.6.14] - 2026-06-14

### Fixed

- `frame switch url <pattern>` / `frame switch name <name>` now fail loud with
  `FrameNotFound` at the switch when the matched frame is a cross-origin OOPIF
  with no execution context in the tab's CDP session — instead of returning
  success and then failing every subsequent `eval`/`capture` with `FrameNotFound`
  (after a per-command probe). The predicate selector already validated the
  context per-candidate; the URL and name selectors now agree, in both headless
  and browser modes, matching the documented "a cross-origin OOPIF is a typed
  FrameNotFound" boundary.

## [0.6.13] - 2026-06-14

### Fixed

- **Browser mode: a `fetch` to a host that accepts the connection but never
  responds no longer wedges the tab.** `handleFetch` ran its `Runtime.evaluate`
  (which awaits the page's `fetch()`) with no deadline, so a never-settling
  request hung inside the per-tab CDP lock forever — blocking every later command
  on that tab through the serialized queue. The awaiting-promise evaluate is now
  bounded by the navigation deadline (the same bound `eval` already had, now
  shared via one helper), returning a typed `Timeout` and freeing the lock —
  matching headless, which bounds the identical evaluate at the cdp_send timeout.
- **Browser mode: a screenshot whose tab closed mid-capture now reports
  `TabNotFound`** (exit 4 → recover via `tab`) instead of a success with an empty
  `screenshot_error` for a page that no longer exists — matching headless.
- **Browser mode: an `eval` in a switched frame retries once if the frame's
  execution context goes stale** between resolution and the evaluate (the frame
  navigated in that window) — mirroring headless `eval_in_active`. A mid-flight
  "execution context destroyed" is still not retried, so a non-idempotent
  expression can't double-fire.

## [0.6.12] - 2026-06-14

### Changed

- The browser-mode popup's "Not connected" hint points at `webpilot setup` plus
  reloading the extension, with `webpilot --browser status` to diagnose — it now
  covers the registered-but-disconnected and version-mismatch causes too, not
  only the never-registered case the previous `setup nm-host`-only hint
  addressed.

## [0.6.11] - 2026-06-14

### Fixed

- A capture that fails partway no longer leaves an orphaned artifact file. The
  screenshot and PDF were written to disk mid-capture, so a later fallible CDP
  step — PDF generation, accessibility-tree retrieval, or the post-capture
  URL/title read failing on a vanished frame — would hard-fail the command while
  the already-written files leaked into the artifacts directory. The image and
  PDF are now held in memory and committed to disk only after every fallible CDP
  step succeeds, the same "nothing outlives a failed capture" rule the annotation
  overlay already follows. The PDF is written before the screenshot, so a write
  failure can't orphan a just-saved image; the screenshot save still degrades to
  `screenshot_error` rather than failing the capture.

## [0.6.10] - 2026-06-14

### Fixed

- A rejected `action type` no longer destroys the field's prior value. Appending
  text the control can't accept — "abc" into an `<input type=number>` that
  already holds "5" — wrote the combined value before the rejection check, and a
  typed control sanitizes the unparseable result to empty, blanking the field
  while reporting the type as rejected. The prior value is now restored before
  failing typed, so a rejected append is a clean no-op, matching the maxlength
  guard that already rejects before any mutation.

## [0.6.9] - 2026-06-14

### Fixed

- `self update` no longer refuses to upgrade off a pre-release build to the
  matching final release as a false "downgrade". The version comparator is now
  SemVer-precedence-aware: a pre-release suffix (`-rc.1`) lowers precedence
  instead of being read as an extra numeric component that ranked the
  pre-release above the final, so `1.2.3-rc.1 → 1.2.3` is correctly an upgrade.
- `record --url … --frames N --duration M` (a contradictory request) is rejected
  BEFORE navigating, so an invalid recording invocation no longer first mutates
  browser state by loading `--url`. The frame-count validation and cap moved
  ahead of the navigation.
- `dom get`/`set` checks the failure flag before the value, so a response that
  carried a value alongside `success: false` can never be rendered as a success —
  upholding the surface's never-map-failure-to-success contract.

## [0.6.8] - 2026-06-14

### Fixed

- **Uploads to a hidden file input now work** — the standard pattern of a styled
  trigger over a `display:none` / `opacity:0` `<input type=file>`. A file input
  is uploadable over CDP regardless of paint, and clicking the visible trigger
  only opens an OS file dialog no automation can drive, so the input is now
  always captured (indexable for `action upload`) and resolves for the upload
  sink without the visibility gate the visible-action paths keep. Previously the
  hidden input was filtered out of every snapshot, leaving the common upload UX
  unreachable.
- **Exported session files are owner-only (0600).** Every WebPilot state file is
  now written 0600, so `session export --output` moving a session — auth cookies
  plus localStorage — to a user-chosen, possibly shared directory no longer
  leaves the secrets world/group-readable. The 0700 directory still gates
  traversal; this is the protection the file keeps when it travels out.
- A `session export` on a page where storage is inaccessible (a `data:` URL, a
  sandboxed frame, storage disabled) now fails with a clean, actionable message
  instead of leaking the bridge's raw V8 exception and its internal stack.

## [0.6.7] - 2026-06-14

### Changed

- `action reload` (headless) subscribes to the load-completion event BEFORE
  issuing `Page.reload`, mirroring the click and history-navigation paths, so a
  fast reload whose `loadEventFired` would otherwise race a late subscription
  can't burn the full reload timeout before settling. It now drains a
  pre-subscribed receiver — the same deterministic pattern those paths use —
  instead of opening a fresh subscription and swallowing its result.

### Documentation

- Corrected the `do_wait` timeout-clamp rationale: `Instant + Duration::from_millis(u64::MAX)`
  does not overflow (verified — only `from_secs(u64::MAX)` does). The clamp's
  real, platform-independent reason is the in-page `setTimeout` i32 ceiling
  (~24.8 days); it also keeps the Rust deadline far inside `Instant`'s range.
- Documented why browser-mode `navigateBoundTab`'s fresh-tab branch is safe
  registering its commit watch after the create — the URL-change settle fallback
  (with an empty `beforeUrl`) covers any missed commit — so it reads as
  intentional rather than diverging from the existing-tab branch's ordering.

## [0.6.6] - 2026-06-14

### Fixed

- Explicit `context close` (headless multi-agent) now respects the same liveness
  lock the context GC does, so it can no longer evict a context another live
  process is actively using. A single `context close NAME` of a context held by
  another live session fails loud with a typed `ContextInUse` error (exit 1);
  `context close --all` skips such contexts and reports them as "kept (in use)"
  rather than wiping a running agent. Closing the context the caller is itself
  bound to is always allowed — the caller's own shared lock is not a foreign
  holder, so a self-close never blocks. Previously only the `ContextClose` policy
  gate, not the liveness invariant, stood between an explicit close and a running
  agent's session; the exclusive liveness lock is now held through disposal, the
  same TOCTOU-safe pattern the GC uses.

## [0.6.5] - 2026-06-14

### Fixed

- A `fetch` whose request never completes — a refused/unresolved host, a blocked
  request, or a CORS denial — now reports a clean, typed message instead of
  leaking the browser's raw `TypeError: Failed to fetch` together with its
  internal V8 stack trace. The rejection is caught at the page boundary and
  rendered the same `Other`-class way the oversize and binary-body guards already
  are, in both headless and browser modes.

### Changed

- The one-line message sanitizer (control / bidi / zero-width neutralization plus
  a codepoint-safe 200-char cap — the `line_safe_clip` twin) moved from a
  browser-module-local copy to the shared errors module as `lineSafeClip`, so the
  fetch-failure, frame-ambiguity, and frame-name error paths share one
  implementation instead of drifting copies.

## [0.6.4] - 2026-06-14

### Fixed

- `tab new <url>` (headless) opens the tab blank and drives the load through the
  same path `action navigate` uses, so the two share one fast, correct
  load-and-failure path. A `tab new` to an unreachable URL now fails as a typed
  `NavigationFailed` in ~0.1s instead of spinning to the full navigation timeout
  (~17s), and an intermittent false `TabNotFound` — the new-target existence
  guard racing a refused-URL error-page transition — is gone, because the tab is
  created at the always-stable `about:blank` before any switch runs. A failed
  open still rolls back to the agent's previous tab (the no-leak contract), now
  re-arming that tab's monitors exactly as a plain `tab switch` does. This
  removes the bespoke settle + `Page.getFrameTree`/`unreachableUrl` probe that
  `tab new` carried in parallel to `navigate`.

## [0.6.3] - 2026-06-14

### Fixed

- `status` validates the manifest against every field Chrome requires to launch
  the host — a present `description` and an absolute, launchable `path` included
  (both verified required against Chrome for Testing) — so a manifest missing
  either is reported as malformed instead of a false "OK".

### Changed

- Browser-mode reload and version-mismatch messages are browser-neutral — they
  point at "your browser's extensions page (e.g. chrome://extensions)" rather
  than Chrome only, matching the multi-browser registration.

### Documentation

- README command-reference table now lists `record` and `profile`; the
  load-unpacked step is browser-neutral.
- Skill: the `--capture` success path returns the destination snapshot directly
  (there is no `success` field); `device reset` returns to the default headless
  viewport (the rendered viewport is shorter than the 1280×720 launch window).

## [0.6.2] - 2026-06-14

### Fixed

- **Browser mode now works with any installed Chrome-family browser**, not only
  branded Google Chrome. `setup` registers the Native Messaging host with every
  Chrome-family browser present (Chrome, Chromium, Brave, Edge, channels), writes
  nothing and says so when none is installed, and `uninstall` removes every
  registration and reclaims the directories it created. Previously a
  Chromium/Brave/Edge user got a success message for a registration their browser
  would never read.
- **`status` validates the full host manifest** (`name`, `type`, `path`, and the
  authorised extension id) before reporting it healthy, so a manifest missing any
  required field — or a wrong `--extension-id` override — is reported as the
  problem it is instead of a false "OK".

### Changed

- Dropped the Chrome-only `--open` auto-launch from `setup`: the Chrome family has
  no shared extensions URL, so setup prints browser-neutral load-unpacked steps
  rather than guessing which browser to open.

## [0.6.1] - 2026-06-14

### Changed

- Moved every dependency to its latest stable release, including two majors —
  `sha2` 0.10 → 0.11 and `similar` 2 → 3 — both verified behavior-preserving:
  the derived extension id is byte-identical (the pinned-constant test still
  passes) and `diff` output (counts, unified hunks, and the no-final-newline
  marker) is unchanged, now locked by a golden test.

## [0.6.0] - 2026-06-14

### Changed

- **Browser-mode setup no longer asks for the extension ID.** The extension's
  id is a stable constant (its manifest pins a public `key`), so the binary now
  derives it from the embedded manifest — exactly the value Chrome assigns at
  load. `setup nm-host` registers the Native Messaging host with no argument
  (`--extension-id` remains, only to authorise a different build), and bare
  `webpilot setup` registers the host automatically, leaving just the one step
  only the user can do: `Load unpacked` in `chrome://extensions`.

### Documentation

- Rewrote the README as a bilingual pair — `README.md` (Korean) and
  `README.en.md` (English) — built around a real-capture "Acme Tasks"
  walkthrough with Mermaid diagrams, every command and example output verified
  against the live CLI. The skill and the `CLAUDE.md` set were fact-checked in
  the same pass (DOM/exit-code/policy/setup surfaces).

## [0.5.0] - 2026-06-14

An extended hardening series over 0.4.0, driven by continuous parallel
adversarial review — headless and browser, against pathological pages and
concurrent agents — until every subsystem converged with no functional defect.
0.4.0 made browser mode a first-class peer of headless on one shared command
surface; 0.5.0 is that surface hardened to convergence.

### Hardened

- **DOM extraction** — the interactive set is the complete, principled ARIA
  widget-role taxonomy: custom listbox / menu / tree / grid items are first-class
  (including keyboard-only `aria-activedescendant` widgets that carry no per-item
  affordance), multi-token roles are honored on any token, visibility is a bare
  `checkVisibility` plus an opacity walk across shadow boundaries, and occlusion
  is a multi-point hit-test. Indices stay strictly snapshot-bound — a changed page
  yields a typed `StaleSnapshot`, never a silent re-resolution against the live DOM.
- **Navigation** settles deterministically across redirect, 204 / download
  stay-put, fragment, cross-origin renderer swap, and history traversal, with no
  fixed sleeps; armed console / network monitors re-install automatically after
  every WebPilot-driven page change.
- **Stability under stress** — a page console argument over 16 MiB no longer
  permanently wedges the headless engine (the CDP frame cap is finite-but-high
  with Chrome's native console buffer discarded before every `Runtime.enable`); a
  long-lived launcher (the MCP server) reaps the Chrome it spawns, so a
  crashed-and-relaunched browser can't accrete zombies; the Native-Messaging host
  bounds its response write; the accessibility tree, PDF, and full-page
  screenshots are file artifacts bounded by the transport cap.
- **Sanitization** — every page-controlled string reaching an agent-facing text
  surface is neutralized (bidi / zero-width / control characters) and flood-capped
  in both modes, so a hostile page can neither spoof a forged element row nor
  flood the output. The raw-retrieval channels (`dom get`, `fetch` body, `eval`
  result, `session export`, cookie values in the JSON channel) stay full by
  design. Session import treats `__proto__` / `constructor` as literal storage
  keys — no prototype pollution.
- **Policy** — the effect-keyed gate is complete and exhaustive-by-construction
  (`device` and `context_close` included), enforced at the one browser-reaching
  sink in each mode, with the host re-parsing every wire command to a typed value
  before enforcing, and a single store under the durable data root that survives
  cache eviction.
- **Headless ↔ browser parity** — each shared concern is one named operation
  (`Transport` / `run<T>` handlers; the `do_*` ↔ service-worker-module mirror
  guarded by `browser_parity.rs`; one discard-before-enable invariant per mode),
  so the two modes cannot silently drift.

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
