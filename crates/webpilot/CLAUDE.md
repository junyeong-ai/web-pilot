# crate: webpilot

CLI·host·extension 이 공유하는 **wire 타입 + 프로토콜** 라이브러리. I/O 없음, 순수 타입과 직렬화.

- `action.rs` — `Action` enum. `clap::Subcommand` + `serde` 동시 derive (CLI 표면 = 와이어 형태). `ActionKind` (액션 판별자), `serde_plain` Display/FromStr.
- `protocol.rs` — `Command` / `Response` / `ResponseData`, `DomProperty`, `FrameSelector`, `RunMode`. 명령 추가 시 `Command` 에 변종 추가. `Command::policy_key()` 가 명령→`PolicyKey` 매핑(게이트 대상만 `Some`).
- `error.rs` — `WebPilotError` (variant-rich, 메시지 파싱 없음). `exit_code()`, `code()`, `WireError` 왕복. 가이던스는 `Display` 가 데이터로 생성. (`StaleSnapshot`, `VersionMismatch` 포함)
- `types.rs` — DOM / cookie / console / network / tab 형태, `PolicyKey`(효과 기준 정책 키 — `ActionKind` ∪ {eval, fetch, dom_set, tab_close, cookie_list, cookie_set, cookie_delete, session_export, session_import}, `From<ActionKind>` exhaustive; 명령→키는 `protocol::Command::policy_key()` — navigate←capture-url/tab-new, eval←console/network-start/predicate, cookie_list←cookie read), `PolicyVerdict`. `DomSnapshot.subframes` = active-frame 밖 HTTP iframe 수.
- `capture.rs` — `CaptureField` + `CaptureOpts`.
- `wait.rs` — `WaitCondition` (selector/text/navigation/idle).
- `settings.rs` — 런타임 설정 단일 레이어(기본값 < `config.toml` < env). `Settings::{timeouts,chrome,context,cdp}`, `get()`/`timeouts()`. 모든 튜닝 가능 env 읽기의 단일 출처.
- `dirs.rs` — per-user runtime/contexts/artifacts 디렉터리 (mode 0700), pure vs materializing accessor. `config_file_path()`. 경로는 env/플랫폼 전용(설정 비대상).
- `ipc.rs` / `native_messaging.rs` — Unix 소켓, Chrome NM 4-byte LE 프레이밍.
- `screenshot.rs` — base64 → resize → PNG.

규약은 `.claude/rules/rust-conventions.md`. enum 의 Display/FromStr 은 손으로 쓰지 말고 `serde_plain::derive_*` 사용.

정책은 와이어 명령이 아니다 — `webpilot policy` 는 CLI-side 로컬 파일 명령이고, `Command`/`ResponseData` 에 정책 변종 없음. enforcement 는 `webpilot-cli` 의 `policy` 가 `Command::policy_key()` 로 **브라우저에 닿는 sink** 에서 수행한다(headless `LocalTransport::send`, browser NM host). CLI-side `IpcTransport` 는 게이트하지 않는다(소켓 직접 호출로 우회 가능하므로).
