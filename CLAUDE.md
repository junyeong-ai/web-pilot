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

**명령 추가 = 5 지점 수정** (두 모드 동시 지원): `protocol::Command` enum + `commands/<x>.rs` 핸들러 + `cli.rs::Cmd::execution()` 분류 + `LocalTransport::send` 한 arm (headless) + `service-worker.js` 의 command 라우터 case (browser). 새 content-script 동작이 필요할 때만 `bridge.js` 에 case 추가. 정책 게이트가 필요하면 `protocol::Command::policy_key()` 에 한 arm 추가(enforcement 는 각 privileged sink 에서 자동).

## DOM Output Format

```
*[1] input#query "Search" type=text autocomplete=search @search
[2] button "Go" @search
--- Page: Example (https://example.com) ---
--- Scroll: 25% (0.5 above, 1.2 below) ---
--- 1 iframe(s) not shown — list: webpilot frame, enter: webpilot frame switch ---
```

`[index]` = `action click N` 의 인자. **인덱스는 직전 `capture` 의 스냅샷에 바인딩**된다 — bridge.js 가 capture 시점의 요소 참조를 저장하고, 인덱스 액션은 그 목록으로 해석한다. 스냅샷이 없거나(캡처 전) 요소가 DOM 에서 사라지면 라이브 DOM 재해석이 아니라 타입드 `StaleSnapshot` 오류(exit 4). `*` = 직전 capture 이후 새로 등장 (URL 변경 시 baseline 자동 리셋). `@landmark` = 의미 컨텍스트. `--- N iframe(s) not shown ---` = active-frame 밖 HTTP iframe 수(`DomSnapshot.subframes`) — `frame switch` 로 진입.

## Wire Protocol

| 영역 | 규칙 |
|---|---|
| Action | `{"kind": "click", "index": 7}` — 단일 정의 (clap+serde, snake_case) |
| ActionKind | snake_case 와이어로 `Action.kind` 와 정확히 일치 |
| PolicyKey | 정책 enforcement 키 — **효과(effect) 기준**. `ActionKind` ∪ {`eval`, `fetch`, `dom_set`, `tab_close`, `cookie_list`, `cookie_set`, `cookie_delete`, `session_export`, `session_import`}. `navigate` 는 URL 로드 효과를 모두 게이트 — `navigate` 액션 + `capture --url` + `tab new URL`. `eval` 은 MAIN-world JS 주입 전부 — `eval` + `frame find` predicate + `console start`/`network start`(모니터 훅 주입). `cookie_list` 는 쿠키 값(세션 토큰) 읽기라 read-only 라도 게이트. `Command::policy_key()` 가 명령→키 매핑(비밀 아닌 읽기는 `None`). enforcement(`policy::parse_and_enforce`)는 **브라우저에 닿는 privileged sink 에서** — headless 는 `LocalTransport::send`, browser 는 **NM Host**(CLI-side `IpcTransport` 아님; host 는 와이어 값을 `Command` 로 파싱 검증 후 enforce — 파싱 실패는 `InvalidArgument` 로 거부해 "Rust 거부 / JS 수용" 강제변환 우회 차단). 저장소는 `artifacts/policies.json` 단일 파일(양 모드 공유), `webpilot policy` 는 로컬 파일 명령(브라우저 왕복 없음) |
| Wait | `{"until": "selector", "value": ".loading"}` — `selector`/`text`/`navigation`/`idle` 중 하나 |
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
| 3 | `ConnectionLost`, `BridgeUnavailable`, `VersionMismatch` | infra |
| 4 | `ElementNotFound`, `StaleSnapshot`, `SelectorNotFound`, `TabNotFound`, `ContextNotFound`, `FrameNotFound` | not-found |
| 5 | `Timeout` | timeout |
| 6 | `PolicyDenied`, `CspViolation` | security |
| 7 | `InvalidArgument` | user error |
| 8 | `NavigationFailed`, `NoPage` | navigation |

`StaleSnapshot` = 인덱스가 가리키던 요소가 capture 이후 DOM 에서 사라짐(재캡처 필요). `VersionMismatch` = 설치된 extension 버전 ≠ 바이너리 번들 버전(`webpilot setup extension` 후 reload).

가이던스 텍스트는 `WebPilotError::Display` 가 데이터로부터 직접 생성. 메시지 파싱·substring 매칭 없음. 외부 크레이트 에러는 `main::into_webpilot_error` 경계에서 `Other` 로 래핑.

## Runtime Paths

```
$WEBPILOT_HOME              명시적 override
$XDG_RUNTIME_DIR/webpilot   Linux/BSD (tmpfs, mode 0700)
~/Library/Caches/webpilot   macOS
~/.cache/webpilot           Linux fallback
```

서브디렉토리: `runtime/` (sockets, PIDs, locks), `contexts/` (멀티 에이전트), `artifacts/` (screenshots, PDFs, sessions, `policies.json`), `chrome-profile/`.

설정은 `webpilot::settings` 단일 레이어로 해석: **기본값 < `config.toml` < env var**. `config.toml`(루트 직하, `WEBPILOT_CONFIG` 로 override)의 `[timeouts]`/`[chrome]`/`[context]`/`[cdp]` 섹션, 또는 `WEBPILOT_*` env(예: `WEBPILOT_NAVIGATION_TIMEOUT_MS`)로 튜닝. 경로 해석만 env/플랫폼 전용(`dirs`, 순환 방지).
