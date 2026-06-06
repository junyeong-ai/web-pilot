# crate: webpilot

CLI·host·extension 이 공유하는 **wire 타입 + 프로토콜** 라이브러리. I/O 없음, 순수 타입과 직렬화.

- `action.rs` — `Action` enum. `clap::Subcommand` + `serde` 동시 derive (CLI 표면 = 와이어 형태). `ActionKind` (액션 판별자), `serde_plain` Display/FromStr.
- `protocol.rs` — `Command` / `Response` / `ResponseData`, `DomProperty`, `FrameSelector`, `RunMode`. 명령 추가 시 `Command` 에 변종 추가.
- `error.rs` — `WebPilotError` (variant-rich, 메시지 파싱 없음). `exit_code()`, `code()`, `WireError` 왕복. 가이던스는 `Display` 가 데이터로 생성.
- `types.rs` — DOM / cookie / console / network / tab 형태, `PolicyKey`(정책 키 — `ActionKind` ∪ {eval, fetch}, `From<ActionKind>` exhaustive), `PolicyVerdict`, `PolicyEntry`(필드 `operation`).
- `capture.rs` — `CaptureField` + `CaptureOpts`.
- `wait.rs` — `WaitCondition` (selector/text/navigation/idle).
- `dirs.rs` — per-user runtime/contexts/artifacts 디렉터리 (mode 0700), pure vs materializing accessor.
- `ipc.rs` / `native_messaging.rs` — Unix 소켓, Chrome NM 4-byte LE 프레이밍.
- `screenshot.rs` — base64 → resize → PNG.

규약은 `.claude/rules/rust-conventions.md`. enum 의 Display/FromStr 은 손으로 쓰지 말고 `serde_plain::derive_*` 사용.
