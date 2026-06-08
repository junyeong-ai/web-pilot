// // Browser-level commands: tabs, frames, status.
// // Mirrors transport/local/browser.rs.

import { err, noPageErr } from "./errors.js";
import { activeFrameId, activeTabId, resolveActiveTab, setActiveFrameId, setActiveTabId } from "./session.js";
import { cdpSend, withCdp } from "./cdp.js";
import { cdpEval, frameWorldContextId } from "./query.js";
import { isHostConnected } from "./host.js";

// ── Tabs ───────────────────────────────────────────────────────────────────

async function handleTabNew(url) {
  const created = await chrome.tabs.create({ url, active: true });
  setActiveTabId(created.id);
  // A fresh tab has its own frame tree — drop any frame scope.
  setActiveFrameId(0);
  return {
    type: "Action",
    success: true,
    new_tab: {
      id: String(created.id),
      url: created.url || url,
      title: created.title || "",
      active: true,
    },
  };
}

// chrome.tabs ids are non-negative integers. `parseInt` is too lenient — it
// reads "123x" as 123 and would act on the wrong tab — so an id must match
// exactly. A malformed id is a `TabNotFound` (exit 4), mirroring headless, where
// it simply fails to match any target id; never a silent action on a coerced tab.
function parseTabId(tabId) {
  if (typeof tabId === "number" && Number.isInteger(tabId) && tabId >= 0) return tabId;
  if (typeof tabId === "string" && /^\d+$/.test(tabId)) return Number(tabId);
  return null;
}

async function handleTabClose(tabId) {
  const id = parseTabId(tabId);
  if (id === null) {
    return tabNotFound(tabId);
  }
  try {
    await chrome.tabs.remove(id);
    return { type: "Action", success: true };
  } catch (e) {
    // A bad or already-closed id is a not-found — typed to match headless
    // (`do_tab_close` → `TabNotFound`, exit 4) instead of a generic exception.
    return { type: "Action", success: false, error: err("TabNotFound", e.message, { tab_id: String(tabId) }) };
  }
}

function tabNotFound(tabId) {
  return {
    type: "Action",
    success: false,
    error: err("TabNotFound", `invalid tab id: ${tabId}`, { tab_id: String(tabId) }),
  };
}

async function handleTabSwitch(tabId) {
  const target = parseTabId(tabId);
  if (target === null) {
    return tabNotFound(tabId);
  }
  try {
    // Make it the active tab within its own window so a user looking at that
    // window sees what the agent drives — but never raise the window to the
    // OS foreground: that steals focus from whatever app the user is in, and
    // capture/eval/actions all reach the tab through CDP regardless of which
    // window is frontmost. The pin below, not OS focus, is what commands follow.
    await chrome.tabs.update(target, { active: true });
    setActiveTabId(target);
    // A different tab has its own frame tree — drop any frame scope.
    setActiveFrameId(0);
    return { type: "Action", success: true };
  } catch (e) {
    return { type: "Action", success: false, error: err("TabNotFound", e.message, { tab_id: tabId }) };
  }
}

async function handleTabList() {
  const tabs = await chrome.tabs.query({});
  // `active` marks the WebPilot-PINNED tab — the one commands act on — to match
  // headless, where `active` is the bound CDP target. Chrome's own UI-foreground
  // flag (`t.active`) is irrelevant: the agent does not control it and commands
  // do not target it. Read the pinned id directly (no resolveActiveTab, which
  // would pin a tab or throw on a vanished pin — a list must do neither).
  return {
    type: "Tabs",
    tabs: tabs.map((t) => ({
      id: String(t.id),
      url: t.url || "",
      title: t.title || "",
      active: t.id === activeTabId,
    })),
  };
}

// ── Status ─────────────────────────────────────────────────────────────────

async function handleStatus() {
  // Report the pinned tab when one is set — that is the tab commands will act
  // on. Status is read-only, so it never pins as a side effect; without a pin
  // it shows the focused window's active tab (what a first command would pin).
  let tab = null;
  if (activeTabId != null) {
    tab = await chrome.tabs.get(activeTabId).catch(() => null);
  }
  if (!tab) {
    [tab] = await chrome.tabs.query({ active: true, lastFocusedWindow: true });
  }
  // Chrome version derives from the user agent (no direct API in MV3 SW).
  const ua = navigator.userAgent || "";
  const m = ua.match(/Chrome\/(\S+)/);
  return {
    type: "Status",
    connected: isHostConnected(),
    mode: "browser",
    // `?? null`, not `|| null`: a pinned tab with an empty title (a page with no
    // `<title>`) keeps `""`, matching headless `do_status`, which maps
    // `document.title` straight through as `Some("")`. `||` would report it as
    // `null` — "no tab" — in browser mode only, a silent cross-mode divergence.
    // `tab` being absent still yields `null` (optional chaining → undefined ?? null).
    tab_url: tab?.url ?? null,
    tab_title: tab?.title ?? null,
    chrome_version: m ? m[1] : null,
    extension_version: chrome.runtime.getManifest().version,
  };
}

