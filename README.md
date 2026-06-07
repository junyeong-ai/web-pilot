# WebPilot

[![Rust](https://img.shields.io/badge/rust-1.96.0-orange?style=flat-square&logo=rust)](https://www.rust-lang.org)

**AI 에이전트를 위한 브라우저 제어 CLI.** 설정 없이 바로 사용 — Chrome이 자동으로 실행됩니다.

---

## 왜 WebPilot인가?

- **제로 설정** — `webpilot capture --include dom --url URL` 한 줄로 시작 (Chrome 자동 실행)
- **풀스택 커맨드** — DOM, 스크린샷, 액션, 네트워크, 콘솔, 쿠키, 세션, 정책
- **Headless + SSO** — 기본 headless, `--browser`로 사용자 Chrome SSO 세션 활용
- **영속 세션** — Chrome 을 살려 두고 후속 명령이 재연결 (런치 비용 1회)
- **AI 최적화** — 토큰 효율적 DOM 출력, 시맨틱 검색, 타입드 에러 안내

---

## 빠른 시작

```bash
# 한 줄 설치 — 바이너리 다운로드 + 체크섬 검증 + 대화형 셋업
curl -fsSL https://raw.githubusercontent.com/junyeong-ai/web-pilot/main/scripts/install.sh | bash

# 바로 사용 (headless — Chrome 자동 실행)
webpilot capture --include dom --url "https://example.com"

# 소스에서 빌드 (체크아웃 안에서 실행, Rust 1.96 필요)
WEBPILOT_BUILD=source bash scripts/install.sh

# SSO가 필요한 경우 (사용자 Chrome 연결)
webpilot setup extension                       # 확장 추출 + Chrome 가이드
webpilot setup nm-host --extension-id <ID>     # Native Messaging host 등록
webpilot --browser capture --include dom
```

설치 시 스킬·확장은 `webpilot setup`이 바이너리에 임베드된 자산으로 설치합니다. 체크아웃 안에서 실행하면 prebuilt/source 빌드를 대화형으로 선택합니다.

| 환경변수 | 기본값 | 설명 |
|---|---|---|
| `WEBPILOT_BUILD` | `prebuilt` | `prebuilt`(릴리스 다운로드) 또는 `source`(`cargo build`) |
| `WEBPILOT_VERSION` | latest | 특정 태그로 핀 (prebuilt, 예: `v0.3.0`) |
| `WEBPILOT_INSTALL_DIR` | `$HOME/.local/bin` | 설치 경로 |
| `WEBPILOT_REPO` | `junyeong-ai/web-pilot` | 포크 사용 시 오버라이드 |
| `WEBPILOT_NO_SETUP=1` | — | 설치 후 `webpilot setup` 자동 실행 생략 |

---

## 주요 기능

### 페이지 캡처
```bash
webpilot capture --include dom --url "https://naver.com"   # DOM 요소 리스트
webpilot capture --include screenshot                       # 뷰포트 스크린샷
webpilot capture --include screenshot --annotate            # 번호 오버레이 스크린샷
webpilot capture --include pdf                              # PDF 생성
webpilot capture --include dom text screenshot              # 통합 JSON 출력
```

### 요소 검색 + 액션
```bash
webpilot find --role button --text "Submit" --click  # 시맨틱 검색 + 클릭
webpilot find --label "Email" --fill "user@test.com" # 레이블 검색 + 입력
webpilot action click 5                              # 인덱스로 클릭
webpilot action type 3 "hello" --clear               # 텍스트 입력
webpilot action key-press Enter                       # 키 입력
```

### 디바이스 에뮬레이션
```bash
webpilot device preset iphone-15                     # 모바일 디바이스 에뮬레이션
webpilot device set --width 390 --height 844 --mobile  # 커스텀 뷰포트
webpilot device reset                                # 에뮬레이션 해제
```

### 모니터링
```bash
webpilot network start && webpilot network read      # 네트워크 요청 추적
webpilot console start && webpilot console read      # JS 콘솔 캡처
```

### 세션 관리
```bash
webpilot cookie list "https://example.com"           # 쿠키 조회
webpilot session export --output session.json        # 세션 저장
webpilot fetch "https://api.example.com" --method POST --body '{}' # 인증 포함 API 호출
```

### 안전 제어
```bash
webpilot policy set --operation navigate --verdict deny # 네비게이션 차단
webpilot policy list                                  # 정책 조회
```

---

## DOM 출력 형식

```
*[1] input#query "Search" type=text @search
[2] button "Go" @search
[3] a "Home" href="/" @nav
--- Page: Example (https://example.com) ---
--- Scroll: 25% (0.5 above, 1.2 below) ---
--- 3 elements (from 120 nodes, 5ms) ---
```

| 표시 | 의미 |
|------|------|
| `[N]` | 요소 인덱스 (`action click N`으로 사용) |
| `*` | 이전 캡처 이후 새로 나타난 요소 |
| `#id` | HTML element id |
| `@ctx` | 랜드마크 컨텍스트 (nav, main, form, search) |

---

## 아키텍처

```
Headless (기본):
  webpilot CLI → CDP WebSocket → Chrome for Testing (headless)
                → bridge.js (Runtime.evaluate로 주입)

Browser (--browser):
  webpilot CLI → Unix Socket → NM Host → Chrome Extension
                → bridge.js (content script)
```

- **단일 바이너리** — `webpilot` 하나로 headless/browser/host 모드 자동 전환
- **단일 코드베이스** — 동일한 `bridge.js`를 두 모드에서 공유
- **풀스택 커맨드** — `webpilot --help`로 전체 목록 확인

---

## 경쟁 비교

| 기능 | WebPilot | agent-browser | browser-use |
|------|:--------:|:-------------:|:-----------:|
| SSO 지원 (`--browser`) | **내장** | ✗ | ✗ |
| 네트워크 모니터링 | **내장** | ✗ | CDP |
| 콘솔 캡처 | **내장** | ✗ | CDP |
| 시맨틱 검색 (`find`) | **내장** | ✗ | XPath |
| DOM 직접 조작 | **내장** | ✗ | ✗ |
| Annotated 스크린샷 | **내장** | 내장 | PIL |
| 세션 export/import | **내장** | auth state | ✗ |
| 멀티 에이전트 격리 | **`--context`** | ✗ | ✗ |
| 런타임 의존성 | **없음 (단일 바이너리)** | Node | Python |

---

## 생애주기

```bash
webpilot setup                 # 스킬 + 확장 + NM host 대화형 셋업
webpilot setup skill           # 스킬만 재설치/갱신
webpilot setup extension       # 확장 추출 + Chrome 안내 (chrome://extensions 자동 오픈)
webpilot setup nm-host --extension-id <ID>

webpilot self update           # 최신 release로 자기 업데이트 (atomic, sha256 검증)
webpilot self update --version 0.3.0   # 특정 버전 핀

webpilot uninstall             # 흔적 없이 제거 (Chrome 종료 + 모든 디렉토리 정리)
```

스킬과 확장은 바이너리에 컴파일 타임 임베드되어 있습니다. 버전 드리프트가 원천적으로 발생하지 않으며, 설치 후 별도 다운로드 단계가 없습니다. 설치 후 Claude Code에서 `/webpilot` 또는 자연어로 자동 활성화됩니다.

---

## 라이선스

MIT
