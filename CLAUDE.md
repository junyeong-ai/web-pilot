# WebPilot - Browser Control Tool for AI Agents

WebPilot lets AI agents (like Claude Code) control Chrome browser through a CLI.
It captures DOM state, takes screenshots, and executes actions (click, type, scroll, navigate).

## Quick Start

### 1. Build
```bash
cargo build --workspace
```

### 2. Use (Headless — Default, No Setup Required)
```bash
webpilot capture --dom --url "https://example.com"
```
Chrome for Testing launches automatically in the background. No extension or manual setup needed.

```bash
webpilot status                    # Check connection
webpilot quit                      # Stop headless Chrome
```

### 3. Browser Mode (Optional — For SSO/Login Sessions)
When you need access to your real Chrome's SSO sessions and cookies:
```bash
webpilot install --extension-id jfghnlpbmpkplmemfemnkfckelipodfk
```
Then load the extension in Chrome (`chrome://extensions` → Load unpacked → `extension/`), and use:
```bash
webpilot --browser capture --dom   # Uses your Chrome with SSO
```

### 4. Global Install (Optional)
```bash
./scripts/install.sh
```
Installs the `webpilot` binary to `~/.local/bin` and the `/webpilot` Claude Code skill.
Supports `--yes`, `--quiet`, `--no-skill`, `--source`, `--download` flags.

## Commands

### Capture Page State
```bash
# DOM only (interactive elements)
webpilot capture --dom

# Navigate to URL first, then capture
webpilot capture --dom --url "https://google.com"

# Screenshot (saves to /tmp/webpilot/ and returns path)
webpilot capture --screenshot

# Both DOM and screenshot
webpilot capture --dom --screenshot --url "https://github.com"
```

DOM output format (each line = one interactive element):
```
[1] input#search-box "Search" type=text @search
[2] button "Google Search" @search
[3] a "Gmail" href="https://mail.google.com/" @nav
--- Page: Google (https://www.google.com/) ---
--- Scroll: entire page visible ---
--- 3 elements (from 847 nodes, 12ms) ---
```

Format: `[index] tag#id "text" attributes @landmark`
- `*` prefix = element appeared since last capture
- `#id` = HTML id (alphanumeric only)
- `@landmark` = parent landmark context (nav, main, form, search, etc.)

### Execute Actions (by element index from `capture --dom`)
```bash
webpilot action click 5                    # Click element [5]
webpilot action type 3 "hello world"       # Type text into element [3]
webpilot action type 3 "new text" --clear  # Clear field first, then type
webpilot action keypress Enter             # Press Enter key
webpilot action keypress a --ctrl          # Ctrl+A (select all)
webpilot action navigate "https://..."     # Go to URL
webpilot action scroll down                # Scroll down 600px
webpilot action scroll up 300              # Scroll up 300px
webpilot action back                       # Browser back
webpilot action forward                    # Browser forward
webpilot action reload                     # Reload page
webpilot action hover 7                    # Hover over element [7]
webpilot action focus 4                    # Focus element [4]
webpilot action drag 3 7                   # Drag element [3] to element [7]
webpilot action drag 3 7 --steps 10        # Drag with more intermediate points
webpilot action select 8 "option_value"    # Select dropdown option
webpilot --browser action upload 5 "/path/to/file"   # Upload file (--browser mode only)
```

### Evaluate JavaScript
```bash
webpilot eval "document.title"                     # Get page title
webpilot eval "document.querySelectorAll('a').length"  # Count links
webpilot eval "location.href"                      # Get current URL
```
Note: In headless mode (default), `eval` runs in the page's MAIN world via CDP (access to page variables, bypasses CSP). In `--browser` mode, `eval` runs in an isolated context (DOM access only, no page JS variables, but CSP-safe).

### Wait for Conditions
```bash
webpilot wait --selector ".results"                # Wait for element to appear
webpilot wait --text "Success"                     # Wait for text on page
webpilot wait --navigation --timeout 15            # Wait for page navigation
webpilot wait --timeout 3                          # Wait for DOM idle (no changes for 500ms)
```

### Manage Tabs
```bash
webpilot tabs                                      # List all tabs
webpilot tabs switch <TAB_ID>                      # Switch to tab
webpilot tabs new "https://example.com"            # Open new tab
webpilot tabs close <TAB_ID>                       # Close tab
webpilot tabs find --url "*github*"                # Find and switch to tab by URL pattern
```

