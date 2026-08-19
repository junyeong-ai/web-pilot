/**
 * WebPilot network monitor.
 *
 * Records the page's `fetch` and `XMLHttpRequest` traffic into
 * `window.__webpilot_network`, the buffer `network read` reads. The page is the
 * store for the same reason the console monitor's is — see monitor-console.js.
 *
 * The one source both modes install — headless registers it per document
 * (`Page.addScriptToEvaluateOnNewDocument`, MAIN world), browser injects it into
 * the pinned tab's main frame at navigation settle.
 */

(() => {
  // Main frame only — see monitor-console.js.
  if (window !== window.top) return;

  // The buffer and the install sentinel are separate globals — see
  // monitor-console.js: one flag doing both jobs means clearing it wipes every
  // entry recorded so far AND wraps `fetch`/XHR a second time.
  if (!Array.isArray(window.__webpilot_network)) {
    window.__webpilot_network = [];
  }
  // The wrappers carry their own mark, and that — not the page-writable flag —
  // is what says whether this document is already hooked: clearing the flag must
  // not make this install run over its own wrappers and report every request
  // twice. The flag is the cheap probe the reconcile reads, so it is repaired
  // rather than trusted.
  const ours = (window.fetch && window.fetch.__webpilot)
    || (XMLHttpRequest.prototype.send && XMLHttpRequest.prototype.send.__webpilot);
  window.__webpilot_network_patched = true;
  if (ours) return;

  // What SHAPE this recorder writes. Chrome outlives the process that hooked a
  // document, so a later build can meet this one still running; the read checks
  // this rather than inferring from the entries, which cannot tell a recorder
  // from another build apart from a page writing entries of its own. Bump it
  // whenever the entry shape changes.
  window.__webpilot_network_shape = 1;

  // Max entries kept in the ring buffer — see monitor-console.js.
  const CAP = 500;

  // Intrinsics captured at install, by binding the receiver where one is needed:
  // a page that later booby-traps Date.now / performance.now (a throwing getter,
  // or a swap) can't break or skew the recording. Every recording is wrapped so
  // it can never break the page's OWN fetch/XHR — the monitor's honest boundary
  // is "may miss an entry", never "breaks the page".
  const nowFn = Date.now;
  const perfObj = performance;
  const perfNowRaw = perfObj.now;
  const perfNow = () => { try { return perfNowRaw.call(perfObj); } catch { return 0; } };

  // A captured URL is clipped like the DOM capture so a giant data: URL can't
  // balloon the buffer or the read's payload. CODEPOINT-safe (a lone surrogate
  // from a split astral pair breaks the entry's JSON serialization) — see
  // monitor-console.js.
  const MAX = 4096;
  const clip = (s) => {
    if (s.length <= MAX) return s;
    const cps = Array.from(s);
    return cps.length > MAX ? cps.slice(0, MAX).join("") + "…[" + cps.length + " chars]" : s;
  };

  const record = (entry) => {
    const buf = window.__webpilot_network;
    buf.push(entry);
    if (buf.length > CAP) {
      buf.shift();
      window.__webpilot_network_dropped = true;
    }
  };

  const origFetch = window.fetch;
  window.fetch = function (...args) {
    let entry = null;
    let t0 = 0;
    try {
      const [resource, config] = args;
      // A Request object carries its own url/method (a config override still
      // wins); String(resource) on one logs "[object Request]" and drops the
      // method.
      const isReq = typeof Request !== "undefined" && resource instanceof Request;
      const url = isReq ? resource.url : String(resource);
      const method = config?.method || (isReq ? resource.method : "GET");
      t0 = perfNow();
      // Record in-flight immediately (no status, duration 0) so a read during a
      // slow request sees it; fill in on completion by mutating this entry.
      entry = { type: "fetch", url: clip(url), method, duration_ms: 0, timestamp: nowFn() };
      record(entry);
    } catch {
      entry = null;
    }
    // origFetch can throw SYNCHRONOUSLY (a bad argument — `fetch()` with no args
    // is a TypeError, not a rejected promise). Stamp the recorded entry as
    // errored instead of leaving it in-flight forever, then rethrow so the page
    // sees the same exception.
    let p;
    try {
      p = origFetch.apply(this, args);
    } catch (e) {
      if (entry) {
        try {
          entry.error = String((e && e.message) || e);
          entry.duration_ms = Math.round(perfNow() - t0);
          entry.timestamp = nowFn();
        } catch {}
      }
      throw e;
    }
    if (!entry) return p;
    return p
      .then((response) => {
        // Re-stamp at completion so `--since` polling, which filters on
        // timestamp, sees the resolved entry; the in-flight start time would sit
        // before a cursor taken after the request began.
        try {
          entry.status = response.status;
          entry.duration_ms = Math.round(perfNow() - t0);
          entry.timestamp = nowFn();
        } catch {}
        return response;
      })
      .catch((err) => {
        try {
          entry.error = String((err && err.message) || err);
          entry.duration_ms = Math.round(perfNow() - t0);
          entry.timestamp = nowFn();
        } catch {}
        throw err;
      });
  };

  window.fetch.__webpilot = 1;

  const xhrProto = XMLHttpRequest.prototype;
  const origOpen = xhrProto.open;
  const origSend = xhrProto.send;
  const xhrMeta = new WeakMap();
  xhrProto.open = function (m, u, ...a) {
    try { xhrMeta.set(this, { method: m, url: clip(String(u)) }); } catch {}
    return origOpen.apply(this, [m, u, ...a]);
  };
  xhrProto.send = function (...a) {
    let entry = null;
    let t0 = 0;
    try {
      t0 = perfNow();
      const meta = xhrMeta.get(this) || {};
      entry = {
        type: "xhr",
        url: meta.url || "",
        method: meta.method || "GET",
        duration_ms: 0,
        timestamp: nowFn(),
      };
      record(entry);
      // status===0 covers abort, timeout AND network/CORS failure alike, so read
      // the actual terminal event instead of labelling every one a "Network
      // error" — an aborted request the page itself cancelled is not a network
      // failure.
      let terminalError;
      this.addEventListener("abort", () => { terminalError = "aborted"; }, { once: true });
      this.addEventListener("timeout", () => { terminalError = "timeout"; }, { once: true });
      this.addEventListener("error", () => { terminalError = "Network error"; }, { once: true });
      this.addEventListener("loadend", () => {
        try {
          entry.status = this.status || undefined;
          entry.error = terminalError;
          entry.duration_ms = Math.round(perfNow() - t0);
          entry.timestamp = nowFn();
        } catch {}
      }, { once: true });
    } catch {
      entry = null;
    }
    // `send()` can throw SYNCHRONOUSLY — before `open`, or on a detached
    // document — and `loadend` never fires for a request that never started, so
    // an entry recorded above would stay in flight forever. Stamp it and rethrow,
    // exactly as the fetch wrapper does for its own synchronous throw.
    try {
      return origSend.apply(this, a);
    } catch (e) {
      if (entry) {
        try {
          entry.error = String((e && e.message) || e);
          entry.duration_ms = Math.round(perfNow() - t0);
          entry.timestamp = nowFn();
        } catch {}
      }
      throw e;
    }
  };
  xhrProto.send.__webpilot = 1;
})();
