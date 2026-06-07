// // Page-mutating actions: dispatch, native key/hover/drag/upload, popup and
// // navigation detection. Mirrors transport/local/action.rs.

import { err, exceptionErr, noPageErr, otherErr } from "./errors.js";
import { PROBE_MS, activeFrameId, resolveActiveTab, setActiveFrameId, setActiveTabId, sleep } from "./session.js";
import { cdpSend, withCdp } from "./cdp.js";
import { ensureBridge, sendToContent } from "./content.js";
import { adoptedDocumentReady, documentReady, settledActionUrl, waitNavigationSettled, watchMainFrameCommit } from "./navigation.js";
import { frameWorldContextId } from "./query.js";

// ── Action ─────────────────────────────────────────────────────────────────

async function handleAction(command) {
  const { action } = command;

  // Policy is enforced by the NM host (`policy::parse_and_enforce`) — the
  // browser-mode privileged sink — before the command is forwarded here, so
  // the service worker never re-checks it. (The CLI-side IpcTransport is only
  // a socket writer and is deliberately NOT a gate; writing the socket
  // directly would bypass it, which is why the host re-validates.)

  // Inject dialog override before any action runs in the page.
  const tab = await resolveActiveTab();
  if (!tab) {
    return { type: "Action", success: false, error: noPageErr() };
  }
  try {
    await chrome.scripting.executeScript({
      target: { tabId: tab.id, frameIds: [0] },
      world: "MAIN",
      func: () => {
        if (!window.__webpilot_dialogs) {
          window.__webpilot_dialogs = [];
          window.alert = (msg) => { window.__webpilot_dialogs.push({ type: "alert", message: String(msg) }); };
          window.confirm = (msg) => { window.__webpilot_dialogs.push({ type: "confirm", message: String(msg) }); return true; };
          window.prompt = (msg, def) => { window.__webpilot_dialogs.push({ type: "prompt", message: String(msg) }); return def || ""; };
        }
      },
    });
  } catch {}

  // Viewport-coordinate / main-document actions cannot target an iframe: their
  // CDP path uses page-level coordinates or a main-document node lookup. Reject
  // inside a switched frame, matching the headless `require_main_frame` guard,
  // so behaviour is identical across modes.
  if (activeFrameId !== 0 && (action.kind === "hover" || action.kind === "drag" || action.kind === "upload")) {
    return {
      type: "Action",
      success: false,
      error: err(
        "InvalidArgument",
        `'${action.kind}' targets the main frame only and an iframe is active. Switch back first: webpilot frame switch main`,
      ),
    };
  }

  let result;

  // SW-handled action kinds (navigation + upload).
  switch (action.kind) {
    case "navigate": {
      const watch = watchMainFrameCommit(tab.id);
      await chrome.tabs.update(tab.id, { url: action.url, active: true });
      await waitNavigationSettled(tab.id, tab.url || "", watch, action.url);
      // A new document invalidates any switched-to frame — reset to main, as
      // the headless transport does on navigation.
      setActiveFrameId(0);
      const landed = await chrome.tabs.get(tab.id).catch(() => null);
      result = { type: "Action", success: true, url_changed: landed?.url || action.url };
      break;
    }

    case "back":
    case "forward": {
      // History traversal runs in the page (`history.back()`), mirroring the
      // headless transport — `chrome.tabs.goBack` refuses even with history
      // present in headless Chrome (measured). The Navigation API makes the
      // no-entry case an honest, immediate typed failure instead of a success
      // that silently did nothing.
      const dir = action.kind;
      const can = await chrome.scripting
        .executeScript({
          target: { tabId: tab.id, frameIds: [0] },
          world: "MAIN",
          func: (d) => (d === "back" ? navigation.canGoBack : navigation.canGoForward),
          args: [dir],
        })
        .then((r) => r?.[0]?.result)
        .catch(() => null);
      if (can === false) {
        return {
          type: "Action",
          success: false,
          error: err("NavigationFailed", `Cannot go ${dir}: no history entry`, {
            url: `history.${dir}()`,
            reason: "no history entry",
          }),
        };
      }
      const watch = watchMainFrameCommit(tab.id);
      await chrome.scripting.executeScript({
        target: { tabId: tab.id, frameIds: [0] },
        world: "MAIN",
        func: (d) => {
          if (d === "back") history.back();
          else history.forward();
        },
        args: [dir],
      });
      // Best-effort settle: the traversal was issued (the no-history case above
      // is the real NavigationFailed), so a slow page that doesn't settle in
      // time is not a failure of the action — wait for the new document, but
      // don't turn a still-loading page into an error. Matches headless, which
      // settles best-effort on history/reload.
      await waitNavigationSettled(tab.id, tab.url || "", watch, `history.${dir}()`).catch(() => {});
      setActiveFrameId(0);
      result = { type: "Action", success: true };
      break;
    }

    case "reload": {
      // The URL never moves on a reload, so commitment is the observed
      // main-frame commit — the headless loaderId case. Best-effort settle: a
      // reload always issues, so a slow load is not a failure (headless parity).
      const watch = watchMainFrameCommit(tab.id);
      await chrome.tabs.reload(tab.id);
      await waitNavigationSettled(tab.id, tab.url || "", watch, "reload").catch(() => {});
      setActiveFrameId(0);
      result = { type: "Action", success: true };
      break;
    }

    case "upload":
      result = await handleUpload(tab.id, action);
      break;

    case "drag":
      result = await handleDrag(tab.id, action);
      break;

    case "hover":
      result = await handleHover(tab.id, action);
      break;

    default:
      result = await dispatchActionToPage(tab, action);
  }

  // Auto-capture DOM after success if requested — exactly like the headless
  // transport: the snapshot describes the tab the agent will act on next (a
  // click-adopted popup included), and a navigating click waits — bounded —
  // for the new document to parse, so the capture is never the dying page. A
  // capture failure must not fail the command: the action's side effect is
  // done, and a retry would run it twice — it is reported as `capture_error`.
  if (command.capture && result?.success) {
    try {
      const t = await resolveActiveTab();
      if (t) {
        // A click-opened popup is often `about:blank` (already complete)
        // before its destination commits — wait past it; a same-tab
        // navigation just waits for the new document to parse.
        if (result.new_tab) await adoptedDocumentReady(t.id, PROBE_MS);
        else if (result.url_changed) await documentReady(t.id, PROBE_MS);
        await ensureBridge(t.id, activeFrameId);
        const dom = await sendToContent(t.id, { type: "extractDom", options: {} }, activeFrameId, 5000);
        // The bridge returns a snapshot (with an `elements` array) on success,
        // or a typed `{success:false, error}` on failure. The latter is truthy
        // — assigning it as `dom` would forward a non-snapshot the CLI can't
        // deserialize, losing the successful action — so discriminate on the
        // snapshot shape and surface a failure as `capture_error`.
        if (dom && Array.isArray(dom.elements)) {
          // Mirror standalone capture: report out-of-scope http iframes so the
          // agent's subframe logic works the same after an action.
          if (activeFrameId === 0) {
            const frames = await chrome.webNavigation.getAllFrames({ tabId: t.id }).catch(() => []);
            dom.subframes = frames.filter((f) => f.frameId !== 0 && f.url?.startsWith("http")).length;
          }
          result.dom = dom;
        } else if (dom && dom.error) {
          result.capture_error = dom.error.message || JSON.stringify(dom.error);
        } else {
          result.capture_error = "no DOM snapshot from content script";
        }
      }
    } catch (e) {
      result.capture_error = e?.message || String(e);
    }
  }

  return result;
}


