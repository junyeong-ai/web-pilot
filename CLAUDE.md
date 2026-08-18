# WebPilot

Chrome browser-control tool for AI agents. Single Rust binary, two modes:
headless (default) and browser. The same engine is also exposed as an **MCP
server** (`webpilot mcp`, stdio JSON-RPC), reusing the same `Transport` and
command handlers.

## Build & Run

```bash
cargo fmt --all -- --check     # CI gates on this — run it in every local pass too
npx oxlint@1.78.0 --deny-warnings extension/   # the extension's gate (.oxlintrc.json)
cargo build --workspace --release
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings

webpilot capture --include dom --url "https://example.com"   # headless (default)
webpilot --browser capture --include dom                     # browser mode (SSO Chrome)
webpilot --context agent-1 capture --include dom             # multi-agent isolation
webpilot mcp            # MCP server (stdio); honors --browser / --context
webpilot status / webpilot quit
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

- `crates/webpilot/` — wire types + protocol (see that directory's `CLAUDE.md`)
- `crates/webpilot-cli/` — the single binary (see that directory's `CLAUDE.md`)
- `extension/` — browser-mode Chrome extension (`bridge.js` contract: `.claude/rules/extension.md`)
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
`tests/browser_parity.rs` fails the build if any `Command` variant lacks a
router case, and `oxlint` gates the extension the way `clippy` gates the crates. Add a `bridge.js` case only when new content-script behavior is
needed. Gate a command by adding an arm to `protocol::Command::policy_key()` —
that match is exhaustive too, so a new command **must declare its gate** and
cannot leak ungated (enforcement runs automatically at each privileged sink).
MCP exposes a curated subset of commands — adding a command is not an MCP change.

## DOM Output Format

```
*[1] input#query "Search" type=text autocomplete=search @search
[2] button "Go" @search
--- Page: Example (https://example.com) ---
--- Scroll: 25% (0.5 above, 1.2 below) ---
--- 1 iframe(s) not shown — list: webpilot frame, enter: webpilot frame url <pattern> ---
```

`[index]` is the argument to `action click N`. **Indices are bound to the last
`capture` snapshot**: bridge.js stores the element references seen at capture
time, and index actions resolve against that list. If there is no snapshot (no
capture yet) or the element has left the DOM, the result is a typed
`StaleSnapshot` error (exit 4), never a re-resolution against the live DOM.
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
matches it. The extraction-side caps live in `bridge.js`, the one place both
modes share: page text, element text, option lists, and the shadow walk.

## Wire Protocol

| Area | Rule |
|---|---|
| Action | `{"kind": "click", "index": 7}` — one definition (clap + serde, snake_case) |
| ActionKind | snake_case wire tag, matching `Action.kind` exactly |
| PolicyKey | Effect-keyed gate: `ActionKind` ∪ {`eval`, `fetch`, `dom_set`, `tab_close`, `cookie_list`, `cookie_set`, `cookie_delete`, `session_export`, `session_import`, `device`, `context_close`, `download`}. Keyed by **effect, not command** — `navigate` gates every URL-load (`navigate` action, `capture --url`, `tab new URL`); `download` gates the file a page makes the browser write, whatever started it — a `deny` becomes Chrome's own `Browser.setDownloadBehavior` refusal, so the transfer never begins; `eval` gates every MAIN-world JS sink (`eval`, `frame find`, `console`/`network start`, `dom set-html`). `Command::policy_key()` maps command→key via an **exhaustive match** (reads → `None`), so a new command can't compile without declaring its gate. Enforced at the browser-reaching sink — `LocalTransport::send` (headless) / NM host (browser), never the CLI `IpcTransport`; the host re-parses the wire value to a typed `Command` before enforcing (blocks a Rust-rejects/JS-coerces bypass). Store `policy/policies.json` under the durable data root (survives cache eviction). Per-effect gating rationale lives in the `PolicyKey` variant doc-comments + the `policy_key()` arms. |
| Wait | `Wait { condition, timeout_ms }`; `condition` is the `until`-tagged `WaitCondition` — `{"until": "selector", "value": ".loading"}`, one of `selector`/`text`/`navigation`/`idle` |
| Capture | `{"include": ["dom","screenshot"], "opts": {...}}` |
| Status | `{connected, mode: "headless"\|"browser", chrome_version, extension_version}` — per-mode semantics |
| Errors | `{"code": "ElementNotFound", "message": "...", "requested": 5, "available": 3}` |
| FrameSelector | `{"by": "url", "pattern": "/auth/"}` — headless supports Name/Url/Predicate too (execution-context routing) |
| DomProperty | `{"kind": "html"}` or `{"kind": "attr", "name": "href"}` |

Display/FromStr for single snake_case enums are derived via `serde_plain` — no
hand-written match tables.

## Output Modes

- **Terminal**: human → stderr, content → stdout
- **Piped**: stdout not a TTY → JSON automatically
- **Forced**: `--json` flag

`CommandOutput` enum → `output::render()`, a single conversion.
`CommandOutput::to_agent_text()` reuses the same renderers for MCP tool results.

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

`StaleSnapshot` = an index's element left the DOM since capture (re-capture
needed). `VersionMismatch` = installed extension version ≠ bundled version
(`webpilot setup extension`, then reload).

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

```
$WEBPILOT_HOME              explicit override
~/Library/Caches/webpilot   macOS
$XDG_RUNTIME_DIR/webpilot   Linux/BSD (tmpfs, mode 0700) — preferred
$XDG_CACHE_HOME/webpilot    Linux fallback, then ~/.cache/webpilot
/tmp/webpilot-<user>        last resort
```
(full resolution order in `dirs.rs`)

Subdirectories: `runtime/` (sockets, PIDs, locks), `contexts/` (multi-agent),
`logs/` (`host.log` + one rotated predecessor — browser mode only: Chrome owns
the NM host's stdio, so its account of a session reaches nobody otherwise; the
CLI keeps stderr, which its caller does capture),
`artifacts/` (screenshots, PDFs, sessions, plus `downloads/<browser-context>/`
for files a page downloads), `chrome-profile/`. Artifacts are swept at session
launch once past `[artifacts] ttl` (7d) — every one is minted under a fresh name,
so nothing else bounds the directory. The **policy store**
(`policy/policies.json`) lives instead under the durable data root
(`$WEBPILOT_DATA_HOME` / `~/Library/Application Support/webpilot` /
`$XDG_DATA_HOME` / `~/.local/share/webpilot`), or under `$WEBPILOT_HOME` when set:
a security config must survive the cache eviction the paths above are subject to.

Settings resolve through one layer, `webpilot::settings`: **defaults <
`config.toml` < env var**. Tune via `config.toml` (under the cache root —
`dirs::config_file_path()` — override the path with `WEBPILOT_CONFIG`) sections
`[timeouts]`/`[chrome]`/`[context]`/`[artifacts]`/`[cdp]`/`[capture]`, or `WEBPILOT_*` env vars
(e.g. `WEBPILOT_NAVIGATION_TIMEOUT_MS`).
Only path resolution is env/platform-specific (`dirs`, to avoid cycles).
