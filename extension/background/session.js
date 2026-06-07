// // Worker session state: the active tab/frame pins, armed-monitor sets, and
// // host-pushed config — persisted across MV3 suspensions and restored before
// // any command runs. Mirrors the persisted-pin state in transport/local/mod.rs.

let activeFrameId = 0;
let activeTabId = null;
const monitoringState = { console: new Set(), network: new Set() };

// An MV3 service worker is killed when idle and restarted on the next event,
// losing in-memory state. `activeTabId`, `activeFrameId` and the monitoring
// sets are persisted to session storage and reloaded here. `RESTORED` resolves
// once all are back; `processCommand` awaits it so a command can never run
// against un-restored state — otherwise the first command after a restart
// would silently target the wrong tab or the main frame instead of the iframe
// the agent had switched to.
const RESTORED = (async () => {
  try {
    const data = await chrome.storage.session?.get([
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
  } catch {}
})();

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
const hostConfig = { navigationTimeoutMs: 15000 };

function applyHostConfig(cfg) {
  const nav = cfg?.timeouts?.navigation_ms;
  if (Number.isFinite(nav) && nav > 0) hostConfig.navigationTimeoutMs = nav;
}

function navigationTimeoutMs() {
  return hostConfig.navigationTimeoutMs;
}

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
    if (tab) return tab;
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

export { RESTORED, activeFrameId, activeTabId, applyHostConfig, monitoringState, navigationTimeoutMs, pruneTabMonitoring, resolveActiveTab, saveMonitoringState, setActiveFrameId, setActiveTabId, sleep };
