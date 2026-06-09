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
  const list = frames || (await chrome.webNavigation.getAllFrames({ tabId }).catch(() => []));
  if (list.some((f) => f.frameId === frameId && f.url?.startsWith("http"))) return null;
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
    } catch {
      throw firstError; // can't even re-inject — surface the original failure
    }
    return await sendOnce();
  }
}

export { ensureBridge, frameVanishedError, injectBridgeOnly, sendToContent };