// The DOM `code` and Windows virtual-key code for a key, so CDP
// `Input.dispatchKeyEvent` fires native behaviour (Tab traversal, Backspace
// deletion, arrow nav) a synthetic KeyboardEvent cannot. Mirrors the headless
// `key_descriptor`. Unknown keys carry no code (0).
function keyDescriptor(key) {
  const named = {
    Enter: ["Enter", 13], Tab: ["Tab", 9], Escape: ["Escape", 27],
    Backspace: ["Backspace", 8], Delete: ["Delete", 46],
    ArrowUp: ["ArrowUp", 38], ArrowDown: ["ArrowDown", 40],
    ArrowLeft: ["ArrowLeft", 37], ArrowRight: ["ArrowRight", 39],
    Home: ["Home", 36], End: ["End", 35], PageUp: ["PageUp", 33], PageDown: ["PageDown", 34],
    Insert: ["Insert", 45], CapsLock: ["CapsLock", 20], " ": ["Space", 32], Space: ["Space", 32],
  };
  if (named[key]) return { code: named[key][0], vk: named[key][1] };
  const fn = /^F([1-9]|1[0-2])$/.exec(key);
  if (fn) return { code: key, vk: 111 + Number(fn[1]) };
  if (key.length === 1) {
    if (/[a-zA-Z]/.test(key)) return { code: `Key${key.toUpperCase()}`, vk: key.toUpperCase().charCodeAt(0) };
    if (/[0-9]/.test(key)) return { code: `Digit${key}`, vk: key.charCodeAt(0) };
  }
  return { code: "", vk: 0 };
}

