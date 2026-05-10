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

Only the most recent minor release line receives security fixes.

| Version | Supported |
| ------- | --------- |
| 0.2.x   | ✅        |
| < 0.2   | ❌        |

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
- Native messaging frames use 4-byte LE length prefixing — no
  message parsing is done on free text.
- Errors are typed (`WebPilotError` variants), never substring-matched.
- Policies are enforced on the wire `Action.kind` enum, not on
  human-readable strings.

If you find a way to break any of the above, that qualifies as a
security report under this policy.
