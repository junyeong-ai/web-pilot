# Security Policy

## Reporting a vulnerability

Please report security issues **privately** through one of:

1. GitHub's [private vulnerability reporting](https://github.com/junyeong-ai/web-pilot/security/advisories/new) (preferred)
2. Email: `junyeong.eom@oliveyoung.co.kr`

Do **not** open a public issue or pull request for security reports.

You can expect:

- Acknowledgement within **3 business days**.
- A fix or mitigation plan within **14 days** for confirmed issues.
- Coordinated disclosure once a release is available.

## Supported versions

Only the most recent minor release line receives security fixes —
upgrade to the [latest release](https://github.com/junyeong-ai/web-pilot/releases/latest)
before reporting.

## Scope

**In scope**

- The `webpilot` CLI binary (`crates/webpilot-cli`)
- The `webpilot` library / wire protocol (`crates/webpilot`)
- The Chrome extension shipped under `extension/`
- CI / release pipelines under `.github/workflows/`

**Out of scope**

- Vulnerabilities in transitive dependencies — please report upstream.
  WebPilot runs `cargo deny` on every PR and on a weekly schedule, so
  RUSTSEC advisories are picked up automatically.
- Issues that require local code execution or physical access to the
  machine running WebPilot.
- Self-XSS through user-supplied JavaScript passed to `webpilot eval`
  — that surface is documented as executing code with full page
  authority by design.

## Hardening guarantees

WebPilot's threat model assumes the operator is trusted and the
target site is not. The runtime keeps these invariants:

- All per-user state lives under `$WEBPILOT_HOME` with mode `0700`.
  The IPC socket is `0600` under a `0700` parent; the Native Messaging
  host manifest restricts `allowed_origins` to the one extension ID.
- Native messaging frames use 4-byte LE length prefixing — no
  message parsing is done on free text.
- Errors are typed (`WebPilotError` variants), never substring-matched.
- Policies are enforced at the process that reaches a browser (headless:
  `LocalTransport::send`; browser: the Native Messaging host), on the
  typed `PolicyKey` derived from `Command::policy_key()` — not on
  human-readable strings, and never only in the CLI-side `IpcTransport`
  (which a local process could bypass by writing the socket directly).
  The host parses each socket payload into a typed `Command` and rejects
  anything that does not deserialize, closing the class of attack where a
  value the strict Rust types refuse is coerced and run by the looser JS
  bridge (e.g. a string where a numeric index is required). A single file
  store (`artifacts/policies.json`) backs both modes; a store that exists
  but cannot be parsed **fails closed** (denies).
- Keys gate by *effect*, not command name, so every surface producing an
  effect is covered by one key: `navigate` covers every URL load
  (`navigate` action, `capture --url`, `tab new URL`); `eval` covers all
  agent-initiated MAIN-world JS (`eval`, the `frame find` predicate, and
  the `console`/`network start` monitoring hooks); `cookie_list` covers
  reading cookie values (`cookie list`/`get`); `session_export`/
  `session_import` cover bulk credential movement; `tab_close` covers
  destroying a tab; `fetch`/`dom_set`/`cookie_set`/`cookie_delete` cover
  the remaining script and mutation surfaces. So **all** credential-read,
  navigation, and code-injection surfaces can be denied, not just the
  obviously-named one.
- The Native Messaging host rejects a command with `VersionMismatch`
  when the installed extension's reported version differs from the
  version bundled into the binary, so a skewed install cannot operate
  on the user's authenticated session under a stale protocol.

**Credential-exposure note.** In browser mode WebPilot operates on the
user's real, authenticated Chrome. The headless profile is also
persistent across runs and can accumulate credentials through a
navigate-then-login flow. In both cases `session export` and `cookie`
commands can read live session cookies and storage; treat their output
as secret material. To deny every credential-read surface, set
`session_export`, `cookie_list`, and `session_import` to `deny` (and
`cookie_set`/`cookie_delete` to deny writes). Policy on these runs at the
privileged sink, so the gate holds on both the headless (`LocalTransport`)
and browser (NM host) paths.

`device` and `context` are intentionally ungated: they emulate the
viewport / manage context lifecycle and cannot read or exfiltrate page
data, so a policy key for them would add surface without value.

If you find a way to break any of the above, that qualifies as a
security report under this policy.
