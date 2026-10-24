# WebPilot

Chrome browser-control tool for AI agents. Single Rust binary, two modes:
headless (default) and browser. The same engine is also exposed as an **MCP
server** (`webpilot mcp`, stdio JSON-RPC), reusing the same `Transport` and
command handlers.

## Build & Run

```bash
# Every local pass runs the gates CI runs:
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
RUSTDOCFLAGS='-D warnings -D rustdoc::broken-intra-doc-links' cargo doc --workspace --no-deps --locked
npm ci && npm run lint       # generated semantics + extension lint; Node in .node-version
shellcheck scripts/*.sh
# CI also checks the declared MSRV: cargo +<workspace rust-version> check --workspace --locked
# After changing accessibility dependencies: npm run build, then npm run lint

# The behavioural suites launch a real Chrome, so they are opt-in (CI runs each in its own job):
WEBPILOT_CHROME="/path/to/Chrome for Testing" \
  cargo test -p webpilot-cli --locked --test e2e_headless -- --ignored
# The same invocation applies to --test e2e_mcp and --test e2e_browser.

webpilot capture --include dom --url "https://example.com"   # headless (default)
webpilot capture --browser --include dom                     # browser mode (SSO Chrome)
webpilot capture --context agent-1 --include dom             # multi-agent isolation
webpilot mcp            # MCP server (stdio); honors --browser / --context
webpilot status
webpilot quit
```

## Architecture

```
   commands/<X>.rs ─┐                ┌─ IpcTransport ──→ Unix socket → NM Host → Extension → bridge.js
   (CLI surface)    ├─→ Transport ──→│
   mcp.rs tools ────┘                └─ LocalTransport ─→ CDP WebSocket → bridge.js (injected)
   (MCP surface; reuses the same handlers)
```

Command handlers are written **once** as `run<T: Transport>`; the two `Transport`
implementations decide the mode. Headless absorbs the NM Host + Extension infra
into in-process Rust. `webpilot mcp` layers a stdio JSON-RPC (MCP) adapter over
the same `Transport`: each tool builds a typed `Command`/`Action` and runs it
through the same handler, inheriting rendering, policy, and mode — no second
implementation.

