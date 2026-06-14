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

export { cdpSend, pruneCdpLock, withCdp };
