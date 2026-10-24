# crate: webpilot

The **wire types + protocol** library shared by the CLI, host, and extension:
wire types + serialization, plus a handful of leaf I/O utilities (path
materialization in `dirs.rs`, config-file read in `settings.rs`, screenshot
encode/write in `screenshot.rs`, and the NM/IPC framing in
`native_messaging.rs`/`ipc.rs`). IPC uses Tokio async I/O; browser lifecycle and CDP live in `webpilot-cli`.

- `action.rs` — the `Action` enum, deriving `clap::Subcommand` and `serde`
  together (the CLI surface *is* the wire shape). `ActionKind` is the action
  discriminant. `ElementIndex` is a 1-based `u32`; `Action::validate_snapshot`
  checks that an indexed action carries the enclosing command's `snapshot_id`.
- `protocol.rs` — `Command` / `Response` / `ResponseData`, `DomProperty`,
  `FrameSelector`, `RunMode`, and the exhaustive per-command classifications
  `Command::policy_key()` and `Command::unconfirmed_operation()`. Response reads
  use bare plural variants (`Tabs`, `Cookies`, `ConsoleEntries`, `NetworkEntries`).
- `error.rs` — `WebPilotError` with `exit_code()` and `code()`, and the
  `WireError` it round-trips through.
- `types.rs` — DOM / cookie / console / network / tab shapes; `PolicyKey` and
  `PolicyVerdict`. `DomSnapshot::to_text()` renders the agent-facing `[index]
  element` format.
- `capture.rs` — `CaptureField` + `CaptureOpts`.
- `wait.rs` — `WaitCondition` (selector / text / navigation / idle), tagged by
  `until`.
- `settings.rs` — the settings layer, `Settings::{timeouts,chrome,context,cdp,capture,artifacts}`.
  `cdp.event_buffer` defaults to 512: one connection's ring carries the browser
  domain plus every flat-protocol page session's events.
- `dirs.rs` — the cache and data roots and every path under them; the module
  doc gives resolution, overrides and the two kinds of accessor.
- `ipc.rs` / `native_messaging.rs` — Unix socket; Chrome NM 4-byte-LE framing.
- `screenshot.rs` — base64 → resize → PNG.
- `url_glob.rs` — the one `*`-glob matcher behind `frame url` and `tab find
  --url`; `urlGlobMatch` in `browser.js` mirrors it.

## Wire shapes

| Area | Rule |
|---|---|
| Action | `Command::Action { action: {"kind": "click", "index": 7}, snapshot_id: "…", capture: false }`; an indexed action carries the observed `snapshot_id` |
| ActionKind | snake_case wire tag, matching `Action.kind` exactly |
| PolicyKey | Effect-keyed gate: `ActionKind` ∪ {`eval`, `fetch`, `dom_set`, `tab_close`, `cookie_list`, `cookie_set`, `cookie_delete`, `session_export`, `session_import`, `device`, `context_close`, `download`}. Keyed by **effect, not command**: `navigate` gates every URL load (`navigate` action, `capture --url`, `tab new URL`); `download` gates the file a page makes the browser write, whatever started it, and a `deny` becomes Chrome's own `Browser.setDownloadBehavior` refusal, so the transfer never begins; `eval` gates every MAIN-world JS sink (`eval`, `frame find`, `console`/`network start`, `dom set-html`). Per-effect rationale lives in the `PolicyKey` variant docs and the `policy_key()` arms. |
| ConsoleEntry | `{"source": "exception", "level": "error", "message": "...", "timestamp": 0}` — `level` is the console API's own taxonomy, so an uncaught error, which the console shows but no `console.*` call made, is typed by `source` (`console`/`exception`/`rejection`) instead of being flattened into `error` |
| Monitor read | `{"entries": [...], "truncated": false, "covers_load": true}` — `truncated` is the eviction flag (the 500-entry cap dropped something); `covers_load` is whether the recorder was in place before the document's first script. It is false for a document built while nothing was attached (one loaded between two CLI invocations, or a popup already loading when its target appears) and always false in browser mode, which injects at navigation settle. The recorders stamp it from the null `documentElement` only a document-start injection sees; the text render adds `--- recorder installed after this document started … ---` |
| Wait | `Wait { condition, timeout_ms, request_id? }`; request-scoped `WaitCancel { request_id }` cancels that wait; `condition` is the `until`-tagged `WaitCondition` — `{"until": "selector", "value": ".loading"}`, one of `selector`/`text`/`navigation`/`idle` |
| Capture | `{"include": ["dom","screenshot"], "opts": {...}}` |
| Status | `{connected, mode: "headless"\|"browser", pin, tab_url, tab_title, chrome_version, extension_version}` — on the wire every field but the first two is omitted when absent (`extension_version` is browser-mode only). `pin` is a `PinState`; the tab pair is the pinned tab, or while `unpinned` the tab the user is viewing, and is absent while `closed`. CLI `status --json` prints every key, null when absent, plus `context`. |
| Tab | `{id, url, title, pinned, viewed}` — `pinned` is the tab commands act on, `viewed` the tab the user is looking at (browser mode; always false headless) |
| Errors | `{"code": "ElementNotFound", "message": "...", "requested": 5, "available": 3}` |
| FrameSelector | `{"by": "url", "pattern": "/auth/"}` — `main` / `name` / `url` / `predicate`, all four in BOTH modes (a predicate is caller JS, so it rides the `eval` gate). Only the curated MCP surface narrows it: `browser_frame` takes name or URL, and `frame find` stays CLI-only |
| DomProperty | `{"kind": "text"}`, `{"kind": "html"}` or `{"kind": "attr", "name": "href"}` |
