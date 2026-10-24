// Browser-level commands: tabs, frames, status.
// Mirrors transport/local/browser.rs.

import { err, exceptionErr, lineSafeClip, noPageErr, topErr } from "./errors.js";
import { activeFrameId, activeTabBorrowed, activeTabId, ensureRestored, pinTab, resolveActiveTab, resolveTabForRead, setActiveFrameId, unpinTab, viewedTab } from "./session.js";
import { cdpEnableRuntime, withCdp } from "./cdp.js";
import { cdpEval, frameWorldContextId } from "./query.js";
import { navigateBoundTab } from "./navigation.js";
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
  // A URL that doesn't parse is a typed InvalidArgument (exit 7), matching
  // headless, where CDP `Target.createTarget` rejects it as invalid params —
  // not the generic chrome.tabs.create throw that reads as Other (exit 1).
  // Parse (not prefix-match): `new URL("http://")` throws, `about:blank` is
  // valid in both modes.
  try {
    new URL(url);
  } catch {
    return {
      type: "Action",
      success: false,
      error: err("InvalidArgument", `invalid URL for tab new: ${url}`),
    };
  }
  // The same open-load-settle path a URL load from an unowned pin takes, so
  // `tab new` answers under the same budget and rolls back the same way: a load
  // that fails or does not settle closes the tab it opened and restores the pin
  // (headless parity).
  try {
    const { tabId } = await navigateBoundTab(url, { newTab: true });
    const landed = await chrome.tabs.get(tabId).catch(() => null);
    const viewed = await viewedTab();
    return {
      type: "Action",
      success: true,
      new_tab: {
        id: String(tabId),
        // The settled, post-redirect URL, not the requested one echoed back.
        url: landed?.url || url,
        // `??`: an untitled document reports the honest "".
        title: landed?.title ?? "",
        pinned: true,
        viewed: viewed?.id === tabId,
      },
    };
  } catch (e) {
    return { type: "Action", success: false, error: exceptionErr(e) };
  }
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
    // Closing the pinned tab is the agent ending its work there: release the pin
    // so the next command starts from first use instead of failing on a tab it
    // closed on purpose. A pinned tab closed any other way stays a TabNotFound.
    if (id === activeTabId) unpinTab();
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
    pinTab(target);
    // A different tab has its own frame tree — drop any frame scope.
    setActiveFrameId(0);
    // Armed console/network monitors follow the working tab, as headless does on
    // every pin move — otherwise `console read` on the switched-to tab would
    // silently miss its logs.
    await rearmMonitors(target);
    return { type: "Action", success: true };
  } catch (e) {
    return { type: "Action", success: false, error: err("TabNotFound", e.message, { tab_id: tabId }) };
  }
}

async function handleTabList() {
  const tabs = await chrome.tabs.query({});
  const viewed = await viewedTab();
  // `pinned` is the tab commands act on, `viewed` the one the user is looking at;
  // they differ whenever the agent works in a tab of its own. Read the pin
  // directly (no resolver, which would borrow a tab or throw on a vanished pin —
  // a list must do neither).
  return {
    type: "Tabs",
    tabs: tabs.map((t) => ({
      id: String(t.id),
      url: t.url || "",
      title: t.title || "",
      pinned: t.id === activeTabId,
      viewed: t.id === viewed?.id,
    })),
  };
}

// ── Status ─────────────────────────────────────────────────────────────────

