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
- **Frames**: `switchFrame` takes `{ selector: { by: "main"|"name"|"url"|"predicate", ... } }`.

## Index resolution (snapshot-bound)
- `extractDOM` stores the picked element references in `state.snapshot` (index order). Index-addressed messages (`executeAction`, `getElementCoords`, `tagElement`) resolve against that stored list via `resolveIndex`, so an index always targets the element the agent saw at capture time — never a freshly-collected element that may have shifted.
- `resolveIndex` revalidates only liveness/visibility (`isConnected` + `isVisible`); a still-connected node whose content changed is legitimate. A missing snapshot (no capture yet) or a removed/hidden element returns `StaleSnapshot` (exit 4) — there is no silent re-resolution against the live DOM.

## New-element semantics
- `state.previousKeys` baseline is reset whenever `location.href` changes — cross-page navigation is not "all elements are new". Within the same URL, `is_new = true` means the element appeared since the last `extractDOM` call.

## Policy & versioning
- The service worker does **not** enforce or store policy. Policy is enforced CLI-side at the transport boundary before a command is sent; the SW just executes.
- Every NM `Ping` (connect-time hello + keepalive) carries `extension_version` from the manifest, so the host can detect a stale install and reject commands with `VersionMismatch`.

## Listener attachment
- The `chrome.runtime.onMessage` listener is removed and re-added on every injection, so SPA bfcache/restore cycles cannot leave a stale listener.

## Limits
- Element text: 300 chars per element.
- Description (`aria-describedby`): 120 chars.
- Label: 80 chars.
- `keyToCode()` maps Enter / Tab / Escape / Backspace / Delete / Arrow* / Home / End / PageUp / PageDown / Space / Insert / CapsLock / F1-F12.

## Service worker (browser mode)
- `nmPort?.postMessage()` for NM communication (optional chaining handles SW restart races).
- `handleStatus()` returns `connected: !!nmPort` — derived from real port state.
- `withCdp(tabId, fn)` serialises concurrent CDP operations per tab.
