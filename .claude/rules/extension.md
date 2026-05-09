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
- **Errors**: `{ success: false, error: { code, message, ...data } }` where `code` matches `WebPilotError` variant (e.g., `ElementNotFound`, `SelectorNotFound`, `Timeout`) and `...data` carries the Rust variant fields (`requested`, `available`, `selector`, `kind`, `elapsed_ms`, ...).
- **Frames**: `switchFrame` takes `{ selector: { by: "main"|"name"|"url"|"predicate", ... } }`.

## New-element semantics
- `state.previousKeys` baseline is reset whenever `location.href` changes — cross-page navigation is not "all elements are new". Within the same URL, `is_new = true` means the element appeared since the last `extractDOM` call.

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
