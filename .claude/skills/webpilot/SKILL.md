---
description: Drive a real Chrome from the command line — open a URL, click an element, type into a form, take a screenshot, run JavaScript, watch network/console traffic, manage cookies and tabs. Use whenever the user pastes a URL, says "open / check / browse / fill / submit / login / scrape / screenshot", asks about a website's contents, or needs to automate any browser flow. Headless Chrome launches automatically — no setup.
argument-hint: "[url]"
allowed-tools: Bash(webpilot *) Read
---

# WebPilot

`webpilot` is the CLI. JSON when piped, human text in a terminal. `--json` forces JSON.

If invoked with a URL argument, start with `webpilot capture --include dom --url <arg>` to see the page's indexed elements before deciding what to do.

## Lifecycle

- Headless Chrome **auto-starts on the first command** — no setup. `webpilot quit` stops it (use it when switching to an unrelated task to free memory).
- `--browser` connects to the user's authenticated Chrome instead — preserves SSO, bookmarks, extensions. Requires a one-time `webpilot setup` (extracts the extension and registers the Native Messaging host).
- `--context NAME` pins a command to an isolated CDP browser context — for parallel agents on the same Chrome.

```bash
webpilot capture --include dom --url "https://example.com"   # headless (default)
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
[3] a "Home" href="/" @navigation
--- Page: Example (https://example.com) ---
--- Scroll: 25% (0.5 above, 1.2 below) ---
```

- `[N]` — the index used by `action click N`, `action type N "text"`, `action scroll-to N`
- `*` — element appeared since the previous capture (page changed)
- `#id` — element id
- `@ctx` — ARIA landmark role (`navigation` `main` `banner` `contentinfo` `complementary` `form` `search`); a `<nav>` and a `<div role="navigation">` both report `@navigation`
- `autocomplete=…` — input semantic hint
- text truncated at 300 chars
- at most 1000 elements are listed; a shortened capture prints `--- index shortened ---`. Nothing is lost — the browser keeps the page's whole index, so `find` matches elements past the listing and `action click N` works on the index it reports

`capture --include dom` lists only **interactive/indexable** elements (links, buttons, inputs, …). To read plain page text (a `<p>`, a revealed value, a heading) use `dom get-text "<selector>"`, `capture --include text`, or `eval`.

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
webpilot capture --include screenshot --full-page            # entire scrollable area (cannot combine with --annotate — exit 7)
webpilot capture --include dom --url URL                     # navigate first, then capture
```

The output also carries `screenshot_width`/`screenshot_height` (the saved
image's pixels) and, when the capture exceeded the long-edge cap and was
downscaled, `screenshot_scale` — map image pixels back to page pixels with
`image px ÷ scale` before doing any coordinate math on a full-page shot.

`--annotate` draws the numbered overlay by inserting a brief, isolated
`position:fixed` container into the page DOM, screenshotting, then removing it.
Two boundaries: (1) a page that watches its own DOM (a `MutationObserver` on
the document root) can observe that insertion — avoid `--annotate` on a
security-sensitive page that might react to DOM tampering; plain
`--include screenshot` never touches the page. (2) The box positions are the
element coordinates at capture time, so on a **live-updating** page (a
running animation/layout shift) a box can land slightly off its element by
the time the shot is taken — the numbering is exact on a settled page (the
common case); `wait idle` first if the page is still moving.

`Read` the file paths returned in `screenshot_path` / `pdf_path` / `accessibility_path` — they are absolute, so just open them. (They live under the per-user cache: `~/Library/Caches/webpilot/artifacts/` on macOS, `$XDG_RUNTIME_DIR`/`$XDG_CACHE_HOME`/`~/.cache/webpilot/artifacts/` on Linux, or `$WEBPILOT_HOME` when set — always trust the returned path rather than guessing.) Artifacts are cache: they are swept a week after they are written, so copy anything the user should keep.

## Action

Take an action by element index from the most recent capture.

```bash
webpilot action click N                            # --ctrl/--shift/--alt/--meta set the modifier
                                                    #   flags the PAGE's handlers see (app-level
                                                    #   ctrl multi-select, shift range-select);
                                                    #   browser-level open-in-new-tab doesn't apply
                                                    #   to a synthetic click — use `tab new URL`
