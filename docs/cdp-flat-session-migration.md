# CDP session model: decisions

Decision record. Headless mode drives every page as a flat-protocol session on
one browser connection since 0.7.0; the current design is described in
`crates/webpilot-cli/CLAUDE.md` (`cdp.rs`, `transport/local/`) and in the code.
This file keeps why it was chosen and the alternatives that were rejected.

## Flat sessions over one connection (adopted)

Headless mode used to open one WebSocket per page target beside the
browser-endpoint connection. The protocol plans to make flat mode the default
and retire the non-flattened one (`Target.attachToTarget` `flatten`, "We plan
to make this the default, deprecate non-flattened mode, and eventually retire
it", crbug.com/991325). The per-target socket also had two costs of its own:

- A cross-site navigation swapped the renderer and left the page socket
  useless, so every such navigation reconnected and re-primed the page. A flat
  session attaches to the target, which survives the swap; only the document's
  execution contexts are re-announced (`reemit_execution_contexts`).
- Overrides that Chrome drops when their client disconnects, such as
  `Emulation.setUserAgentOverride`, had one more connection to lose. They are
  re-applied per command (`ensure_device_emulation`) either way.

What the dedicated socket gave for free and a shared connection has to
reproduce: event isolation per page (`SessionEvents` filters the shared ring to
one `sessionId`), a prompt end to a wait whose tab closed (the detach watcher,
backstopped by Chrome's `-32001`), and teardown of every task a session spawned
(`CdpSession::Drop`), without which a long-lived `webpilot mcp` leaked a client
per Chrome restart.

## Cross-origin iframes through child sessions (rejected)

The hoped-for payoff was reaching out-of-process iframes through
`Target.setAutoAttach { flatten: true }` child sessions. Measured on Chrome for
Testing 150.0.7871.115 with the launch flags of that time: a parent on
`http://127.0.0.1:PORT` embedding `<iframe src="http://localhost:PORT/child">`
never produced a `Target.attachedToTarget` for the iframe, nor an `iframe` or
`page` entry in `Target.getTargets`, under per-session or browser-level
auto-attach, with or without `--site-per-process`. The iframe appears in
`Page.getFrameTree` only as a placeholder with an empty URL, which is why
`count_http_subframes` counts it as 0 and `frame switch` returns
`FrameNotFound`.

There is nothing to attach to, so the design could not deliver or even be
verified against a local fixture. Cross-origin iframes in a signed-in browser
are a browser-mode concern, handled by the extension, which this headless code
cannot help. Revisit if a later Chrome lists headless cross-site iframes as
attachable targets: rerun the fixture above first.

## Console and network monitors stay in the page (decided)

`Network.enable` events reach only the connection that enabled them, while the
armed-monitor contract spans processes: `network start` in one CLI invocation,
the navigation in a second, `network read` in a third. No headless process
lives across those boundaries to collect CDP events, so the in-page buffer
(`window.__webpilot_network`, `window.__webpilot_console`) is the persistence.
The cost is accepted: subresource loads, service-worker fetches and WebSockets
are not recorded. Revisit only if a persistent per-context collector exists.

## WebDriver BiDi (rejected)

WebPilot drives Chrome only. CDP remains the complete protocol for Chrome in
Puppeteer and Playwright, and nothing WebPilot needs is BiDi-only.
