// // Browser-level commands: tabs, frames, status.
// // Mirrors transport/local/browser.rs.

import { err, exceptionErr, noPageErr, topErr } from "./errors.js";
import { activeFrameId, activeTabId, navigationTimeoutMs, resolveActiveTab, setActiveFrameId, setActiveTabId } from "./session.js";
import { cdpSend, withCdp } from "./cdp.js";
import { cdpEval, frameWorldContextId } from "./query.js";
import { adoptedDocumentReady } from "./navigation.js";
import { rearmMonitors } from "./state.js";
import { isHostConnected } from "./host.js";

// Match a `frame url` pattern against a frame URL: every non-`*` run of the
// pattern must appear in the URL in order, `*` spanning any run between them — a
// *contains* match (`/auth/` matches any URL holding it), the headless
// `url_glob_match` twin. An empty or all-`*` pattern is rejected by the CLI.
function urlGlobMatch(pattern, url) {
  let cursor = 0;
  for (const segment of pattern.split("*")) {
    if (!segment) continue;
    const rel = url.indexOf(segment, cursor);
    if (rel === -1) return false;
    cursor = rel + segment.length;
  }
  return true;
}

// ── Tabs ───────────────────────────────────────────────────────────────────

async function handleTabNew(url) {
  const created = await chrome.tabs.create({ url, active: true });
  setActiveTabId(created.id);
  // A fresh tab has its own frame tree — drop any frame scope.
  setActiveFrameId(0);
  // Settle on a ready page and report its real, post-redirect URL/title — `tab
  // new` lands like `navigate`, so the agent's next action cannot race the new
  // tab's load and a redirect is reflected, not the requested URL echoed back.
  // `adoptedDocumentReady` waits to leave about:blank and parse (the headless
  // `wait_navigation_settled(before_url: "about:blank")` twin); best-effort, so
  // a tab that never leaves about:blank returns at the deadline.
  await adoptedDocumentReady(created.id, navigationTimeoutMs());
  // Armed monitors follow the agent's working tab onto the new one (headless
  // parity) — done after the page settles so the hooks inject into the real
  // document, not the transient about:blank.
  await rearmMonitors(created.id);
  const settled = await chrome.tabs.get(created.id).catch(() => null);
  return {
    type: "Action",
    success: true,
    new_tab: {
      id: String(created.id),
      url: settled?.url || created.url || url,
      title: settled?.title || created.title || "",
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
    // Armed console/network monitors follow the working tab, as headless re-arms
    // on every pin move — otherwise `console read` on the switched-to tab would
    // silently miss its logs.
    await rearmMonitors(target);
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
  // on. Status is read-only, so it never pins as a side effect.
  let tab = null;
  if (activeTabId != null) {
    // A pin is set: report THAT tab, or nothing if it has died. Do NOT fall
    // through to the focused tab — `resolveActiveTab` throws TabNotFound for a
    // dead pin, so every other command fails on it; status showing a healthy
    // focused tab instead would tell the agent it is somewhere its commands
    // can't reach. A null tab here mirrors that "the pinned tab is gone".
    tab = await chrome.tabs.get(activeTabId).catch(() => null);
  } else {
    // No pin: show the focused window's active tab — what a first command would
    // pin. (resolveActiveTab pins the same tab; status just doesn't persist it.)
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
  // A non-http page has no webpilot-addressable frame tree, so this is NoPage —
  // exactly like `frame switch` — not an empty list that an agent would read as
  // "this page simply has no iframes". (An http page with no iframes still
  // resolves a tab and correctly returns an empty list below.)
  if (!tab) return topErr(noPageErr());

  // The pinned tab can close between resolving it and reading its frames:
  // `getAllFrames` then resolves null (or rejects). Surface the typed TabNotFound
  // the agent recovers from — never an empty list it would read as "this page has
  // no iframes", and never the raw `null.map` TypeError the old `.catch(() => [])`
  // left for the null-resolve case. Headless `do_frame_list` likewise propagates a
  // `Page.getFrameTree` failure rather than returning empty.
  const all = await chrome.webNavigation.getAllFrames({ tabId: tab.id }).catch(() => null);
  if (!all) {
    return topErr(
      err("TabNotFound", `Tab not found: ${tab.id}. List: webpilot tab`, { tab_id: String(tab.id) }),
    );
  }
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

  // A persisted active-frame id can outlive its frame — the page changed while
  // the MV3 service worker was suspended, so the restored scope points at a frame
  // the tree no longer has. `frame list` is the recovery path: drop it back to
  // main and REPORT the reset (active_frame_id below) rather than a scope that no
  // longer exists. Both modes keep a vanished scope until then, so a scoped
  // command FrameNotFounds first instead of silently retargeting to main; headless
  // `do_frame_list` mirrors this exact reset.
  if (activeFrameId !== 0 && !all.some((f) => f.frameId === activeFrameId)) {
    setActiveFrameId(0);
  }

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

// A pattern frame selector (`name` / `url`) that matched more than one frame is
// ambiguous: switching into whichever came first would silently scope every
// later command to a frame the agent may not have meant. Fail loud with the
// match list (headless parity) so the agent refines the pattern or uses a
// `frame predicate` — the precise escape hatch, which stays first-match.
function ambiguousFrameSwitch(hits, what) {
  if (hits.length <= 1) return null;
  const urls = hits.map((f) => f.url).join(", ");
  return {
    type: "FrameSwitched",
    success: false,
    frame_id: activeFrameIdWire(),
    error: err(
      "InvalidArgument",
      `${hits.length} frames match ${what} — refine it or use \`frame predicate\` to pick one: ${urls}`,
    ),
  };
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

  // `getAllFrames` resolves null (or rejects) when the tab closed between the
  // resolve above and this read — surface the dead tab as the typed TabNotFound
  // the agent re-pins from, not a FrameNotFound that reads as "bad selector"
  // (the same guard `frame list` applies).
  const frames = await chrome.webNavigation.getAllFrames({ tabId: tab.id }).catch(() => null);
  if (!frames) {
    return {
      type: "FrameSwitched",
      success: false,
      frame_id: activeFrameIdWire(),
      error: err("TabNotFound", `Tab not found: ${tab.id}. List: webpilot tab`, { tab_id: String(tab.id) }),
    };
  }
  const httpFrames = frames.filter((f) => f.frameId !== 0 && f.url?.startsWith("http"));

  let matched = null;

  if (selector.by === "name") {
    const hits = [];
    for (const f of httpFrames) {
      if ((await readFrameName(tab.id, f.frameId)) === selector.value) hits.push(f);
    }
    const ambiguous = ambiguousFrameSwitch(hits, `name "${selector.value}"`);
    if (ambiguous) return ambiguous;
    matched = hits[0] ?? null;
  } else if (selector.by === "url") {
    const hits = httpFrames.filter((f) => f.url && urlGlobMatch(selector.pattern || "", f.url));
    const ambiguous = ambiguousFrameSwitch(hits, `url pattern "${selector.pattern}"`);
    if (ambiguous) return ambiguous;
    matched = hits[0] ?? null;
  } else if (selector.by === "predicate") {
    // A predicate is arbitrary caller JS, gated by the `eval` key — enforced
    // by the NM host before the command is forwarded here. It runs through the
    // same debugger-routed evaluation as `eval`, so a frame's CSP cannot block
    // it (headless parity) — one withCdp session probes every candidate frame.
    // A failure of the SESSION itself — the debugger attach — propagates typed
    // through the router; a predicate that THREW is remembered and surfaced
    // below, never disguised as FrameNotFound.
    const probe = await withCdp(tab.id, async (tid) => {
      await cdpSend(tid, "Runtime.enable", {});
      let error = null;
      for (const f of httpFrames) {
        const uniqueContextId = await frameWorldContextId(tid, tab.id, f.frameId, "MAIN");
        if (uniqueContextId == null) continue;
        let r;
        try {
          r = await cdpEval(tid, selector.js, uniqueContextId);
        } catch (e) {
          // The eval itself faulted (not a clean false). Remember it and surface
          // it after the loop, never swallow it into a FrameNotFound — only an
          // unreachable frame (null context, above) is a silent skip, matching the
          // success===false path and headless behaviour.
          error = exceptionErr(e);
          continue;
        }
        if (r && r.success === false && r.error) { error = r.error; continue; }
        if (r?.success && r.result && JSON.parse(r.result) === true) return { frame: f };
      }
      return { error };
    });
    matched = probe.frame || null;
    if (!matched && probe.error) {
      return { type: "FrameSwitched", success: false, frame_id: activeFrameIdWire(), error: probe.error };
    }
  }

  if (matched) {
    setActiveFrameId(matched.frameId);
    return {
      type: "FrameSwitched",
      success: true,
      frame_id: String(matched.frameId),
      // Resolve the frame's name from the live document (headless reads it from
      // the frame tree): the switch response carries it so `frame switch by name`
      // is discoverable, and so both modes return the same shape.
      name: (await readFrameName(tab.id, matched.frameId)) || null,
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