// ── Frames ─────────────────────────────────────────────────────────────────

function activeFrameIdWire() {
  return activeFrameId === 0 ? null : String(activeFrameId);
}

async function handleFrameList() {
  const tab = await resolveActiveTab();
  if (!tab) return { type: "Frames", frames: [], active_frame_id: null };

  const all = await chrome.webNavigation.getAllFrames({ tabId: tab.id }).catch(() => []);
  const frames = all.map((f) => ({
    frame_id: String(f.frameId),
    url: f.url || "",
    name: null,
    parent_frame_id: f.parentFrameId >= 0 ? String(f.parentFrameId) : null,
    is_main: f.frameId === 0,
  }));

  await Promise.allSettled(all.map(async (f, idx) => {
    if (f.frameId === 0 || !f.url?.startsWith("http")) return;
    frames[idx].name = await readFrameName(tab.id, f.frameId);
  }));

  return { type: "Frames", frames, active_frame_id: activeFrameIdWire() };
}

// Read a frame's `window.name` with a PRECOMPILED injected function: no
// dynamic code, so neither the page's CSP nor the extension's can refuse it,
// and no debugger attach is needed for a plain property read.
async function readFrameName(tabId, frameId) {
  return chrome.scripting
    .executeScript({
      target: { tabId, frameIds: [frameId] },
      world: "MAIN",
      func: () => window.name,
    })
    .then((r) => r?.[0]?.result || null)
    .catch(() => null);
}

async function handleFrameSwitch(selector) {
  selector = selector || { by: "main" };

  if (selector.by === "main") {
    setActiveFrameId(0);
    return { type: "FrameSwitched", success: true, frame_id: null, name: "main", url: null };
  }

  const tab = await resolveActiveTab();
  if (!tab) {
    return { type: "FrameSwitched", success: false, frame_id: activeFrameIdWire(), error: noPageErr() };
  }

  const frames = await chrome.webNavigation.getAllFrames({ tabId: tab.id }).catch(() => []);
  const httpFrames = frames.filter((f) => f.frameId !== 0 && f.url?.startsWith("http"));

  let matched = null;

  if (selector.by === "name") {
    for (const f of httpFrames) {
      if ((await readFrameName(tab.id, f.frameId)) === selector.value) {
        matched = f;
        break;
      }
    }
  } else if (selector.by === "url") {
    const needle = (selector.pattern || "").replace(/\*/g, "");
    matched = httpFrames.find((f) => f.url?.includes(needle));
  } else if (selector.by === "predicate") {
    // A predicate is arbitrary caller JS, gated by the `eval` key — enforced
    // by the NM host before the command is forwarded here. It runs through the
    // same debugger-routed evaluation as `eval`, so a frame's CSP cannot block
    // it (headless parity) — one withCdp session probes every candidate frame.
    // Per-frame failures degrade to "didn't match" (the inner guards); a
    // failure of the SESSION itself — the debugger attach — propagates typed
    // through the router, never disguised as FrameNotFound.
    matched = await withCdp(tab.id, async (tid) => {
      await cdpSend(tid, "Runtime.enable", {});
      for (const f of httpFrames) {
        const uniqueContextId = await frameWorldContextId(tid, tab.id, f.frameId, "MAIN");
        if (uniqueContextId == null) continue;
        const r = await cdpEval(tid, selector.js, uniqueContextId).catch(() => null);
        if (r?.success && r.result && JSON.parse(r.result) === true) return f;
      }
      return null;
    });
  }

  if (matched) {
    setActiveFrameId(matched.frameId);
    return {
      type: "FrameSwitched",
      success: true,
      frame_id: String(matched.frameId),
      url: matched.url,
    };
  }

  const sel = JSON.stringify(selector);
  return {
    type: "FrameSwitched",
    success: false,
    frame_id: activeFrameIdWire(),
    error: err("FrameNotFound", `No matching frame: ${sel}`, { selector: sel }),
  };
}

export { handleFrameList, handleFrameSwitch, handleStatus, handleTabClose, handleTabList, handleTabNew, handleTabSwitch };
