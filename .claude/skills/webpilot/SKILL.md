---
description: Drive a real Chrome from the command line — open a URL, click an element, type into a form, take a screenshot, run JavaScript, watch network/console traffic, manage cookies and tabs. Use whenever the user pastes a URL, says "open / check / browse / fill / submit / login / scrape / screenshot", asks about a website's contents, or needs to automate any browser flow. Headless Chrome launches automatically — no setup.
argument-hint: "[url]"
allowed-tools: Bash Read
version: 0.2.0
---

# WebPilot

`webpilot` is the CLI. JSON when piped, human text in a terminal. `--json` forces JSON.

```bash
webpilot capture --include dom --url "https://example.com"   # headless
webpilot --browser capture --include dom                      # user's Chrome (SSO)
webpilot --context agent-1 capture --include dom              # isolated context
```

## Workflow: capture → decide → act → verify

```bash
webpilot capture --include dom --url "https://example.com"   # 1. see indexed elements
# read [N] markers, pick a target                            # 2. decide
webpilot action click 5                                       # 3. act
webpilot capture --include dom                                # 4. verify (check url_changed)
```

## Element output

```
*[1] input#query "Search" type=text autocomplete=search @search
[2] button "Go" @search
[3] a "Home" href="/" @nav
--- Page: Example (https://example.com) ---
--- Scroll: 25% (0.5 above, 1.2 below) ---
```

- `[N]` — the index used by `action click N`, `action type N "text"`, `action scroll-to N`
- `*` — element appeared since the previous capture (page changed)
- `#id` — element id
- `@ctx` — landmark (`nav`, `main`, `form`, `search`, `header`, `footer`)
- `autocomplete=…` — input semantic hint
- text truncated at 300 chars

## Capture

```bash
webpilot capture --include dom                               # default
webpilot capture --include screenshot                        # PNG (path returned)
webpilot capture --include text                              # text_content field
webpilot capture --include pdf                               # PDF (path returned)
webpilot capture --include accessibility                     # CDP a11y tree (path returned)
webpilot capture --include dom screenshot text               # multiple in one call
webpilot capture --include screenshot --annotate             # numbered overlay on shot
webpilot capture --include dom --bounds                      # adds bounds: {x,y,w,h}
webpilot capture --include dom --occlusion                   # marks occluded:true elements
webpilot --browser capture --include screenshot --full-page  # tile-stitched (browser only)
webpilot capture --include dom --url URL                     # navigate first, then capture
```

Artifacts land in `~/Library/Caches/webpilot/artifacts/` (macOS) or `$XDG_CACHE_HOME/webpilot/artifacts/`. `Read` the file paths returned in `screenshot_path` / `pdf_path` / `accessibility_path` to inspect.

## Action

Take an action by element index from the most recent capture.

```bash
webpilot action click N
webpilot action type N "text" --clear              # --clear replaces existing value
webpilot action keypress Enter --ctrl --shift      # also --alt --meta
webpilot action navigate "https://…"               # cross-origin reconnect handled
webpilot action scroll down --amount 600           # or `up`
webpilot action scroll-to N                        # bring [N] into view
webpilot action back / forward / reload
webpilot action hover N / focus N
webpilot action select N "value"                   # <select> option
webpilot action drag SRC TGT --steps 5             # mouse-driven drag between two indices
webpilot action upload N "/abs/path/file.pdf"      # works in both modes
webpilot action click N --capture                  # auto-capture DOM after success
```

Action responses include `url_changed` if navigation happened and `new_tab` if a `target="_blank"` opened (it is auto-switched to).

## Find (semantic search → optional chained action)

```bash
webpilot find --role button --text "Submit"        # at least one filter required
webpilot find --label "Email" --fill "a@b.com"     # find + type
webpilot find --placeholder "Search" --click       # find + click first match
webpilot find --tag input
```

## Wait

```bash
webpilot wait selector ".results"   --timeout 10
webpilot wait text "Saved"          --timeout 10
webpilot wait navigation            --timeout 15   # next load event
webpilot wait idle                  --timeout 10   # 500ms DOM-mutation idle
```

## Inspect / mutate page

```bash
webpilot eval 'document.title'                     # expression
webpilot eval '({a:1, b:2})'                       # object literal works
webpilot eval 'console.log("x"); 7'                # multi-statement (returns 7)

webpilot dom get-text "h1"
webpilot dom get-html ".card"
webpilot dom get-attr "input" "value"
webpilot dom set-text "h1" "New title"
webpilot dom set-html "td" "<p>cell</p>"
webpilot dom set-attr "input" "value" "x"
```

## Frames (iframes)

```bash
webpilot frames                            # list, with active_frame_id
webpilot frames switch "frame-name"        # name attribute
webpilot frames url "*pattern*"            # URL substring
webpilot frames find "window.foo === 1"    # JS predicate per-frame
webpilot frames main                       # back to top frame
```

After switching, all subsequent eval / capture / dom commands run inside that frame's execution context until you switch back.

## Tabs

```bash
webpilot tabs                              # list (id / url / title / active)
webpilot tabs new "https://…"
webpilot tabs switch <ID>
webpilot tabs close <ID>
webpilot tabs find --url "*pattern*"
```

## Cookies / session

```bash
webpilot cookies list "https://example.com"
webpilot cookies get  "https://example.com" SESSION
webpilot cookies set  "https://example.com" name value --secure --httponly
webpilot cookies delete "https://example.com" name

webpilot session export                            # → ~/Library/Caches/webpilot/artifacts/session_*.json
webpilot session export --output /tmp/s.json
webpilot session import /tmp/s.json                # cookies + localStorage
```

