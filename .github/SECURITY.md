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
  Dependency-changing PRs and pushes run `cargo deny` and `npm audit`;
  the same audits run weekly to pick up new advisories.
- Issues that require local code execution or physical access to the
  machine running WebPilot.
- Self-XSS through user-supplied JavaScript passed to `webpilot eval`
  — that surface is documented as executing code with full page
  authority by design.

## Hardening guarantees

WebPilot's threat model assumes the operator is trusted and the
target site is not. The runtime keeps these invariants:

- Per-user cache and durable data roots are created with mode `0700`.
  Their platform paths and the `$WEBPILOT_HOME` / `$WEBPILOT_DATA_HOME`
  overrides are documented in `crates/webpilot/src/dirs.rs`. The IPC socket is
  `0600` under a `0700` parent; the Native Messaging host manifest restricts
  `allowed_origins` to the one extension ID.
- Native messaging frames use 4-byte LE length prefixing — no
  message parsing is done on free text.
- Internal errors and exit codes use typed `WebPilotError` variants. External
  CDP errors are normalized at the adapter boundary before downstream handling.
- Policies are enforced at the process that reaches a browser (headless:
  `LocalTransport::send`; browser: the Native Messaging host), on the
  typed `PolicyKey` derived from `Command::policy_key()` — not on
  human-readable strings, and never only in the CLI-side `IpcTransport`
  (which a local process could bypass by writing the socket directly).
  The host parses each socket payload into a typed `Command` and rejects
  anything that does not deserialize, closing the class of attack where a
  value the strict Rust types refuse is coerced and run by the looser JS
  bridge (e.g. a string where a numeric index is required). A single file
  store (`policy/policies.json`, under the durable data root) backs both modes; a store that exists
  but cannot be parsed **fails closed** (denies). Each process resolves that root
  from its own environment, and the NM host runs in Chrome's: with
  `WEBPILOT_HOME` or `WEBPILOT_DATA_HOME` set in the shell but not where Chrome
  starts, `webpilot policy` edits a store browser mode never reads.
- Keys gate by *effect*, not command name. `navigate` covers URL loads
  through actions, `capture --url`, and `tab new`; `eval` covers page-world
  evaluation, `frame find`, monitor installation, and `dom set-html`.
  Cookie and session keys gate their dedicated credential APIs. Device
  emulation and context disposal use `device` and `context_close`.
  The exhaustive `Command::policy_key()` and headless command handlers are
  the authoritative mapping. These gates do not isolate a page's own effects:
  an allowed click can submit data or execute page code.
- Indexed actions require the observed `snapshot_id` and revalidate stored
  node identity, visibility, and selected semantic properties. This detects
  stale observations; it does not authenticate the page or establish which
  business record a control affects.
- The Native Messaging host rejects a command with `VersionMismatch`
  when the installed extension's reported version differs from the
  version bundled into the binary, within that host/extension pair. After a binary update, reload the
  extension to reconnect it to a new host; an already-running host retains
  its old binary and settings.

**Credential exposure and policy limits.** Browser mode operates on the
user's authenticated Chrome. The persistent headless profile can also contain
credentials. Cookie/session commands, page text, screenshots, DOM reads and
eval results can expose sensitive data. Omitting password input values from
DOM snapshots does not make other outputs secret-free.

Policy is a guardrail for a steered agent, not a sandbox against a same-user
process that can edit the policy store or reach headless Chrome's local CDP
endpoint. Use `policy default deny` and allow only required effects. Allowing
`eval` permits page code to reproduce narrower denied effects; denying cookie
and session keys alone does not prevent all credential access or exfiltration.
Page content and MAIN-world monitoring buffers remain untrusted.

The `download` gate and command-scoped download reports are headless-only.
Browser mode follows the user's download rules and does not gate or report
those downloads. Confirmation and prompt dialogs are dismissed by default;
`chrome.accept_dialogs` explicitly enables acceptance.

If you find a way to break any of the above, that qualifies as a
security report under this policy.
