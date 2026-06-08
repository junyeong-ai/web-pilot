# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
