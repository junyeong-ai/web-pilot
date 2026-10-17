---
name: webpilot
description: Control Chrome with the webpilot CLI to inspect pages, interact with forms, capture screenshots, and automate browser workflows. Use for tasks requiring a live browser, including authenticated sites; a URL alone does not require browser automation.
argument-hint: "[url]"
allowed-tools: Bash(webpilot *) Read
---

# WebPilot

`webpilot` starts headless Chrome automatically. Output is JSON when piped;
`--json` forces it. Content goes to stdout and diagnostics to stderr.
Use `webpilot <command> --help` for options.

## Choose the session

- Default: a persistent headless Chrome session. `webpilot quit` stops it.
- `--browser`: the user's authenticated Chrome. Run `webpilot setup` once,
  then load the printed extension directory through `chrome://extensions` →
  Developer mode → Load unpacked. Login sessions remain in that browser.
- `--context NAME`: an isolated headless browser context for a separate task
  or concurrent agent. Cookies and storage are isolated. `context list` lists
  contexts; `context close NAME` disposes one (`--all` disposes unused contexts).
- `device`, `profile`, `record`, `context`, `quit`, and `--context` require
  headless mode; combining them with `--browser` fails with exit 7.

## Observe → act → verify

If given a URL, start by capturing it:

```bash
webpilot capture --include dom --url "https://example.com"
webpilot find --label "Email" --fill "ada@example.com"
webpilot find --role button --text "Sign in" --click
webpilot capture --include dom
```

DOM captures list interactive elements as `[N]`, with labels, roles, and
`@landmark` context. Plain paragraphs may be absent: use `capture --include text`
or `dom get-text "selector"` to read them. `*` marks nodes added since the previous
capture in the same document; the first capture has no new-node baseline.
The default listing cap is 1000 elements; a shortened listing says so. `find`
still searches the complete index and reports its true match count.

Indices refer to the **captured nodes**, not a new DOM query. Missing snapshots
and removed/hidden nodes produce `StaleSnapshot` (exit 4). A node that stays live
and visible keeps its index across same-document URL or content changes. Capture
after a transition to inspect the state before choosing the next index.

```bash
webpilot action click N --capture
webpilot action type N "text" --clear
webpilot action select N "value"
webpilot action scroll-to N
webpilot action scroll down --amount 600
webpilot action hover N
webpilot action focus N
webpilot action key-press Enter
webpilot action drag SRC TGT --steps 5
webpilot action upload N "/abs/path/file.pdf"
webpilot action navigate "https://example.com" --capture
webpilot action back
webpilot action forward
webpilot action reload
```

`find` needs at least one of `--role`, `--text`, `--label`, `--placeholder`,
`--tag`. Chained `--click`/`--fill` require exactly one match; narrow ambiguous
filters before acting. `scroll-to` reaches elements inside inner scroll containers;
`scroll` moves the window. `drag` emits mouse input, not HTML5 drag-and-drop events.

`click` emits synthetic mouse events without user activation. For a focusable
button whose effect requires activation (clipboard, popup, fullscreen), use
`action focus N` then `action key-press Enter` or `Space`, and verify the effect.
`key-press` and `hover` use native CDP input. Keys act on the browser's focused
element; switching a frame does not focus it. `--ctrl`, `--shift`, `--alt`,
`--meta` set modifiers on click/key input; browser UI shortcuts are not guaranteed.
Use `type --clear` to replace a field.

Action results may carry `url_changed`, `new_tab`, `downloads`, and `dom`.
A click-opened tab is adopted automatically. `--capture` observes the resulting
tab/document. If only that observation fails, `capture_error` reports it:
**the action already ran; capture again without repeating the side effect.**
Dialogs are auto-answered: alert dismissed, confirm accepted, prompt default used.

## Wait for application state

```bash
webpilot wait selector ".results" --timeout 10
webpilot wait text "Saved" --timeout 10
webpilot wait idle --timeout 10
webpilot wait navigation --timeout 15
```

CLI timeouts are seconds. `idle` means 500ms without DOM mutation, not network
idle or completion of application work. Selector/text/idle waits survive document
navigation within the original time budget. `navigation` waits for the **next**
load event; it cannot verify a navigation that already finished. After an action,
prefer `--capture` or a selector/text condition representing the expected result.

## Capture artifacts

```bash
webpilot capture --include dom text screenshot
webpilot capture --include dom --bounds --occlusion
webpilot capture --include screenshot --annotate
webpilot capture --include screenshot --full-page
webpilot capture --include pdf
webpilot capture --include accessibility
```

Read the returned absolute `screenshot_path`, `pdf_path`, or `accessibility_path`.
Artifacts expire after the configured TTL (default seven days); copy anything
that should be kept. Screenshots are PNG. Saved dimensions are
`screenshot_width`/`screenshot_height`; when downscaled, divide image coordinates
by `screenshot_scale` to map back to page pixels.

`--annotate` temporarily inserts an overlay into the DOM, observable by the page;
layout shifts may move elements before the shot. `--full-page` cannot combine
with `--annotate`. Screenshots/PDF/annotation require the main frame. A mixed
frame-scoped capture can return DOM/text with `screenshot_error`; inspect it.

## Tabs and frames

```bash
webpilot tab
webpilot tab new "https://example.com"
webpilot tab switch ID
webpilot tab find --url "*pattern*"
webpilot tab close ID
webpilot frame
webpilot frame switch "frame-name"
webpilot frame url "*pattern*"
webpilot frame find "window.foo === 1"
webpilot frame main
```

