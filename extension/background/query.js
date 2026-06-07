// // Page queries: eval (debugger-routed, CSP-immune), wait, dom get/set, fetch.
// // Mirrors transport/local/query.rs.

import { err, exceptionErr, noPageErr, otherErr, timeoutErr } from "./errors.js";
import { activeFrameId, resolveActiveTab, sleep } from "./session.js";
import { cdpSend, withCdp } from "./cdp.js";
import { ensureBridge, sendToContent } from "./content.js";

// ── Eval ───────────────────────────────────────────────────────────────────

// Resolve the MAIN-world CDP execution context of a webNavigation frame, inside
// an attached debugger session. Extension frame ids are integers; CDP contexts
// carry opaque frame GUIDs — the bridge between the two id spaces is a one-shot
// nonce: a PRECOMPILED function (no dynamic code, so page CSP cannot block it)
// stamps the frame's window, and the default context whose global carries the
// stamp is the frame's context. URL matching would be ambiguous for same-URL
// sibling frames; the nonce never is. A cross-origin out-of-process iframe has
// no context in the tab's session and resolves to null — the same boundary the
// headless per-page context map has.
async function frameMainContextId(tid, tabId, frameId) {
  const contexts = [];
  const onEvent = (source, method, params) => {
    if (source.tabId === tabId && method === "Runtime.executionContextCreated") {
      contexts.push(params.context);
    }
  };
  chrome.debugger.onEvent.addListener(onEvent);
  const nonce = `wp-${Date.now()}-${Math.random().toString(36).slice(2)}`;
  try {
    // Toggle the domain so existing contexts re-announce into our listener.
    await cdpSend(tid, "Runtime.disable", {});
    await cdpSend(tid, "Runtime.enable", {});
    await chrome.scripting.executeScript({
      target: { tabId, frameIds: [frameId] },
      world: "MAIN",
      func: (n) => { window.__wp_frame_nonce = n; },
      args: [nonce],
    });
    const deadline = Date.now() + 2000;
    const probed = new Set();
    while (Date.now() < deadline) {
      for (const c of contexts) {
        if (probed.has(c.id) || c.auxData?.type !== "default") continue;
        probed.add(c.id);
        const r = await cdpSend(tid, "Runtime.evaluate", {
          expression: "window.__wp_frame_nonce",
          contextId: c.id,
          returnByValue: true,
        }).catch(() => null);
        if (r?.result?.value === nonce) return c.id;
      }
      await sleep(25);
    }
    return null;
  } finally {
    chrome.debugger.onEvent.removeListener(onEvent);
    chrome.scripting.executeScript({
      target: { tabId, frameIds: [frameId] },
      world: "MAIN",
      func: () => { delete window.__wp_frame_nonce; },
    }).catch(() => {});
  }
}

// One evaluation contract for every frame: decide the form by COMPILING
// (`compileScript` parses without evaluating — a runtime
// `throw new SyntaxError(...)` from a valid expression must not run the code
// twice), expression-first so `{a:1}` is an object literal, promise awaited.
// Debugger-routed evaluation is not subject to the page's CSP, so a hardened
// page (`script-src 'self'`) keeps full eval in any frame — headless parity.
// Requires Runtime enabled on the session.
async function cdpEval(tid, code, contextId) {
  const scope = contextId != null ? { executionContextId: contextId } : {};
  const compiled = await cdpSend(tid, "Runtime.compileScript", {
    expression: `(${code})`,
    sourceURL: "webpilot://eval-form-probe",
    persistScript: false,
    ...scope,
  });
  const form = compiled.exceptionDetails ? code : `(()=>(${code}))()`;
  const ev = await cdpSend(tid, "Runtime.evaluate", {
    expression: form,
    returnByValue: true,
    awaitPromise: true,
    ...(contextId != null ? { contextId } : {}),
  });
  if (ev.exceptionDetails) {
    const msg = ev.exceptionDetails.exception?.description || ev.exceptionDetails.text || "JS exception";
    return { success: false, error: otherErr(msg) };
  }
  const v = ev.result?.value;
  return { success: true, result: v !== undefined ? JSON.stringify(v) : null };
}

async function handleEval(command) {
  const tab = await resolveActiveTab();
  if (!tab) return { type: "Eval", success: false, error: noPageErr() };

  try {
    const r = await withCdp(tab.id, async (tid) => {
      await cdpSend(tid, "Runtime.enable", {});
      let contextId;
      if (activeFrameId !== 0) {
        contextId = await frameMainContextId(tid, tab.id, activeFrameId);
        if (contextId == null) {
          return {
            success: false,
            error: err(
              "FrameNotFound",
              `frame ${activeFrameId} has no reachable execution context`,
              { frame_id: String(activeFrameId) },
            ),
          };
        }
      }
      return cdpEval(tid, command.code, contextId);
    });
    return { type: "Eval", ...r };
  } catch (e) {
    return { type: "Eval", success: false, error: exceptionErr(e) };
  }
}

// ── Wait ───────────────────────────────────────────────────────────────────

