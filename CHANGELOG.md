# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- **A click that starts several downloads no longer reports only the first
  (headless).** The watch ended when every transfer it had already heard about
  finished — but `settled` says a transfer is done, never that the command is
  done starting them, so the absence of an announcement was read as the absence
  of a download. One click that exports two files, or exports one and schedules
  another a beat later, returned naming one: measured, a second export 300 ms
  behind the first was missed on every run, the command answering at 55 ms. The
  action response is the only record a download ever gets — there is no
  after-the-fact listing — so the file was invisible, in a directory named by
  GUIDs. The window now belongs to the command: one that has started a download
  (or was told one is coming) watches out its budget before returning, and only
  then reports. Commands that download nothing are unchanged and pay nothing; a
  command that does now takes that budget (~2s) to answer.

## [0.9.1] - 2026-08-19

### Fixed

- **A headless `status` with nothing running no longer reads like a broken
  install.** `Connected: false` there is the resting state — the session is
  started by the first command that needs a page, and `status` deliberately does
  not start one — but the render said only that, so someone checking why a
  command had not worked saw two bare lines and concluded the install was
  broken (reported). Browser mode's not-connected paths have always explained
  themselves; the headless one now does too, naming what starts a session and
  that `--browser` reports a separate connection. The JSON is unchanged.

## [0.9.0] - 2026-08-19

### Fixed

- **An empty `console read` / `network read` no longer means two different things
  (both modes).** A buffer with nothing in it read identically whether the page had
  reported nothing or nothing had been watching when it did — which is exactly how
  a "console errors" check passes a deploy whose bundle threw on load. Some
  documents are genuinely out of reach: one built while no WebPilot process was
  attached (the page navigated itself between two commands, or opened a popup,
  already loading when its target appears), and every document in browser mode,
  which injects at navigation settle because a document-start MAIN-world injection
  in the user's own Chrome is either a content script matching every tab they
  browse or a session-long debugger banner. That cannot be closed by attaching
  harder, so the reads now SAY it: each carries `covers_load`, stamped by the
  recorder from the null `documentElement` that only a document-start injection
  sees, and the human render adds `--- recorder installed after this document
  started — anything reported before then is not in this buffer ---`. Silence now
  means coverage. In headless, re-driving the load (`action reload`) closes the
  gap and the next read says so.

- **A session served by one transport (`webpilot mcp`, the NM host) no longer
  drifts from the browser it drives (headless).** Which page a command acts on was
  resolved once, when the transport opened, so a tab that closed between two
  commands of the same session — the session's own `tab close`, another process's,
  the page's own `window.close()` — left every later command running against a
  dead CDP session and answering an infra `ConnectionLost` where the truth was a
  typed `TabNotFound`; the MCP server then threw the whole transport away and
  reopened, losing the frame scope and the monitor registrations with it. Device
  emulation had the same shape with a worse symptom: it was applied only at open,
  and its two halves have different lifetimes — the metrics override outlives the
  CDP client that set it while the user-agent override reverts with it — so a
  `device set` from another process landed a mobile viewport on a live session
  behind an unchanged desktop user agent, a spliced identity WebPilot created and
  reported nowhere, and binding another tab (`tab new`, `tab switch`, an adopted
  popup) dropped the emulation entirely, in the CLI too: the new tab's own load
  carried the real user agent and viewport. The page binding and the emulation are
  now re-established per command at the sink every command passes, next to the
  download disposition and the monitor hooks that already were — read from the pin
  and the persisted record rather than from what the process set up at launch, with
  CDP work only when something actually changed. `tests/e2e_mcp.rs` drives the real
  stdio JSON-RPC surface to hold it.

- **The tab a headless session acts on no longer changes without the agent
  asking (headless).** The active-tab pin was written only by `tab switch` /
  `tab new` / `tab close`, so any command that had to DERIVE a page — a fresh
  attach, or the recovery after the pinned tab closed — picked one from
  `Target.getTargets` and left nothing behind: the next CLI process, being a
  fresh process, picked again from a list Chrome orders by its own map over
  random target GUIDs, so two consecutive commands could act on different tabs.
  Worse, the dead-pin signal was consumed by whichever command RESOLVED it
  first, including commands that never report it — after a `tab` list or a
  `cookie list`, the page command that followed ran on an arbitrary survivor and
  returned success, the silent retarget the pin exists to prevent. Every derived
  target is now pinned where it is chosen, so separate processes agree; the dead
  pin stays on disk until the one loud `TabNotFound` announces it, and that
  report is what moves the pin onto the fallback. A `tab` list marks no tab
  active while the pin is dead, and an explicit `tab switch` / `tab new` answers
  a dead pin instead of leaving a long-lived session (`webpilot mcp`) reporting
  the tab the agent already left.

