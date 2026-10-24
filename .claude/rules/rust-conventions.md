---
paths:
  - "crates/**/*.rs"
---

# Rust Conventions

## Single source of truth
- `Action` (in `webpilot::action`) derives both `clap::Subcommand` and `serde::{Serialize,Deserialize}`. The same enum is the CLI surface and the wire protocol — no parallel `BrowserAction` / `ActionCommand` / `to_browser_action` mapping.
- Use the shared `WaitCondition`, `CaptureField`, `CaptureOpts`, `FrameSelector`, and `DomProperty` definitions for command inputs instead of parallel untyped representations.
- A unit-variant enum serialized as a plain string takes its `Display`/`FromStr` from `serde_plain::derive_*`, never hand-written, so its text form is its wire value.

## Naming
- Subcommand enums: singular `XCommand` (e.g., `TabCommand`, `FrameCommand`).
- Args structs: `XArgs` with subcommand field named `command` (or `action` / `condition` when more specific).
- Protocol commands: noun-scoped commands are `NounVerb` (`TabList`, `CookieSet`, `FrameSwitch`); a top-level single-concept command is the bare verb (`Capture`, `Eval`, `Wait`, `Fetch`).
- Initialism casing: `NmError` / `CdpClient` (Rust idiom), not `NMError` / `CDPClient`.
- String parsing: `impl FromStr` — never bespoke `parse(s)` / `from_str_lossy(s)` helpers.

## Error handling
- `WebPilotError` carries typed fields per variant (`ElementNotFound { requested, available }`, `Timeout { kind, elapsed_ms }`, …). Guidance comes from `Display` over those fields and the exit code from `exit_code()` — never from message substrings.
- Classify external protocol errors at their adapter boundary; downstream code matches the resulting types. External crate errors wrap into `WebPilotError::Other` at `main.rs::into_webpilot_error`.
- Preserve `WebPilotError` through command handlers' `anyhow::Result` so callers retain typed fields and exit codes.
- A dispatched mutation with a lost or timed-out response may already have run. Preserve `OutcomeUnknown`; never replay it as connection recovery.

## Settings and paths
- Every tunable resolves through `webpilot::settings`; add new ones there, never a scattered `std::env::var`. Read them in place (`webpilot::settings::timeouts().<field>`), with no per-crate facade.
- Resolve every path through `webpilot::dirs`, never a hard-coded `/tmp/...`. Path resolution reads only env and platform, never settings, which would be circular. Inspection-only code reads a pure `*_path()` accessor, adding one if the path lacks it, so looking never creates state.

## Forbidden
- `unwrap_or_default()` on `serde_json::to_string*` — use `expect("static")` or surface the error.
- `args.iter().any(|a| a.contains("..."))` heuristics for mode detection — use a strict, documented check.
- Re-export shims, deprecated aliases, "removed for X" comments. Delete cleanly.