async function handleWait(command) {
  const tab = await resolveActiveTab();
  if (!tab) return { type: "Wait", success: false, error: noPageErr() };

  const cond = command.condition || { until: "idle" };
  const timeoutMs = command.timeout_ms || 10000;

  if (cond.until === "navigation") {
    let listener;
    try {
      await Promise.race([
        new Promise((resolve) => {
          listener = (tid, info, updated) => {
            if (tid === tab.id && info.status === "complete" && updated.url?.startsWith("http")) {
              chrome.tabs.onUpdated.removeListener(listener);
              listener = null;
              resolve();
            }
          };
          chrome.tabs.onUpdated.addListener(listener);
        }),
        new Promise((_, rej) => setTimeout(() => rej(new Error("nav-timeout")), timeoutMs)),
      ]);
      return { type: "Wait", success: true };
    } catch {
      return { type: "Wait", success: false, error: timeoutErr("navigation", timeoutMs) };
    } finally {
      if (listener) chrome.tabs.onUpdated.removeListener(listener);
    }
  }

  // Selector / text / idle — delegate to bridge.js with the same condition shape.
  try {
    await ensureBridge(tab.id, activeFrameId);
    const r = await sendToContent(
      tab.id,
      { type: "wait", condition: cond, timeout_ms: timeoutMs },
      activeFrameId,
      timeoutMs + 2000,
    );
    if (r.success) return { type: "Wait", success: true };
    return { type: "Wait", success: false, error: r.error || timeoutErr("wait", timeoutMs) };
  } catch (e) {
    // A thrown exception here is the bridge CALL failing (the content script is
    // gone, eval was refused), not the wait condition timing out — the real
    // timeout comes back as `r.error` above. Surface it typed (BridgeUnavailable
    // keeps exit 3) instead of masking every infra failure as a Timeout.
    return { type: "Wait", success: false, error: exceptionErr(e) };
  }
}

// ── DOM property get/set ───────────────────────────────────────────────────

function bridgeMessageForDom(action /* "set"|"get" */, command) {
  const prop = command.property;
  const kind = prop?.kind;
  if (action === "set") {
    if (kind === "html") return { type: "setHtml", selector: command.selector, value: command.value };
    if (kind === "text") return { type: "setText", selector: command.selector, value: command.value };
    if (kind === "attr") return { type: "setAttr", selector: command.selector, attr: prop.name, value: command.value };
  } else {
    if (kind === "html") return { type: "getHtml", selector: command.selector };
    if (kind === "text") return { type: "getText", selector: command.selector };
    if (kind === "attr") return { type: "getAttr", selector: command.selector, attr: prop.name };
  }
  return null;
}

async function handleDomSet(command) {
  const tab = await resolveActiveTab();
  if (!tab) return { type: "CommandResult", success: false, error: noPageErr() };
  const msg = bridgeMessageForDom("set", command);
  if (!msg) return { type: "CommandResult", success: false, error: otherErr("Invalid property") };
  try {
    await ensureBridge(tab.id, activeFrameId);
    const r = await sendToContent(tab.id, msg, activeFrameId);
    return { type: "CommandResult", success: r.success, error: r.error || null };
  } catch (e) {
    return { type: "CommandResult", success: false, error: exceptionErr(e) };
  }
}

async function handleDomGet(command) {
  const tab = await resolveActiveTab();
  if (!tab) return { type: "CommandResult", success: false, error: noPageErr() };
  const msg = bridgeMessageForDom("get", command);
  if (!msg) return { type: "CommandResult", success: false, error: otherErr("Invalid property") };
  try {
    await ensureBridge(tab.id, activeFrameId);
    const r = await sendToContent(tab.id, msg, activeFrameId);
    return {
      type: "CommandResult",
      success: r.success,
      value: r.value || null,
      error: r.error || null,
    };
  } catch (e) {
    return { type: "CommandResult", success: false, error: exceptionErr(e) };
  }
}

// ── Fetch ──────────────────────────────────────────────────────────────────

// The fetch expression, shared in spirit with headless `do_fetch`: it streams
// the response and FAILS LOUD past the byte cap rather than reading an
// unbounded body into the worker (and the NM pipe) — a truncated body returned
// as success would be a silent lie. Returns `{oversize}` when over the limit.
const FETCH_MAX_BODY_BYTES = 10 * 1024 * 1024;
function fetchExpression(command) {
  return `(async () => {
    const r = await fetch(${JSON.stringify(command.url)}, {
      method: ${JSON.stringify(command.method || "GET")},
      headers: {"Content-Type": "application/json"},
      credentials: "include",
      ${command.body ? `body: ${JSON.stringify(command.body)},` : ""}
    });
    const MAX = ${FETCH_MAX_BODY_BYTES};
    const reader = r.body && r.body.getReader();
    if (!reader) return { status: r.status, body: "" };
    const parts = []; let total = 0;
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      total += value.length;
      if (total > MAX) { try { await reader.cancel(); } catch (e) {} return { status: r.status, oversize: MAX }; }
      parts.push(value);
    }
    const merged = new Uint8Array(total); let off = 0;
    for (const p of parts) { merged.set(p, off); off += p.length; }
    return { status: r.status, body: new TextDecoder().decode(merged) };
  })()`;
}

async function handleFetch(command) {
  const tab = await resolveActiveTab();
  if (!tab) return { type: "FetchResult", success: false, error: noPageErr() };
  try {
    const r = await withCdp(tab.id, async (tid) => {
      const code = fetchExpression(command);
      const ev = await cdpSend(tid, "Runtime.evaluate", {
        expression: code, awaitPromise: true, returnByValue: true,
      });
      return ev.result?.value;
    });
    if (r && r.oversize) {
      return { type: "FetchResult", success: false, error: err("Other", `response body exceeds the ${r.oversize}-byte fetch limit`) };
    }
    if (r) {
      return { type: "FetchResult", success: true, status: r.status, body: r.body };
    }
    return { type: "FetchResult", success: false, error: otherErr("No fetch result") };
  } catch (e) {
    return { type: "FetchResult", success: false, error: exceptionErr(e) };
  }
}

export { cdpEval, frameMainContextId, handleDomGet, handleDomSet, handleEval, handleFetch, handleWait };