- **An armed console / network monitor no longer misses what a page does while
  it loads (headless).** The hooks were injected once a navigation settled, so
  everything the new document logged or fetched from its own scripts — the whole
  parse-to-ready window, which is the entire lifetime of a page that throws on
  load — was recorded nowhere, and `console read` answered with the same empty
  buffer a genuinely quiet page gives. They are now registered per document
  (`Page.addScriptToEvaluateOnNewDocument`), so they are in place before the
  document's first script runs: a page's startup `console.log` and `fetch` land
  in the buffer on every path a WebPilot process drives a document into being —
  `navigate`, `reload`, history, a click that lands on a new page,
  `capture --url`, `tab new` — and on a redirect the page fires itself while a
  process is alive to see it, which for `webpilot mcp` is the whole session. Two
  documents stay out of reach, both because nothing was attached when they were
  built: one loaded between two CLI invocations, and a popup the page opened,
  which is already loading when its target appears. Browser mode keeps injecting
  at settle: it drives the user's own Chrome, where a document-start MAIN-world
  injection is either a registered content script, which matches URLs and so
  would reach every tab the user is browsing, or a debugger attach held for the
  session, which leaves Chrome's debugging banner up.

### Added

- **`console read` reports what the page's console shows, not only what it was
  asked to print.** An uncaught exception and an unhandled rejection are what a
  broken page produces instead of a log line, and neither was a `console.*` call,
  so neither reached the buffer: a deploy whose bundle threw on load read exactly
  like a deploy with a clean console. They are recorded now, each typed by a new
  `source` field (`console` / `exception` / `rejection`) rather than flattened
  into an `error` an agent would have to tell apart by reading the message —
  `level` stays the console API's own taxonomy. An exception carries the
  browser's own text and the location it names ("Script error." with none, for a
  cross-origin script, exactly as the console prints it), so
  `console read --level error` is now the "did this page break" question. A
  FAILED `console.assert` joins them, under the console spec's own "Assertion
  failed" label; a passing one prints nothing and records nothing.

  Two things are deliberately not recorded. An error the page cancels
  (`event.preventDefault()`) is one the browser does not print either, so
  recording it would fail a page whose console is clean. A subresource that fails
  to load fires its event at the element and names no reason there — not the
  status, not even whether the request was refused or the bytes were unusable —
  so an entry made from it could only say that something failed, in words
  WebPilot composed. The console shows such a failure and WebPilot still reports
  it nowhere: the network monitor watches `fetch`/XHR, not element loads.

### Changed

- **A document hooked by another build's recorder is a typed error, not an empty
  list.** Chrome outlives the process that installed a recorder, so an upgraded
  binary meets documents still running an older one, whose entries carry a shape
  it does not know — they were dropped one at a time and the read answered
  `entries: []`, the shape a quiet page gives. The recorder now stamps the shape
  it writes and the read checks that stamp, so the verdict is about the recorder
  rather than inferred from the entries: a page writing junk of its own still has
  it dropped and the real entries still read, and the answer does not change with
  the `--since` window being asked for.
- **The `eval` gate is now re-checked against monitor injection before every
  command, not after every navigation.** A deny that lands mid-session removes
  the registration rather than merely skipping the next re-arm, so a long-lived
  process (`webpilot mcp`) enforces it as promptly as a fresh CLI invocation
  does. `console read` on a document the deny kept the monitor out of stays the
  typed `InvalidArgument` it was, never an empty success.

## [0.8.1] - 2026-08-18

### Added

- **The installer asks nothing by default.** A `curl | bash` run on a terminal
  used to stop and ask which build to take, then hand the terminal to an
  interactive `webpilot setup` — so the one-line install was not actually one
  line, and the only escape (`WEBPILOT_NO_SETUP=1`) skipped setup rather than
  completing it. It now takes the prebuilt binary and finishes setup unattended,
  where each prompt resolves to its safe answer, and every decision has a flag
  (`--source`, `--version`, `--install-dir`, `--no-setup`, `--help`).
  `--interactive` restores the guided prompts.
