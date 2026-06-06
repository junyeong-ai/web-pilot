# WebPilot

AI agent용 Chrome 브라우저 제어 CLI. 단일 Rust 바이너리, headless(기본) / browser 두 모드.

## Build & Run

```bash
cargo build --workspace --release
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings

webpilot capture --include dom --url "https://example.com"   # headless (default)
webpilot --browser capture --include dom                     # browser mode (SSO Chrome)
webpilot --context agent-1 capture --include dom             # multi-agent 격리
webpilot status / webpilot quit
```

## Architecture

```
                       ┌─ IpcTransport ──→ Unix socket → NM Host → Extension → bridge.js
   commands/<X>.rs ──→ │
   (single source)     └─ LocalTransport ─→ CDP WebSocket → bridge.js (injected)
```

명령 핸들러는 `run<T: Transport>`로 **한 번만** 작성되고, `Transport` trait 의 두 구현이 모드를 결정한다. 헤드리스는 NM Host + Extension 인프라를 전부 in-process Rust 로 흡수한다.

- `crates/webpilot/` — wire types + protocol (상세: 해당 디렉터리 `CLAUDE.md`)
- `crates/webpilot-cli/` — 단일 바이너리 (상세: 해당 디렉터리 `CLAUDE.md`)
- `extension/` — browser 모드 Chrome 확장 (`bridge.js` 규약: `.claude/rules/extension.md`)
- Rust 규약: `.claude/rules/rust-conventions.md`

**명령 추가 = 4 지점 수정** (두 모드 동시 지원): `protocol::Command` enum + `commands/<x>.rs` 핸들러 + `LocalTransport::send` 한 arm (headless) + `service-worker.js` 의 command 라우터 case (browser). 새 content-script 동작이 필요할 때만 `bridge.js` 에 case 추가.

## DOM Output Format

```
*[1] input#query "Search" type=text autocomplete=search @search
[2] button "Go" @search
--- Page: Example (https://example.com) ---
--- Scroll: 25% (0.5 above, 1.2 below) ---
```

`[index]` = `action click N` 의 인자. `*` = 직전 capture 이후 새로 등장 (URL 변경 시 bridge.js 내부에서 baseline 자동 리셋). `@landmark` = 의미 컨텍스트.

## Wire Protocol

| 영역 | 규칙 |
|---|---|
| Action | `{"kind": "click", "index": 7}` — 단일 정의 (clap+serde, snake_case) |
| ActionKind | snake_case 와이어로 `Action.kind` 와 정확히 일치 |
| PolicyKey | 정책 enforcement 키 — `ActionKind` ∪ {`eval`, `fetch`}, `From<ActionKind>` exhaustive. `PolicySet`/`PolicyEntry` 의 와이어 필드명은 `operation` |
| Wait | `{"until": "selector", "value": ".loading"}` — 4 변종 중 하나만 |
| Capture | `{"include": ["dom","screenshot"], "opts": {...}}` |
| Status | `{connected, mode: "headless"\|"browser", chrome_version, extension_version}` — 모드별 의미 분리 |
| Errors | `{"code": "ElementNotFound", "message": "...", "requested": 5, "available": 3}` |
| FrameSelector | `{"by": "url", "pattern": "/auth/"}` — 헤드리스도 Name/Url/Predicate 모두 지원 (execution context 라우팅) |
| DomProperty | `{"kind": "html"}` 또는 `{"kind": "attr", "name": "href"}` |

snake_case 단일 enum 의 Display/FromStr 은 `serde_plain` 으로 파생 — 손으로 쓴 match 테이블 없음.

## Output Modes

- **Terminal**: human → stderr, content → stdout
- **Piped**: stdout 이 TTY 가 아니면 자동 JSON
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

가이던스 텍스트는 `WebPilotError::Display` 가 데이터로부터 직접 생성. 메시지 파싱·substring 매칭 없음. 외부 크레이트 에러는 `main::into_webpilot_error` 경계에서 `Other` 로 래핑.

## Runtime Paths

```
$WEBPILOT_HOME              명시적 override
$XDG_RUNTIME_DIR/webpilot   Linux/BSD (tmpfs, mode 0700)
~/Library/Caches/webpilot   macOS
~/.cache/webpilot           Linux fallback
```

서브디렉토리: `runtime/` (sockets, PIDs, locks), `contexts/` (멀티 에이전트), `artifacts/` (screenshots, PDFs, sessions), `chrome-profile/`. 타임아웃은 `WEBPILOT_*_TIMEOUT_MS` 환경변수로 override.
