/**
 * WebPilot console monitor.
 *
 * Records what the page reports into `window.__webpilot_console`, the buffer
 * `console read` reads: its `console.*` calls, plus the errors the browser
 * reports on its behalf — the exceptions that reach the top of the stack and the
 * promise rejections nothing handles. Every message is the browser's own text,
 * never a description this recorder composes. The page is
 * the store because it is the only one that outlives a CLI process: the command
 * that arms the monitor and the command that reads it are separate processes,
 * and the page records continuously between them.
 *
 * The one source both modes install — headless registers it per document
 * (`Page.addScriptToEvaluateOnNewDocument`, MAIN world), browser injects it into
 * the pinned tab's main frame at navigation settle.
 */

(() => {
  // Only the main frame's buffer is ever read, so hooking a subframe would put
  // this patch in a third-party document for nothing. Browser mode targets
  // `frameIds: [0]`; headless registers against every frame of the target, so
  // the invariant is stated here, where it holds for both.
  if (window !== window.top) return;

  // Always (re-)attach the recorder. Gating on `window.__webpilot_console`
  // alone fails after `console clear` because an empty array is truthy and the
  // patch wouldn't reinstall. A separate sentinel keeps `start` idempotent
  // without that hazard.
  if (!Array.isArray(window.__webpilot_console)) {
    window.__webpilot_console = [];
  }
  if (window.__webpilot_console_patched) return;
  window.__webpilot_console_patched = true;

  // Max entries kept in the ring buffer. The read reports `truncated` from the
  // eviction flag set below, never from this number, so the cap lives only here.
  const CAP = 500;

  // Capture Date.now at install so a page that later booby-traps it (a throwing
  // getter) can't break the recording — and even if recording does throw, the
  // page's OWN console call still fires (see the try below). The monitor's
  // honest boundary is "may miss an entry", never "breaks the page".
  const nowFn = Date.now;

  // Clip a captured message like the DOM capture clips text: a runaway
  // `console.log("x".repeat(5e7))` must not balloon the buffer or the read's
  // payload. CODEPOINT-safe via Array.from (like bridge.js's clip): a bare
  // `slice` cuts by UTF-16 code unit and can split an astral pair into a lone
  // surrogate, which breaks the entry's JSON serialization through CDP
  // returnByValue / native messaging. The marker keeps the clip visible.
  const MAX = 4096;
  const clip = (s) => {
    if (s.length <= MAX) return s;
    const cps = Array.from(s);
    return cps.length > MAX ? cps.slice(0, MAX).join("") + "…[" + cps.length + " chars]" : s;
  };

  const text = (v) => { try { return String(v); } catch { return "[object]"; } };

  const record = (entry) => {
    const buf = window.__webpilot_console;
    buf.push(entry);
    // Evict the oldest past the cap and RECORD that an eviction happened: the
    // read's `truncated` flag is driven by this, not by `length >= cap`, so a
    // buffer sitting at exactly the cap (nothing dropped yet) isn't falsely
    // reported truncated.
    if (buf.length > CAP) {
      buf.shift();
      window.__webpilot_console_dropped = true;
    }
  };

  const orig = {
    log: console.log,
    error: console.error,
    warn: console.warn,
    info: console.info,
    debug: console.debug,
  };
  ["log", "error", "warn", "info", "debug"].forEach((m) => {
    console[m] = (...args) => {
      try {
        record({
          source: "console",
          level: m,
          message: clip(args.map(text).join(" ")),
          timestamp: nowFn(),
        });
      } catch {}
      orig[m].apply(console, args);
    };
  });

  // A page cancels the report of an error or a rejection by cancelling its
  // event, and the browser then prints nothing — so recording one would put an
  // entry in the buffer that the page's console never showed. The verdict is
  // only final once every listener has run, and this recorder is installed
  // before the page's own, so the entry is held until the dispatch is over and
  // committed only if the browser reported it too.
  //
  // "Once the dispatch is over" has to be a task: for `unhandledrejection`,
  // which the browser dispatches while draining microtasks, a microtask still
  // observes the pre-cancel verdict. A port message is the task that is not a
  // timer — timers are clamped to seconds in a backgrounded tab, which is
  // exactly where a browser-mode agent's pinned tab sits.
  const channel = new MessageChannel();
  const commits = [];
  channel.port1.onmessage = () => { const commit = commits.shift(); if (commit) commit(); };
  const reportedBy = (event, entry) => {
    commits.push(() => { if (!event.defaultPrevented) record(entry); });
    channel.port2.postMessage(0);
  };

  // No capture phase, so this is exactly the window's own error report. A
  // subresource that fails to load fires `error` at the ELEMENT and so never
  // reaches here: that event names no reason — not the status, not even whether
  // the request was refused or the bytes were unusable — so an entry made from
  // it could only say that something failed, in words this recorder invented.
  // The console shows such a failure; WebPilot does not report it anywhere yet.
  window.addEventListener("error", (event) => {
    try {
      // `message` is the browser's own text ("Uncaught TypeError: …"), and is
      // "Script error." with no location for a cross-origin script — the same
      // sanitized report the console prints.
      const where = event.filename ? ` (${event.filename}:${event.lineno}:${event.colno})` : "";
      reportedBy(event, {
        source: "exception",
        level: "error",
        message: clip(text(event.message) + where),
        timestamp: nowFn(),
      });
    } catch {}
  });

  window.addEventListener("unhandledrejection", (event) => {
    try {
      reportedBy(event, {
        source: "rejection",
        level: "error",
        message: clip(text(event.reason)),
        timestamp: nowFn(),
      });
    } catch {}
  });
})();