// The text a key contributes to its keyDown, or null for a key that produces
// none. `Enter` carries a carriage return — the signal Chromium's implicit
// form submission keys on, without which key-press Enter never submits. Other
// named keys (Tab, arrows) produce no text so they act, not type.
function printableKeyText(key) {
  if (key === "Enter") return "\r";
  if (key === "Space") return " ";
  if (key.length === 1 && key.charCodeAt(0) >= 0x20) return key;
  return null;
}

// Dispatch a key as a native CDP input event through the tab's debugger.
async function dispatchKeyPress(tabId, action) {
  return withCdp(tabId, async (tid) => {
    const m = action.modifiers || {};
    const modifiers = (m.alt ? 1 : 0) | (m.ctrl ? 2 : 0) | (m.meta ? 4 : 0) | (m.shift ? 8 : 0);
    const { code, vk } = keyDescriptor(action.key);
    const text = (!m.ctrl && !m.alt && !m.meta) ? printableKeyText(action.key) : null;
    // `nativeVirtualKeyCode` is omitted on purpose: it is platform-native
    // (macOS != Windows), and sending the Windows code on macOS mis-maps the
    // key to an unrelated browser accelerator. `windowsVirtualKeyCode` + key +
    // code is the portable set Chrome resolves from everywhere.
    const base = { modifiers, key: action.key, code, windowsVirtualKeyCode: vk };
    await cdpSend(tid, "Input.dispatchKeyEvent", text != null
      ? { ...base, type: "keyDown", text }
      : { ...base, type: "keyDown" });
    await cdpSend(tid, "Input.dispatchKeyEvent", { ...base, type: "keyUp" });
    return { success: true };
  });
}

async function dispatchActionToPage(tab, action) {
  // A click-opened tab is caught by its creation events, registered before the
  // action runs — no detection window, no sleep, and reliable even for popups
  // that are still about:blank at creation. Only a tab the ACTED-ON tab opened
  // qualifies — an unrelated tab created during the action (the user, another
  // extension) must not capture the pin. Two correlation signals, first wins:
  // `openerTabId` (the tab relationship) and `onCreatedNavigationTarget`'s
  // `sourceTabId` (the navigation initiator — present even for rel=noopener
  // popups, which deliberately carry no opener).
  let openedTabId = null;
  const onCreated = (t) => {
    if (openedTabId == null && t.openerTabId === tab.id) openedTabId = t.id;
  };
  const onNavTarget = (d) => {
    if (openedTabId == null && d.sourceTabId === tab.id) openedTabId = d.tabId;
  };
  chrome.tabs.onCreated.addListener(onCreated);
  chrome.webNavigation.onCreatedNavigationTarget.addListener(onNavTarget);
  const urlBefore = tab.url;
  // Watch for a same-tab navigation the action itself triggers (a link click,
  // a handler setting `location.href`, an Enter that submits) — registered
  // BEFORE the action, mirroring headless `settled_action_url`. A single
  // post-action `tabs.get` would miss a navigation still in flight and let the
  // auto-capture snapshot the dying page.
  const navWatch = watchMainFrameCommit(tab.id);

  try {
    // `key_press` dispatches a native CDP key event so Tab/Backspace/arrow/
    // text behaviour actually fires (a synthetic KeyboardEvent only notifies
    // JS listeners); it still flows through this popup/navigation wrapper
    // because Enter can submit a form. Every other action runs in the page.
    let r;
    if (action.kind === "key_press") {
      r = await dispatchKeyPress(tab.id, action);
    } else {
      await ensureBridge(tab.id, activeFrameId);
      r = await sendToContent(tab.id, { type: "executeAction", action }, activeFrameId);
    }
    const result = { type: "Action", ...r };

    if (openedTabId != null) {
      const newTab = await chrome.tabs.get(openedTabId).catch(() => null);
      if (newTab) {
        await chrome.tabs.update(newTab.id, { active: true });
        // The agent's working tab moved — re-pin so every later command
        // follows it, and drop any frame scope (a new tab's tree is its own).
        setActiveTabId(newTab.id);
        setActiveFrameId(0);
        result.new_tab = {
          id: String(newTab.id),
          url: newTab.url || "",
          title: newTab.title || "",
          active: true,
        };
      }
    }

    // Report the settled destination of a navigation the action triggered (the
    // popup case is handled above via `new_tab`); a non-navigating action adds
    // no url_changed and pays no wait.
    const settledUrl = await settledActionUrl(tab.id, urlBefore, navWatch);
    if (settledUrl && settledUrl !== urlBefore) result.url_changed = settledUrl;

    return result;
  } catch (e) {
    return { type: "Action", success: false, error: exceptionErr(e) };
  } finally {
    navWatch.dispose();
    chrome.tabs.onCreated.removeListener(onCreated);
    chrome.webNavigation.onCreatedNavigationTarget.removeListener(onNavTarget);
  }
}

