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
webpilot capture --include dom --browser                     # the user's Chrome; --context NAME isolates headless
webpilot mcp            # MCP server (stdio); honors --browser / --context
webpilot quit           # stop the headless Chrome and every context in it
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
- `extension/` — the browser-mode extension, plus the page-side scripts both
  modes inject (headless embeds the same files): `content/bridge.js` (isolated
  world) and `content/monitor-*.js` (MAIN world)

## Adding a command

Most Rust steps below are exhaustive matches, so the compiler flags any missed:

1. A `protocol::Command` variant, with arms in `Command::policy_key()` (its gate,
   or `None` when ungated; reads that return credentials are gated) and
   `Command::unconfirmed_operation()` (whether a reply lost after dispatch makes
   it `OutcomeUnknown` or keeps the connection error).
2. `commands/mod.rs` (`pub mod` + enum variant) and a `commands/<x>.rs` handler.
3. `cli.rs`: a `Cmd::execution()` classification and a `dispatch_via_transport` arm.
4. Headless: a `LocalTransport::send` arm with its `do_*` body, and a
   `command_needs_active_page` arm (does it act on the pinned page?).
5. Browser: a case in `extension/background/router.js` and its handler in the
   domain module that mirrors the Rust file (`action`/`capture`/`query`/`state`/
   `browser`.js ↔ the same-named `.rs`). JS is not compiler-checked:
   `tests/browser_parity.rs` fails `cargo test` when a `Command` variant lacks a
   router case, and `oxlint` gates the extension the way `clippy` gates the crates.

Add a `bridge.js` case only for new content-script behavior; MCP exposes a
curated subset, so a new command is not an MCP change. A headless-only command
that drives raw CDP has no `Command`: classify its `Cmd` `HeadlessOnly` and gate
its effect with `policy::enforce_key` in the handler and in `run_headless_mode`.

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

`[index]` with the snapshot ID addresses an observed element: indexed CLI actions
take `--snapshot-id ID`, indexed MCP tools `snapshot_id`. A newer DOM extraction
in that frame (including `find`), a different document, a removed or hidden node,
or a changed accessible name, role, href or input type returns `StaleSnapshot`
(exit 4). `*` marks an element new since the previous capture of the same
document; `@landmark` is semantic context.

| Footer | `DomSnapshot` field | Meaning |
|---|---|---|
| `page text clipped` | `text_truncated` | the page has more text than the capture cap |
| `N iframe(s) not shown` | `subframes` | HTTP iframes outside the active frame; enter one with `frame url <pattern>`, or `frame switch <name>` when it has a name |
| `shadow DOM clipped` | `shadow_truncated` | the shadow-host budget ran out, so controls may be missing from the index |
| `index shortened` | `elements_truncated` | the page has more elements than `[capture] max_elements` renders |
| `N offscreen element(s) not listed` | `offscreen_omitted` | an action's follow-up capture (`--capture`, MCP action tools) lists only the viewport and what the action added (`DomSnapshot::omit_offscreen`); a capture lists the whole page |

The last two are render-only: the browser keeps the whole index, so an omitted
element stays addressable and `find` still matches it. `max_elements` applies
once, in `CommandOutput::dom`, so no surface emits an unbounded index; the
extraction caps (page text, element text, option lists, the shadow walk) live in
the shared `bridge.js`.

## Output Modes

