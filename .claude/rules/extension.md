---
paths:
  - "extension/**"
---

# Chrome Extension & bridge.js

`bridge.js` is the single content-side entry point shared by headless mode (Rust `include_str!` → `Runtime.evaluate`) and browser mode (manifest content script).

## Public binding
- `window.__webpilot_handle(msg)` — sole exported function. Returns a value or a Promise. Namespaced to avoid colliding with page globals.

## Wire shapes
- **Action**: `{ kind: "click" | "type" | "key_press" | "navigate" | "scroll" | "scroll_to" | "back" | "forward" | "reload" | "hover" | "focus" | "select" | "upload" | "drag", ...fields }`. `kind` is snake_case to match Rust `serde(rename_all = "snake_case")`.
- **Wait**: `{ condition: { until: "selector"|"text"|"navigation"|"idle", value? }, timeout_ms }`.
- **Errors**: `{ success: false, error: { code, message, ...data } }` where `code` matches `WebPilotError` variant (e.g., `ElementNotFound`, `StaleSnapshot`, `SelectorNotFound`, `Timeout`) and `...data` carries the Rust variant fields (`requested`, `available`, `index`, `selector`, `kind`, `elapsed_ms`, ...).
- **Frames**: switching is **not** a bridge message — headless drives CDP
  directly (`do_frame_switch`), browser uses the service worker
  (`handleFrameSwitch`). The bridge has no `switchFrame` case.

## Index resolution (snapshot-bound)
- `extractDom` stores the picked element references in `state.snapshot` (index order). Index-addressed messages (`executeAction`, `getElementCoords`, `tagElement`) resolve against that stored list via `resolveIndex`, so an index always targets the element the agent saw at capture time — never a freshly-collected element that may have shifted.
- `resolveIndex` revalidates only liveness/visibility (`isConnected` + `isVisible`); a still-connected node whose content changed is legitimate. A missing snapshot (no capture yet) or a removed/hidden element returns `StaleSnapshot` (exit 4) — there is no silent re-resolution against the live DOM.

## Interactivity & visibility (principled, not heuristic)
- Interactive set = the semantic allowlist (`a[href]`, `button`, inputs, ARIA roles, `contenteditable`, `details>summary`) ∪ explicit click markers (`onclick`, `jsaction`, framework `@click`/`data-action`, and `tabindex >= 0` only — `tabindex=-1` is script-only focus, not an affordance) ∪ the **innermost** `cursor:pointer` element (one that wraps no already-collected interactive node, so a clickable card doesn't shadow its real button). Size is not a gate; visibility is.
- `isVisible` delegates to the platform's `el.checkVisibility({ contentVisibilityAuto, opacityProperty, visibilityProperty })` plus a non-zero rect, so `display:contents`, `visibility:collapse`, `content-visibility:auto`, and `opacity:0` are all handled. The same predicate gates extraction and action-time revalidation, so they can't drift.

## New-element semantics
- `is_new` is detected by **node identity**: an element is new when its reference was absent from the previous snapshot (`state.snapshot`). Identity is collision-free and churn-free — a re-rendered-but-same node stays not-new; a remounted node is correctly new. On the first capture after a `location.href` change there is no meaningful prior set, so nothing is flagged (a fresh page is not "all new"). There is no content-hash key.

## Policy & versioning
- The service worker does **not** enforce or store policy. Policy is enforced at the privileged sink that reaches the browser — `LocalTransport::send` (headless) or the NM host (browser) — never in the SW or the CLI-side `IpcTransport`; the SW just executes.
- Every NM `Ping` (connect-time hello + keepalive) carries `extension_version` from the manifest, so the host can detect a stale install and reject commands with `VersionMismatch`.

## Listener attachment
- The `chrome.runtime.onMessage` listener is removed and re-added on every injection, so SPA bfcache/restore cycles cannot leave a stale listener.

## Limits
- Element text: 300 chars per element. Description (`aria-describedby`): 120 chars. Label / option text: 80 chars. Shadow-DOM traversal is bounded by a shadow-host budget (not a depth cap), warning when it clips.
- All truncation is codepoint-safe (`clip()`): it never splits a UTF-16 surrogate pair, so a clipped astral character can't emit a lone surrogate that would break the snapshot's JSON serialization.
- `keyToCode()` maps Enter / Tab / Escape / Backspace / Delete / Arrow* / Home / End / PageUp / PageDown / Space / Insert / CapsLock / F1-F12.

## Service worker (browser mode)
- `nmPort?.postMessage()` for NM communication (optional chaining handles SW restart races).
- `handleStatus()` returns `connected: !!nmPort` — derived from real port state.
- `withCdp(tabId, fn)` serialises concurrent CDP operations per tab.
