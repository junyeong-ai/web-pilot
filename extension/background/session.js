// // Worker session state: the active tab/frame pins, armed-monitor sets, and
// // host-pushed config — persisted across MV3 suspensions and restored before
// // any command runs. Mirrors the persisted-pin state in transport/local/mod.rs.

let activeFrameId = 0;
let activeTabId = null;
const monitoringState = { console: new Set(), network: new Set() };

// An MV3 service worker is killed when idle and restarted on the next event,
// losing in-memory state. `activeTabId`, `activeFrameId` and the monitoring
// sets are persisted to session storage and restored here before any command
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
        "monitoringTabs",
      ]);
      if (data?.activeTabId != null) activeTabId = data.activeTabId;
      if (data?.activeFrameId != null) activeFrameId = data.activeFrameId;
      if (data?.monitoringTabs) {
        (data.monitoringTabs.console || []).forEach((id) => monitoringState.console.add(id));
        (data.monitoringTabs.network || []).forEach((id) => monitoringState.network.add(id));
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
    monitoringTabs: {
      console: [...monitoringState.console],
      network: [...monitoringState.network],
    },
  });
}

// A closed tab's monitoring entries must not accumulate for the worker's
// lifetime. The pin is deliberately NOT cleared on tab removal: a vanished
// pin is a typed TabNotFound at the next command, never a silent retarget.
function pruneTabMonitoring(tabId) {
  const hadConsole = monitoringState.console.delete(tabId);
  const hadNetwork = monitoringState.network.delete(tabId);
  if (hadConsole || hadNetwork) saveMonitoringState();
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
async function resolveActiveTab() {
  if (activeTabId != null) {
    const tab = await chrome.tabs.get(activeTabId).catch(() => null);
    if (tab) {
      // The pinned tab must be an injectable http(s) page. A pin left on a
      // chrome:// / about: page has no bridge, so a command there is NoPage
      // (navigate first) — not a confusing BridgeUnavailable from a failed
      // inject. Matches the focused fallback below, which already returns null
      // for a non-http tab, so every caller already handles this.
      return tab.url?.startsWith("http") ? tab : null;
    }
    const e = new Error(`Tab not found: ${activeTabId}. List: webpilot tab`);
    e.code = "TabNotFound";
    e.data = { tab_id: String(activeTabId) };
    throw e;
  }
  const [focused] = await chrome.tabs.query({ active: true, lastFocusedWindow: true });
  if (!focused?.url?.startsWith("http")) return null;
  setActiveTabId(focused.id);
  return focused;
}

export { PROBE_MS, activeFrameId, activeTabId, annotationPaintMs, applyHostConfig, ensureRestored, monitoringState, navigationTimeoutMs, pruneTabMonitoring, resolveActiveTab, saveMonitoringState, setActiveFrameId, setActiveTabId, sleep };
