/**
 * WebPilot Service Worker.
 *
 * Bridges Native Messaging Host commands to content scripts and CDP.
 * Wire format matches the Rust `protocol::Command` / `protocol::ResponseData`
 * enums exactly — there is no shape translation layer.
 */
console.log("[WebPilot] Service Worker loaded");

import { pruneCdpLock } from "./cdp.js";
import { injectBridgeOnly, installDialogOverride } from "./content.js";
import { connectToHost, isHostConnected } from "./host.js";
import { activeTabId, ensureRestored } from "./session.js";
import { rearmMonitors } from "./state.js";

// Every chrome.* event listener is registered HERE, synchronously, at the top
// of the module graph — MV3 requires listener registration to complete during
// worker startup, and keeping them in the entry module makes that invariant
// impossible to break from inside a domain module.

chrome.tabs.onRemoved.addListener((tabId) => {
  pruneCdpLock(tabId);
});

// Every document COMMITTED in the pinned tab gets the dialog override at the
// earliest moment the extension can reach it — the per-action injection covers
// only the frames that exist when the action starts, so an iframe a click
// handler CREATES (whose script then calls alert) would otherwise raise a
// native modal and wedge the tab. Scoped to the pinned tab: the user's other
// tabs keep their native dialogs. Includes non-http child documents
// (about:srcdoc, about:blank) — those are exactly where handler-spawned
// dialogs live; the pin itself is always http, which resolveActiveTab
// guarantees.
chrome.webNavigation.onCommitted.addListener(async (details) => {
  try {
    await ensureRestored();
  } catch {
    return;
  }
  if (details.tabId !== activeTabId) return;
  await installDialogOverride(details.tabId, { frameIds: [details.frameId] });
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
  // user had started. If the restore fails, skip the re-arm (it would consult an
  // empty `monitoringState` and wrongly conclude no monitor was armed); the next
  // command's restore will recover it.
  try {
    await ensureRestored();
  } catch {
    return;
  }
  // Re-inject the bridge here: the manifest content script does not re-run on a
  // bfcache restore, so this refreshes its `onMessage` listener for the restored
  // document.
  await injectBridgeOnly(tabId, 0);
  // Monitor re-arm is the BACKSTOP — the command paths (navigate / back /
  // forward / reload / click-nav / `capture --url`) re-arm at settle, earlier
  // than this `load`-time event, so a fetch the new page fires before `load`
  // is still caught. Scoped to the PINNED tab: the armed intent is agent-level,
  // and the agent's monitor must never inject MAIN-world hooks into an
  // unrelated tab the user is browsing.
  if (tabId === activeTabId) {
    await rearmMonitors(tabId);
  }
});

connectToHost();
