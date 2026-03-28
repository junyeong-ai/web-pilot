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

### 4. Install Claude Code Skill (Optional)
```bash
./scripts/install-skill.sh
```
Installs the `/webpilot` skill for Claude Code. Auto-activates on browser tasks.

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
webpilot action select 8 "option_value"    # Select dropdown option
webpilot action upload 5 "/path/to/file"   # Upload file (CDP, shows debugger banner)
```

### Evaluate JavaScript
```bash
webpilot eval "document.title"                     # Get page title
webpilot eval "document.querySelectorAll('a').length"  # Count links
webpilot eval "location.href"                      # Get current URL
```
Note: `eval` runs in the page's MAIN world (access to page variables). May fail on pages with strict CSP that blocks `unsafe-eval`.

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
webpilot capture --screenshot --fullpage             # Full-page screenshot (tile-and-stitch)
webpilot capture --accessibility                     # Accessibility tree via CDP (shows debugger banner)
```

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

### "Not connected" error
1. Check that the Extension is loaded in Chrome: `chrome://extensions`
2. Check that NM manifest is installed: `cat ~/Library/Application\ Support/Google/Chrome/NativeMessagingHosts/com.webpilot.host.json`
3. Verify the extension ID in the manifest matches Chrome's Extension ID
4. Reload the Extension in Chrome (click ↻ icon)
5. Check service worker console: click "service worker" link on the Extension page

### Extension shows errors
- Open `chrome://extensions` → WebPilot → click "service worker" link
- Check the Console tab for `[WebPilot]` logs and errors

### Socket file
The IPC socket is at `/tmp/webpilot-<username>.sock`.
If it exists but status fails, the host process may have crashed.
Delete it and reload the Extension: `rm /tmp/webpilot-*.sock`

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
│           └── commands/       # 18 command modules
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
