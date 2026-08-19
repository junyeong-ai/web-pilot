# crate: webpilot

The **wire types + protocol** library shared by the CLI, host, and extension:
wire types + serialization, plus a handful of leaf I/O utilities (path
materialization in `dirs.rs`, config-file read in `settings.rs`, screenshot
encode/write in `screenshot.rs`, and the NM/IPC framing in
`native_messaging.rs`/`ipc.rs`). No async runtime, no browser — that lives in
`webpilot-cli`.

- `action.rs` — the `Action` enum, deriving `clap::Subcommand` and `serde`
  together (the CLI surface *is* the wire shape). `ActionKind` is the action
  discriminant; its Display/FromStr come from `serde_plain`. `ElementIndex` is a
  1-based `u32`.
- `protocol.rs` — `Command` / `Response` / `ResponseData`, `DomProperty`,
  `FrameSelector`, `RunMode`. Add a command by adding a `Command` variant.
  `Command::policy_key()` maps command → `PolicyKey`; the match is **exhaustive**
  (read-only commands return `None` explicitly), so a new command cannot leak
  ungated. Response reads use bare plural variants (`Tabs`, `Cookies`,
  `ConsoleEntries`, `NetworkEntries`).
- `error.rs` — `WebPilotError` (variant-rich, no message parsing). `exit_code()`,
  `code()`, and `WireError` round-trip. Guidance comes from `Display` over the
  data. Includes `StaleSnapshot` and `VersionMismatch`.
- `types.rs` — DOM / cookie / console / network / tab shapes; `PolicyKey` (the
  effect-based policy key — `ActionKind` ∪ {eval, fetch, dom_set, tab_close,
  cookie_list, cookie_set, cookie_delete, session_export, session_import, device,
  context_close, download}, with an exhaustive `From<ActionKind>`); `PolicyVerdict`. `DomSnapshot.subframes` is
  the count of HTTP iframes outside the active frame. `DomSnapshot::to_text()`
  renders the agent-facing `[index] element` format.
- `capture.rs` — `CaptureField` + `CaptureOpts`.
- `wait.rs` — `WaitCondition` (selector / text / navigation / idle), tagged by
  `until`.
- `settings.rs` — single settings layer (defaults < `config.toml` < env). The
  one source for every tunable env read; `Settings::{timeouts,chrome,context,cdp,capture,artifacts}`.
  `cdp.event_buffer` defaults to 512: one connection's ring carries the browser
  domain plus every flat-protocol page session's events.
- `dirs.rs` — the two per-user roots (mode 0700): the evictable cache
  (`root()` — runtime / contexts / logs / artifacts / chrome-profile) and the
  durable data root (`data_root()` — the unpacked extension, the policy store,
  `skill-install.sha256`); pure vs materializing accessors; `config_file_path()`.
  Paths are env/platform only (not settings-driven).
- `ipc.rs` / `native_messaging.rs` — Unix socket; Chrome NM 4-byte-LE framing.
- `screenshot.rs` — base64 → resize → PNG.

Conventions: `.claude/rules/rust-conventions.md`. Never hand-write an enum's
Display/FromStr — use `serde_plain::derive_*`.

Policy is not a wire command — `webpilot policy` is a CLI-side local file command,
and there is no policy variant in `Command`/`ResponseData`. Enforcement lives in
`webpilot-cli`'s `policy`, which uses `Command::policy_key()` at the **sink that
reaches the browser** (`LocalTransport::send` headless, the NM host browser). The
CLI-side `IpcTransport` does not gate (writing the socket directly would bypass
it), so the host re-validates every wire value as a typed `Command` first.
