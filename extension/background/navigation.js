// // Navigation settle: commit watch, settled-wait, and document readiness.
// // Mirrors the navigation half of transport/local/mod.rs.

import { navigationTimeoutMs, resolveActiveTab, setActiveFrameId, setActiveTabId, sleep } from "./session.js";
import { rearmMonitors } from "./state.js";

// ── Navigation settle ───────────────────────────────────────────────────────
// One predicate for every navigation this worker performs, mirroring the
// headless `navigation_settled`: committed — the main frame's URL left
// `beforeUrl`, or a main-frame commit was observed (the reload case, where the
// URL never moves) — AND parsed, readyState past "loading". Deadline-polled,
// no fixed sleeps, and the debugger is never attached (no banner). The commit
// watch must be registered before the navigation is issued.

function watchMainFrameCommit(tabId) {
  const watch = {
    started: false,
    committed: false,
    crossDocument: false,
    documentId: null,
    error: null,
    dispose: () => {},
  };
  const mark = (d, crossDocument) => {
    if (d.tabId !== tabId || d.frameId !== 0) return;
    watch.committed = true;
    // The committed document's identity — the equivalent of the headless
    // loaderId. A redirect chain updates it to the latest commit, which is
    // the document worth settling on.
    watch.documentId = d.documentId ?? null;
    // onCommitted is a NEW document — the headless "loaderId is Some" case, so
    // the frame tree is replaced. Same-document navigations (pushState history
    // traversal, fragment jumps) leave the document AND its frames intact, so a
    // frame the agent switched into survives them (headless returns early for
    // exactly this case without clearing the active frame).
    if (crossDocument) watch.crossDocument = true;
  };
  const onCommitted = (d) => mark(d, true);
  const onSameDocument = (d) => mark(d, false);
  // A main-frame navigation that has BEGUN but not yet committed — the headless
  // `frameStartedLoading` signal. A click whose handler sets `location.href`
  // fires this before any commit, so an action that triggered a navigation is
  // recognised (and waited out) even when the commit is still in flight.
  const startedListener = (d) => {
    if (d.tabId === tabId && d.frameId === 0) watch.started = true;
  };
  // A failed navigation surfaces here, never as a commit. ERR_ABORTED is left
  // PENDING (it may still settle — a redirect/abort that proceeds); any other
  // errorText is a hard failure the settle loop fails fast on. This is the
  // headless split read off the Page.navigate response, mirrored.
  const errorListener = (d) => {
    if (d.tabId === tabId && d.frameId === 0) watch.error = d.error || "navigation error";
  };
  chrome.webNavigation.onCommitted.addListener(onCommitted);
  chrome.webNavigation.onHistoryStateUpdated.addListener(onSameDocument);
  chrome.webNavigation.onReferenceFragmentUpdated.addListener(onSameDocument);
  chrome.webNavigation.onBeforeNavigate.addListener(startedListener);
  chrome.webNavigation.onErrorOccurred.addListener(errorListener);
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
    chrome.webNavigation.onCommitted.removeListener(onCommitted);
    chrome.webNavigation.onHistoryStateUpdated.removeListener(onSameDocument);
    chrome.webNavigation.onReferenceFragmentUpdated.removeListener(onSameDocument);
    chrome.webNavigation.onBeforeNavigate.removeListener(startedListener);
    chrome.webNavigation.onErrorOccurred.removeListener(errorListener);
  };
  return watch;
}

// The URL the main frame settled on after a page action — the browser twin of
// headless `settled_action_url`. A non-navigating action (type, scroll, a click
// that opens no document) returns `beforeUrl` with no wait; an action that began
// a main-frame navigation (a link click, a handler setting `location.href`, an
// Enter that submits a form) waits — bounded — for the new document to parse and
// returns its URL, so a following auto-capture reads the destination, not the
// dying page. Never throws: the action's side effect is already done. The watch
// must be registered BEFORE the action so the start/commit it triggers is seen.
async function settledActionUrl(tabId, beforeUrl, watch) {
  // The navigation-start signal (`watch.started`, from onBeforeNavigate) is the
  // decision authority, not a fixed grace window: a guessed delay would either
  // add latency to every non-navigating action or still miss a slow start, so
  // none is added. The `chrome.tabs.get` await yields the event loop so an
  // onBeforeNavigate dispatched during the action is processed before the check
  // — the browser's nearest equivalent of headless's in-order CDP event buffer.
  // The residual gap (a start that dispatches after this point) is inherent to
  // MV3's cross-channel event ordering and degrades to a best-effort capture the
  // agent re-runs, never a wrong action; headless, on one ordered socket, has no
  // such gap.
  const tab = await chrome.tabs.get(tabId).catch(() => null);
  const urlNow = tab?.url || "";
  if (!watch.started && !watch.committed && (!urlNow || urlNow === beforeUrl)) {
    watch.dispose();
    return beforeUrl; // nothing navigated — hot path, no settle wait
  }
  try {
    await waitNavigationSettled(tabId, beforeUrl, watch, "action navigation");
  } catch {
    // Didn't settle within the deadline — report wherever the frame is now
    // rather than failing the action whose side effect already happened.
  }
  const settled = await chrome.tabs.get(tabId).catch(() => null);
  return settled?.url || beforeUrl;
}