**Two process lifetimes, one browser.** A CLI invocation is a fresh process per
command; `webpilot mcp` and the NM host are single processes that serve every
command for a whole session — and Chrome outlives them all. So per-process state
is only ever right for one of the two: cache something the browser owns and the
long-lived process believes a setting another process already cleared, while
work hung off startup or off `LocalTransport::open` runs once and never again.
Anything a command depends on is re-established **per command**, at the sink
every command passes (`LocalTransport::send`, the host's request path): which page
is bound, the download disposition, the device emulation, the monitor hooks — each
read from what the browser and the persisted record actually say, never from what
this process set up at launch. `tests/e2e_mcp.rs` drives `webpilot mcp` over its
stdio JSON-RPC to hold that half of the contract.

- `crates/webpilot/` — wire types + protocol (see that directory's `CLAUDE.md`)
- `crates/webpilot-cli/` — the single binary (see that directory's `CLAUDE.md`)
- `extension/` — browser-mode Chrome extension, and the page-side scripts both
  modes share: `content/bridge.js` (isolated world) and `content/monitor-*.js`
  (MAIN-world recorders), each embedded by the Rust crate and injected by the
  extension (`bridge.js` contract: `.claude/rules/extension.md`)
- Rust conventions: `.claude/rules/rust-conventions.md`

**Adding a command** (both modes at once) — the Rust edits are all **exhaustive
matches, so the compiler forces every one**: a `protocol::Command` variant +
`commands/mod.rs` (`pub mod` + enum variant) + a `commands/<x>.rs` handler +
`cli.rs::Cmd::execution()` classification + a `cli.rs::dispatch_via_transport`
arm + a `LocalTransport::send` arm and its `do_*` body + a
`command_needs_active_page` arm (also exhaustive — classify whether the command
acts on the pinned page) (headless). The browser
side adds a case in `extension/background/router.js` and its handler in the
domain module that mirrors the Rust file (`action`/`capture`/`query`/`state`/
`browser`.js ↔ the same-named `.rs`) — JS, so not compiler-checked, but
`tests/browser_parity.rs` fails `cargo test` if any `Command` variant lacks a
router case, and `oxlint` gates the extension the way `clippy` gates the crates. Add a `bridge.js` case only when new content-script behavior is
needed. Gate a command by adding an arm to `protocol::Command::policy_key()` —
that match is exhaustive too, so a new command **must declare its gate** and
cannot leak ungated (enforcement runs automatically at each privileged sink).
MCP exposes a curated subset of commands — adding a command is not an MCP change.
Also classify the command in `Command::unconfirmed_operation()`: when an IPC
reply is lost after dispatch, effects become `OutcomeUnknown` while reads retain
the connection error. This exhaustive classification is separate from policy.

## DOM Output Format

```
*[1] input#query role=textbox "Search" type=text autocomplete=search @search
[2] button "Go" @search
--- Snapshot: ID ---
--- Page: Example (https://example.com) ---
--- Scroll: 25% (0.5 above, 1.2 below) ---
--- 1 iframe(s) not shown — list frames, then switch to one by URL pattern ---
--- 2 elements (from 120 nodes, 5ms) ---
```

`[index]` and `snapshot_id` identify an observed element. Indexed CLI actions
require `--snapshot-id ID`; MCP indexed tools require `snapshot_id`. A newer DOM extraction (including `find`) in that frame or a different document, removed/hidden nodes, or changed accessible
names, roles, hrefs or input types returns `StaleSnapshot` (exit 4).
`*` marks an element new since the last capture — detected by **node identity**
against the previous snapshot, suppressed on the first capture in a new document
(a fresh page starts with no snapshot, so it is not "all new"; a same-document
`pushState`/hash change keeps the baseline, so elements it adds are flagged).
`@landmark` is semantic context.
`--- N iframe(s) not shown ---` is the count of HTTP iframes outside the active
frame (`DomSnapshot.subframes`); enter one with `frame url <pattern>` (or
`frame switch <name>` for a named frame).
`--- shadow DOM clipped (host budget exceeded) — some controls may be omitted ---`
appears when a shadow-component-heavy page exhausts the traversal budget
(`DomSnapshot.shadow_truncated`), so the index may be incomplete.
`--- index shortened ---` appears when the page carries more interactive
elements than `[capture] max_elements` renders (`DomSnapshot.elements_truncated`).
That bound is on the RENDER, applied once in `CommandOutput::dom` so no surface
can emit an unbounded index — the browser keeps the whole index, so an element
past it stays addressable and `find` (which renders only its matches) still
matches it. The extraction-side caps live in `bridge.js`, which both modes share:
page text, element text, option lists, and the shadow walk.
`--- N offscreen element(s) not listed ---` appears on an action's follow-up
capture (`--capture`, the MCP action tools), which lists only the elements in the
viewport and the ones the action added (`DomSnapshot::omit_offscreen`); the
omitted ones keep their indices. A capture lists the whole page.

## Wire Protocol

The wire shapes and their rules live with the types, in `crates/webpilot/CLAUDE.md`.

## Output Modes

- **Default**: text — content → stdout, notes → stderr, on a terminal and in a
  pipe alike (an agent's shell tool reads a pipe; the text render is written for it)
- **`--json`**: the structured form on stdout

`CommandOutput` enum → `output::render()`, a single conversion.
`CommandOutput::to_json()` supplies CLI JSON and MCP structured content;
`to_agent_text()` reuses the text renderers for MCP text blocks.

## Error Handling

| code | variant | exit |
|---|---|---|
| 0 | success | — |
| 1 | `Other`, `Session`, `ContextInUse` | unknown / session / context held by another live process |
| 2 | _(clap arg parse)_ | CLI usage error — unknown flag / non-numeric index / missing arg |
| 3 | `ConnectionLost`, `BridgeUnavailable`, `VersionMismatch` | infra |
| 4 | `ElementNotFound`, `StaleSnapshot`, `SelectorNotFound`, `TabNotFound`, `ContextNotFound`, `CookieNotFound`, `FrameNotFound` | not-found |
| 5 | `Timeout` | timeout |
| 6 | `PolicyDenied` | security |
| 7 | `InvalidArgument` | user error |
| 8 | `NavigationFailed`, `NoPage` | navigation |
| 9 | `OutcomeUnknown` | dispatched operation may already have run; observe before retrying |

`StaleSnapshot` = an observation no longer identifies an actionable element
(re-capture needed). `VersionMismatch` compares the connected extension with
the running host's bundled version. After updating the binary/assets, reload
the extension to replace the old host process too.

Guidance text is produced directly from data by `WebPilotError::Display` — no
message parsing or substring matching. External crate errors are wrapped into
`Other` at the `main::into_webpilot_error` boundary.

## Security Model — honest boundaries

- Policy is a **guardrail against a steered agent, not a sandbox against a
  malicious same-user process**: the store and `webpilot policy` belong to the
  same user the agent runs as. Protect them externally if that matters.
- `eval` is the master key: with `eval` allowed, narrower denies (navigate /
  fetch / cookie_list / session_export) are advisory — page JS reproduces those
  effects. Deny `eval` first; `policy default deny` + allowlist is the
  least-privilege mode.
- The `download` gate is headless-only. Browser mode drives the user's own
  Chrome, where `chrome.downloads` reports no initiating tab and offers no
  pre-transfer block: a report there could only be "some download happened while
  the command ran", which would credit the user's own browsing to the agent and
  leak its path, and a deny could only cancel after bytes had landed. Attaching
  the debugger per tab would scope it, but only by leaving Chrome's debugging
  banner up for the session. So browser-mode downloads follow the user's own
  browser — their download folder, their rules — and WebPilot neither reports nor
  gates them.
- Headless CDP is a `127.0.0.1` WebSocket on a random TCP port. A same-user
  local process can reach it directly, bypassing the in-process gate. Accepted:
  Chrome's pipe alternative cannot serve the reconnect-across-processes model
  (separate CLI invocations re-attach to one persistent Chrome), and a broker
  daemon to close a same-user-only exposure adds more failure modes than it
  removes. Browser mode does not share this: the only path to the authenticated
  Chrome is the NM host behind a 0600 socket, which parses and enforces policy.

## Runtime Paths

Two per-user roots, both mode 0700, resolved in `dirs.rs` (its module doc gives
the order per platform; `WEBPILOT_HOME` and `WEBPILOT_DATA_HOME` override):

- **Cache root**, evictable: `runtime/` (sockets, PIDs, locks), `contexts/`,
  `logs/` (`host.log` + one rotated predecessor; browser mode only, because
  Chrome owns the NM host's stdio), `artifacts/` (screenshots, PDFs, sessions,
  `downloads/<browser-context>/`), `chrome-profile/`, `config.toml`.
- **Data root**, durable: the unpacked extension, `policy/policies.json` and
  `skill-install.sha256`. A loaded extension, a security config and the skill's
  provenance digest must survive cache eviction. `policy/` follows
  `WEBPILOT_HOME` when it is set (`policy_dir_path`).

Artifacts are swept past `[artifacts] ttl_secs` (7 days); each is minted under a
fresh name, so nothing else bounds the directory. The sweep is due at most hourly
(`runtime/last-sweep`) and runs from the sinks every command passes, for the
reason "Two process lifetimes" gives.

Settings resolve through one layer, `webpilot::settings`: **defaults <
`config.toml` < env var**. Tune via `config.toml` (under the cache root —
`dirs::config_file_path()` — override the path with `WEBPILOT_CONFIG`) sections
`[timeouts]`/`[chrome]`/`[context]`/`[artifacts]`/`[cdp]`/`[capture]`, or `WEBPILOT_*` env vars
(e.g. `WEBPILOT_NAVIGATION_TIMEOUT_MS`). `[timeouts] download_window_ms` is the one a
caller trades directly: it is how long a command that started a download keeps
watching for the ones it started, so it sets both the answer time of such a
command and how deferred an export can be and still be reported.
Only path resolution is env/platform-specific (`dirs`, to avoid cycles).