async function handleUpload(tabId, action) {
  try {
    await ensureBridge(tabId, activeFrameId);
    // Stash the EXACT snapshot element in the content script (object identity;
    // a stale index is a typed StaleSnapshot here, a non-file element a typed
    // InvalidArgument), then resolve that stored reference to a CDP objectId in
    // the content script's ISOLATED world and set the file on THAT object. No
    // marker attribute and no document-order re-query, so a page can neither
    // observe nor redirect the target between resolve and sink, and the direct
    // object reaches a file input inside an open shadow root. Parity: action.rs.
    try {
      const prep = await sendToContent(tabId, { type: "prepareUpload", index: action.index }, activeFrameId);
      if (prep && prep.success === false) {
        return { type: "Action", success: false, error: prep.error };
      }

      const outcome = await withCdp(tabId, async (tid) => {
        const uniqueContextId = await frameWorldContextId(tid, tabId, activeFrameId, "ISOLATED");
        if (uniqueContextId == null) {
          return { success: false, error: otherErr("upload: could not reach the content-script context") };
        }
        const ev = await cdpSend(tid, "Runtime.evaluate", {
          expression: "window.__webpilot_state.uploadTarget",
          uniqueContextId,
          returnByValue: false,
        });
        const objectId = ev?.result?.objectId;
        if (!objectId) {
          return { success: false, error: err("StaleSnapshot", `[${action.index}] left the DOM before upload`, { index: action.index }) };
        }
        await cdpSend(tid, "DOM.setFileInputFiles", { objectId, files: [action.path] });
        return { success: true };
      });
      if (outcome.success === false) {
        return { type: "Action", success: false, error: outcome.error };
      }
    } finally {
      // Release the stored reference no matter how the attempt ended — even a
      // failed prepare — so a failed upload never pins a detached node.
      await sendToContent(tabId, { type: "clearUpload" }, activeFrameId, 3000).catch(() => {});
    }

    return { type: "Action", success: true };
  } catch (e) {
    return { type: "Action", success: false, error: exceptionErr(e) };
  }
}

async function handleHover(tabId, action) {
  try {
    await ensureBridge(tabId, activeFrameId);
    // Resolve the element centre through the bridge (it owns the index map),
    // then move the real cursor there over CDP — a synthetic mouseover only
    // fires JS listeners, never the browser's internal :hover state, so this
    // mirrors headless `do_hover` for true CSS-hover fidelity.
    const coords = await sendToContent(
      tabId,
      { type: "getElementCoords", source: action.index, target: action.index },
      activeFrameId,
    );
    if (!coords || coords.error) {
      return { type: "Action", success: false, error: coords?.error || otherErr("hover: no coordinates") };
    }
    await withCdp(tabId, async (tid) => {
      await cdpSend(tid, "Input.dispatchMouseEvent", {
        type: "mouseMoved", x: coords.sx, y: coords.sy,
      });
    });
    return { type: "Action", success: true };
  } catch (e) {
    return { type: "Action", success: false, error: exceptionErr(e) };
  }
}

async function handleDrag(tabId, action) {
  try {
    await ensureBridge(tabId, activeFrameId);
    // The bridge resolves both element centres in one call (it knows the index
    // map); the service worker then drives the pointer over CDP exactly as the
    // headless path does.
    const coords = await sendToContent(
      tabId,
      { type: "getElementCoords", source: action.source, target: action.target },
      activeFrameId,
    );
    if (!coords || coords.error) {
      return { type: "Action", success: false, error: coords?.error || otherErr("drag: no coordinates") };
    }

    const { sx, sy, tx, ty } = coords;
    const steps = Math.max(action.steps || 1, 1);
    await withCdp(tabId, async (tid) => {
      await cdpSend(tid, "Input.dispatchMouseEvent", {
        type: "mousePressed", x: sx, y: sy, button: "left", clickCount: 1,
      });
      await sleep(50);
      for (let i = 1; i <= steps; i++) {
        const r = i / steps;
        await cdpSend(tid, "Input.dispatchMouseEvent", {
          type: "mouseMoved", x: sx + (tx - sx) * r, y: sy + (ty - sy) * r, button: "left",
        });
        await sleep(20);
      }
      await cdpSend(tid, "Input.dispatchMouseEvent", {
        type: "mouseReleased", x: tx, y: ty, button: "left", clickCount: 1,
      });
    });

    return { type: "Action", success: true };
  } catch (e) {
    return { type: "Action", success: false, error: exceptionErr(e) };
  }
}

export { handleAction };
