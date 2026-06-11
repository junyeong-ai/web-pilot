// // Content-script link: bridge injection and request/response messaging.
// // Mirrors the invoke_bridge / isolated-world bridge half of
// // transport/local/mod.rs — here the manifest content script is the bridge's
// // isolated world; ensureBridge just guarantees it is injected.

import { sleep } from "./session.js";
import { err } from "./errors.js";

// The MAIN-world dialog override: a native alert/confirm/prompt modal blocks
// the page thread until a human clicks — under automation that wedges every
// later command on the tab. Accept-with-default semantics mirror the headless
// dialog responder (Page.handleJavaScriptDialog accept:true), so a page
// branching on a dialog behaves identically in both modes. Idempotent
// (sentinel-guarded), so re-installing is a no-op. Scoped to the agent's
// pinned tab only — the user's other tabs keep their native dialogs.
async function installDialogOverride(tabId, target) {
  try {
    await chrome.scripting.executeScript({
      target: { tabId, ...target },
      world: "MAIN",
      func: () => {
        if (!window.__webpilot_dialogs) {
          window.__webpilot_dialogs = [];
          window.alert = (msg) => { window.__webpilot_dialogs.push({ type: "alert", message: String(msg) }); };
          window.confirm = (msg) => { window.__webpilot_dialogs.push({ type: "confirm", message: String(msg) }); return true; };
          // A real `prompt` returns the DEFAULT stringified when accepted —
          // `prompt(msg, 0)` yields "0" and `prompt(msg, null)` yields
          // "null" (WebIDL DOMString coercion, like alert(null)); only a
          // MISSING argument (undefined) takes the parameter default "".
          window.prompt = (msg, def) => { window.__webpilot_dialogs.push({ type: "prompt", message: String(msg) }); return def === undefined ? "" : String(def); };
        }
      },
    });
  } catch {}
}

// A bridge call (capture, wait, dom get/set) routed to a since-vanished iframe
// must read as FrameNotFound (exit 4 → recapture), not the BridgeUnavailable
// (exit 3 → infra) a failed injection into a missing frame would otherwise
// produce. The main frame (0) exists as long as the tab does, so only sub-frames
// are probed. Pass an already-fetched `frames` list to avoid a redundant
// getAllFrames. Mirrors headless `bridge_context_id`, which is FrameNotFound for
// an unmapped active frame.
async function frameVanishedError(tabId, frameId, frames = null) {
  if (frameId === 0) return null;
  // getAllFrames resolves `null` (not a rejection) when the tab is gone, so the
  // `.catch` alone would not guard it — the `list &&` does. Any non-array result
  // means the frame can't be confirmed present, which for a non-main active
  // frame is FrameNotFound (exit 4 → recapture), never a thrown TypeError.
  const list = frames || (await chrome.webNavigation.getAllFrames({ tabId }).catch(() => null));
  if (list && list.some((f) => f.frameId === frameId && f.url?.startsWith("http"))) return null;
  const sel = `frame ${frameId}`;
  return err("FrameNotFound", `Frame not found: ${sel}`, { selector: sel });
}

async function ensureBridge(tabId, frameId = 0) {
  for (let attempt = 0; attempt < 3; attempt++) {
    try {
      await Promise.race([
        chrome.scripting.executeScript({ target: { tabId, frameIds: [frameId] }, files: ["content/bridge.js"] }),
        new Promise((_, r) => setTimeout(() => r(new Error("inject timeout")), 3000)),
      ]);
    } catch {}

    await sleep(50 + attempt * 100);
    try {
      const pong = await Promise.race([
        chrome.tabs.sendMessage(tabId, { type: "ping" }, { frameId }),
        new Promise((_, r) => setTimeout(() => r(new Error("ping timeout")), 2000)),
      ]);
      if (pong?.ok) return;
    } catch {}
    console.warn(`[WebPilot] Bridge verify failed (attempt ${attempt + 1}/3, tab=${tabId}, frame=${frameId})`);
  }
  // Callers resolve the tab and probe the frame BEFORE calling here, but
  // either can vanish in the async gap (the pinned tab closes; an iframe
  // navigates away) — and then every inject/ping above failed for a reason
  // that is NOT infra. Re-probe at the failure point, tab first (a gone tab
  // makes any frame answer moot): a closed tab is TabNotFound and a gone
  // frame is FrameNotFound (both exit 4 → recover), reserving
  // BridgeUnavailable (exit 3 → infra) for a page that exists but won't
  // answer — the same split the headless transport makes.
  const tab = await chrome.tabs.get(tabId).catch(() => null);
  if (!tab) {
    const te = new Error(`Tab not found: ${tabId}. List: webpilot tab`);
    te.code = "TabNotFound";
    te.data = { tab_id: String(tabId) };
    throw te;
  }
  const gone = await frameVanishedError(tabId, frameId);
  if (gone) {
    const fe = new Error(gone.message);
    fe.code = gone.code;
    fe.data = { selector: gone.selector };
    throw fe;
  }
  const e = new Error("Page is not responding — try reloading the page");
  e.code = "BridgeUnavailable";
  throw e;
}

async function injectBridgeOnly(tabId, frameId = 0) {
  try {
    await Promise.race([
      chrome.scripting.executeScript({ target: { tabId, frameIds: [frameId] }, files: ["content/bridge.js"] }),
      new Promise((_, r) => setTimeout(() => r(new Error("inject timeout")), 3000)),
    ]);
  } catch {}
}

const SEND_TIMEOUT_MSG = "Page did not respond in time";

async function sendToContent(tabId, message, frameId = 0, timeoutMs = 10000) {
  const sendOnce = () => Promise.race([
    chrome.tabs.sendMessage(tabId, message, { frameId }),
    new Promise((_, r) => setTimeout(() => r(new Error(SEND_TIMEOUT_MSG)), timeoutMs)),
  ]);

  try {
    return await sendOnce();
  } catch (firstError) {
    // The only recovery for ANY first failure is the same: re-inject the
    // bridge and try once more. So attempt it unconditionally rather than
    // matching Chrome's "Receiving end does not exist" message string — if
    // re-injection doesn't help (tab gone, eval refused), the retry throws and
    // the real error surfaces. Substring-matching a localized Chrome message
    // is exactly the message-parsing the typed-error convention forbids.
    try {
      await ensureBridge(tabId, frameId);
    } catch (e) {
      // A TYPED re-inject failure is the root cause — FrameNotFound (the
      // frame vanished; exit 4 → recapture) or BridgeUnavailable (exit 3) —
      // and outranks the untyped first send error, which would collapse to
      // Other. Only an untyped throw falls back to the original failure.
      if (e?.code) throw e;
      throw firstError;
    }
    return await sendOnce();
  }
}

export { ensureBridge, frameVanishedError, injectBridgeOnly, installDialogOverride, sendToContent };