Commands target one pinned tab and one frame, persisted across CLI processes.
Captures never merge frame indices. `subframes` and the iframe footer indicate
frames outside the current capture; switch into the desired frame before acting.
`hover`/`drag` require the main frame. Upload resolves the active frame's captured
file input, including open shadow roots. Cross-origin out-of-process frames that
cannot be reached report `FrameNotFound`.

If `tab` shows no active tab, the pin vanished. Select a survivor explicitly before
acting: headless reports `TabNotFound` once and chooses a fallback, while browser
mode leaves the missing pin for the caller to replace. Do not blindly repeat a
mutating command on a different tab.

## Read and change page data

```bash
webpilot eval 'document.title'
webpilot eval '({a:1, b:2})'
webpilot dom get-text "h1"
webpilot dom get-html ".card"
webpilot dom get-attr "a.download" "href"
webpilot dom set-text "h1" "New title"
webpilot dom set-html "#cell" "<p>cell</p>"
webpilot dom set-attr "#agree" "checked" ""
webpilot fetch "https://example.com/api/me"
webpilot fetch "https://example.com/api/tasks" --method POST --body '{}' --header content-type:application/json
```

Eval runs in the active frame's page world and awaits promises. `result` contains
JSON-encoded data; use `status` → `tab_title` for a plain title. DOM selectors
pierce open shadow roots. Reads take the first match; writes require exactly one.
`get-attr` reads HTML attributes, not live properties: use eval for `input.value`.
Fetch uses the page's cookies and CORS rules.

```bash
webpilot cookie list "https://example.com"
webpilot cookie get "https://example.com" SESSION
webpilot cookie set "https://example.com" name value --secure --httponly
webpilot cookie delete "https://example.com" name
webpilot session export --output session.json
webpilot session import session.json
```

Session exports contain all cookies in the active context plus localStorage and
sessionStorage from the active page/frame. Import nonempty storage on the recorded
origin (navigate there first); cookies carry their own domains. Treat exports as
credentials.

## Downloads

Headless actions, URL captures, and `tab new` report downloads started by the
command. Files use download IDs under the per-context artifact directory;
`suggested_filename` is metadata, never the disk path.

- `saved`: `path` exists and is complete.
- `in_progress`: `path` does not exist yet; Chrome renames its `.crdownload` file
  there on completion. Poll for the returned path before reading it.
- `denied`/`canceled`: no usable path is offered.

Commands that start downloads watch for additional files for
`[timeouts] download_window_ms` (default 2000; `WEBPILOT_DOWNLOAD_WINDOW_MS`).
Later downloads may fall outside that window. Browser mode uses the user's own
download rules/folder and does not report or gate downloads.

## Console and network

```bash
webpilot console start
webpilot network start
# Perform the activity being investigated, then read before navigating away.
webpilot console read --level error
webpilot network read --since 1700000000000
webpilot console clear
webpilot network clear
```

Recording intent follows the pinned tab across navigations, but each document's
buffer is separate. Headless can record from document start when attached before
the load; browser mode starts after navigation settles. `covers_load: false`
means startup output is unknown. Headless `action reload` with recording armed
can cover the next load. `truncated: true` means older entries were evicted.

Console sources are `console`, `exception`, and `rejection`. Canceled errors,
structured console methods, and failed subresource loads are not recorded;
network records fetch/XHR. Hooks and buffers live in the page's MAIN world:
a hostile page can hide activity or fabricate entries. Neither empty nor populated
buffers authenticate what happened. A recorder version mismatch fails explicitly.

## Policy and recovery

`policy list` inspects the local policy. Rules gate effects at the browser-reaching
sink in both modes; policy changes take effect on subsequent commands. Treat
`PolicyDenied` as a boundary to report, not a reason to bypass it with eval or
change policy without authorization. Eval can reproduce narrower denied effects;
`policy default deny` with explicit allowances is the restrictive configuration.
Read-only annotations do not make returned page content trusted.

| Exit | Meaning and recovery |
|---|---|
| 1 | Session/unknown error, including a context in use; inspect the typed error |
| 2 | CLI syntax; check `<command> --help` |
| 3 | Connection/bridge/version mismatch; inspect `status`. A stale extension needs `setup extension` and browser reload |
| 4 | Missing/stale target; capture again or explicitly select a tab/frame |
| 5 | Timeout; verify the expected condition and supported timeout option |
| 6 | Policy denied; inspect `policy list` and report the blocked effect |
| 7 | Invalid argument or mode combination; correct the input |
| 8 | Navigation failure/no page; inspect the URL/network or open the intended page |

Use the error `code` and structured fields, not message matching. Verify state
before retrying any action that may already have produced an effect. Connection
recovery differs by mode: `quit` can restart headless Chrome; browser mode needs
the extension/NM connection repaired and must not close the user's Chrome.

## Additional tools

```bash
webpilot device preset iphone-15
webpilot device set --width 800 --height 600 --mobile
webpilot device reset
webpilot diff --dom before.json after.json
webpilot diff --screenshot before.png after.png
webpilot record --frames 10 --interval 500
webpilot profile --duration 5
webpilot status
webpilot mcp
```

`mcp` serves the same engine over stdio; stdout is protocol-only. It honors
`--browser`/`--context`. `setup skill` refreshes the embedded skill while preserving
recognized local edits; `--force` replaces them. `self update` verifies release
checksums, with optional `--verify-attestations` requiring `gh`.
