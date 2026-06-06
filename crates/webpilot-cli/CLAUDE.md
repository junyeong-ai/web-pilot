# crate: webpilot-cli

단일 바이너리 `webpilot`. `main.rs` 가 시작 시 역할을 분기한다: **CLI**(기본) vs **NM Host**(`argv[1]` 이 `chrome-extension://<32-char [a-p]>` 인 경우만, strict).

## 핵심 구조

- `cli.rs` — clap 진입. `Cmd::execution()` 이 명령 토폴로지의 단일 소스(`Local`/`Status`/`Quit`/`HeadlessOnly`/`TransportGeneric`). 명령 추가 시 컴파일러가 분류를 강제. (`policy` 는 `Local` — 로컬 파일 명령)
- `commands/` — 명령 핸들러 한 세트. 각 핸들러는 `run<T: Transport>` 로 작성돼 두 모드 동시 지원. headless 전용(`profile`/`record`/`device`/`context`)만 `&mut LocalTransport` 직접 사용. `policy` 는 transport 없는 로컬 명령(`policy::set`/`load`/`clear`).
- `policy.rs` — 단일 파일 정책 저장소(`artifacts/policies.json`) + `enforce(&Command)` / `parse_and_enforce(&Value)`. fail-closed. **브라우저에 닿는 sink** 에서만 enforce: headless 는 `LocalTransport::send`, browser 는 host(아래). `parse_and_enforce` 는 host 가 받은 와이어 값을 `Command` 로 파싱 검증(파싱 실패=`InvalidArgument` 거부) 후 enforce.
- `transport/` — `Transport` trait(`send(Command) -> ResponseData`)이 명령 로직과 I/O 사이의 유일한 경계.
  - `ipc.rs` — `IpcTransport` (browser 모드). 정책 미게이트 — 단순 소켓 writer, host 가 게이트.
  - `local/` — `LocalTransport` (headless), **도메인별로 분할**. `send` 첫 줄에서 `policy::enforce`:
    - `mod.rs` — struct, `open`, Transport impl, bridge 주입, **navigation**(`navigate_reconnect`)
    - `action.rs` — page-mutating (click/type/scroll/drag, `do_action`). `require_main_frame` 가 iframe 활성 시 viewport-좌표 액션(hover/drag/upload) 차단.
    - `capture.rs` — DOM / screenshot / PDF / accessibility tree. `count_http_subframes` → `DomSnapshot.subframes`.
    - `query.rs` — eval / wait / dom get·set / fetch
    - `state.rs` — cookies / console·network 모니터 / session
    - `browser.rs` — tab / frame / status
  - `local_context.rs` — per-user CDP browser-context 저장 (multi-agent, `MAX_CONTEXTS`).
- `cdp.rs` — `CdpClient` (tokio-tungstenite WebSocket). id→oneshot 라우팅, heartbeat, `ConnectionLost`/`Timeout` 매핑.
- `session.rs` — Chrome 라이프사이클 + `flock` 런치 락, `headless_viewport()`(settings).
- `host.rs` — NM host 프로세스 (IPC ↔ stdin/stdout). browser 모드의 정책 sink: 요청을 forward 전 `policy::parse_and_enforce` 로 검증·게이트. 버전 게이트: extension 의 Ping(extension_version) 을 번들 버전과 대조, 불일치 시 `VersionMismatch` 로 거부.
- `output.rs` — `CommandOutput` → human/json 렌더.
- `assets.rs` — compile-time 임베드 skill + extension (`include_dir!`). `expected_extension_version()`(번들 manifest 버전).
- `stitch.rs` — full-page 타일 스티칭.

타임아웃은 `webpilot::settings::timeouts().<field>` 로 직접 읽는다(양 크레이트 단일 패턴, 별도 파사드 없음).

## Navigation (`mod.rs::navigate_reconnect`)

완료 판정은 **단일 술어** `navigation_settled(page, loader_id, before_url)`:
- committed = (loader_id 일치) **또는** (frame URL ≠ before_url)
- ready = readyState 가 `interactive`/`complete`

URL 변경 → cross-site 렌더러 스왑 가능 → fresh 세션 rebind. URL 동일 → same-site reload → 기존 세션 재사용(loaderId 로 신·구 문서 구분). loaderId 없고 에러 없으면 same-document(fragment) → 즉시 완료(프레임 보존). `net::ERR_ABORTED` 는 즉시 실패가 아니라 pending — 이후 settle 하면 Ok, deadline 까지 settle 안 되면 `NavigationFailed`.

규약: `.claude/rules/rust-conventions.md`. 명령 추가 5 지점: `protocol::Command` + `commands/<x>.rs` + `cli.rs::Cmd::execution()` 분류 + `LocalTransport::send` arm (headless) + `extension/background/service-worker.js` command 라우터 case (browser). 새 content-script 동작 필요 시에만 `bridge.js` case. 정책 게이트 필요 시 `Command::policy_key()` 에 arm 추가.
