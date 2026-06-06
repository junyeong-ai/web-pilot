# crate: webpilot-cli

단일 바이너리 `webpilot`. `main.rs` 가 시작 시 역할을 분기한다: **CLI**(기본) vs **NM Host**(`argv[1]` 이 `chrome-extension://<32-char [a-p]>` 인 경우만, strict).

## 핵심 구조

- `cli.rs` — clap 진입. `Cmd::execution()` 이 명령 토폴로지의 단일 소스(`Local`/`Status`/`Quit`/`HeadlessOnly`/`TransportGeneric`). 명령 추가 시 컴파일러가 분류를 강제.
- `commands/` — 명령 핸들러 한 세트. 각 핸들러는 `run<T: Transport>` 로 작성돼 두 모드 동시 지원. headless 전용(`profile`/`record`/`device`/`context`)만 `&mut LocalTransport` 직접 사용.
- `transport/` — `Transport` trait(`send(Command) -> ResponseData`)이 명령 로직과 I/O 사이의 유일한 경계.
  - `ipc.rs` — `IpcTransport` (browser 모드).
  - `local/` — `LocalTransport` (headless), **도메인별로 분할**:
    - `mod.rs` — struct, `open`, Transport impl, bridge 주입, **navigation**(`navigate_reconnect`)
    - `action.rs` — page-mutating (click/type/scroll/drag, `do_action`)
    - `capture.rs` — DOM / screenshot / PDF / accessibility tree
    - `query.rs` — eval / wait / dom get·set / fetch
    - `state.rs` — cookies / console·network 모니터 / session / `policy_store`
    - `browser.rs` — tab / frame / status
  - `local_context.rs` — per-user CDP browser-context 저장 (multi-agent, `MAX_CONTEXTS`).
- `cdp.rs` — `CdpClient` (tokio-tungstenite WebSocket). id→oneshot 라우팅, heartbeat, `ConnectionLost`/`Timeout` 매핑.
- `session.rs` — Chrome 라이프사이클 + `flock` 런치 락, `HEADLESS_VIEWPORT_*`.
- `host.rs` — NM host 프로세스 (IPC ↔ stdin/stdout).
- `output.rs` — `CommandOutput` → human/json 렌더.
- `assets.rs` — compile-time 임베드 skill + extension (`include_dir!`).
- `timeouts.rs` — `WEBPILOT_*_TIMEOUT_MS` env override.
- `stitch.rs` — full-page 타일 스티칭.

## Navigation (`mod.rs::navigate_reconnect`)

완료 판정은 **단일 술어** `navigation_settled(page, loader_id, before_url)`:
- committed = (loader_id 일치) **또는** (frame URL ≠ before_url)
- ready = readyState 가 `interactive`/`complete`

URL 변경 → cross-site 렌더러 스왑 가능 → fresh 세션 rebind. URL 동일 → same-site reload → 기존 세션 재사용(loaderId 로 신·구 문서 구분). loaderId 없고 에러 없으면 same-document(fragment) → 즉시 완료(프레임 보존). `net::ERR_ABORTED` 는 즉시 실패가 아니라 pending — 이후 settle 하면 Ok, deadline 까지 settle 안 되면 `NavigationFailed`.

규약: `.claude/rules/rust-conventions.md`. 명령 추가 4 지점: `protocol::Command` + `commands/<x>.rs` + `LocalTransport::send` arm (headless) + `extension/background/service-worker.js` command 라우터 case (browser). 새 content-script 동작 필요 시에만 `bridge.js` case.
