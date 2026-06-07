// // Navigation settle: commit watch, settled-wait, and document readiness.
// // Mirrors the navigation half of transport/local/mod.rs.

import { navigationTimeoutMs, sleep } from "./session.js";

// ── Navigation settle ───────────────────────────────────────────────────────
// One predicate for every navigation this worker performs, mirroring the
// headless `navigation_settled`: committed — the main frame's URL left
// `beforeUrl`, or a main-frame commit was observed (the reload case, where the
// URL never moves) — AND parsed, readyState past "loading". Deadline-polled,
// no fixed sleeps, and the debugger is never attached (no banner). The commit
// watch must be registered before the navigation is issued.

function watchMainFrameCommit(tabId) {
  const watch = { committed: false, documentId: null, dispose: () => {} };
  const listener = (d) => {
    if (d.tabId === tabId && d.frameId === 0) {
      watch.committed = true;
      // The committed document's identity — the equivalent of the headless
      // loaderId. A redirect chain updates it to the latest commit, which is
      // the document worth settling on.
      watch.documentId = d.documentId ?? null;
    }
  };
  // Same-document navigations (pushState history traversal, fragment jumps)
  // commit via their own events, never onCommitted — all three together make
  // every navigation kind observable, including one whose URL doesn't move.
  const events = [
    chrome.webNavigation.onCommitted,
    chrome.webNavigation.onHistoryStateUpdated,
    chrome.webNavigation.onReferenceFragmentUpdated,
  ];
  events.forEach((ev) => ev.addListener(listener));
  // Self-disposing and idempotent: callers register the watch BEFORE issuing
  // the navigation, so if that issuance throws, `waitNavigationSettled` (which
  // would dispose) is never reached. A backstop timer removes the listeners
  // regardless, bounding the leak to one settle window instead of the worker's
  // lifetime; the explicit dispose on the normal path clears the timer.
  let disposed = false;
  const timer = setTimeout(() => watch.dispose(), navigationTimeoutMs() + 1000);
  watch.dispose = () => {
    if (disposed) return;
    disposed = true;
    clearTimeout(timer);
    events.forEach((ev) => ev.removeListener(listener));
  };
  return watch;
}

async function waitNavigationSettled(tabId, beforeUrl, watch, url) {
  const start = Date.now();
  try {
    while (Date.now() - start < navigationTimeoutMs()) {
      const tab = await chrome.tabs.get(tabId).catch(() => null);
      if (tab && (watch.committed || (tab.url && tab.url !== beforeUrl))) {
        // Bind the readiness probe to the COMMITTED document, not just "the
        // frame right now": on a same-URL reload the old document can still
        // report readyState "complete" in the beat between commit and swap.
        // The frame's live documentId must match the commit we observed (or,
        // when the commit carried none, its URL must have left beforeUrl) —
        // the same discrimination headless gets from its loaderId.
        const frame = await chrome.webNavigation
          .getFrame({ tabId, frameId: 0 })
          .catch(() => null);
        const documentMatches = watch.documentId
          ? frame?.documentId === watch.documentId
          : Boolean(frame?.url && frame.url !== beforeUrl);
        if (documentMatches) {
          // Probe readiness; a probe failing mid renderer swap just retries on
          // the next poll, exactly like the headless time-boxed PROBE.
          const ready = await chrome.scripting
            .executeScript({ target: { tabId, frameIds: [0] }, func: () => document.readyState })
            .then((r) => r?.[0]?.result)
            .catch(() => null);
          if (ready === "interactive" || ready === "complete") return;
        }
      }
      await sleep(150);
    }
  } finally {
    watch.dispose();
  }
  const e = new Error(`Navigation did not settle: ${url}`);
  e.code = "NavigationFailed";
  e.data = { url, reason: `not settled within ${navigationTimeoutMs()}ms` };
  throw e;
}


// Wait — bounded — for a freshly adopted popup to settle on the document the
// click actually opened. A click-opened tab commonly exists first as
// `about:blank` (already `complete`); a bare readyState probe would capture
// that blank page. Register the commit watch BEFORE checking the current URL
// so a commit that fires in between is not missed.
async function adoptedDocumentReady(tabId, timeoutMs) {
  const isReal = (u) => u && u !== "about:blank";
  await new Promise((resolve) => {
    // Every settle path — commit, already-real, AND timeout — routes through
    // `done()` so the listener is always removed; a bare `setTimeout(resolve)`
    // would leak `onCommitted` on the timeout path (a popup that stays blank).
    let timer;
    const done = () => {
      clearTimeout(timer);
      chrome.webNavigation.onCommitted.removeListener(onCommitted);
      resolve();
    };
    const onCommitted = (d) => {
      if (d.tabId === tabId && d.frameId === 0 && isReal(d.url)) done();
    };
    timer = setTimeout(done, timeoutMs);
    chrome.webNavigation.onCommitted.addListener(onCommitted);
    chrome.tabs.get(tabId).then((tab) => {
      if (tab && isReal(tab.url)) done();
    }).catch(() => {});
  });
  await documentReady(tabId, timeoutMs);
}

// Wait — bounded — until the tab's main-frame document has parsed, so a
// post-action capture never reads a committed-but-empty page. The listener is
// registered BEFORE the readyState probe: a DOMContentLoaded that fires
// between the two still resolves the wait instead of forcing the timeout.
async function documentReady(tabId, timeoutMs) {
  let settle;
  const settled = new Promise((resolve) => (settle = resolve));
  const timer = setTimeout(settle, timeoutMs);
  const onReady = (d) => {
    if (d.tabId === tabId && d.frameId === 0) settle();
  };
  chrome.webNavigation.onDOMContentLoaded.addListener(onReady);
  try {
    const [probe] = await chrome.scripting
      .executeScript({ target: { tabId }, func: () => document.readyState })
      .catch(() => []);
    if (probe?.result && probe.result !== "loading") return;
    await settled;
  } finally {
    clearTimeout(timer);
    chrome.webNavigation.onDOMContentLoaded.removeListener(onReady);
  }
}

export { adoptedDocumentReady, documentReady, waitNavigationSettled, watchMainFrameCommit };
