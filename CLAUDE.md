# WebPilot

Chrome browser-control tool for AI agents. Single Rust binary, two modes:
headless (default) and browser. The same engine is also exposed as an **MCP
server** (`webpilot mcp`, stdio JSON-RPC), reusing the same `Transport` and
command handlers.

## Build & Run

```bash
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
arm + a `LocalTransport::send` arm and its `do_*` body (headless). The browser
side adds a `service-worker.js` router case — JS, so not compiler-checked, but
`tests/browser_parity.rs` fails the build if any `Command` variant lacks one.
Add a `bridge.js` case only when new content-script behavior is needed. Gate a command by adding an arm to `protocol::Command::policy_key()` —
that match is exhaustive too, so a new command **must declare its gate** and
cannot leak ungated (enforcement runs automatically at each privileged sink).
MCP exposes a curated subset of commands — adding a command is not an MCP change.

## DOM Output Format

```
*[1] input#query "Search" type=text autocomplete=search @search
[2] button "Go" @search
--- Page: Example (https://example.com) ---
--- Scroll: 25% (0.5 above, 1.2 below) ---
--- 1 iframe(s) not shown — list: webpilot frame, enter: webpilot frame switch ---
```

`[index]` is the argument to `action click N`. **Indices are bound to the last
`capture` snapshot**: bridge.js stores the element references seen at capture
time, and index actions resolve against that list. If there is no snapshot (no
capture yet) or the element has left the DOM, the result is a typed
`StaleSnapshot` error (exit 4), never a re-resolution against the live DOM.
`*` marks an element new since the last capture — detected by **node identity**
against the previous snapshot, suppressed on the first capture after a URL
change (a fresh page is not "all new"). `@landmark` is semantic context.
`--- N iframe(s) not shown ---` is the count of HTTP iframes outside the active
frame (`DomSnapshot.subframes`); enter one with `frame switch`.

## Wire Protocol

| Area | Rule |
|---|---|
| Action | `{"kind": "click", "index": 7}` — one definition (clap + serde, snake_case) |
| ActionKind | snake_case wire tag, matching `Action.kind` exactly |
| PolicyKey | Policy-enforcement key, keyed by **effect**: `ActionKind` ∪ {`eval`, `fetch`, `dom_set`, `tab_close`, `cookie_list`, `cookie_set`, `cookie_delete`, `session_export`, `session_import`}. `navigate` gates every URL-load effect — the `navigate` action + `capture --url` + `tab new URL`. `eval` gates all MAIN-world JS injection — `eval` + the `frame find` predicate + `console start`/`network start` (monitor-hook injection). `cookie_list` is gated even though read-only because it reads cookie values (session tokens). `Command::policy_key()` maps command → key (non-secret reads → `None`); the match is exhaustive so a new command must declare its gate. Enforcement (`policy::parse_and_enforce`) runs **at the privileged sink that reaches the browser** — `LocalTransport::send` (headless) and the **NM Host** (browser), never the CLI-side `IpcTransport`. The host parses the wire value into a typed `Command` before enforcing — a parse failure is rejected as `InvalidArgument`, blocking a "Rust rejects / JS coerces" bypass. Store is a single `artifacts/policies.json` shared by both modes; `webpilot policy` is a local file command (no browser round-trip). |
| Wait | `{"until": "selector", "value": ".loading"}` — one of `selector`/`text`/`navigation`/`idle` |
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
| 1 | `Other`, `Session` | unknown / session |
| 3 | `ConnectionLost`, `BridgeUnavailable`, `VersionMismatch` | infra |
| 4 | `ElementNotFound`, `StaleSnapshot`, `SelectorNotFound`, `TabNotFound`, `ContextNotFound`, `FrameNotFound` | not-found |
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
$XDG_RUNTIME_DIR/webpilot   Linux/BSD (tmpfs, mode 0700)
~/Library/Caches/webpilot   macOS
~/.cache/webpilot           Linux fallback
```

Subdirectories: `runtime/` (sockets, PIDs, locks), `contexts/` (multi-agent),
`artifacts/` (screenshots, PDFs, sessions, `policies.json`), `chrome-profile/`.

Settings resolve through one layer, `webpilot::settings`: **defaults <
`config.toml` < env var**. Tune via `config.toml` (repo root, override with
`WEBPILOT_CONFIG`) sections `[timeouts]`/`[chrome]`/`[context]`/`[cdp]`/
`[capture]`, or `WEBPILOT_*` env vars (e.g. `WEBPILOT_NAVIGATION_TIMEOUT_MS`).
Only path resolution is env/platform-specific (`dirs`, to avoid cycles).
