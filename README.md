# WebPilot

[![Rust](https://img.shields.io/badge/rust-1.96.0-orange?style=flat-square&logo=rust)](https://www.rust-lang.org)

**Browser-control CLI for AI agents.** Zero setup — Chrome launches automatically.

---

## Why WebPilot?

- **Zero setup** — start with one line: `webpilot capture --include dom --url URL` (Chrome auto-launches)
- **Full command surface** — DOM, screenshots, actions, network, console, cookies, sessions, policies
- **Headless + SSO** — headless by default; `--browser` drives your real Chrome with its SSO sessions
- **Persistent session** — Chrome stays warm; every later command reconnects (launch cost paid once)
- **AI-native** — token-efficient DOM output, semantic search, typed errors with guidance, and an **MCP server** (`webpilot mcp`) over the same engine

---

## Quick Start

```bash
# One-line install — binary download + checksum verification + interactive setup
curl -fsSL https://raw.githubusercontent.com/junyeong-ai/web-pilot/main/scripts/install.sh | bash

# Use immediately (headless — Chrome auto-launches)
webpilot capture --include dom --url "https://example.com"

# Build from source (run inside a checkout; requires Rust 1.96)
WEBPILOT_BUILD=source bash scripts/install.sh

# When you need SSO (drive your own Chrome)
webpilot setup extension                       # extract the extension + Chrome guide
webpilot setup nm-host --extension-id <ID>     # register the Native Messaging host
webpilot --browser capture --include dom

# Expose the same engine to any MCP host
webpilot mcp
```

The installer sets up the skill and extension from assets embedded in the
binary via `webpilot setup`. Inside a checkout it offers prebuilt/source
interactively.

| Env var | Default | Meaning |
|---|---|---|
| `WEBPILOT_BUILD` | `prebuilt` | `prebuilt` (release download) or `source` (`cargo build`) |
| `WEBPILOT_VERSION` | latest | pin a tag (prebuilt, e.g. `v0.3.0`) |
| `WEBPILOT_INSTALL_DIR` | `$HOME/.local/bin` | install path |
| `WEBPILOT_REPO` | `junyeong-ai/web-pilot` | override when using a fork |
| `WEBPILOT_NO_SETUP=1` | — | skip the automatic `webpilot setup` after install |

---

## Highlights

### Page capture
```bash
webpilot capture --include dom --url "https://example.com"  # DOM element list
webpilot capture --include screenshot                       # viewport screenshot
webpilot capture --include screenshot --annotate            # numbered-overlay screenshot
webpilot capture --include screenshot --full-page           # whole page, one shot
webpilot capture --include pdf                              # PDF
webpilot capture --include dom text screenshot              # combined JSON output
```

### Find + act
```bash
webpilot find --role button --text "Submit" --click  # semantic search + click
webpilot find --label "Email" --fill "user@test.com" # label search + fill
webpilot action click 5                              # click by index
webpilot action type 3 "hello" --clear               # type text
webpilot action key-press Enter                      # key press
```

### Device emulation
```bash
webpilot device preset iphone-15                       # mobile device emulation
webpilot device set --width 390 --height 844 --mobile  # custom viewport
webpilot device reset                                  # clear emulation
```

### Monitoring
```bash
webpilot network start && webpilot network read      # track network requests
webpilot console start && webpilot console read      # capture JS console
```

### Sessions
```bash
webpilot cookie list "https://example.com"           # read cookies
webpilot session export --output session.json        # save session state
webpilot fetch "https://api.example.com" --method POST --body '{}'  # authed API call
```

### Safety controls
```bash
webpilot policy default deny                             # least-privilege lockdown
webpilot policy set --operation click --verdict allow    # then allowlist
webpilot policy set --operation navigate --verdict deny  # or deny selectively
webpilot policy list                                     # inspect
```

---

## DOM output format

```
*[1] input#query "Search" type=text @search
[2] button "Go" @search
[3] a "Home" href="/" @nav
--- Page: Example (https://example.com) ---
--- Scroll: 25% (0.5 above, 1.2 below) ---
--- 3 elements (from 120 nodes, 5ms) ---
```

| Marker | Meaning |
|------|------|
| `[N]` | element index (use with `action click N`) |
| `*` | element new since the previous capture |
| `#id` | HTML element id |
| `@ctx` | landmark context (nav, main, form, search) |

---

## Architecture

```
Headless (default):
  webpilot CLI → CDP WebSocket → Chrome for Testing (headless)
               → bridge.js (injected via Runtime.evaluate)

Browser (--browser):
  webpilot CLI → Unix socket → NM Host → Chrome extension
               → bridge.js (content script)

MCP (webpilot mcp):
  MCP host → stdio JSON-RPC → the same Transport + handlers as the CLI
```

- **One binary** — `webpilot` switches between CLI / host / MCP roles automatically
- **One codebase** — the same `bridge.js` serves both modes
- **Full surface** — `webpilot --help` for the complete command list

---

## Comparison

| Feature | WebPilot | agent-browser | browser-use |
|------|:--------:|:-------------:|:-----------:|
| SSO sessions (`--browser`) | **built in** | ✗ | ✗ |
| Network monitoring | **built in** | ✗ | CDP |
| Console capture | **built in** | ✗ | CDP |
| Semantic search (`find`) | **built in** | ✗ | XPath |
| Direct DOM read/write | **built in** | ✗ | ✗ |
| Annotated screenshots | **built in** | built in | PIL |
| Session export/import | **built in** | auth state | ✗ |
| Multi-agent isolation | **`--context`** | ✗ | ✗ |
| MCP server | **built in** | ✗ | ✗ |
| Runtime dependencies | **none (single binary)** | Node | Python |

---

## Lifecycle

```bash
webpilot setup                 # interactive setup: skill + extension + NM host
webpilot setup skill           # (re)install just the skill
webpilot setup extension       # extract the extension + Chrome guide (opens chrome://extensions)
webpilot setup nm-host --extension-id <ID>

webpilot self update           # self-update to the latest release (atomic, sha256-verified)
webpilot self update --version 0.3.0   # pin a version

webpilot uninstall             # quit Chrome + remove everything the binary created
```

The skill and extension are embedded in the binary at compile time: version
drift cannot happen, and there is no post-install download. After setup,
Claude Code activates the skill via `/webpilot` or plain natural language.

---

## License

MIT