async function handleStatus() {
  // Restore the persisted pin FIRST. `status` is QUEUE_EXEMPT (it must answer a
  // health check even on a busy worker), but after an MV3 idle eviction the
  // in-memory `activeTabId` is null until a queued command restores it — and
  // `status` is a common FIRST command. Without this restore it would fall
  // through to the focused tab and misreport the pin (the next real command
  // self-corrects, but a health check must not lie). `ensureRestored` is a fast
  // `storage.session` read, not a queue op, so QUEUE_EXEMPT still holds.
  await ensureRestored();
  // Report the pinned tab and how it is held — or, with nothing pinned, the tab
  // the user is viewing, which a read borrows when it is an http(s) page. Status
  // is read-only, so it never pins as a side effect. A dead pin reports no tab:
  // showing a healthy focused tab instead would tell the agent it is somewhere
  // its commands can't reach.
  let tab;
  let pin;
  if (activeTabId == null) {
    tab = await viewedTab();
    pin = "unpinned";
  } else {
    tab = await chrome.tabs.get(activeTabId).catch(() => null);
    pin = !tab ? "closed" : activeTabBorrowed ? "borrowed" : "own";
  }
  // Chrome version derives from the user agent (no direct API in MV3 SW).
  const ua = navigator.userAgent || "";
  const m = ua.match(/Chrome\/(\S+)/);
  return {
    type: "Status",
    connected: isHostConnected(),
    mode: "browser",
    pin,
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
  const tab = await resolveTabForRead();
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
      err("TabNotFound", `Tab not found: ${tab.id}`, { tab_id: String(tab.id) }),
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

// A frame selector that matched more than one frame is ambiguous: switching
// into whichever came first would silently scope every later command to a
// frame the agent may not have meant. Fail loud with the match list (headless
// parity) so the agent refines the pattern — the same contract every selector
// kind holds, predicates included.
function ambiguousFrameSwitch(hits, what) {
  if (hits.length <= 1) return null;
  const urls = hits.map((f) => lineSafeClip(f.url)).join(", ");
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
      error: err("TabNotFound", `Tab not found: ${tab.id}`, { tab_id: String(tab.id) }),
    };
  }
  const httpFrames = frames.filter((f) => f.frameId !== 0 && f.url?.startsWith("http"));

  let matched = null;

  if (selector.by === "name") {
    const hits = [];
    for (const f of httpFrames) {
      if ((await readFrameName(tab.id, f.frameId)) === selector.value) hits.push(f);
    }
    // The frame name is page-controlled — sanitize/cap it via lineSafeClip (the
    // line_safe twin) so a hostile name can't spoof or flood this error.
    const ambiguous = ambiguousFrameSwitch(hits, `name "${lineSafeClip(selector.value)}"`);
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
      await cdpEnableRuntime(tid);
      let error = null;
      const frames = [];
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
        if (r?.success && r.result && JSON.parse(r.result) === true) frames.push(f);
      }
      return { frames, error };
    });
    // The strict-selector contract (`frame url`, `tab find`, `find --click`):
    // a predicate true in MORE than one frame would silently scope every later
    // command to whichever matched first. Fail loud naming the frames
    // (headless parity).
    if (probe.frames.length > 1) {
      const urls = probe.frames.map((f) => lineSafeClip(f.url)).join(", ");
      return {
        type: "FrameSwitched",
        success: false,
        frame_id: activeFrameIdWire(),
        error: err("InvalidArgument", `${probe.frames.length} frames match the predicate — narrow it (urls: ${urls})`),
      };
    }
    matched = probe.frames[0] || null;
    if (!matched && probe.error) {
      return { type: "FrameSwitched", success: false, frame_id: activeFrameIdWire(), error: probe.error };
    }
  }

  if (matched) {
    // A cross-origin OOPIF has no execution context in this tab's CDP session, so
    // it never resolves — committing the switch would return success, then fail
    // every later eval/capture with FrameNotFound (after a per-command probe). For
    // Url/Name, confirm the MAIN context resolves AT THE SWITCH and fail loud if
    // not (the predicate path already resolved its match's context, so it is
    // exempt). Headless does the same via its in-memory frame-context map.
    if (selector.by !== "predicate") {
      const resolvable = await withCdp(tab.id, async (tid) => {
        await cdpEnableRuntime(tid);
        return (await frameWorldContextId(tid, tab.id, matched.frameId, "MAIN")) != null;
      }).catch(() => false);
      if (!resolvable) {
        const sel = JSON.stringify(selector);
        return {
          type: "FrameSwitched",
          success: false,
          frame_id: activeFrameIdWire(),
          error: err("FrameNotFound", `No matching frame: ${sel}`, { selector: sel }),
        };
      }
    }
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
