---
paths:
  - "extension/**"
---

# Chrome Extension & bridge.js

bridge.js is shared between headless (`include_str!` → `Runtime.evaluate`) and browser mode (content script).

- Error responses: `{ success: false, error: { message, code } }` with PascalCase codes
- Text limit: 300 chars per element
- `keyToCode()` maps special keys (Enter, Tab, Space, Arrow, etc.)
- Service worker uses `nmPort?.postMessage()` (optional chaining) for NM communication
- `handleStatus()` returns `connected: !!nmPort` — real NM port state, not hardcoded