async function waitNavigationSettled(tabId, beforeUrl, watch, url) {
  const start = Date.now();
  try {
    while (Date.now() - start < navigationTimeoutMs()) {
      const tab = await chrome.tabs.get(tabId).catch(() => null);
      if (!tab) {
        // The tab vanished mid-navigation (closed, or a page `window.close()`).
        // That is a gone pin — a typed TabNotFound the agent re-pins from, the
        // way `resolveActiveTab` already types one — not a full-timeout wait that
        // then surfaces as a misleading NavigationFailed. (`chrome.tabs.get`
        // resolves for a live tab whatever its navigation state, so a null here
        // is a genuine vanish, never a transient mid-nav blip.)
        const e = new Error(`Tab not found: ${tabId}. List: webpilot tab`);
        e.code = "TabNotFound";
        e.data = { tab_id: String(tabId) };
        throw e;
      }
      if (watch.error && watch.error !== "net::ERR_ABORTED") {
        // A hard navigation error (DNS, connection refused) — fail fast and
        // typed, exactly as headless returns immediately for a non-ERR_ABORTED
        // errorText. ERR_ABORTED is left pending below: it may still settle.
        const e = new Error(`Navigation failed: ${url}`);
        e.code = "NavigationFailed";
        e.data = { url, reason: watch.error };
        throw e;
      }
      if (watch.committed || (tab.url && tab.url !== beforeUrl)) {
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
  // Deadline expired. A recorded navigation error — an ERR_ABORTED that never
  // settled — is a NavigationFailed; otherwise the navigation started and simply
  // never finished parsing, which is a Timeout (exit 5 → retry), not a failure
  // (exit 8). Headless draws the identical split: `start_error` if present, else
  // a typed Timeout.
  if (watch.error) {
    const e = new Error(`Navigation failed: ${url}`);
    e.code = "NavigationFailed";
    e.data = { url, reason: watch.error };
    throw e;
  }
  const e = new Error(`Navigation did not settle within ${navigationTimeoutMs()}ms: ${url}`);
  e.code = "Timeout";
  e.data = { kind: "navigation", elapsed_ms: navigationTimeoutMs() };
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

// After a click navigated the switched iframe, wait — bounded — until that frame
// holds a NEW committed document (its `documentId` differs from `beforeDocId`)
// that has parsed. An iframe-internal navigation never touches the top URL the
// main settle watches, so without this the post-action capture reads the
// pre-click document. The browser analogue of headless
// `await_live_active_frame_context`; the documentId is the SW's stand-in for the
// frame's execution-context identity.
async function waitActiveFrameSettled(tabId, frameId, beforeDocId) {
  const deadline = Date.now() + navigationTimeoutMs();
  while (Date.now() < deadline) {
    const frames = await chrome.webNavigation.getAllFrames({ tabId }).catch(() => null);
    const f = frames?.find((x) => x.frameId === frameId);
    if (f && f.documentId !== beforeDocId) {
      const [probe] = await chrome.scripting
        .executeScript({ target: { tabId, frameIds: [frameId] }, func: () => document.readyState })
        .catch(() => []);
      if (probe?.result && probe.result !== "loading") return;
    }
    await sleep(50);
  }
}

// Navigate the bound tab to `url`, or create + pin a fresh tab when there is no
// injectable http tab to reuse (a chrome://newtab focus, an about: page, a
// vanished page). This is the single tab-resolution path for a top-level
// navigation, shared by `action navigate` and `capture --url` so the two can't
// drift: navigate is how an agent REACHES an http page, so it must succeed from
// a non-http start exactly as headless navigates its bound about:blank — never a
// `NoPage`. Resets the frame scope and re-arms monitors at settle, mirroring the
// headless `navigate_reconnect`. Returns the settled tab id.
async function navigateBoundTab(url) {
  const existing = await resolveActiveTab();
  let tabId;
  let beforeUrl = "";
  let watch;
  if (existing) {
    tabId = existing.id;
    beforeUrl = existing.url || "";
    watch = watchMainFrameCommit(tabId);
    await chrome.tabs.update(tabId, { url, active: true });
  } else {
    const t = await chrome.tabs.create({ url, active: true });
    tabId = t.id;
    setActiveTabId(tabId);
    watch = watchMainFrameCommit(tabId);
  }
  await waitNavigationSettled(tabId, beforeUrl, watch, url);
  // A cross-document navigation replaces the frame tree — drop any stale frame
  // scope so a capture after `frame switch` then a navigate is main-frame-scoped.
  // A same-document nav (hash/pushState) leaves the document and its frames
  // intact, so a frame the agent switched into stays valid (headless returns
  // early for that case without clearing the active frame). A freshly created
  // tab is always main-scoped.
  if (watch.crossDocument || !existing) setActiveFrameId(0);
  // Re-arm console/network hooks at settle (headless parity) so traffic the new
  // page fires before `load` is captured, not lost to the `onCompleted` gap.
  await rearmMonitors(tabId);
  return tabId;
}

export {
  adoptedDocumentReady,
  documentReady,
  navigateBoundTab,
  settledActionUrl,
  waitActiveFrameSettled,
  waitNavigationSettled,
  watchMainFrameCommit,
};
