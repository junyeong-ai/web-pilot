// Page queries: eval (debugger-routed, CSP-immune), wait, dom get/set, fetch.
// Mirrors transport/local/query.rs.

import { err, exceptionErr, noPageErr, otherErr, timeoutErr } from "./errors.js";
import { PROBE_MS, activeFrameId, navigationTimeoutMs, resolveActiveTab, sleep } from "./session.js";
import { cdpSend, cdpEnableRuntime, cdpReemitContexts, withCdp } from "./cdp.js";
import { ensureBridge, frameVanishedError, sendToContent, sendToContentOnce } from "./content.js";

// ── Eval ───────────────────────────────────────────────────────────────────

// Resolve the CDP execution context of a webNavigation frame's `world`
// ("MAIN" for page eval, "ISOLATED" for the extension's content-script world),
// inside an attached debugger session. Extension frame ids are integers; CDP
// contexts carry opaque frame GUIDs — the bridge between the two id spaces is a
// one-shot nonce: a PRECOMPILED function (no dynamic code, so page CSP cannot
// block it) stamps the chosen world's window, and the context whose global
// carries the stamp is that world's context. URL matching would be ambiguous
// for same-URL sibling frames; the nonce never is, and because the stamp is
// written only in `world`, the matching context is unambiguously that world's
// (no auxData-type guess). A cross-origin out-of-process iframe has no context
// in the tab's session and resolves to null — the same boundary headless has.
// Resolve a switched frame's `{ contextId, frameId }` in `world`: the
// `uniqueContextId` for evaluation, and the CDP `frameId` (from the matched
// context's auxData) for frame-scoped CDP calls like Accessibility.getFullAXTree.
// `frameWorldContextId` is the thin wrapper that returns only the contextId.
async function resolveFrameWorld(tid, tabId, frameId, world) {
  const contexts = [];
  const onEvent = (source, method, params) => {
    if (source.tabId === tabId && method === "Runtime.executionContextCreated") {
      contexts.push(params.context);
    }
  };
  chrome.debugger.onEvent.addListener(onEvent);
  // Both the property KEY and its value are unique per probe: a fixed key
  // would clobber (and on cleanup, delete) a page's own property of that name.
  const nonce = `wp-${Date.now()}-${Math.random().toString(36).slice(2)}`;
  const key = `__wp_probe_${nonce}`;
  try {
    // Toggle the domain so existing contexts re-announce into our listener.
    await cdpReemitContexts(tid);
    try {
      await chrome.scripting.executeScript({
        target: { tabId, frameIds: [frameId] },
        world,
        func: (k, n) => { window[k] = n; },
        args: [key, nonce],
      });
    } catch {
      // The frame was removed (or its world is unreachable) before we could
      // stamp it — surface as FrameNotFound (null → handleEval), matching the
      // headless boundary, rather than letting the raw scripting error escape
      // as a generic Other/exit 1.
      return null;
    }
    const deadline = Date.now() + PROBE_MS;
    const probed = new Set();
    while (Date.now() < deadline) {
      const matches = [];
      for (const c of contexts) {
        // A context with no string uniqueId can't be addressed unambiguously
        // (omitting it would silently target the default context), so skip it —
        // the headless listener applies the same guard before mapping.
        if (typeof c.uniqueId !== "string" || probed.has(c.uniqueId)) continue;
        probed.add(c.uniqueId);
        const r = await cdpSend(tid, "Runtime.evaluate", {
          expression: `window[${JSON.stringify(key)}]`,
          uniqueContextId: c.uniqueId,
          returnByValue: true,
        }).catch(() => null);
        // Return the uniqueContextId, not the reusable integer id: it stays
        // valid across a renderer swap and can never resolve to a different
        // context that reused the integer.
        if (r?.result?.value === nonce) matches.push(c);
      }
      // Exactly one context must carry the nonce. The stamp is written only in
      // the targeted frame+world, so a second match means a same-origin page
      // copied the probe global across windows to spoof the target — refuse to
      // guess which is real rather than returning the wrong frame's context.
      if (matches.length === 1) {
        const m = matches[0];
        return { contextId: m.uniqueId, frameId: m.auxData?.frameId ?? null };
      }
      if (matches.length > 1) return null;
      await sleep(25);
    }
    return null;
  } finally {
    chrome.debugger.onEvent.removeListener(onEvent);
    chrome.scripting.executeScript({
      target: { tabId, frameIds: [frameId] },
      world,
      func: (k) => { delete window[k]; },
      args: [key],
    }).catch(() => {});
  }
}