webpilot action type N "text" --clear              # --clear replaces existing value
webpilot action key-press Enter --ctrl --shift      # also --alt --meta; real key event
                                                    #   (Tab/Backspace/arrows/Enter-submit work;
                                                    #   acts on the focused element — click/focus first)
                                                    #   Chords reach the PAGE (shift+Arrow extends a
                                                    #   selection; JS shortcut listeners see the
                                                    #   modifier keys) — but BROWSER-level shortcuts
                                                    #   (ctrl/cmd+A select-all, copy/paste) have no UI
                                                    #   layer headless; use `type --clear` or eval
                                                    #   `el.select()` instead.
webpilot action navigate "https://…"               # cross-origin reconnect handled
webpilot action scroll down --amount 600           # or `up` — moves the WINDOW scroller
webpilot action scroll-to N                        # bring [N] into view (works through
                                                    #   inner scroll containers too — use this
                                                    #   on app-shell pages where the window
                                                    #   doesn't scroll: Scroll line shows 0 below)
webpilot action back / forward / reload
webpilot action hover N / focus N
webpilot action select N "value"                   # <select> option
webpilot action drag SRC TGT --steps 5             # mouse-driven drag between two indices
                                                    #   (mousedown/move/up only — sliders and
                                                    #   mouse-based sortables work; an HTML5
                                                    #   dragstart/drop-API sortable won't react)