`session export` covers all browser cookies (scoped to the active `--context` if set), plus `localStorage` and `sessionStorage` of the active page.

## Network + console (live monitors, persist across navigations)

```bash
webpilot network start
# ... actions / page activity ...
webpilot network read --since 1700000000000        # ms epoch filter (optional)
webpilot network clear

webpilot console start
webpilot console read --level error                # log | warn | info | debug | error
webpilot console clear
```

## Fetch with the page's session

```bash
webpilot fetch "https://api.example.com/me"
webpilot fetch "https://…" --method POST --body '{"a":1}'
```

Runs `fetch()` in the page so cookies and CORS apply as the page sees them.

## Device emulation

```bash
webpilot device preset iphone-15        # iphone-15-pro pixel-8 ipad-pro galaxy-s24
webpilot device set --width 800 --height 600 --scale 1.0
webpilot device set --width 800 --height 600 --mobile         # add --mobile flag (no value)
webpilot device set ... --user-agent "MyUA"
webpilot device reset                                          # back to 1280×720
```

## Multi-agent contexts (headless only)

```bash
webpilot --context agent-A capture --include dom --url "https://…"
webpilot --context agent-B capture --include dom --url "https://…"   # isolated cookies
webpilot context list
webpilot context close agent-A         # or `--all`
webpilot --context agent-A quit        # same effect
```

Each context is a separate CDP browser context: cookies, localStorage, history are isolated. Up to 16 concurrent. Context state lives in `~/Library/Caches/webpilot/contexts/`.

## Safety policy (gate which actions are allowed)

```bash
webpilot policy set --action click  --verdict deny     # blocks all clicks
webpilot policy set --action type   --verdict allow
webpilot policy list
webpilot policy clear
```

`--action` accepts: `click | type | key_press | navigate | back | forward | reload | scroll | scroll_to | hover | focus | select | upload | drag`.

A blocked action fails with `PolicyDenied` (exit 6).

## Diff / record / profile

```bash
webpilot diff before.json after.json                # auto-detects: .json → DOM, .png → pixel
webpilot diff --dom        a.json b.json
webpilot diff --screenshot a.png  b.png             # writes diff.png

webpilot record --frames 10 --interval 500          # PNG sequence in artifacts dir
webpilot record --duration 5000 --interval 200 --dom

webpilot profile --duration 5                       # → .cpuprofile (Chrome DevTools format)
```

## Browser mode (`--browser`) — user's authenticated Chrome

```bash
webpilot install --extension-id <32-char-id>        # one-time NM host manifest install
# then load extension/ in chrome://extensions
webpilot --browser status
webpilot --browser capture --include dom            # uses logged-in tab
```

Headless-only commands (`device`, `profile`, `record`, `context`) reject `--browser` with `InvalidArgument`.

## Lifecycle

```bash
webpilot status                                # connected, mode, chrome_version, tab info
webpilot quit                                  # stop headless Chrome
webpilot --context agent-A quit                # close one context (Chrome stays up)
```

## Exit codes

| code | meaning |
|------|---------|
| 0 | success |
| 1 | session error / unknown |
| 3 | infrastructure (Chrome connection, bridge) |
| 4 | not found (element / selector / tab / context / frame) |
| 5 | timeout |
| 6 | security (`PolicyDenied`, `CspViolation`) |
| 7 | invalid argument |
| 8 | navigation failed / no page |

Errors carry typed data: `ElementNotFound { requested, available }`, `SelectorNotFound { selector }`, `Timeout { kind, elapsed_ms }`, `NavigationFailed { url, reason }`, `PolicyDenied { action }`. Treat the `code` field as authoritative; the `message` is a human-readable rendering.

## Decision guide

| situation | command |
|-----------|---------|
| first time on a page | `capture --include dom --url URL` |
| need an LLM-readable image | `capture --include dom screenshot --annotate` |
| know the button/label | `find --role button --text "X" --click` (or `--fill`) |
| form by label | `find --label "Email" --fill "a@b.com"` |
| page still rendering | `wait selector ".content"` or `wait idle` |
| after `action navigate` | check `url_changed` in response, or `wait navigation` |
| element below the fold | `action scroll-to N` then `action click N` |
| inside an iframe | `frames switch "name"` → ops → `frames main` |
| API call with auth | `fetch URL --method POST --body '…'` |
| debugging a failed click | `network start` → action → `network read`; same with `console` |
| mobile layout | `device preset iphone-15` then capture |
| logged-in site | `webpilot --browser …` (after `install`) |
| parallel agents on same Chrome | `--context agent-1`, `--context agent-2` |

## Error recovery

| error code | what to do |
|------------|------------|
| `ElementNotFound` (4) | indices changed — re-run `capture --include dom` |
| `SelectorNotFound` (4) | check CSS selector / `find` filters |
| `NoPage` (8) | call `capture --include dom --url URL` first |
| `Timeout` (5) | raise with `--timeout`, or run `network read` to see what's pending |
| `BridgeUnavailable` / `ConnectionLost` (3) | `webpilot quit` then retry; check `webpilot status` |
| `PolicyDenied` (6) | inspect `webpilot policy list` and clear/relax with `policy set` |
| `InvalidArgument` (7) | re-read the relevant `webpilot help <command>` |
| `NavigationFailed` (8) | URL/network issue — try `eval 'navigator.onLine'`, retry |