- **Build provenance can be verified.** The release workflow has always attested
  its artefacts, but nothing checked them: a SHA-256 sidecar travels the same
  channel as the archive, so it proves the transfer and not the origin.
  `webpilot self update --verify-attestations` and `install.sh
  --verify-attestations` now tie the bytes to a run of this repository's
  workflow via the `gh` CLI. Opt-in, and hard-failing when asked for — a
  verification the caller requested and did not get aborts the install.

### Changed

- **`webpilot uninstall` is now `webpilot self uninstall`.** It removes the
  running binary along with everything that binary deployed — the same object
  `self update` replaces — so the two halves of one lifecycle no longer sit in
  different namespaces. The old spelling keeps working, unlisted, so installed
  copies of `scripts/uninstall.sh` are unaffected.

### Fixed

- **A link targeting a frame by name no longer stalls the click.** A `target`
  that is not a keyword names a browsing context, and the clicking frame can read
  only its own name and a same-origin ancestor's — so a link into a named child
  frame, the shape a frameset-style console is built from, looked like it opened
  a new window. Nothing then appeared to adopt, which is exactly what a download
  that discards its own tab looks like, so every such click sat out the whole
  download watch before returning. The name is now resolved against the frame
  tree, which carries each context's live name: a match means the click loads
  that frame, and the command waits for that frame to commit rather than for a
  context that was never going to appear — returning as soon as it does. The
  tree is the authority on purpose — an `<iframe name>` attribute only seeds the
  name, so a frame that has since renamed itself answers to the new name alone
  and a link carrying the stale attribute still opens a context, exactly as the
  browser does it. A named-frame link whose response turns out to be an
  attachment commits nothing, and that is the case the wait still covers, so the
  download is reported rather than lost to a command that returned first.
- **`self update` refreshes the Claude skill.** It already re-deployed the
  extension, whose drift the host rejects at connect time; the skill had no such
  gate, so an updated binary was left described by its predecessor's
  documentation and nothing said so. It could not simply be overwritten either:
  the skill is the one deployed artefact a user may legitimately edit, and by
  content alone a stale copy and an edited one are the same thing. Each install
  now records the digest it wrote, so a later one can tell its own copy — which
  it refreshes silently — from the user's, which it keeps and names in the
  report. Only a build that records can be recognised, so the hop out of a
  release that did not (0.8.0 and earlier) still reports the skill as kept; the
  outgoing build claims its own copy on the way past, which makes every hop
  after this one automatic.

## [0.8.0] - 2026-08-18

### Added

- **The Native Messaging host writes a log.** Chrome owns that process's stdio,
  so in browser mode the host's own account of a session reached nobody: a
  failure there left nothing to read. It now writes `logs/host.log` under the
  runtime root, rotating to `host.log.1` at 1 MB — renamed rather than
  truncated, so the previous session survives. The CLI keeps stderr, where its
  caller does capture it.
- **Downloads are a reported command outcome, and the files are WebPilot's.** A
  navigation that resolves to an attachment is a stay-put — the page never moves
  — so a command that downloaded a file used to return success on an unchanged
  snapshot with nothing to show for it, and an agent reading that as a no-op
  retried and downloaded again. `capture`, every action, and `tab new` now carry
  a `downloads` list naming what was written. Files land under
  `artifacts/downloads/<browser-context>/`, partitioned like cookies and storage
  already are, rather than in the user's OS download folder where nothing
  WebPilot owns would reclaim them. Chrome names each file by its download id
  (`allowAndName`), so a server's `Content-Disposition` can no longer choose a
  path on disk; the name it suggested travels as metadata.
- **`policy set --operation download`.** The verdict selects Chrome's own
  download behavior, so a `deny` refuses the transfer in the browser instead of
  cancelling it after the bytes start. A refused download is still reported, and
  carries no path.

### Changed

- **Artifacts expire.** Screenshots, PDFs, accessibility trees, exported
  sessions and downloaded files are each minted under a fresh name, so the
  directory had no bound at all and only ever grew. They are now swept a week
  after they are written (`[artifacts] ttl`, `WEBPILOT_ARTIFACT_TTL`). The sweep
  is due at most hourly and runs from the sinks every command passes, since a
  Chrome session outlives any one process; it cannot race a capture, because a
  file being written has an mtime days newer than the cutoff.