### Navigate Iframes
```bash
webpilot frames                                       # List all frames
webpilot frames switch "ifm_main_contents"            # Switch to frame by name
webpilot frames url "*ehr*"                           # Switch by URL pattern
webpilot frames find "document.body.contentEditable==='true'"  # By JS predicate
webpilot frames main                                  # Back to main frame
```
After switching, all commands (eval, action, capture, dom) execute in that frame.

### Read/Write DOM Elements
```bash
webpilot dom set-html "td:nth-child(3)" "<p>content</p>"   # Set innerHTML
webpilot dom set-text "span.checkbox" "■"                   # Set textContent
webpilot dom set-attr "input#name" "value" "홍길동"          # Set attribute
webpilot dom get-html "div.content"                          # Get innerHTML
webpilot dom get-text "h1"                                   # Get textContent
webpilot dom get-attr "input" "type"                         # Get attribute
```

### Fetch with Browser Session
```bash
webpilot fetch "https://api.example.com/data"                # GET with cookies
webpilot fetch "https://api.example.com" --method POST --body '{"key":"val"}'
```
Executes `fetch()` in the browser context with `credentials: "include"` — uses all session cookies (SSO, auth tokens).

### Find Elements by Semantic Properties
```bash
webpilot find --role button --text "Submit"          # Find by ARIA role + text
webpilot find --role link                            # Find by ARIA role (implicit roles supported)
webpilot find --label "Email"                         # Find by form label
webpilot find --placeholder "Search" --click          # Find + click first match
webpilot find --tag input --text "Name" --fill "John" # Find + type text
```

### Compare Snapshots (Diff)
```bash
webpilot diff --dom before.json after.json           # Text diff of DOM snapshots
webpilot diff --screenshot before.png after.png       # Pixel diff (% changed + diff image)
```

### Network Monitoring
```bash
webpilot network start                               # Start monitoring fetch/XHR requests
webpilot network read                                 # Read captured requests
webpilot network read --since 1700000000000           # Filter by timestamp
webpilot network clear                                # Clear captured requests
```
No debugger banner — uses fetch/XHR interception, not CDP.

### Console Capture
```bash
webpilot console start                               # Start capturing console output
webpilot console read                                 # Read captured entries
webpilot console read --level error                   # Filter by level
webpilot console clear                                # Clear captured entries
```

### Session State
```bash
webpilot session export                              # Export cookies + localStorage
webpilot session export --output ~/session.json       # Custom output path
webpilot session import ~/session.json                # Restore session
```

### Action Safety Policy
```bash
webpilot policy set --action navigate --verdict deny  # Block navigation
webpilot policy set --action click --verdict allow     # Allow clicks
webpilot policy list                                   # Show policies
webpilot policy clear                                  # Remove all policies
```

### Manage Cookies
```bash
webpilot cookies list "https://example.com"         # List all cookies
webpilot cookies get "https://example.com" "name"    # Get specific cookie
webpilot cookies set "https://example.com" "k" "v" --secure --httponly
webpilot cookies delete "https://example.com" "name"
```

### Advanced Capture
```bash
webpilot capture --annotate                          # Annotated screenshot: numbered labels on elements
webpilot capture --dom --occlusion                   # Detect elements hidden behind overlays
webpilot capture --pdf                               # Generate PDF
webpilot capture --pdf --url "https://..."           # Navigate + PDF
webpilot --browser capture --screenshot --full-page  # Full-page screenshot (--browser mode only)
webpilot --browser capture --accessibility           # Accessibility tree (--browser mode only)
```

### Device Emulation
```bash
webpilot device set --width 390 --height 844 --mobile  # Custom viewport + mobile emulation
webpilot device set --width 1920 --height 1080 --scale 2.0  # HiDPI viewport
webpilot device set --width 390 --height 844 --mobile --user-agent "Mozilla/5.0..."  # Custom user agent
webpilot device preset iphone-15                        # Use a built-in preset
webpilot device reset                                   # Clear emulation
```
Available presets: iphone-15, iphone-15-pro, pixel-8, ipad-pro, galaxy-s24

### CPU Profiling
```bash
webpilot profile --duration 5                          # Profile for 5 seconds
webpilot profile --duration 10 --url "https://..."     # Navigate + profile
```
Saves `.cpuprofile` (importable in Chrome DevTools).

