// Worker session state: the active tab/frame pins, armed-monitor flags, and
// host-pushed config — persisted across MV3 suspensions and restored before
// any command runs. Mirrors the persisted-pin state in transport/local/mod.rs.

let activeFrameId = 0;
let activeTabId = null;
// The armed-monitor intent is AGENT-level, not per-tab — the headless model
// exactly (one persisted flag per kind, re-armed on the pinned tab at every
// pin move and navigation settle). Keying it by tab id would tie the intent to
// one tab's lifetime: closing the pinned tab would silently disarm a monitor
// the agent started and never stopped, and the next `read` would claim
// "monitoring is not active" — a lie about the agent's own state.
const monitoringState = { console: false, network: false };

// An MV3 service worker is killed when idle and restarted on the next event,
// losing in-memory state. `activeTabId`, `activeFrameId` and the monitoring
// flags are persisted to session storage and restored here before any command
// runs — otherwise the first command after a restart would silently target the
// wrong tab or the main frame instead of the iframe the agent had switched to.
//
// `ensureRestored()` THROWS if the store is unreadable rather than resolving with
// default (empty) state: a swallowed failure is indistinguishable from "no
// session", so dispatching against it would silently retarget the pin onto the
// focused tab — the very thing the restore exists to prevent. It is memoized once
// it succeeds; a failed attempt is NOT cached, so the next caller retries instead
// of wedging the worker on a transient storage hiccup.
let sessionRestored = false;
let restorePromise = null;

function ensureRestored() {
  if (sessionRestored) return Promise.resolve();
  if (!restorePromise) {
    restorePromise = (async () => {
      const data = await chrome.storage.session.get([
        "activeTabId",
        "activeFrameId",
        "monitoring",
      ]);
      if (data?.activeTabId != null) activeTabId = data.activeTabId;
      if (data?.activeFrameId != null) activeFrameId = data.activeFrameId;
      if (data?.monitoring) {
        monitoringState.console = !!data.monitoring.console;
        monitoringState.network = !!data.monitoring.network;
      }
      sessionRestored = true;
    })().finally(() => {
      // Drop the in-flight promise so a rejected attempt re-runs next time;
      // a fulfilled one is gated out by `sessionRestored` above.
      if (!sessionRestored) restorePromise = null;
    });
  }
  return restorePromise;
}

function setActiveFrameId(id) {
  activeFrameId = id;
  chrome.storage.session?.set({ activeFrameId: id });
}

function setActiveTabId(id) {
  activeTabId = id;
  chrome.storage.session?.set({ activeTabId: id });
}

function saveMonitoringState() {
  chrome.storage.session?.set({
    monitoring: {
      console: monitoringState.console,
      network: monitoringState.network,
    },
  });
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

// Resolved host settings, pushed alongside every Pong — an MV3 worker is
// routinely suspended and restarted with empty state, and re-Pings on wake,
// which re-delivers this. The defaults here equal the host's own defaults, so
// behaviour before the first Config (or under an untuned install) is
// unchanged; only an operator-tuned value diverges, and then both modes move
// together. Unknown fields are ignored for forward compatibility.
// Defaults match the headless settings defaults; the host overwrites them on
// connect (and after an SW restart) with the operator's resolved settings, so a
// single source of truth tunes both modes.
const hostConfig = { navigationTimeoutMs: 15000, annotationPaintMs: 200 };

function applyHostConfig(cfg) {
  const nav = cfg?.timeouts?.navigation_ms;
  if (Number.isFinite(nav) && nav > 0) hostConfig.navigationTimeoutMs = nav;
  const paint = cfg?.timeouts?.annotation_paint_ms;
  if (Number.isFinite(paint) && paint >= 0) hostConfig.annotationPaintMs = paint;
}

function navigationTimeoutMs() {
  return hostConfig.navigationTimeoutMs;
}

// Time to let the annotation overlay paint before the screenshot — the same
// settings value headless uses (`timeouts.annotation_paint`), so the two modes
// can never drift on a hardcoded magic number.
function annotationPaintMs() {
  return hostConfig.annotationPaintMs;
}

// Bounded window for an async browser event to be observed — a document to
// parse after a navigation, or a frame's execution context to appear. The
// browser twin of the headless `PROBE` constant (also 2s). A miss degrades to a
// best-effort result (a slightly-early capture, a typed FrameNotFound), never a
// wrong one, so it is a fixed structural bound, not an operator knob.
const PROBE_MS = 2000;

// ── Active tab ──────────────────────────────────────────────────────────────
// Browser mode binds every command to ONE tab, exactly as headless binds to
// one target: pinned on first use (the focused window's active http tab),
// moved only by an explicit `tab switch` / `tab new` / a tab the acted-on page
// opened. A vanished pin is a typed TabNotFound — never a silent retarget to
// whatever tab happens to be focused, which would route the agent's actions to
// a page it has not seen. One pin per extension by design: multi-agent
// isolation is a headless `--context` feature. The pin's lifetime is the
// browser session (storage.session) on purpose — tab ids are meaningless
// across a browser restart, so a fresh session re-pins on first use exactly
// like a first run.
// The pinned tab, or the typed TabNotFound a vanished pin raises. `null` means no
// tab is pinned yet — the caller picks the unpinned fallback (a bridge command
// has no page; a navigate creates one). No http filtering here: the two resolvers
// below apply it (or not) per their needs.
async function pinnedTabOrThrow() {
  if (activeTabId == null) return null;
  const tab = await chrome.tabs.get(activeTabId).catch(() => null);
  if (tab) return tab;
  const e = new Error(`Tab not found: ${activeTabId}. List: webpilot tab`);
  e.code = "TabNotFound";
  e.data = { tab_id: String(activeTabId) };
  throw e;
}

async function resolveActiveTab() {
  const pinned = await pinnedTabOrThrow();
  if (pinned) {
    // The pinned tab must be an injectable http(s) page. A pin left on a
    // chrome:// / about: page has no bridge, so a command there is NoPage
    // (navigate first) — not a confusing BridgeUnavailable from a failed
    // inject. Matches the focused fallback below, which already returns null
    // for a non-http tab, so every caller already handles this.
    return pinned.url?.startsWith("http") ? pinned : null;
  }
  const [focused] = await chrome.tabs.query({ active: true, lastFocusedWindow: true });
  if (!focused?.url?.startsWith("http")) return null;
  setActiveTabId(focused.id);
  return focused;
}

// `navigate` is the one command that REPLACES the tab's URL, so it needs no
// injectable bridge and must reuse the pinned tab even when that tab is non-http
// (a pin left on about:blank / chrome://), where `resolveActiveTab` returns null
// to steer bridge commands to a NoPage. Headless navigates its bound target in
// place regardless of the old URL; without this, browser mode would orphan the
// non-http pin and open a SECOND tab — a tab-state divergence. With no pin yet it
// defers to `resolveActiveTab`'s focused-http-or-create path: a first navigate has
// no agent tab to reuse (exactly as headless creates its bound target at session
// open), and that path deliberately does NOT hijack the user's focused non-http
// tab (e.g. their new-tab page).
async function resolveActiveTabForNavigation() {
  return (await pinnedTabOrThrow()) ?? resolveActiveTab();
}

export { PROBE_MS, activeFrameId, activeTabId, annotationPaintMs, applyHostConfig, ensureRestored, monitoringState, navigationTimeoutMs, resolveActiveTab, resolveActiveTabForNavigation, saveMonitoringState, setActiveFrameId, setActiveTabId, sleep };
