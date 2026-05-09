# WebPilot

AI agent용 Chrome 브라우저 제어 CLI. 단일 Rust 바이너리, headless / browser 두 모드.

## Build & Run

```bash
cargo build --workspace --release
webpilot capture --include dom --url "https://example.com"     # Headless (default)
webpilot --browser capture --include dom                       # Browser mode (SSO Chrome)
webpilot --context agent-1 capture --include dom               # 멀티 에이전트 격리
webpilot status
webpilot quit
```

## Architecture

```
                       ┌─ IpcTransport ──→ Unix socket → NM Host → Extension → bridge.js
   commands/<X>.rs ──→ │
   (single source)     └─ LocalTransport ─→ CDP WebSocket → bridge.js (injected)
```

명령 핸들러는 한 번만 작성됨. `Transport` trait 의 두 구현이 모드를 결정. 헤드리스는 NM Host + Extension 인프라가 전부 in-process Rust 로 흡수됨.

## Crates

```
crates/webpilot/        wire types + protocol
  action.rs             Action enum (clap + serde 동시 derive)
  capture.rs            CaptureField + CaptureOpts
  error.rs              WebPilotError variant-rich, no message parsing
  wait.rs               WaitCondition (selector/text/navigation/idle)
  protocol.rs           Command / Response / DomProperty / FrameSelector
  dirs.rs               per-user runtime/contexts/artifacts dirs (mode 0700)
  ipc.rs                Unix domain socket
  native_messaging.rs   Chrome NM 4-byte LE encoding
  screenshot.rs         base64 → resize → PNG
  types.rs              DOM / cookie / console / policy / tab shapes

crates/webpilot-cli/    single binary
  main.rs               mode dispatch (--nm-host strict check)
  cli.rs                clap entry, headless/browser dispatch
  cdp.rs                CdpClient (WebSocket transport)
  host.rs               NM host process (IPC ↔ stdin/stdout)
  output.rs             CommandOutput → human/json renderer
  session.rs            Chrome lifecycle + flock launch lock
  timeouts.rs           WEBPILOT_*_TIMEOUT_MS env overrides
  stitch.rs             full-page tile stitcher
  transport/
    mod.rs              Transport trait, lift_error helper
    ipc.rs              IpcTransport (browser mode)
    local_context.rs    per-user CDP browser-context store
    local/              LocalTransport (headless mode) — split by domain
      mod.rs            struct, open, Transport impl, bridge helpers, navigation
      action.rs         do_action + do_drag (page-mutating)
      capture.rs        do_capture (DOM, screenshot, PDF, accessibility tree)
      query.rs          do_evaluate, do_wait, do_dom_*, do_fetch (page query)
      state.rs          cookies, console+network monitoring, session, policies
      browser.rs        do_tab_*, do_frame_*, do_status (browser-level + frames)
  commands/             single command-handler set, generic over Transport
    action.rs   capture.rs   console.rs   cookie.rs    diff.rs
    dom.rs      eval.rs      fetch.rs     find.rs      frame.rs
    install.rs  network.rs   policy.rs    session.rs   status.rs
    tab.rs      wait.rs      profile.rs   record.rs    device.rs    context.rs

extension/              Chrome extension (browser mode)
  content/bridge.js     __webpilot_handle entry point
  background/service-worker.js   NM ↔ CDP router
```

명령 추가 시 수정 지점: `protocol::Command` enum + `cmd/<x>.rs` + `LocalTransport::send` 한 arm + bridge.js 한 case. 4 곳, 두 모드 동시 지원.

## DOM Output Format

```
*[1] input#query "Search" type=text autocomplete=search @search
[2] button "Go" @search
--- Page: Example (https://example.com) ---
--- Scroll: 25% (0.5 above, 1.2 below) ---
```

`[index]` = `webpilot action click N` 의 인자. `*` = 직전 capture 이후 새로 등장 (URL 변경 시 baseline 자동 리셋, bridge.js 내부에서). `@landmark` = 의미 컨텍스트.

## Wire Protocol

| 영역 | 규칙 |
|---|---|
| Action | `{"kind": "click", "index": 7}` — 단일 정의 (clap+serde, snake_case) |
| ActionKind | snake_case 와이어로 `Action.kind` 와 정확히 일치 — 정책 enforcement 정합 보장 |
| Wait | `{"until": "selector", "value": ".loading"}` — 4 변종 중 하나만 |
| Capture | `{"include": ["dom","screenshot"], "opts": {...}}` |
| Status | `{connected, mode: "headless"\|"browser", chrome_version, extension_version}` — 모드별 의미가 분리됨 |
| Errors | `{"code": "ElementNotFound", "message": "...", "requested": 5, "available": 3}` |
| FrameSelector | `{"by": "url", "pattern": "/auth/"}` — 헤드리스 모드도 Name/Url/Predicate 모두 지원 (execution context 라우팅) |
| DomProperty | `{"kind": "html"}` 또는 `{"kind": "attr", "name": "href"}` |

## Output Modes

- **Terminal**: human → stderr, content → stdout
- **Piped**: stdout이 TTY가 아니면 자동 JSON
- **Forced**: `--json` 플래그

`CommandOutput` enum → `output::render()` 단일 변환.

## Error Handling

| code | variant | exit |
|---|---|---|
| 0 | success | — |
| 1 | `Other`, `Session` | unknown / session |
| 3 | `ConnectionLost`, `BridgeUnavailable` | infra |
| 4 | `ElementNotFound`, `SelectorNotFound`, `TabNotFound`, `ContextNotFound`, `FrameNotFound` | not-found |
| 5 | `Timeout` | timeout |
| 6 | `PolicyDenied`, `CspViolation` | security |
| 7 | `InvalidArgument` | user error |
| 8 | `NavigationFailed`, `NoPage` | navigation |

가이던스 텍스트는 `WebPilotError::Display` 가 데이터로부터 직접 생성. 메시지 파싱·substring 매칭 없음.

## Runtime Paths

```
$WEBPILOT_HOME              명시적 override
$XDG_RUNTIME_DIR/webpilot   Linux/BSD (tmpfs, mode 0700)
~/Library/Caches/webpilot   macOS
~/.cache/webpilot           Linux fallback
```

서브디렉토리: `runtime/` (sockets, PIDs, locks), `contexts/` (멀티 에이전트), `artifacts/` (screenshots, PDFs, sessions, profiles), `chrome-profile/`.

## Testing

```bash
cargo test --workspace          # 32 unit + integration tests
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release           # release profile (lto=thin, strip, panic=abort)
```
