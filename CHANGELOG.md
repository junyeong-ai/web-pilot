# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- `status` validates the manifest against every field Chrome requires to launch
  the host — a present `description` and an absolute, launchable `path` included
  (both verified required against Chrome for Testing) — so a manifest missing
  either is reported as malformed instead of a false "OK".

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
