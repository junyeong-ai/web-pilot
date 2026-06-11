// // Content-script link: bridge injection and request/response messaging.
// // Mirrors the invoke_bridge / isolated-world bridge half of
// // transport/local/mod.rs — here the manifest content script is the bridge's
// // isolated world; ensureBridge just guarantees it is injected.

import { sleep } from "./session.js";
import { err } from "./errors.js";

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
  // Callers probe the frame BEFORE calling here, but a subframe can vanish in
  // the async gap (an iframe mid-navigation) — and then every inject/ping
  // above failed for a reason that is NOT infra. Re-probe at the failure
  // point: a gone frame is FrameNotFound (exit 4 → recapture), reserving
  // BridgeUnavailable (exit 3 → infra) for a frame that exists but won't
  // answer — the same split headless `bridge_context_id` makes.
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

export { ensureBridge, frameVanishedError, injectBridgeOnly, sendToContent };