### Record Sequential Frames (AI Analysis)
```bash
webpilot record --frames 5 --interval 500              # 5 frames, 500ms apart
webpilot record --frames 10 --interval 1000 --dom      # Include DOM per frame
webpilot record --duration 5000 --interval 500          # Duration-based
webpilot record --frames 3 --url "https://..."          # Navigate + record
```

### Context Isolation (Multi-Agent)
```bash
webpilot --context agent-1 capture --dom --url "https://naver.com"
webpilot --context agent-2 capture --dom --url "https://google.com"
webpilot context list                                  # List active contexts
webpilot context close agent-1                         # Close a context
webpilot context close --all                           # Close all contexts
```
Each `--context` gets isolated cookies, localStorage, cache via CDP BrowserContext.
Max 16 concurrent contexts, 1-hour TTL with automatic cleanup.

### Check Status
```bash
webpilot status
```

## Output Modes

- **Terminal** (stdout is TTY): human-readable text
- **Piped** (stdout is not TTY): JSON automatically
- **Force JSON**: add `--json` flag

## Typical Usage Flow (by AI Agent)

```bash
# 1. See what's on the page
webpilot capture --dom --url "https://google.com"

# 2. AI reads the element list and decides to type in the search box [1]
webpilot action type 1 "Rust programming"

# 3. AI presses Enter to search
webpilot action keypress Enter

# 4. AI captures the results page
webpilot capture --dom --screenshot

# 5. AI reads the screenshot file and DOM to analyze results
```

## Architecture

```
Headless mode (default):
  AI Agent → webpilot CLI
    → CDP WebSocket → Chrome for Testing (headless)
    → bridge.js injected via Runtime.evaluate
    → DOM extraction, actions, screenshots

Browser mode (--browser):
  AI Agent → webpilot --browser CLI
    → Unix Socket → NM Host → Chrome Extension
    → bridge.js via content script
    → SSO sessions preserved
```

Single binary `webpilot` with three modes:
- **CLI mode** (default): launches headless Chrome, communicates via CDP
- **Browser mode** (`--browser`): connects to user's Chrome via Extension + NM
- **Host mode** (auto-detected when launched by Chrome): bridges Extension ↔ CLI

## Troubleshooting

### Headless mode (default)

**"Chrome not found"**
- Install Chrome or Chrome for Testing, or set `WEBPILOT_CHROME=/path/to/chrome`
- If agent-browser is installed, its Chrome for Testing is auto-detected

**"CDP timeout"**
- Chrome may be busy. Run `webpilot quit` and retry
- Check if Chrome process is running: `ps aux | grep "Chrome for Testing"`

**Session stuck**
- `webpilot quit` stops the headless Chrome
- PID file at `/tmp/webpilot-<user>-headless.pid`
- WebSocket URL at `/tmp/webpilot-<user>-headless.ws`

### Browser mode (--browser)

**"Not connected"**
1. Run `webpilot install --extension-id <ID>`
2. Load extension in Chrome: `chrome://extensions` → Load unpacked → `extension/`
3. Reload the extension (↻ icon)

**Extension errors**
- `chrome://extensions` → WebPilot → "service worker" link → Console tab

## Project Structure

```
web-pilot/
├── crates/
│   ├── webpilot/           # Shared library
│   │   └── src/
│   │       ├── protocol.rs     # Command/Response types
│   │       ├── types.rs        # DomSnapshot, InteractiveElement, serialize_dom()
│   │       ├── native_messaging.rs  # NM protocol (browser mode)
│   │       ├── ipc.rs          # Unix Socket IPC (browser mode)
│   │       └── screenshot.rs   # Image processing
│   │
│   └── webpilot-cli/       # CLI binary
│       └── src/
│           ├── main.rs         # Mode detection (CLI/Host)
│           ├── cdp.rs          # CDP WebSocket client (headless mode)
│           ├── headless.rs     # Headless command execution via CDP
│           ├── session.rs      # Chrome lifecycle management
│           ├── cli.rs          # CLI routing (headless default, --browser)
│           ├── host.rs         # NM Host bridge (browser mode)
│           ├── output.rs       # JSON/Human output + format_error()
│           ├── stitch.rs       # Full-page screenshot tile stitching
│           └── commands/       # 21 command modules
│
├── extension/              # Chrome Extension (browser mode)
│   ├── manifest.json
│   ├── background/service-worker.js
│   ├── content/bridge.js         # DOM extraction + actions (shared with headless)
│   ├── sidepanel/
│   └── popup/
│
└── CLAUDE.md               # This file
```
