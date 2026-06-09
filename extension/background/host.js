// // Native Messaging link to the host binary: connect, keepalive Ping,
// // queueing, and inbound dispatch. The extension-side peer of host.rs.

import { applyHostConfig } from "./session.js";
import { processCommand } from "./router.js";
import { setMonitorPolicy } from "./state.js";

const NM_HOST = "com.webpilot.host";
const KEEPALIVE_INTERVAL = 25000;

let nmPort = null;
let keepaliveTimer = null;
let connectionRetries = 0;

// ── NM connection ──────────────────────────────────────────────────────────

function connectToHost() {
  if (nmPort) return;
  try {
    nmPort = chrome.runtime.connectNative(NM_HOST);
    console.log("[WebPilot] Connected to native host");
    connectionRetries = 0;

    // Bind each connection's messages to that exact port, so a reply always
    // goes back to the host process that sent the request — never to a
    // reconnected host whose fresh id space could match the id to a different
    // pending request.
    const port = nmPort;
    port.onMessage.addListener((request) => handleHostMessage(request, port));
    nmPort.onDisconnect.addListener(() => {
      const error = chrome.runtime.lastError?.message || "unknown";
      console.log("[WebPilot] Native host disconnected:", error);
      nmPort = null;
      clearInterval(keepaliveTimer);

      connectionRetries++;
      const delay = Math.min(2000 * connectionRetries, 30000);
      setTimeout(connectToHost, delay);
    });

    // Every Ping carries our manifest version (connect-time hello + keepalive),
    // so the host can detect a stale install and reject CLI commands loudly.
    const ping = () =>
      nmPort?.postMessage({
        id: 0,
        command: { type: "Ping", extension_version: chrome.runtime.getManifest().version },
      });
    ping();
    clearInterval(keepaliveTimer);
    keepaliveTimer = setInterval(ping, KEEPALIVE_INTERVAL);
  } catch (e) {
    console.error("[WebPilot] Failed to connect:", e);
    connectionRetries++;
    setTimeout(connectToHost, Math.min(5000 * connectionRetries, 30000));
  }
}

// Commands execute strictly in arrival order. The worker's state — the pinned
// tab, the active frame, every commit watch — is one set of globals by design
// (browser mode is single-agent), so concurrency here would interleave that
// state across commands: one command's navigation flipping another's commit
// watch, a click's re-pin retargeting a sibling's auto-capture. The queue makes
// the documented serial model an enforced one.
let commandQueue = Promise.resolve();

// Pure reads that touch no command state — answerable while a long command
// (a 15s navigation) holds the queue. A health check must not report dead
// because the worker is busy.
const QUEUE_EXEMPT = new Set(["Status", "Ping"]);

function handleHostMessage(request, port) {
  const { id, command, monitor_policy } = request;
  // The host pushes its resolved settings alongside every Pong; apply them
  // before the command-only early return below would drop the message.
  if (request.result?.type === "Config") {
    applyHostConfig(request.result);
    return;
  }
  if (!command) return;
  if (QUEUE_EXEMPT.has(command.type)) {
    // Status/Ping skip the queue, but the host still stamps them with its latest
    // monitor verdict. Apply it so a page-initiated re-arm BETWEEN commands sees
    // the current `eval` policy, not the last queued command's (possibly stale)
    // one — matching headless, which reads the live store on every re-install.
    // Queued commands still apply their own verdict in order inside the queue.
    if (monitor_policy) setMonitorPolicy(monitor_policy);
    processCommandWithKeepAlive(id, command, port);
    return;
  }
  // Adopt this command's monitor verdicts INSIDE the queue, the instant before
  // it runs — not on arrival — so a later command's verdicts can't overwrite an
  // earlier command's before that earlier one (and its post-nav re-arm) runs.
  commandQueue = commandQueue
    .then(() => {
      if (monitor_policy) setMonitorPolicy(monitor_policy);
      return processCommandWithKeepAlive(id, command, port);
    })
    .catch(() => {});
}

async function processCommandWithKeepAlive(id, command, port) {
  // Reset 30s idle timer while command is in flight.
  const keepAlive = setInterval(() => {
    chrome.runtime.getPlatformInfo(() => {});
  }, 20000);
  try {
    await processCommand(id, command, port);
  } finally {
    clearInterval(keepAlive);
  }
}

// Function declaration on purpose: browser.js imports this through a
// module cycle (host -> router -> browser -> host), and a hoisted
// declaration is callable even before this module body finishes evaluating.
function isHostConnected() {
  return !!nmPort;
}

export { connectToHost, isHostConnected };
