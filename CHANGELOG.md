# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
