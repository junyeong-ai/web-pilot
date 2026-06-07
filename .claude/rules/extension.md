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
- Shadow-DOM coverage: the semantic and the marker passes both pierce open shadow roots (`queryAllDeep`, each bounded by its own `SHADOW_HOST_BUDGET`), so a web component whose clickable part lives in its shadow root is captured. The `cursor:pointer` pass is light-DOM only — `Node.contains` does not cross shadow boundaries, so its innermost/wrapping computation is undefined there; a pointer-styled element carrying no marker/role/semantic tag inside a shadow root (exotic) is the one uncovered case.
- `isVisible` delegates to the platform's `el.checkVisibility({ contentVisibilityAuto, opacityProperty, visibilityProperty })` plus a non-zero rect, so `display:contents`, `visibility:collapse`, `content-visibility:auto`, and `opacity:0` are all handled. The same predicate gates extraction and action-time revalidation, so they can't drift.

## New-element semantics
- `is_new` is detected by **node identity**: an element is new when its reference was absent from the previous snapshot (`state.snapshot`). Identity is collision-free and survives re-renders that keep the node (a framework that *replaces* nodes will mark replacements new — that is the identity model's honest trade against content-hash collisions). With no usable baseline — the first capture in a document, or the first after a `location.href` change — nothing is flagged: a fresh page is not "all new". There is no content-hash key.

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
- `handleStatus()` returns `connected: !!nmPort` — derived from real port state — and reports the pinned tab without ever pinning as a side effect.
- `withCdp(tabId, fn)` serialises concurrent CDP operations per tab; per-tab state (`cdpLocks`, monitoring sets) is pruned on `tabs.onRemoved`.
- **Active tab**: every command targets one pinned tab (`resolveActiveTab`, persisted in `storage.session` across SW restarts), pinned on first use to the focused window's active http tab, moved only by `tab switch` / `tab new` / a click-opened tab. A vanished pin is a typed `TabNotFound` — never a silent retarget. Browser mode is single-agent by design; multi-agent isolation is headless `--context`.
- **Commands are serialized**: one global promise queue executes commands in arrival order, because the worker's state (pin, frame, commit watches) is one set of globals — concurrency would interleave it across commands. Pure reads (`Status`, `Ping`) bypass the queue: a health check must not report dead because the worker is busy.
- **Click-opened tabs** are correlated by two signals (first wins): `tabs.onCreated`'s `openerTabId`, and `webNavigation.onCreatedNavigationTarget`'s `sourceTabId` — the latter covers `rel=noopener` popups, which deliberately carry no opener.
- **Navigation** settles through `waitNavigationSettled`, mirroring headless `navigation_settled`: committed (a main-frame commit event — `onCommitted` / `onHistoryStateUpdated` / `onReferenceFragmentUpdated` — or the URL leaving its pre-navigation value) AND parsed (readyState past `loading`), with the probe **bound to the committed document** via the commit's `documentId` (the headless loaderId equivalent — a same-URL reload can't settle on the old document). Deadline-bounded, no fixed sleeps, no debugger. History nav runs `history.back()/forward()` in the page (`chrome.tabs.goBack` refuses even with history in headless Chrome — measured); `navigation.canGoBack/Forward` makes a missing entry an immediate typed `NavigationFailed` in both modes.
- **Screenshots** are CDP `Page.captureScreenshot` (viewport; `captureBeyondViewport` for full-page), never `chrome.tabs.captureVisibleTab`: CDP captures the target's own surface, so it works on a backgrounded tab without raising the window to the OS foreground (which macOS won't force anyway) and needs only the `debugger` permission, not an `<all_urls>` host grant. No tiling. Capture fails loud with typed errors (headless parity); only screenshots degrade explicitly via `screenshot_error`. An action's `--capture` snapshots the tab the agent will act on next, waiting bounded for the right document: a navigated same-tab document to parse, or a click-adopted popup to **leave `about:blank`** and commit its destination before parsing (a popup is born blank-and-complete, so a bare readyState check would snapshot the empty page). The auto-capture discriminates a real snapshot (an `elements` array) from a typed bridge error — the error becomes `capture_error`, never a malformed `dom`. A capture failure never fails the action (a retry would re-run the side effect).
- **`tab switch`** activates the target within its own window and re-pins it, but never raises the window to the OS foreground — that would hijack the user's focus from another app, and every command reaches the tab through CDP regardless of which window is frontmost.
- **Eval**: main frame via CDP with the headless expression contract — the form (expression vs statements) is decided by **parsing, never by executing and retrying** (`Runtime.compileScript` on CDP, bare `new Function` construction in the scripting path), so a runtime `throw new SyntaxError(...)` cannot run the code twice; promise awaited. A switched frame routes via `scripting.executeScript(frameIds)` and surfaces a forbidding page CSP as typed `CspViolation`.
- **`key_press`** is a native CDP `Input.dispatchKeyEvent` (headless: Rust on the page session; browser: the worker via the debugger), never a synthetic `KeyboardEvent` — so Tab traverses focus, Backspace deletes, arrows navigate, printable keys insert text, and `Enter` submits a form (it carries `text:"\r"`, the signal Chromium's implicit submission keys on). `nativeVirtualKeyCode` is omitted (it is platform-native; sending the Windows code on macOS mis-maps the key to a browser accelerator); `windowsVirtualKeyCode`+`key`+`code` is the portable set. It flows through the navigation/popup detection because Enter can navigate.