// The uniqueContextId alone — what evaluation needs. `null` when the frame's
// world can't be resolved (removed frame / cross-origin OOPIF), same boundary.
async function frameWorldContextId(tid, tabId, frameId, world) {
  const resolved = await resolveFrameWorld(tid, tabId, frameId, world);
  return resolved ? resolved.contextId : null;
}

// One evaluation contract for every frame: decide the form by COMPILING
// (`compileScript` parses without evaluating — a runtime
// `throw new SyntaxError(...)` from a valid expression must not run the code
// twice), expression-first so `{a:1}` is an object literal, promise awaited.
// Debugger-routed evaluation is not subject to the page's CSP, so a hardened
// page (`script-src 'self'`) keeps full eval in any frame — headless parity.
// Requires Runtime enabled on the session.
// Sentinel that distinguishes "the eval deadline fired" from any real evaluate
// result (a CDP response is always an object, never this symbol).
const EVAL_DEADLINE_REACHED = Symbol("eval-deadline-reached");

async function cdpEval(tid, code, uniqueContextId) {
  // The form probe compiles WITHOUT a context — a parse (expression vs
  // statements) is context-independent, and compileScript takes only the
  // integer executionContextId, never a uniqueContextId. The real evaluation
  // targets the frame's context by its stable uniqueContextId.
  const compiled = await cdpSend(tid, "Runtime.compileScript", {
    expression: `(${code})`,
    sourceURL: "webpilot://eval-form-probe",
    persistScript: false,
  });
  const form = compiled.exceptionDetails ? code : `(()=>(${code}))()`;
  // Bound the awaitPromise evaluate: a page expression resolving to a
  // never-settling promise (`new Promise(()=>{})`) would otherwise hang inside
  // `withCdp` forever — wedging the per-tab CDP lock and, through the global
  // command queue, every later command. Headless bounds the identical evaluate
  // with `send_with_timeout(cdp_send)` (transport/local/mod.rs); this is the
  // browser-mode parity. Only THIS call awaits a page promise — the other
  // cdpSends settle promptly, so they need no bound.
  const evalCall = cdpSend(tid, "Runtime.evaluate", {
    expression: form,
    returnByValue: true,
    awaitPromise: true,
    ...(uniqueContextId != null ? { uniqueContextId } : {}),
  });
  // A settle AFTER the deadline (or after `withCdp` detaches, which rejects the
  // in-flight command) is harmless — the race has resolved — but must be caught
  // so it is not an unhandled rejection. The race below still observes the
  // original promise; this extra handler only swallows a late outcome.
  evalCall.catch(() => {});
  let timer;
  const deadline = navigationTimeoutMs();
  const ev = await Promise.race([
    evalCall,
    new Promise((resolve) => {
      timer = setTimeout(() => resolve(EVAL_DEADLINE_REACHED), deadline);
    }),
  ]).finally(() => clearTimeout(timer));
  if (ev === EVAL_DEADLINE_REACHED) {
    // The page promise never settled within the window. `withCdp` returns
    // normally here (not a throw), so its `finally` detaches and frees the lock;
    // the abandoned `evalCall` is cancelled by that detach and swallowed above.
    return { success: false, error: timeoutErr("eval", deadline) };
  }
  if (ev.exceptionDetails) {
    const msg = ev.exceptionDetails.exception?.description || ev.exceptionDetails.text || "JS exception";
    return { success: false, error: otherErr(msg) };
  }
  return { success: true, result: JSON.stringify(decodeRemoteObject(ev.result)) };
}

// Faithfully render a CDP RemoteObject. CDP omits `value` for results JSON
// cannot carry — `undefined`, `NaN`/`±Infinity`/`-0`, `BigInt`, functions,
// symbols. Collapsing them to `null` would tell the agent the page returned
// `null` when it did not. Fall back to the engine's own rendering (the
// `unserializableValue` literal, the bare `undefined`, or the `description`),
// and only a genuinely empty object to `null`. Mirrors the headless
// `decode_eval_result`. `"value" in remote` (not truthiness) keeps a real
// `null`/`0`/`false`/`""` intact.
function decodeRemoteObject(remote) {
  if (remote && "value" in remote) return remote.value;
  if (typeof remote?.unserializableValue === "string") return remote.unserializableValue;
  if (remote?.type === "undefined") return "undefined";
  return typeof remote?.description === "string" ? remote.description : null;
}