- **A capture lists at most `[capture] max_elements` (1000) interactive
  elements.** Page text, element text, option lists and the shadow walk were all
  bounded; the index — the largest part of a capture — was not, and an ordinary
  encyclopedia article reaches four figures of links on its own, costing tens of
  thousands of tokens in a single response. The bound is on what is rendered, not
  on what is extracted: the browser keeps the page's whole index, so an element
  past the listing stays addressable and `find` still matches it. A shortened
  capture sets `elements_truncated` and says so in the footer. `find` is bounded
  by the same knob — it reports the true match count and asks for a narrower
  filter rather than returning the whole page as rows.

## [0.7.1] - 2026-07-11

### Fixed

- **A click whose handler navigates programmatically (`location.href` /
  `location.assign` / `form.submit()`) now reliably reports `url_changed` and
  settles the destination.** Static link/form analysis can't predict a
  handler-initiated navigation, so the bridge now also listens for the
  Navigation API's `navigate` event during the click — a synchronous, reliable
  signal for a cross-document load (`destination.sameDocument === false`),
  distinct from a same-document hash/pushState. This removes the settle's
  dependence on the queued `frameStartedLoading` racing the click's response,
  which surfaced as a flaky `url_changed: null` under load.
- **Headless `tab switch` / popup adoption now commit the pin atomically.** The
  new page is fully primed (bridge world installed, main frame resolved) BEFORE
  the transport's pin is moved, so a tab that closes mid-switch leaves the
  current pin untouched instead of retargeting onto a page with no bridge world.
  Popup adoption depends on this: a failed switch keeps the pin on the opener
  rather than silently pointing at a half-primed popup. (`prime_page` is shared
  by `open` and the switch path so the two priming flows cannot drift.)
- **A CDP session command whose reply and the tab's close land in the same
  window no longer discards the delivered reply.** The flat-session response
  wait now polls the reply channel before consulting the session-liveness flag,
  so a completed command returns its result even if the tab closed an instant
  later — matching what the per-target socket delivered; the next command still
  gets the typed tab-gone signal.

## [0.7.0] - 2026-07-11

### Added

- **MCP: the tool surface now covers the full page-interaction loop.** Eleven
  new tools over the same handlers the CLI uses: `browser_hover`,
  `browser_focus`, `browser_scroll_to`, `browser_drag`, `browser_upload`,
  `browser_back`, `browser_forward`, `browser_reload`, `browser_find` (semantic
  filters with optional click/fill chaining, deserializing the same `FindArgs`
  the CLI parses), `browser_tabs` (list/new/switch/close/find), and
  `browser_frame` (list/switch/url/main). `browser_screenshot` gains
  `full_page` and `annotate`. Every tool now carries a display `title` and,
  where it differs from the spec defaults, `annotations`
  (`readOnlyHint`/`destructiveHint`/`idempotentHint`); every input schema sets
  `additionalProperties: false`. Environment management (cookies, session,
  device, policy, monitors) stays CLI-only by curation.
- **MCP: protocol version negotiation.** `initialize` echoes the client's
  requested revision when supported (2025-11-25, 2025-06-18) and answers with
  the newest otherwise, per the MCP lifecycle.

### Fixed

- **`find` rejects `--click` combined with `--fill` at the handler**, not only
  in the CLI parser — the MCP `browser_find` path reaches the handler without
  clap's `conflicts_with`, and would otherwise silently run the click and drop
  the fill text.

### Changed

- Headless CDP now uses one WebSocket to the browser endpoint and drives the
  pinned page through a flat-protocol session (`Target.attachToTarget
  { flatten: true }`) on it, replacing the per-target socket. A cross-site
  navigation no longer reconnects and re-primes a fresh socket — the session is
  attached to the target and survives the renderer swap, so only document state
  resets. Each session's events are filtered to its `sessionId` (preserving the
  per-page event isolation the dedicated socket gave), and a session detach ends
  an in-flight `wait` at once instead of at its deadline. A cross-site swap
  force-re-emits the new document's execution contexts so a dropped
  `executionContextCreated` can't strand the bridge, and `CdpSession::Drop`
  reaps every task it spawned so the connection can't leak across a long-lived
  server's Chrome-restart cycles. A tab switch re-attaches a session instead of
  opening a new socket. `cdp.event_buffer` default raised 256 → 512.
  (`docs/cdp-flat-session-migration.md` records the design, and why the
  follow-on OOPIF phase was rejected after headless Chrome was measured NOT to
  expose cross-site iframes as attachable targets.)
