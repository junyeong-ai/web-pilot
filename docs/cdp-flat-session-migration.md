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

## Cross-site iframes through child sessions (adopted in 0.14)

Site isolation renders a cross-site iframe in a process of its own. The page's
session sees it only from outside: `Page.getFrameTree` leaves it out and its
documents' contexts never reach the page's `Runtime`. Chrome lists each one
as a `Target.getTargets` entry of type `iframe` whose id is its frame id, with
`parentId` naming the parent target; a flat session attached to it reaches
its frames, and the root of its `Page.getFrameTree` names the owning frame
as `parentId`. Measured on Chrome for Testing 157.0.8103.78 with WebPilot's
launch flags: a click dispatched on the page's session at the `<iframe>`'s
content-box origin (`DOM.getFrameOwner` + `DOM.getBoxModel` in the parent's
session) plus the element's quad in the child's session lands as a trusted
click; `Input.insertText` and key events on the page's session reach the
focused input inside the iframe; a dialog raised inside it is reported on the
page's session; and when the iframe navigates to its parent's site its
session detaches and the frame rejoins the parent's tree.

So every frame has a host session (`transport/local/frames.rs`). Remote
sessions are attached from the target list when a command needs them, rather
than by `Target.setAutoAttach`, so the set a command sees is the browser's
at that moment and needs no event ordering. Browser mode reaches the same
frames through child sessions the tab's debugger auto-attaches, only when
`webNavigation` reports the frame in another process than the main frame.

This was rejected in 0.7 on Chrome for Testing 150, whose headless mode listed
no `iframe` target for a cross-site iframe under any auto-attach setting;
Chrome 155 and later list them.

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
