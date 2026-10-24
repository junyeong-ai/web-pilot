// Worker session state: the active tab/frame pins, armed-monitor flags, and
// host-pushed config — persisted across MV3 suspensions and restored before
// any command runs. Mirrors the persisted-pin state in transport/local/mod.rs.

import { rearmMonitors } from "./state.js";

let activeFrameId = 0;
let activeTabId = null;
// A borrowed pin is the tab the user was viewing, taken by a read while nothing
// was pinned: commands act on it, but loading a URL opens a tab of the agent's
// own rather than replacing the user's page.
let activeTabBorrowed = false;
// The armed-monitor intent is AGENT-level, not per-tab — the headless model
// exactly (one persisted flag per kind, re-armed on the pinned tab at every
// pin move and navigation settle). Keying it by tab id would tie the intent to
// one tab's lifetime: closing the pinned tab would silently disarm a monitor
// the agent started and never stopped, and the next `read` would claim
// "monitoring is not active" — a lie about the agent's own state.
const monitoringState = { console: false, network: false };

// An MV3 service worker is killed when idle and restarted on the next event,
// losing in-memory state. The pin, `activeFrameId` and the monitoring
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
        "activeTabBorrowed",
        "activeFrameId",
        "monitoring",
      ]);
      if (data?.activeTabId != null) activeTabId = data.activeTabId;
      activeTabBorrowed = data?.activeTabBorrowed === true;
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

// Pin the tab commands act on. Every pin is the agent's own unless a read
// borrowed the user's viewed tab.
function pinTab(tabId, { borrowed = false } = {}) {
  activeTabId = tabId;
  activeTabBorrowed = borrowed;
  chrome.storage.session?.set({ activeTabId: tabId, activeTabBorrowed: borrowed });
}

// Release the pin: the next command starts over from first use.
function unpinTab() {
  pinTab(null);
  setActiveFrameId(0);
}

// The pin as it stands, for a command that must put it back if it fails.
function currentPin() {
  return { tabId: activeTabId, borrowed: activeTabBorrowed, frameId: activeFrameId };
}

function restorePin(pin) {
  pinTab(pin.tabId, { borrowed: pin.borrowed });
  setActiveFrameId(pin.frameId);
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
const hostConfig = { acceptDialogs: false, navigationTimeoutMs: 15000, annotationPaintMs: 200 };

function applyHostConfig(cfg) {
  if (typeof cfg?.accept_dialogs === "boolean") hostConfig.acceptDialogs = cfg.accept_dialogs;
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
function acceptDialogs() {
  return hostConfig.acceptDialogs;
}

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
// Browser mode binds every command to ONE tab, as headless binds to one target,
// but it shares Chrome with its user, so the pin records how it was chosen. With
// nothing pinned, loading a URL opens a tab of the agent's own, a read borrows
// the tab the user is viewing, and any other command is refused (NoPinnedTab):
// an effect never lands on a page the agent has neither opened nor observed.
// Effects act on a borrowed tab, but loading a URL opens a new tab rather than
// replacing the user's page. Once set, the pin moves only by `tab new` /
// `tab switch` / a tab a URL load or the acted-on page opened, and `tab close`
// on it releases it. A tab that vanishes any other way is a typed
// TabNotFound — never a silent retarget to whatever tab is focused. One pin per
// extension by design: multi-agent isolation is a headless `--context` feature.
// The pin lives as long as the browser session (storage.session): tab ids mean
// nothing across a restart, so a fresh session starts unpinned like a first run.

// The pinned tab, or the typed TabNotFound a vanished pin raises; `null` when
// nothing is pinned. No http filtering here: the resolvers below apply it.
async function pinnedTabOrThrow() {
  if (activeTabId == null) return null;
  const tab = await chrome.tabs.get(activeTabId).catch(() => null);
  if (tab) return tab;
  const e = new Error(`Tab not found: ${activeTabId}`);
  e.code = "TabNotFound";
  e.data = { tab_id: String(activeTabId) };
  throw e;
}

// The tab the user is viewing: the active tab of the last focused window.
async function viewedTab() {
  const [tab] = await chrome.tabs.query({ active: true, lastFocusedWindow: true });
  return tab ?? null;
}

// The tab an effect acts on. `null` when the pinned tab is not an http(s) page:
// it has no bridge, so the caller reports NoPage (navigate first) rather than a
// confusing BridgeUnavailable from a failed inject.
async function resolveActiveTab() {
  const pinned = await pinnedTabOrThrow();
  if (!pinned) {
    const e = new Error("No tab is pinned yet");
    e.code = "NoPinnedTab";
    throw e;
  }
  return pinned.url?.startsWith("http") ? pinned : null;
}

// The tab a read observes. With nothing pinned it borrows the tab the user is
// viewing, so the commands that follow act on the page this read reported. A
// borrow is a pin move, so armed monitors follow it as they follow every other.
async function resolveTabForRead() {
  const pinned = await pinnedTabOrThrow();
  if (pinned) return pinned.url?.startsWith("http") ? pinned : null;
  const viewed = await viewedTab();
  if (!viewed?.url?.startsWith("http")) return null;
  pinTab(viewed.id, { borrowed: true });
  await rearmMonitors(viewed.id);
  return viewed;
}

export {
  acceptDialogs,
  PROBE_MS,
  activeFrameId,
  activeTabBorrowed,
  activeTabId,
  annotationPaintMs,
  applyHostConfig,
  currentPin,
  ensureRestored,
  monitoringState,
  navigationTimeoutMs,
  pinTab,
  pinnedTabOrThrow,
  resolveActiveTab,
  resolveTabForRead,
  restorePin,
  saveMonitoringState,
  setActiveFrameId,
  sleep,
  unpinTab,
  viewedTab,
};
