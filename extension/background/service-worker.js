/**
 * WebPilot Service Worker.
 *
 * Bridges Native Messaging Host commands to content scripts and CDP.
 * Wire format matches the Rust `protocol::Command` / `protocol::ResponseData`
 * enums exactly — there is no shape translation layer.
 */
console.log("[WebPilot] Service Worker loaded");

import { pruneCdpLock } from "./cdp.js";
import { connectToHost, isHostConnected } from "./host.js";
import { RESTORED, pruneTabMonitoring } from "./session.js";
import { rearmMonitors } from "./state.js";

// Every chrome.* event listener is registered HERE, synchronously, at the top
// of the module graph — MV3 requires listener registration to complete during
// worker startup, and keeping them in the entry module makes that invariant
// impossible to break from inside a domain module.

chrome.tabs.onRemoved.addListener((tabId) => {
  pruneCdpLock(tabId);
  pruneTabMonitoring(tabId);
});

chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  if (msg.type === "status") {
    sendResponse({ connected: isHostConnected() });
    return false;
  }
});

chrome.runtime.onInstalled.addListener(() => {
  console.log("[WebPilot] Extension installed");
  connectToHost();
});

chrome.runtime.onStartup.addListener(() => {
  console.log("[WebPilot] Chrome started");
  connectToHost();
});

chrome.webNavigation.onCompleted.addListener(async (details) => {
  if (details.frameId !== 0) return;
  if (!details.url?.startsWith("http")) return;
  const tabId = details.tabId;
  // This fires independently of command processing, so it must also wait for
  // the post-restart restore before consulting `monitoringState` — otherwise a
  // navigation right after an SW restart would skip re-injecting a monitor the
  // user had started. This is the BACKSTOP for monitor re-arm: the command paths
  // (navigate/back/forward/reload/click-nav/`capture --url`) re-arm at settle,
  // earlier than `load`, so a fetch the new page fires before `load` is caught.
  await RESTORED;
  await rearmMonitors(tabId);
});

connectToHost();
