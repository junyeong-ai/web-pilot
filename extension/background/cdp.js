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

// `sessionId` addresses a child session the tab's debugger attached — a
// cross-site iframe rendered in a process of its own; without it the command
// goes to the tab's own session.
async function cdpSend(tabId, method, params = {}, sessionId) {
  return chrome.debugger.sendCommand(sessionId ? { tabId, sessionId } : { tabId }, method, params);
}

// The protocol error a command was answered with: `chrome.debugger` rejects with
// the error object serialized as the message. Any other rejection (the session
// detached, the tab closed) means no answer arrived.
function protocolError(error) {
  try {
    const parsed = JSON.parse(error?.message);
    return typeof parsed?.code === "number" && typeof parsed.message === "string" ? parsed : null;
  } catch {
    return null;
  }
}

// The targeted context no longer resolves, so the call never started and may be
// re-issued (headless `ContextGone`).
function isStaleContext(error) {
  return /Cannot find context with specified id/.test(protocolError(error)?.message ?? "");
}

// An effect whose command was rejected either never ran — the browser answered
// with a protocol error — or may have: no answer arrived, or the document was torn
// down mid-call (headless `unconfirmed_effect`). Only the second is OutcomeUnknown.
async function cdpSendEffect(tabId, method, params, operation, sessionId) {
  try {
    return await cdpSend(tabId, method, params, sessionId);
  } catch (error) {
    const answered = protocolError(error);
    const cutOff = /Execution context was destroyed|Inspected target navigated or closed/.test(answered?.message ?? "");
    if (answered && !cutOff) throw error;
    throw Object.assign(new Error(`Execution outcome unknown for ${operation}; observe before retrying`), {
      code: "OutcomeUnknown", data: { operation, reason: String(error?.message ?? error) },
    });
  }
}

// Enable the Runtime domain with Chrome's native console buffer discarded first.
// `Runtime.enable` REPLAYS that buffer, and Chrome keeps every console argument
// unclipped and unbounded — but WebPilot reads console only through its MAIN-world
// hook (`window.__webpilot_console`), never this buffer, so the replay is pure
// overhead (re-transferred on every enable, plus unbounded Chrome memory). The
// discard is best-effort; the enable is the operation. The browser-mode twin of
// headless `reemit_execution_contexts` / the connect-time prime in mod.rs, so the
// discard-before-enable invariant holds in BOTH modes.
async function cdpEnableRuntime(tabId, sessionId) {
  try {
    await cdpSend(tabId, "Runtime.discardConsoleEntries", {}, sessionId);
  } catch {}
  await cdpSend(tabId, "Runtime.enable", {}, sessionId);
}

// Toggle the domain so existing contexts re-announce into our listener,
// discarding the console buffer between (see `cdpEnableRuntime`).
async function cdpReemitContexts(tabId) {
  await cdpSend(tabId, "Runtime.disable", {});
  await cdpEnableRuntime(tabId);
}

export { cdpSend, cdpSendEffect, cdpEnableRuntime, cdpReemitContexts, isStaleContext, pruneCdpLock, withCdp };