- **Default**: text — content → stdout, notes → stderr, on a terminal and in a
  pipe alike (an agent's shell tool reads a pipe; the text render is written for it)
- **`--json`**: the structured form on stdout

`CommandOutput` → `output::render()`, a single conversion. `to_json()` supplies
CLI JSON and MCP structured content; `to_agent_text()` the MCP text blocks.

## Error Handling

| code | variant | exit |
|---|---|---|
| 0 | success | — |
| 1 | `Other`, `Session`, `ContextInUse` | unknown / session / context held by another live process |
| 2 | _(clap arg parse)_ | CLI usage error — unknown flag / non-numeric index / missing arg |
| 3 | `ConnectionLost`, `BridgeUnavailable`, `VersionMismatch` | infra |
| 4 | `ElementNotFound`, `StaleSnapshot`, `SelectorNotFound`, `TabNotFound`, `NoPinnedTab`, `ContextNotFound`, `CookieNotFound`, `FrameNotFound` | not-found |
| 5 | `Timeout` | timeout |
| 6 | `PolicyDenied` | security |
| 7 | `InvalidArgument` | user error |
| 8 | `NavigationFailed`, `NoPage` | navigation |
| 9 | `OutcomeUnknown` | dispatched operation may already have run; observe before retrying |

`StaleSnapshot` asks for a new capture. `VersionMismatch` means the connected
extension differs from the version bundled into the running host; after an
update, reload the extension so a new host process starts.

## Security Model — honest boundaries

- Policy is a **guardrail against a steered agent, not a sandbox against a
  malicious same-user process**: the store and `webpilot policy` belong to the
  same user the agent runs as. Protect them externally if that matters.
- Policy is enforced only where a process reaches the browser:
  `LocalTransport::send` (headless), the NM host (browser), and the handlers of
  the headless-only commands that drive raw CDP (`device`, `context close`).
  `IpcTransport` and the extension only carry commands; `webpilot policy` edits
  the store locally, with no wire command. The store, its fail-closed reads and
  each sink: the `policy.rs` module doc; what each key gates: `PolicyKey`.
- `eval` is the master key: with `eval` allowed, narrower denies (navigate /
  fetch / cookie_list / session_export) are advisory — page JS reproduces those
  effects. Deny `eval` first; `policy default deny` + allowlist is the
  least-privilege mode.
- The `download` gate is headless-only. In the user's own Chrome,
  `chrome.downloads` names no initiating tab and cannot block before the
  transfer: a report would credit the user's browsing to the agent and leak its
  path, and a deny could only cancel after the bytes landed. A per-tab debugger
  would scope it only by keeping Chrome's debugging banner up, so browser-mode
  downloads follow the user's own folder and rules, unreported and ungated.
- Headless CDP is a `127.0.0.1` WebSocket on a random TCP port, so a same-user
  process can bypass the in-process gate. Accepted: Chrome's pipe transport
  cannot serve separate CLI invocations re-attaching to one persistent Chrome,
  and a broker daemon would add more failure modes than this same-user exposure.
  Browser mode has no such path: its Chrome is reachable only through the NM
  host behind a 0600 socket.

## Runtime Paths

Two per-user roots, both mode 0700. The `dirs.rs` module doc gives their
resolution per platform and what `WEBPILOT_HOME` / `WEBPILOT_DATA_HOME` move.

- **Cache root**, evictable: `runtime/` (sockets, PIDs, locks), `contexts/`,
  `logs/` (`host.log` + one rotated predecessor; browser mode only, because
  Chrome owns the NM host's stdio), `artifacts/` (screenshots, PDFs, sessions,
  `downloads/<browser-context>/`), `chrome-profile/`, `config.toml`.
- **Data root**, durable: the unpacked extension, `policy/policies.json` and
  `skill-install.sha256`. A loaded extension, the deny rules and the skill's
  provenance digest must survive cache eviction.

Artifacts are swept past `[artifacts] ttl_secs` (7 days), since each is minted
under a fresh name; the sweep is due at most hourly (`runtime/last-sweep`) and
runs from the sinks every command passes, as "Two process lifetimes" requires.

Settings resolve through `webpilot::settings`: **defaults < `config.toml` < env
var**. `config.toml` sits under the cache root (`WEBPILOT_CONFIG` moves it) with
sections `[timeouts]`/`[chrome]`/`[context]`/`[artifacts]`/`[cdp]`/`[capture]`;
`settings.rs` names each key, its `WEBPILOT_*` variable and what it trades.