- Toolchain: Rust 1.97.0; the workspace declares `rust-version` so an older
  toolchain fails with a clear MSRV error instead of a rustc parse error.
- CI: third-party actions are pinned to full commit SHAs (Dependabot keeps
  them current); `actions/checkout` v7; the node24 opt-in env var removed
  (default since 2026-06-16); Dependabot applies a 7-day cooldown to version
  updates and groups security updates into one PR per ecosystem.
- `scripts/install.sh` / `scripts/uninstall.sh` run every side-effecting step
  from a `main` invoked on the file's last line, so a truncated `curl | bash`
  stream defines functions and stops instead of executing half an install;
  the installer also detects Apple silicon under Rosetta
  (`hw.optional.arm64`) and installs the native arm64 binary.
- `docs/cdp-flat-session-migration.md` records the CDP transport direction:
  the planned flat-session migration (OOPIF support), why the in-page monitor
  hooks are the architecturally forced design for cross-process monitoring,
  and why WebDriver BiDi is rejected for a Chrome-only tool.

## [0.6.23] - 2026-06-15

### Fixed

Two more siblings of the 0.6.22 event-ring-lag bug — a settle path that trusts a
CDP event's absence misfires when a busy page's event burst evicts that event
from the broadcast ring. Both now confirm against an authoritative observable:

- **Headless: a navigating `action click` no longer reports `url_changed: null`
  (no navigation)** when the click's `Page.frameStartedLoading`/commit event was
  dropped under lag. The click-settle drain treated a `Lagged` as an ordinary
  empty drain and returned the PRE-click URL, so the agent saw "nothing
  happened" for a click that did navigate. On a lagged drain it now reads the
  live target URL (the same authoritative fallback the live commit-wait already
  used), so `url_changed` reflects where the click actually landed.
- **Headless: a click-opened popup is now adopted even when its
  `Target.targetCreated` event was dropped** under lag. The adoption drain
  returned `None` (popup not followed, pin left on the opener) on a `Lagged`; it
  now falls back to enumerating live targets (`Target.getTargets`) for the
  opener's child page, adopting it — or, if the enumeration is ambiguous (more
  than one match), declining rather than pinning the wrong tab.

## [0.6.22] - 2026-06-15

### Fixed

- **Headless `action back` / `forward` no longer falsely reports
  `NavigationFailed` for a same-URL or same-document history hop** when its
  navigation event was dropped. A busy page's event burst can overflow the CDP
  event ring and evict the very `Page.frameNavigated` /
  `Page.navigatedWithinDocument` the settle awaits; the wait then can't tell that
  from a genuine no-op, and the URL-moved fallback misses a hop whose URL is
  unchanged (a `pushState` entry, a same-URL back). The traversal is now
  confirmed against the navigation history's **current index** — the definitive
  position signal, which moves iff a real back/forward landed (whatever the hop's
  document or URL) and survives a dropped event. A genuine no-op (no index move)
  still surfaces the typed `NavigationFailed`.

### Changed

- **The content bridge's `back` / `forward` / `reload` cases now return the same
  "dispatched via CDP, not bridge" routing-mismatch error** as the other
  CDP-dispatched action kinds (`navigate` / `upload` / `drag` / `hover` /
  `key_press`), instead of silently performing the action. These cases are
  unreachable (both drivers handle history and reload before the bridge
  fall-through), but a bridge `history.back()` / `location.reload()` would have
  returned success the instant it fired, skipping the driver's navigation
  settle — so the uniform "a routing mismatch is a loud error, never a silent
  unsettled half-action" invariant now holds for every CDP-dispatched kind.

## [0.6.21] - 2026-06-15

### Fixed

