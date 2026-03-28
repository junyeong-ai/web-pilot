---
name: webpilot
description: "Open websites, click buttons, fill forms, take screenshots, and read page content using a real Chrome browser via CLI. Use this skill whenever the user wants to go to a URL, interact with a web page, scrape or extract data from a site, check what's on a page, automate web tasks, submit forms, capture screenshots, monitor network requests, manage cookies, or work with web apps. Also trigger when the user mentions: a website URL, opening a page, browser, Chrome, web scraping, web automation, portal, dashboard, login page, iframe, or checking a site. Even if the user just pastes a URL and asks to 'check it' or 'see what's there', use this skill. No setup needed — Chrome launches automatically."
argument-hint: "[url]"
allowed-tools: Bash, Read, Grep, Glob
---

# WebPilot — Browser Control CLI

No setup required. Chrome launches automatically in the background.

```bash
webpilot capture --dom --url "https://example.com"    # Just works
webpilot --browser capture --dom                       # SSO mode (user's Chrome)
```

All commands prefixed with `webpilot`. JSON when piped, human-readable in terminal.

## Workflow: Capture → Decide → Act → Verify

```bash
webpilot capture --dom --url "https://example.com"   # 1. See elements
# Read [index] list, pick target                      # 2. Decide
webpilot action click 5                               # 3. Act (returns url_changed if navigated)
webpilot capture --dom                                # 4. Verify
```

## Element Output Format

```
*[1] input#query "Search" type=text @search
[2] button "Go" @search
[3] a "Home" href="/" @nav
--- Page: Example (https://example.com) ---
--- Scroll: 25% (0.5 above, 1.2 below) ---
```

- `[N]` = element index (use with `action click N`, `action type N "text"`)
- `*` prefix = element is NEW since last capture (appeared after your action)
- `#id` = HTML element id
- `@ctx` = landmark context (nav, main, form, search, header, footer)

## Commands

| Category | Command | Description |
|----------|---------|-------------|
| **Capture** | `capture --dom` | Interactive elements |
| | `capture --screenshot` | Viewport screenshot |
| | `capture --annotate` | Screenshot with numbered labels |
| | `capture --dom --text --screenshot` | All at once (single JSON) |
| | `capture --dom --occlusion` | Mark occluded elements |
| | `--browser capture --screenshot --fullpage` | Full-page screenshot (browser only) |
| **Find** | `find --role button --text "Submit"` | Semantic search |
| | `find --placeholder "Search" --click` | Find + click |
| | `find --label "Email" --fill "a@b.com"` | Find + type |
| **Action** | `action click N` | Click element [N] |
| | `action type N "text" --clear` | Type into [N] |
| | `action keypress Enter` | Press key |
| | `action navigate "URL"` | Go to URL |
| | `action scroll down` / `up` | Scroll page |
| | `action hover N` / `focus N` | Hover/focus |
| | `action select N "value"` | Select dropdown option |
| | `--browser action upload N "/path"` | File upload (browser only) |
| **Wait** | `wait --selector ".results"` | Wait for element |
| | `wait --text "Success"` | Wait for text |
| | `wait --navigation` | Wait for page load |
| **Inspect** | `eval "document.title"` | Run JavaScript |
| | `dom get-text "h1"` | Get text by selector |
| | `dom set-html "td" "<p>val</p>"` | Set innerHTML |
| | `dom set-attr "input" "value" "x"` | Set attribute |
| **Frames** | `frames` | List iframes |
| | `frames switch "name"` | Switch to iframe |
| | `frames main` | Back to main |
| **Monitor** | `network start` / `read` / `clear` | Track fetch/XHR |
| | `console start` / `read` / `clear` | Capture JS logs |
| **Session** | `tabs` | List tabs |
| | `tabs switch ID` / `new URL` / `close ID` | Manage tabs |
| | `cookies list "URL"` | List cookies |
| | `session export --output f.json` | Save session |
| | `session import f.json` | Restore session |
| | `fetch "URL" --method POST --body '{}'` | Fetch with cookies |
| **Safety** | `policy set --action navigate --verdict deny` | Block actions |
| | `policy list` / `clear` | Manage policies |
| **Compare** | `diff --dom a.json b.json` | DOM diff |
| | `diff --screenshot a.png b.png` | Pixel diff |
| **Status** | `status` | Connection check |

## Key Behaviors

- Actions return `url_changed` when navigation occurs and `new_tab` when `target="_blank"` opens
- New tabs are auto-detected and switched to automatically
- `network start` / `console start` persist across page navigations (auto-reinjected)
- `--annotate` automatically enables `--dom --screenshot --bounds`
- `find` with `--click` or `--fill` executes the action on the first match

## Decision Guide

| Situation | Command |
|-----------|---------|
| First time on page | `capture --dom --url "URL"` |
| Need visual context | `capture --annotate` |
| Know the button text | `find --role button --text "Submit" --click` |
| Form field by label | `find --label "Email" --fill "value"` |
| Page still loading | `wait --selector ".content"` or `wait --navigation` |
| Working with iframes | `frames` → `frames switch "name"` → commands → `frames main` |
| API call with auth | `fetch "URL" --method POST --body '{}'` |
| Debug failing action | `network start` → action → `network read` |
| Verify action worked | Check `url_changed` in response, or `capture --dom` |
| Complex DOM edit | `dom set-html "selector" "<html>"` |
| Track JS errors | `console start` → actions → `console read --level error` |

## Error Recovery

| Error | Action |
|-------|--------|
| "Element [N] not found" | Re-run `capture --dom` — indices changed after page update |
| "No web page tab" | Run `capture --dom --url "URL"` to navigate first |
| "Chrome not found" | Install Chrome or set `WEBPILOT_CHROME=/path/to/chrome` |
| "CDP timeout" | Chrome may be busy. Try `webpilot quit` then retry |
| "Timed out" | Increase timeout: `wait --timeout 15` |
| Action didn't work | Check `network read` for API errors, or `console read` for JS errors |
| Need SSO/login | Use `--browser` flag: `webpilot --browser capture --dom` |