webpilot action upload N "/abs/path/file.pdf"      # works in both modes
webpilot action click N --capture                  # auto-capture DOM after success
```

Action responses include `url_changed` if navigation happened and `new_tab` if a `target="_blank"` opened (it is auto-switched to; `--capture` then snapshots the tab you're now on). A successful `--capture` returns the destination DOM snapshot directly; if the snapshot itself fails, the response carries a `capture_error` field instead — the action's side effect already ran, so **do not retry the action**; just run `capture --include dom`.

`click` dispatches the event in the page, so it carries **no user activation**
(`navigator.userActivation` stays false). Chrome gates a handful of page features
on activation, and refuses them silently — no error, no console message. If a
button does nothing and the response is a bare success, that is the likely
reason. It shows up on buttons that **export or download after an `await`**,
copy to the clipboard, open a popup window, or go fullscreen.

`key-press` is a real browser key event and does carry activation, so drive
those through focus instead — `focus` fails loudly if the element cannot take
focus, so this never misfires silently:

```bash
webpilot action focus N && webpilot action key-press Enter
```

This reaches anything focusable that Enter or Space activates (`button`,
`a[href]`, submit controls). A non-focusable `<div onclick>` export button has no
activation path — drive its effect with `eval` instead.

A link or navigation that resolves to a **file** leaves the page exactly where
it was — the download is the whole outcome. Those responses carry a `downloads`
list (`capture`, every action, `tab new`), so a command that looks like a no-op
is never actually one:

```
Downloaded: ~/Library/Caches/webpilot/artifacts/downloads/default/<guid> ("invoice.pdf" from https://…)
```

`state` is what Chrome did, not what was asked of it:

- `saved` — finished; `path` holds every byte, so `Read` it.
- `in_progress` — still transferring. `path` does **not** exist yet: Chrome
  streams into a `.crdownload` file beside it and renames it into place when the
  transfer finishes, so the path appearing is an exact "it is complete" signal to
  poll for.
- `denied` — the `download` policy refused it. No file exists.
- `canceled` — the transfer broke (reset connection, full disk, a browser
  block). Whatever reached disk is a fragment and no path is offered.

The file is named by its download id, so a page cannot pick a path on your disk;
`suggested_filename` is what the server called it. Files land under WebPilot's
artifact root (per `--context`), never the OS download folder.

Headless only. Under `--browser` a download goes to the user's own download
folder and is not reported — check with them rather than assuming a click that
downloaded did nothing.

Javascript dialogs are **auto-answered** so they never block automation:
`alert` is dismissed, `confirm` answers **true**, `prompt` returns its
**default value** — identically in both modes. A flow that needs the
cancel/false branch must drive the page another way (e.g. `eval`).

## Find (semantic search → optional chained action)

```bash
webpilot find --role button --text "Submit"        # at least one filter required
webpilot find --label "Email" --fill "a@b.com"     # find + type
webpilot find --placeholder "Search" --click       # find + click
webpilot find --tag input
```

A broad filter is bounded like a capture's listing: `count` is the true number of matches, `matches_truncated` says the rows were shortened, and the summary asks you to narrow it.

`--click`/`--fill` require the filter to match **exactly one** element — an
ambiguous filter fails loud listing the matches (narrow it, or use
`action click N`). A bare `find` lists its matches up to the same 1000-row bound, with the true `count`.

## Wait

```bash
webpilot wait selector ".results"   --timeout 10
webpilot wait text "Saved"          --timeout 10
webpilot wait navigation            --timeout 15   # next load event
webpilot wait idle                  --timeout 10   # 500ms DOM-mutation idle
```

`selector`/`text`/`idle` waits survive document navigations: if the page
redirects mid-wait, the condition re-arms against the new document with the
remaining budget — so it's safe to `wait selector` for an element you expect
only after a redirect lands.

## Inspect / mutate page

```bash
webpilot eval 'document.title'                     # expression
webpilot eval '({a:1, b:2})'                       # object literal works
webpilot eval 'console.log("x"); 7'                # multi-statement (returns 7)
webpilot eval '-7 * 6'                             # a leading `-` is fine (so is `type N -- "-1"` style isn't needed)
# `result` is JSON-encoded: a string comes back quoted ("\"Title\""). To just
# read the current page title, `webpilot status` → `tab_title` is plain text.

webpilot dom get-text "h1"
webpilot dom get-html ".card"
webpilot dom get-attr "a.download" "href"   # reads the HTML attribute
webpilot dom set-text "h1" "New title"
webpilot dom set-html "#cell-3" "<p>cell</p>"
webpilot dom set-attr "#agree" "checked" ""
# get-attr reads the HTML *attribute*, not the live DOM *property*. A value
# the user typed lives on the `.value` property, so `get-attr input value`
# returns null — read a typed value with `eval 'el.value'` or `--include dom`.
# `dom set-*` requires the selector to match exactly ONE element (an ambiguous
# selector fails loud naming the count — refine with #id / :nth-of-type).
# `dom get-*` reads the first match. Selectors pierce open shadow roots in
# both directions (light DOM first), so a web component's field works here.
```

## Frames (iframes)

```bash
webpilot frame                            # list, with active_frame_id
webpilot frame switch "frame-name"        # name attribute
webpilot frame url "*pattern*"            # URL substring
webpilot frame find "window.foo === 1"    # JS predicate per-frame (must match exactly ONE)
webpilot frame main                       # back to top frame
```

`capture` is always scoped to one frame — the active frame, or the main frame by default. It never merges frames, so an element's `[N]` index is the same index its action resolves against in that frame. When the main frame contains HTTP iframes, capture appends `--- N iframe(s) not shown — list: webpilot frame, enter: webpilot frame url <pattern> ---`; enter one with `frame url`/`frame switch` to capture and act inside it, then `frame main` to return. After switching, eval / dom / capture / actions all scope to that frame until you switch back. (Viewport-coordinate actions — `hover`, `drag`, `upload` — only work in the main frame; run them after `frame main`.)

## Tabs

```bash
webpilot tab                              # list (id / url / title / active)
webpilot tab new "https://…"
webpilot tab switch <ID>
webpilot tab close <ID>
webpilot tab find --url "*pattern*"
```

## Cookies / session

```bash
webpilot cookie list "https://example.com"
webpilot cookie get  "https://example.com" SESSION
webpilot cookie set  "https://example.com" name value --secure --httponly
webpilot cookie delete "https://example.com" name

webpilot session export                            # writes artifacts/session_*.json (trust the returned path — see note above)
webpilot session export --output /tmp/s.json
webpilot session import /tmp/s.json                # cookies + localStorage
```

`session export` covers all browser cookies (scoped to the active `--context` if set), plus `localStorage` and `sessionStorage` of the active page. Storage is **origin-scoped**: the export records the page's origin, and `import` writes storage only when the current page is on that same origin (navigate there first; a mismatch is a typed error and nothing is written). Cookies import regardless — each carries its own domain.

## Network + console (live monitors, per page)

```bash
webpilot network start
# ... actions / page activity ...
webpilot network read --since 1700000000000        # ms epoch filter (optional)
webpilot network clear

webpilot console start
webpilot console read --level error                # log | warn | info | debug | error
webpilot console clear
```

The recorder is injected into the page and captures activity from `start`
onward (load-time entries that fire before the hooks attach are missed). The
entry buffer lives on the page, so a navigation wipes it — `read` what you need
before navigating away. Recording itself stays armed across navigations in both
modes: the hooks are re-installed automatically after every WebPilot-driven
page change (`navigate`, `back`/`forward`, `reload`, a click that lands on a
new page, `tab switch`/`tab new`). Re-arming re-checks the policy store, so
denying `eval` also stops armed monitors from injecting.

## Fetch with the page's session

```bash
webpilot fetch "https://api.example.com/me"
webpilot fetch "https://…" --method POST --body '{"a":1}' --header content-type:application/json
webpilot fetch "https://…" --method POST --body 'a=1&b=2' --header content-type:application/x-www-form-urlencoded
```

Runs `fetch()` in the page so cookies and CORS apply as the page sees them.

## Device emulation

```bash
webpilot device preset iphone-15        # iphone-15-pro pixel-8 ipad-pro galaxy-s24
webpilot device set --width 800 --height 600 --scale 1.0
webpilot device set --width 800 --height 600 --mobile         # add --mobile flag (no value)
webpilot device set ... --user-agent "MyUA"
webpilot device reset                                          # clear emulation — back to the default headless viewport
```

## Multi-agent contexts (headless only)

```bash
webpilot --context agent-A capture --include dom --url "https://…"
webpilot --context agent-B capture --include dom --url "https://…"   # isolated cookies
webpilot context list
webpilot context close agent-A         # or `--all`
```

Each context is a separate CDP browser context: cookies, localStorage, history are isolated. Up to 16 concurrent. Context state lives under the cache root's `contexts/` subdir (per-OS root as in the artifact-path note above).

## Safety policy (gate which actions are allowed)

```bash
webpilot policy set --operation click  --verdict deny     # blocks all clicks
webpilot policy set --operation type   --verdict allow
webpilot policy default deny                              # baseline for operations without a rule
webpilot policy list                                      # default + all rules
webpilot policy clear                                     # back to permissive default, no rules
```

Least-privilege mode: `policy default deny`, then `policy set … --verdict allow`
for exactly what the task needs. Deny `eval` first when locking down — with
`eval` allowed, page JS can reproduce navigate/fetch/cookie effects, so
narrower denies are advisory.

`--operation` accepts any action kind — `click | type | key_press | navigate | back | forward | reload | scroll | scroll_to | hover | focus | select | upload | drag` — plus the non-action operations that run code, mutate state, or move credentials: `eval`, `fetch`, `dom_set` (gates `dom set-text` / `set-attr`; `dom set-html` runs an inline-handler JS sink so it is gated by `eval`, not `dom_set`), `tab_close`, `cookie_list` (gate `cookie list` **and** `cookie get` — both return live cookie values), `cookie_set` / `cookie_delete`, `session_export` / `session_import`, `device` (emulation: viewport + UA spoofing), `context_close` (`context close` destroys a context and all its tabs), and `download` (a file the page makes the browser write — a `deny` refuses the transfer itself, so nothing reaches disk).

Keys gate by **effect**, not command name:
- `navigate` blocks every URL load — the `navigate` action, `capture --url`, and `tab new URL` — so denying it actually prevents the agent from reaching new pages.
- `eval` blocks all agent-initiated MAIN-world JS — `webpilot eval`, the `frame find` predicate, **and** the `console start` / `network start` monitoring hooks (they inject JS to wrap `console`/`fetch`).

A URL-less `capture`, buffer reads (`console`/`network read` & `clear`), `dom get`, and `find` are never gated. To deny all credential reads, set `session_export`, `cookie_list`, and `session_import` to `deny`.

Policies are a single local file (`policy/policies.json`, under the durable data root so deny rules survive cache eviction) read identically in both modes — `webpilot policy` never touches the browser, and a rule applies the moment it's set. A blocked operation fails with `PolicyDenied` (exit 6).

## Diff / record / profile

```bash
webpilot diff before.json after.json                # auto-detects: .json → DOM, .png → pixel
webpilot diff --dom        a.json b.json
webpilot diff --screenshot a.png  b.png             # writes a diff image; its path is in the output

webpilot record --frames 10 --interval 500          # PNG sequence in artifacts dir
webpilot record --duration 5 --interval 200 --dom   # 5 seconds, fractional allowed (`--duration 0.5`)

webpilot profile --duration 5                       # → .cpuprofile (Chrome DevTools format)
```

## Browser mode (`--browser`) — user's authenticated Chrome

```bash
webpilot setup                                      # skill + extension + NM host (extension id auto-detected)
# load the printed extension path via your browser's extensions page (e.g. chrome://extensions) → Developer mode → Load unpacked — the one manual step
webpilot --browser status
webpilot --browser capture --include dom            # uses logged-in tab
```

The extension id is a fixed constant (the manifest pins a public `key`), so the
NM host is registered without it — `webpilot setup` does skill + extension + host
in one go, leaving only the manual `Load unpacked`. The granular steps still
exist (`setup extension`, `setup nm-host`); `setup nm-host --extension-id` is only
for authorising a different build.

`--browser` rejects `device`, `profile`, `record`, `context`, `quit`, and `--context NAME` with `InvalidArgument` (exit 7) — these all assume the headless lifecycle, which is not yours to manage in browser mode.

## Status

```bash
webpilot status                                # connected, mode, chrome_version, tab info
```

`webpilot quit` (stop Chrome) and `webpilot context close NAME` (one context only) are documented under Lifecycle and Multi-agent contexts above.

## Maintenance / integration

```bash
webpilot mcp                                   # serve the same engine as a stdio MCP server
                                               # (honors --browser / --context; for MCP host config, not interactive use)
webpilot setup skill                           # refresh this skill from the binary's embedded copy
webpilot self update                           # atomic self-update to the latest release (sha256-verified)
webpilot self update --version X.Y.Z           # pin a version (required for downgrades)
webpilot uninstall --yes                       # quit Chrome + remove binary, skill, extension, NM host, policy store, cache
```

## Exit codes

| code | meaning |
|------|---------|
| 0 | success |
| 1 | session error / unknown |
| 2 | CLI usage error (clap: unknown flag, non-numeric index, missing arg) — fix the command line, not the page |
| 3 | infrastructure (Chrome connection, bridge, `VersionMismatch`) |
| 4 | not found (element / `StaleSnapshot` / selector / tab / context / frame) |
| 5 | timeout |
| 6 | security (`PolicyDenied`) |
| 7 | invalid argument (semantically wrong but well-formed) |
| 8 | navigation failed / no page |

Errors carry typed data: `ElementNotFound { requested, available }`, `StaleSnapshot { index }`, `SelectorNotFound { selector }`, `Timeout { kind, elapsed_ms }`, `NavigationFailed { url, reason }`, `PolicyDenied { operation }`, `VersionMismatch { extension, expected }`. Treat the `code` field as authoritative; the `message` is a human-readable rendering. `VersionMismatch` (browser mode) means the installed extension is stale — re-run `webpilot setup extension` and reload it at your browser's extensions page (e.g. `chrome://extensions`).

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
| inside an iframe | `frame switch "name"` → ops → `frame main` |
| API call with auth | `fetch URL --method POST --body '…' --header content-type:application/json` |
| debugging a failed click | `network start` → action → `network read`; same with `console` |
| mobile layout | `device preset iphone-15` then capture |
| logged-in site | `webpilot --browser …` (after `setup`) |
| parallel agents on same Chrome | `--context agent-1`, `--context agent-2` |

## Error recovery

| error code | what to do |
|------------|------------|
| `ElementNotFound` (4) | index out of range for the current snapshot — re-run `capture --include dom` |
| `StaleSnapshot` (4) | the page changed since the last capture (or there was none) — re-run `capture --include dom`, then act |
| `SelectorNotFound` (4) | valid selector but no match — **or you're on the wrong page/tab**: `webpilot tab` to check the active page, then `action navigate URL` (or `tab switch`) to re-pin |
| `NoPage` (8) | call `capture --include dom --url URL` first |
| `Timeout` (5) | raise with `--timeout`, or run `network read` to see what's pending |
| `BridgeUnavailable` / `ConnectionLost` (3) | `webpilot quit` then retry; check `webpilot status` |
| `PolicyDenied` (6) | inspect `webpilot policy list` and clear/relax with `policy set` |
| `InvalidArgument` (7) | re-read the relevant `webpilot help <command>` |
| `NavigationFailed` (8) | URL/network issue — try `eval 'navigator.onLine'`, retry |

## Pitfalls

- **Indices are snapshot-bound.** `[N]` resolves against the exact elements the most recent `capture --include dom` emitted — not a live re-query. After any DOM-changing action (especially `action navigate`, form submits, or page transitions), re-capture before the next index-based action; otherwise you get a typed `StaleSnapshot` (exit 4) rather than a silent wrong-element click.
- **`frame switch` and `tab switch` persist across CLI processes.** Once you switch, every later command (in any new shell) runs in that frame/tab until you switch back (`frame main`, or another `tab switch`). Watch for "wrong frame" symptoms after long sessions.
- **`action navigate` may race the DOM.** Cross-origin loads and SPA route changes finish at unpredictable times — chain `wait selector` / `wait idle`, or pass `--capture` so the DOM is re-read after settle.
- **Cookies and storage are per-context.** With `--context X` set, expect zero data sharing across contexts. `session export` only covers the active context's data.
- **Headless-only options.** `device`, `profile`, `record`, `context`, `quit`, and the `--context NAME` flag all reject `--browser` with `InvalidArgument` (exit 7). Drop `--browser` or stay in headless.
