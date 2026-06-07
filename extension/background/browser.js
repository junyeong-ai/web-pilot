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

async function handleTabClose(tabId) {
  try {
    await chrome.tabs.remove(parseInt(tabId, 10));
    return { type: "Action", success: true };
  } catch (e) {
    // A bad or already-closed id is a not-found — typed to match headless
    // (`do_tab_close` → `TabNotFound`, exit 4) instead of a generic exception.
    return { type: "Action", success: false, error: err("TabNotFound", e.message, { tab_id: tabId }) };
  }
}

async function handleTabSwitch(tabId) {
  try {
    const target = parseInt(tabId, 10);
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
    tab_url: tab?.url || null,
    tab_title: tab?.title || null,
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
    if (!matched) {
      matched = httpFrames.find((f) => f.url?.includes(selector.value));
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

export { handleFrameList, handleFrameSwitch, handleStatus, handleTabClose, handleTabList, handleTabNew, handleTabSwitch, readFrameName };
