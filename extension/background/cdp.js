// Debugger transport: per-tab attach serialisation and command send.
// Mirrors crates/webpilot-cli/src/cdp.rs.

const CDP_VERSION = "1.3";

const cdpLocks = new Map();

function pruneCdpLock(tabId) {
  cdpLocks.delete(tabId);
}

async function withCdp(tabId, fn) {
  const prev = cdpLocks.get(tabId) || Promise.resolve();
  const op = prev.then(async () => {
    await chrome.debugger.attach({ tabId }, CDP_VERSION);
    try {
      return await fn(tabId);
    } finally {
      await chrome.debugger.detach({ tabId }).catch(() => {});
    }
  });
  cdpLocks.set(tabId, op.catch(() => {}));
  return op;
}

async function cdpSend(tabId, method, params = {}) {
  return chrome.debugger.sendCommand({ tabId }, method, params);
}

// Enable the Runtime domain with Chrome's native console buffer discarded first.
// `Runtime.enable` REPLAYS that buffer, and Chrome keeps every console argument
// unclipped and unbounded — but WebPilot reads console only through its MAIN-world
// hook (`window.__webpilot_console`), never this buffer, so the replay is pure
// overhead (re-transferred on every enable, plus unbounded Chrome memory). The
// discard is best-effort; the enable is the operation. The browser-mode twin of
// headless `reemit_execution_contexts` / the connect-time prime in mod.rs, so the
// discard-before-enable invariant holds in BOTH modes.
async function cdpEnableRuntime(tabId) {
  try {
    await cdpSend(tabId, "Runtime.discardConsoleEntries", {});
  } catch {}
  await cdpSend(tabId, "Runtime.enable", {});
}

// Toggle the domain so existing contexts re-announce into our listener,
// discarding the console buffer between (see `cdpEnableRuntime`).
async function cdpReemitContexts(tabId) {
  await cdpSend(tabId, "Runtime.disable", {});
  await cdpEnableRuntime(tabId);
}

export { cdpSend, cdpEnableRuntime, cdpReemitContexts, pruneCdpLock, withCdp };