- **Browser mode: the CLI now bounds the size of a host response it reads.** The
  client read the host's reply with an unbounded `read_line`, so the timeout
  capped the wait but not the bytes — a peer that streams a giant body without a
  newline could grow CLI memory toward OOM. It now caps the read (mirroring the
  host's existing inbound request cap), with a ceiling above the largest a real
  response can be (the 100 MiB an extension can hand the host). The 0600 socket
  makes this robustness/hygiene rather than a trust boundary, but the two
  directions are now symmetric.
- **A snapshot's `page_url` / `page_title` are now clipped at the source**, like
  every per-element field already is. `location.href` (a multi-MB `data:`/`blob:`
  URL) and `document.title` (page-settable to any length) otherwise crossed the
  wire untruncated — the same balloon-the-snapshot-past-the-transport-cap failure
  the per-element `href` clip prevents, but for the whole capture. Clipped to 2048
  (URL) and 200 (title); real pages are unaffected and the rendered output is
  unchanged.

## [0.6.20] - 2026-06-15

### Fixed

- **Browser mode: the NM host now forwards a freshly-built command envelope**
  instead of the caller's raw JSON mutated in place. It re-serialized
  `command` (stripping unmodeled fields INSIDE it) but preserved arbitrary
  top-level SIBLING fields — so a non-CLI socket writer could attach
  `result: { type: "Config" }`, which the service worker applies and then
  early-returns on, dropping the gated command (the CLI hangs out its full
  response timeout) while adopting attacker-chosen config. The host now emits
  only the three protocol-defined fields (`id` / `command` / `monitor_policy`),
  discarding every other caller field by construction — completing the
  "forward only what policy validated" intent the command re-serialization began.
- **Headless `device set` / `device preset` is now all-or-nothing.** The three
  CDP overrides (metrics → touch → user agent) are not a transaction: a failure
  on the 2nd or 3rd left the earlier one live in Chrome while the caller errored
  and persisted nothing — so a later session believed no device was set while
  Chrome kept emulating a half-applied one. On any failure `apply` now rolls
  every override back to default before surfacing the error.
- **Headless `device reset` now reports a failure to remove the persisted device
  file** instead of swallowing it. A silent failure left the file in place, so
  the next `open` re-applied the device the user had just reset. (An
  already-absent file is still success.)
- **MCP `browser_screenshot` / `browser_snapshot` now reject unknown arguments.**
  Their schemas advertised no input but omitted `additionalProperties: false`, so
  a client guessing at an unsupported option (e.g. `full_page` on a screenshot)
  had it silently dropped and got a different result than it asked for. The
  schema now forbids unknown properties, matching the action tools.
- **Headless `record --dom` no longer leaves an orphaned screenshot** when a
  frame's DOM capture fails. Each frame now captures both the screenshot and the
  DOM before writing either file, so a DOM failure aborts the frame cleanly
  rather than leaving a `.png` with no matching `.dom.json`.

## [0.6.19] - 2026-06-15

### Fixed

- **A misconfigured `config.toml`/`WEBPILOT_*` timeout is now rejected at startup
  instead of panicking mid-operation.** `settings::validate` already refused a
  ZERO deadline/interval (it would fail-instant or busy-spin); it now also
  refuses one larger than `i32::MAX` ms (~24.8 days) — the same bound the
  agent-facing `wait` timeout already clamps to. Past it, the `Instant + Duration`
  deadline (or `sleep`) that every timeout feeds can overflow and panic on some
  platforms, deep inside an operation, far from the bad value. The whole-config
  load (`settings::init`, run by both the CLI and the NM host) now reports the
  offending key up front — loudly, never a silent clamp, consistent with how this
  layer already handles a zero. `annotation_paint` (where zero legitimately means
  "no delay") is bounded above too, since an astronomical paint sleep overflows
  the same arithmetic.

## [0.6.18] - 2026-06-15

### Fixed

- **Browser mode: a `console read` / `network read` no longer collapses to a
  misleading `ConnectionLost`** when a page-reachable MAIN-world monitor buffer
  holds a tampered `timestamp` or `duration_ms`. The service-worker sanitizers
  type-checked these with a bare `typeof === "number"`, so a non-integer/negative
  `timestamp` (`1.5` / `-1`) or non-finite `duration_ms` (`NaN` → JSON `null`)
  passed the filter but then failed the CLI's whole-response decode into Rust
  `u64`/`f64` — breaking the entire read, where headless tolerates the identical
  input. The sanitizers now mirror headless exactly: console coerces a bad
  `timestamp` to `0` and keeps the entry (`as_u64().unwrap_or(0)`), and network
  drops only the malformed entry and keeps the rest (per-entry `from_value().ok()`).
  Monitors are best-effort against a hostile page by design, but a tampered entry
  must degrade gracefully and consistently across modes, not fake an infra error.

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