async function handleEval(command) {
  const tab = await resolveActiveTab();
  if (!tab) return { type: "Eval", success: false, error: noPageErr() };

  try {
    const r = await withCdp(tab.id, async (tid) => {
      await cdpEnableRuntime(tid);
      let uniqueContextId;
      if (activeFrameId !== 0) {
        uniqueContextId = await frameWorldContextId(tid, tab.id, activeFrameId, "MAIN");
        if (uniqueContextId == null) {
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
      return cdpEval(tid, command.code, uniqueContextId);
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
  // `?? `, not `|| `: a zero timeout is a valid "poll once, don't wait" request
  // that headless preserves, so coercing 0 to the 10s default here would make
  // the two modes diverge. Clamp at the `setTimeout` ceiling (i32::MAX ms,
  // ~24.8 days): a larger value silently overflows the in-page timer and fires
  // immediately (a false instant timeout) — the headless `do_wait` clamps the
  // same value (where it also prevents an Instant-overflow panic), so the two
  // modes agree on a degenerate timeout too.
  const timeoutMs = Math.min(command.timeout_ms ?? 10000, 0x7fffffff);

  if (cond.until === "navigation") {
    // The main frame finished a document navigation — the headless
    // `Page.loadEventFired` twin. `webNavigation.onCompleted` (frame 0) is
    // scheme-agnostic, so unlike a `tabs.onUpdated` + `http` filter it doesn't
    // miss a navigation to a non-http document.
    let listener;
    let removedListener;
    try {
      await Promise.race([
        new Promise((resolve) => {
          listener = (d) => {
            if (d.tabId === tab.id && d.frameId === 0) {
              chrome.webNavigation.onCompleted.removeListener(listener);
              listener = null;
              resolve();
            }
          };
          chrome.webNavigation.onCompleted.addListener(listener);
        }),
        // The pinned tab can CLOSE during the wait (window.close() in a load
        // handler, the user closing it) — the awaited navigation then cannot
        // ever complete, and sitting out the full timeout to report `Timeout`
        // would tell the agent "navigation didn't finish" when the truth is
        // "the tab is gone" (exit 4 → recover via `tab`).
        new Promise((_, rej) => {
          removedListener = (closedId) => {
            if (closedId === tab.id) rej(new Error("tab-closed"));
          };
          chrome.tabs.onRemoved.addListener(removedListener);
        }),
        new Promise((_, rej) => setTimeout(() => rej(new Error("nav-timeout")), timeoutMs)),
      ]);
      return { type: "Wait", success: true };
    } catch (e) {
      if (e?.message === "tab-closed") {
        return {
          type: "Wait",
          success: false,
          error: err("TabNotFound", `Tab not found: ${tab.id}. List: webpilot tab`, { tab_id: String(tab.id) }),
        };
      }
      return { type: "Wait", success: false, error: timeoutErr("navigation", timeoutMs) };
    } finally {
      if (listener) chrome.webNavigation.onCompleted.removeListener(listener);
      if (removedListener) chrome.tabs.onRemoved.removeListener(removedListener);
    }
  }

  // Selector / text / idle — delegate to bridge.js with the same condition
  // shape. A document navigation mid-poll tears the content script down and
  // rejects the in-flight send — which does not invalidate the wait's intent:
  // the condition may well be satisfied by the NEW document (a redirect landing
  // on the page the agent is waiting for). Re-arm against it with the
  // REMAINING budget (headless re-arms identically; Playwright's selector
  // waits survive navigations the same way). Typed failures from the re-probe
  // — TabNotFound / FrameNotFound / BridgeUnavailable — are root causes and
  // surface immediately; only the untyped port-death class re-arms.
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    try {
      const frameGone = await frameVanishedError(tab.id, activeFrameId);
      if (frameGone) return { type: "Wait", success: false, error: frameGone };
      await ensureBridge(tab.id, activeFrameId);
      const remaining = Math.max(0, deadline - Date.now());
      const r = await sendToContentOnce(
        tab.id,
        { type: "wait", condition: cond, timeout_ms: remaining },
        activeFrameId,
        remaining + 2000,
      );
      if (r.success) return { type: "Wait", success: true };
      const error = r.error || timeoutErr("wait", timeoutMs);
      // The bridge's per-round timer knows only its own (post-re-arm) residual
      // budget; the agent asked for `timeoutMs` total. Report the full ask.
      if (error.code === "Timeout") error.elapsed_ms = timeoutMs;
      return { type: "Wait", success: false, error };
    } catch (e) {
      // A thrown exception here is the bridge CALL failing, not the wait
      // condition timing out — the real timeout comes back as `r.error` above.
      // Typed = root cause (BridgeUnavailable keeps exit 3, a vanished tab or
      // frame exit 4); untyped past the deadline = the budget is spent, which
      // is the Timeout the in-page timer would have reported. Untyped BEFORE
      // the deadline is the document going away under the poll — re-arm.
      if (e?.code) return { type: "Wait", success: false, error: exceptionErr(e) };
      if (Date.now() >= deadline) {
        const desc = cond.value != null
          ? `wait ${cond.until} ${JSON.stringify(String(cond.value))}`
          : `wait ${cond.until}`;
        return { type: "Wait", success: false, error: timeoutErr(desc, timeoutMs) };
      }
    }
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
    const frameGone = await frameVanishedError(tab.id, activeFrameId);
    if (frameGone) return { type: "CommandResult", success: false, error: frameGone };
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
    const frameGone = await frameVanishedError(tab.id, activeFrameId);
    if (frameGone) return { type: "CommandResult", success: false, error: frameGone };
    await ensureBridge(tab.id, activeFrameId);
    const r = await sendToContent(tab.id, msg, activeFrameId);
    return {
      type: "CommandResult",
      // `?? null`, not `|| null`: a DOM value can legitimately be the empty
      // string — `getText` on an empty element, `getAttr` on a present-but-empty
      // attribute (`disabled=""`) — and `""` must survive distinct from `null`
      // (the attribute being absent). `||` would collapse both to `null`, losing
      // the present-empty case and diverging from headless, which keeps `""`.
      success: r.success,
      value: r.value ?? null,
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
      method: ${JSON.stringify(command.method ?? "GET")},
      // The caller's headers as [name, value] pairs — fetch takes them verbatim;
      // no implied content type. credentials:include keeps the session contract.
      headers: ${JSON.stringify(command.headers ?? [])},
      credentials: "include",
      ${command.body != null ? `body: ${JSON.stringify(command.body)},` : ""}
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
    let body;
    try { body = new TextDecoder("utf-8", { fatal: true }).decode(merged); }
    catch (e) { return { status: r.status, binary: total }; }
    return { status: r.status, body };
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
      // A rejected fetch (DNS, connection refused, CORS) surfaces as an eval
      // exception, not a value — raise it instead of returning `undefined` and
      // reporting the misleading "no result". Headless does the same: its
      // `page.evaluate(...)?` propagates the exception.
      if (ev.exceptionDetails) {
        throw new Error(
          ev.exceptionDetails.exception?.description ||
            ev.exceptionDetails.text ||
            "fetch failed",
        );
      }
      return ev.result?.value;
    });
    if (r.oversize) {
      return { type: "FetchResult", success: false, error: err("Other", `response body exceeds the ${r.oversize}-byte fetch limit`) };
    }
    // A body that isn't valid UTF-8 is binary; a lossy-decoded string under a
    // success status would be mojibake the agent can't tell from text. Fail
    // loud with the byte count — headless parity.
    if (r.binary != null) {
      return { type: "FetchResult", success: false, error: err("Other", `response body is not valid UTF-8 (${r.binary} bytes); fetch returns text, not binary`) };
    }
    return { type: "FetchResult", success: true, status: r.status, body: r.body };
  } catch (e) {
    return { type: "FetchResult", success: false, error: exceptionErr(e) };
  }
}

export { cdpEval, frameWorldContextId, handleDomGet, handleDomSet, handleEval, handleFetch, handleWait, resolveFrameWorld };
