// Page-mutating actions: dispatch, native key/hover/drag/upload, popup and
// navigation detection. Mirrors transport/local/action.rs.

import { err, exceptionErr, noPageErr, otherErr } from "./errors.js";
import { PROBE_MS, activeFrameId, resolveActiveTab, setActiveFrameId, setActiveTabId, sleep } from "./session.js";
import { cdpSend, withCdp } from "./cdp.js";
import { ensureBridge, frameVanishedError, installDialogOverride, sendToContent } from "./content.js";
import { adoptedDocumentReady, documentReady, navigateBoundTab, settledActionUrl, waitActiveFrameSettled, waitHistoryTraversed, waitNavigationSettled, watchMainFrameCommit } from "./navigation.js";
import { frameWorldContextId } from "./query.js";
import { countHttpSubframes } from "./capture.js";
import { rearmMonitors } from "./state.js";

// ── Action ─────────────────────────────────────────────────────────────────

async function handleAction(command) {
  const { action } = command;

  // Policy is enforced by the NM host (`policy::parse_and_enforce`) — the
  // browser-mode privileged sink — before the command is forwarded here, so
  // the service worker never re-checks it. (The CLI-side IpcTransport is only
  // a socket writer and is deliberately NOT a gate; writing the socket
  // directly would bypass it, which is why the host re-validates.)

  // Inject the dialog override before any action runs in the page — into
  // EVERY frame: a click's handler can call `alert`/`confirm`/`prompt` from
  // any frame (a third-party iframe included), and a native modal in any of
  // them blocks the page thread until the action's content call times out,
  // with no recovery. Accept-with-default semantics mirror the headless
  // dialog responder (Page.handleJavaScriptDialog accept:true), so a page
  // branching on a dialog behaves identically in both modes. The override is
  // idempotent, so re-installing on each action is a no-op.
  // `navigate` resolves its own target (the bound tab whatever its scheme, or a
  // fresh one) via `navigateBoundTab` below — it is how an agent REACHES an http
  // page, so it must NOT require one to start from. Every OTHER action needs an
  // injectable http page now, so it goes through the http-required
  // `resolveActiveTab`. (`navigate` also needs no dialog override: it runs no
  // page JS that could call alert/confirm/prompt, and a beforeunload prompt is a
  // separate native dialog the override can't suppress anyway.)
  const tab = action.kind === "navigate" ? null : await resolveActiveTab();
  if (!tab && action.kind !== "navigate") {
    return { type: "Action", success: false, error: noPageErr() };
  }
  if (tab) {
    await installDialogOverride(tab.id, { allFrames: true });
  }

  // Viewport-coordinate actions (`hover`, `drag`) cannot target an iframe: their
  // CDP path uses page-level coordinates, so inside a switched frame they would
  // act on the wrong position. Reject them, matching the headless
  // `require_main_frame` guard. (`upload` is NOT gated — it resolves in the active
  // frame's content-script world and sets the file on a frame-independent CDP
  // objectId, so it works inside a switched iframe; headless agrees.)
  if (activeFrameId !== 0 && (action.kind === "hover" || action.kind === "drag")) {
    return {
      type: "Action",
      success: false,
      error: err(
        "InvalidArgument",
        `'${action.kind}' targets the main frame only and an iframe is active. Switch back first: webpilot frame main`,
      ),
    };
  }

  let result;

  // SW-handled action kinds (navigation + upload).
  switch (action.kind) {
    case "navigate": {
      // navigateBoundTab resolves-or-creates the target, settles, resets the
      // frame scope, and re-arms monitors — the same path `capture --url` uses.
      // A typed failure (TabNotFound for a vanished pin) keeps its code; a raw
      // navigation error becomes NavigationFailed.
      try {
        const tabId = await navigateBoundTab(action.url);
        const landed = await chrome.tabs.get(tabId).catch(() => null);
        result = { type: "Action", success: true, url_changed: landed?.url || action.url };
      } catch (e) {
        result = {
          type: "Action",
          success: false,
          error: e?.code
            ? exceptionErr(e)
            : err("NavigationFailed", e.message, { url: action.url, reason: e.message }),
        };
      }
      break;
    }

    case "back":
    case "forward": {
      // History traversal runs in the page (`history.back()`), mirroring the
      // headless transport — `chrome.tabs.goBack` refuses even with history
      // present in headless Chrome (measured). Decided by OUTCOME, not
      // prediction: `navigation.canGoBack/Forward` only sees the contiguous
      // same-origin run of session history (Navigation API spec), so it falsely
      // reports "no history entry" for a cross-origin adjacent entry that
      // `history.back()` traverses to fine (an OAuth/SSO redirect, leaving a
      // search engine for a result), blocking a valid traversal. We issue the
      // traversal and settle on what actually happened — a main-frame commit
      // (onCommitted / onHistoryStateUpdated / onReferenceFragmentUpdated, all
      // recorded on `watch.committed`) — and only a genuine no-op surfaces a
      // typed NavigationFailed. The watch is registered BEFORE the traversal so
      // a commit that fires immediately is never missed.
      const dir = action.kind;
      const before = tab.url || "";
      const watch = watchMainFrameCommit(tab.id);
      await chrome.scripting.executeScript({
        target: { tabId: tab.id, frameIds: [0] },
        world: "MAIN",
        func: (d) => {
          if (d === "back") history.back();
          else history.forward();
        },
        args: [dir],
      });
      if (!(await waitHistoryTraversed(watch, tab.id, before))) {
        watch.dispose();
        return {
          type: "Action",
          success: false,
          error: err("NavigationFailed", `Cannot go ${dir}: no history entry`, {
            url: `history.${dir}()`,
            reason: "no history entry",
          }),
        };
      }
      // It committed — settle the document for a following capture. Best-effort:
      // a slow page that doesn't parse in time is not a failure of a traversal
      // that already happened (headless parity).
      await waitNavigationSettled(tab.id, before, watch, `history.${dir}()`).catch(() => {});
      setActiveFrameId(0);
      await rearmMonitors(tab.id);
      result = { type: "Action", success: true };
      break;
    }

    case "reload": {
      // The URL never moves on a reload, so commitment is the observed
      // main-frame commit — the headless loaderId case. Best-effort settle: a
      // reload always issues, so a slow load is not a failure (headless parity).
      const watch = watchMainFrameCommit(tab.id);
      await chrome.tabs.reload(tab.id);
      await waitNavigationSettled(tab.id, tab.url || "", watch, "reload").catch(() => {});
      setActiveFrameId(0);
      await rearmMonitors(tab.id);
      result = { type: "Action", success: true };
      break;
    }

    case "upload":
      result = await handleUpload(tab.id, action);
      break;

    case "drag":
      result = await handleDrag(tab.id, action);
      break;

    case "hover":
      result = await handleHover(tab.id, action);
      break;

    default:
      result = await dispatchActionToPage(tab, action);
  }

  // Auto-capture DOM after success if requested — exactly like the headless
  // transport: the snapshot describes the tab the agent will act on next (a
  // click-adopted popup included), and a navigating click waits — bounded —
  // for the new document to parse, so the capture is never the dying page. A
  // capture failure must not fail the command: the action's side effect is
  // done, and a retry would run it twice — it is reported as `capture_error`.
  if (command.capture && result?.success) {
    try {
      const t = await resolveActiveTab();
      if (t) {
        // A click-opened popup is often `about:blank` (already complete)
        // before its destination commits — wait past it; a same-tab
        // navigation just waits for the new document to parse.
        if (result.new_tab) await adoptedDocumentReady(t.id, PROBE_MS);
        else if (result.url_changed) await documentReady(t.id, PROBE_MS);
        await ensureBridge(t.id, activeFrameId);
        const dom = await sendToContent(t.id, { type: "extractDom", options: {} }, activeFrameId, 5000);
        // The bridge returns a snapshot (with an `elements` array) on success,
        // or a typed `{success:false, error}` on failure. The latter is truthy
        // — assigning it as `dom` would forward a non-snapshot the CLI can't
        // deserialize, losing the successful action — so discriminate on the
        // snapshot shape and surface a failure as `capture_error`.
        if (dom && Array.isArray(dom.elements)) {
          // Mirror standalone capture: report out-of-scope http iframes, scoped
          // to the active frame, so the agent's subframe logic works the same
          // after an action — including while switched into an iframe, where a
          // main-frame-only gate would drop the count to 0 and hide a nested
          // iframe. Headless `capture_action_snapshot` is likewise unconditional.
          const frames = await chrome.webNavigation.getAllFrames({ tabId: t.id }).catch(() => []);
          dom.subframes = countHttpSubframes(frames, activeFrameId);
          result.dom = dom;
        } else if (dom && dom.error) {
          result.capture_error = dom.error.message || JSON.stringify(dom.error);
        } else {
          result.capture_error = "no DOM snapshot from content script";
        }
      } else {
        // The pin landed on a non-http page (a popup that stayed about:blank, a
        // chrome:// destination) — there is nothing to snapshot. Surface it as a
        // capture_error so `--capture` never silently yields no snapshot and a
        // clean success.
        result.capture_error = noPageErr().message;
      }
    } catch (e) {
      result.capture_error = e?.message || String(e);
    }
  }

  return result;
}


// The DOM `code` and Windows virtual-key code for a key, so CDP
// `Input.dispatchKeyEvent` fires native behaviour (Tab traversal, Backspace
// deletion, arrow nav) a synthetic KeyboardEvent cannot. Mirrors the headless
// `key_descriptor`. Unknown keys carry no code (0).
function keyDescriptor(key) {
  const named = {
    Enter: ["Enter", 13], Tab: ["Tab", 9], Escape: ["Escape", 27],
    Backspace: ["Backspace", 8], Delete: ["Delete", 46],
    ArrowUp: ["ArrowUp", 38], ArrowDown: ["ArrowDown", 40],
    ArrowLeft: ["ArrowLeft", 37], ArrowRight: ["ArrowRight", 39],
    Home: ["Home", 36], End: ["End", 35], PageUp: ["PageUp", 33], PageDown: ["PageDown", 34],
    Insert: ["Insert", 45], CapsLock: ["CapsLock", 20], " ": ["Space", 32], Space: ["Space", 32],
  };
  if (named[key]) return { code: named[key][0], vk: named[key][1] };
  const fn = /^F([1-9]|1[0-2])$/.exec(key);
  if (fn) return { code: key, vk: 111 + Number(fn[1]) };
  // `[...key].length`, not `key.length`: count Unicode code points, so an astral
  // character (emoji) reads as one key, matching the headless `chars().count()`.
  // Plain `.length` is the UTF-16 unit count, where `'😀'.length === 2` would
  // wrongly fall through to the reject path the two modes must agree on.
  if ([...key].length === 1) {
    if (/[a-zA-Z]/.test(key)) return { code: `Key${key.toUpperCase()}`, vk: key.toUpperCase().charCodeAt(0) };
    if (/[0-9]/.test(key)) return { code: `Digit${key}`, vk: key.charCodeAt(0) };
    // Any other single character carries no platform code/vk but types via its
    // `text` (handled by printableKeyText) — `@`, `ñ`, `=`, `😀`.
    return { code: "", vk: 0 };
  }
  // A multi-character string that is neither a named key nor F1–F12 is not a key
  // — `null` signals the caller to reject it instead of dispatching a no-op
  // (e.g. a typo'd "Entr") that would report success while doing nothing.
  return null;
}

// The text a key contributes to its keyDown, or null for a key that produces
// none. `Enter` carries a carriage return — the signal Chromium's implicit
// form submission keys on, without which key-press Enter never submits. Other
// named keys (Tab, arrows) produce no text so they act, not type.
function printableKeyText(key) {
  if (key === "Enter") return "\r";
  if (key === "Space") return " ";
  // Code-point count (so an emoji is one key) and `codePointAt` (so the astral
  // code point, not its lone high surrogate, is range-checked).
  if ([...key].length === 1 && key.codePointAt(0) >= 0x20) return key;
  return null;
}

// Dispatch a key as a native CDP input event through the tab's debugger.
async function dispatchKeyPress(tabId, action) {
  const descriptor = keyDescriptor(action.key);
  if (descriptor === null) {
    // Return the wrapped Action shape (`{success:false, error}`) the caller
    // spreads into the response — a bare `err(...)` would spread to
    // `{type:"Action", code, message}` with no success/error field, which the
    // Rust side can't parse as an Action and mislabels ConnectionLost instead of
    // the InvalidArgument headless returns for the same unknown key.
    return {
      success: false,
      error: err(
        "InvalidArgument",
        `Unknown key: ${JSON.stringify(action.key)} — use a single character, a named key (Enter/Tab/Escape/Backspace/Delete/Arrow*/Home/End/PageUp/PageDown/Space/Insert/CapsLock), or F1–F12`,
      ),
    };
  }
  return withCdp(tabId, async (tid) => {
    const m = action.modifiers || {};
    const modifiers = (m.alt ? 1 : 0) | (m.ctrl ? 2 : 0) | (m.meta ? 4 : 0) | (m.shift ? 8 : 0);
    const { code, vk } = descriptor;
    // A shifted ASCII letter is its uppercase form on every Latin layout, so
    // honor it: Shift+a delivers "A" — both the inserted `text` and the event
    // `key` — not "a" with only the shiftKey flag (which leaves a field
    // lowercase and an `e.key === "A"` listener unmatched). Shifted
    // digits/punctuation are layout-specific (US `1`→`!`, others differ), so
    // those are left unchanged rather than assume a keyboard layout.
    const shiftLetter = (s) => (m.shift && /^[a-zA-Z]$/.test(s) ? s.toUpperCase() : s);
    const rawText = !m.ctrl && !m.alt && !m.meta ? printableKeyText(action.key) : null;
    const text = rawText != null ? shiftLetter(rawText) : null;
    // `nativeVirtualKeyCode` is omitted on purpose: it is platform-native
    // (macOS != Windows), and sending the Windows code on macOS mis-maps the
    // key to an unrelated browser accelerator. `windowsVirtualKeyCode` + key +
    // code is the portable set Chrome resolves from everywhere.
    // The bitmask alone does not PRESS the modifier: Chromium's built-in
    // editing commands (Ctrl+A select-all, Shift+Arrow selection) key off real
    // modifier key events, so a chord is bracketed like a physical keyboard —
    // each held modifier goes down (rawKeyDown, accumulating the mask) before
    // the main key and comes up in reverse after it (headless parity;
    // empirically verified there). Bits mirror the mask: Alt=1 Ctrl=2 Meta=4
    // Shift=8.
    const held = [
      m.ctrl && ["Control", "ControlLeft", 17, 2],
      m.alt && ["Alt", "AltLeft", 18, 1],
      m.shift && ["Shift", "ShiftLeft", 16, 8],
      m.meta && ["Meta", "MetaLeft", 91, 4],
    ].filter(Boolean);
    // A modifier that went down MUST come back up even when a later send
    // throws on a still-live connection (a transient failure): a latched
    // Control would turn every subsequent click into a ctrl-click. `pressed`
    // records what actually went down; the finally releases it in reverse
    // before any error propagates. One failed release still tries the rest
    // (maximal cleanup), and the first release error surfaces when the chord
    // itself succeeded — a stuck key must never be silent. Headless parity.
    const pressed = [];
    let chordErr = null;
    try {
      let acc = 0;
      for (const m of held) {
        acc |= m[3];
        await cdpSend(tid, "Input.dispatchKeyEvent", {
          type: "rawKeyDown", modifiers: acc, key: m[0], code: m[1], windowsVirtualKeyCode: m[2],
        });
        pressed.push(m);
      }
      // The spacebar's canonical DOM `key` is " ", not the "Space" token a caller
      // may spell it as — Chrome rejects "Space" as a `key` value (it lands as an
      // empty e.key), so an `e.key === " "` listener would miss the Space
      // spelling. Normalize to the character it produces (headless parity).
      const keyName = action.key === "Space" ? " " : action.key;
      const base = { modifiers, key: shiftLetter(keyName), code, windowsVirtualKeyCode: vk };
      await cdpSend(tid, "Input.dispatchKeyEvent", text != null
        ? { ...base, type: "keyDown", text }
        : { ...base, type: "keyDown" });
      await cdpSend(tid, "Input.dispatchKeyEvent", { ...base, type: "keyUp" });
    } catch (e) {
      chordErr = e;
    }
    let racc = pressed.reduce((mask, [, , , bit]) => mask | bit, 0);
    let releaseErr = null;
    for (const [mkey, mcode, mvk, bit] of [...pressed].reverse()) {
      racc &= ~bit;
      try {
        await cdpSend(tid, "Input.dispatchKeyEvent", {
          type: "keyUp", modifiers: racc, key: mkey, code: mcode, windowsVirtualKeyCode: mvk,
        });
      } catch (e) {
        releaseErr = releaseErr ?? e;
      }
    }
    // The chord's own error wins; else a release failure must surface rather
    // than report a stuck key as success.
    if (chordErr) throw chordErr;
    if (releaseErr) throw releaseErr;
    // Enter can submit a form, and that navigation is QUEUED — its commit may
    // land after this response, so hint `navigates` for Enter (the only native
    // key that loads a document) so `settledActionUrl` waits the PROBE for it
    // instead of declaring "nothing navigated" and capturing the pre-submit page.
    return { success: true, navigates: action.key === "Enter" };
  });
}

async function dispatchActionToPage(tab, action) {
  // A click-opened tab is caught by its creation events, registered before the
  // action runs — no detection window, no sleep, and reliable even for popups
  // that are still about:blank at creation. Only a tab the ACTED-ON tab opened
  // qualifies — an unrelated tab created during the action (the user, another
  // extension) must not capture the pin. Two correlation signals, first wins:
  // `openerTabId` (the tab relationship) and `onCreatedNavigationTarget`'s
  // `sourceTabId` (the navigation initiator — present even for rel=noopener
  // popups, which deliberately carry no opener).
  let openedTabId = null;
  const onCreated = (t) => {
    if (openedTabId == null && t.openerTabId === tab.id) openedTabId = t.id;
  };
  const onNavTarget = (d) => {
    if (openedTabId == null && d.sourceTabId === tab.id) openedTabId = d.tabId;
  };
  chrome.tabs.onCreated.addListener(onCreated);
  chrome.webNavigation.onCreatedNavigationTarget.addListener(onNavTarget);
  const urlBefore = tab.url;
  // Watch for a same-tab navigation the action itself triggers (a link click,
  // a handler setting `location.href`, an Enter that submits) — registered
  // BEFORE the action, mirroring headless `settled_action_url`. A single
  // post-action `tabs.get` would miss a navigation still in flight and let the
  // auto-capture snapshot the dying page.
  const navWatch = watchMainFrameCommit(tab.id);
  // Snapshot the switched iframe's committed document id before the action: a
  // click that navigates that iframe replaces its document, and a different
  // documentId is how the settle below knows to wait for the new page rather than
  // capture the old one — the active-frame analogue of the main-frame navWatch.
  let activeFrameDocId = null;
  let activeFrames = null;
  if (activeFrameId !== 0) {
    activeFrames = await chrome.webNavigation.getAllFrames({ tabId: tab.id }).catch(() => null);
    activeFrameDocId = activeFrames?.find((f) => f.frameId === activeFrameId)?.documentId ?? null;
  }

  try {
    // `key_press` dispatches a native CDP key event so Tab/Backspace/arrow/
    // text behaviour actually fires (a synthetic KeyboardEvent only notifies
    // JS listeners); it still flows through this popup/navigation wrapper
    // because Enter can submit a form. Every other action runs in the page.
    let r;
    if (action.kind === "key_press") {
      r = await dispatchKeyPress(tab.id, action);
    } else {
      // The switched frame must still exist before a bridge action touches it: a
      // frame that vanished since the capture is FrameNotFound (exit 4 → recapture),
      // not the BridgeUnavailable (exit 3 → infra) a failed inject yields —
      // matching wait/dom/capture and headless `bridge_context_id`. key_press is
      // exempt: it targets browser FOCUS, not the active frame's bridge. Reuse the
      // frame tree already fetched for the documentId above.
      const frameGone = await frameVanishedError(tab.id, activeFrameId, activeFrames);
      if (frameGone) {
        navWatch.dispose();
        return { type: "Action", success: false, error: frameGone };
      }
      await ensureBridge(tab.id, activeFrameId);
      r = await sendToContent(tab.id, { type: "executeAction", action }, activeFrameId);
    }
    const result = { type: "Action", ...r };
    // `navigates` (a new TOP document), `frame_navigates` (the CURRENT frame) and
    // `downloads` (the Navigation API saw a download start) are internal bridge
    // hints, not part of the typed Action response. Browser mode resolves the
    // destination from its own main-frame watch, but a link click QUEUES its
    // navigation, so the events can land after this response — `navigates` tells
    // `settledActionUrl` to wait for that start instead of missing it (e.g. a
    // `target=_top` link clicked inside a switched iframe), while
    // `frame_navigates` drives the iframe-internal settle. `downloads` steers the
    // headless transport's announcement wait and has no browser-mode counterpart,
    // since downloads there are the user's own browser's business, and
    // `opens_context`/`target_name` feed the headless frame-tree lookup that
    // steers it — this side runs no such lookup, and its popup adoption is
    // driven by `tabs.onCreated` rather than by either hint.
    // Read what this side uses, then drop every hint. `downloads` MUST go: the
    // wire response models it as a list of files, so leaking the hint's boolean
    // fails the host reply's typed parse and turns a click that SUCCEEDED into a
    // transport error. The others are unmodelled and would merely ride along
    // ignored — dropped all the same, so the reply carries only its typed shape.
    const navHint = r.navigates === true;
    const frameNavigates = r.frame_navigates === true;
    delete result.navigates;
    delete result.frame_navigates;
    delete result.downloads;
    delete result.opens_context;
    delete result.target_name;

    // Report the settled destination of a same-tab navigation the action
    // triggered; a non-navigating action adds no url_changed and pays no wait.
    // Run this BEFORE reading `openedTabId`: a click-opened popup's
    // `tabs.onCreated` / `onCreatedNavigationTarget` can be dispatched after the
    // content-script response but while both listeners are still registered, and
    // `settledActionUrl`'s `chrome.tabs.get`/`webNavigation` awaits yield the
    // event loop so that event is delivered first. A single pre-settle check
    // would miss it and leave the pin silently on the opener.
    const settledUrl = await settledActionUrl(tab.id, urlBefore, navWatch, navHint);
    if (settledUrl && settledUrl !== urlBefore) {
      result.url_changed = settledUrl;
    }
    // Three distinct outcomes the top URL conflates (mirrors headless do_action):
    //   • the switched iframe VANISHED — a main-frame nav destroyed it (whether or
    //     not the URL changed: a `target=_top` link to the same URL, a reload).
    //     Drop the dead scope and re-arm the new main document.
    //   • no switched frame and the main URL changed — re-arm main.
    //   • a switched iframe is still LIVE — an iframe-only navigation (settle the
    //     active frame below), or a top pushState that left it intact. A
    //     same-document URL change is not a new document, so resetting on the URL
    //     here would wrongly drop a live frame.
    let frameVanished = false;
    if (activeFrameId !== 0) {
      const frames = await chrome.webNavigation.getAllFrames({ tabId: tab.id }).catch(() => null);
      frameVanished = frames !== null && !frames.some((f) => f.frameId === activeFrameId);
    }
    if (frameVanished) {
      setActiveFrameId(0);
    }
    if (frameVanished || (activeFrameId === 0 && settledUrl && settledUrl !== urlBefore)) {
      // Re-arm console/network hooks at settle — headless re-installs them the
      // instant the navigation settles, not at the later `load` event.
      await rearmMonitors(tab.id);
    } else if (activeFrameId !== 0 && frameNavigates) {
      // A click inside the switched iframe that navigated THAT iframe built a new
      // document without touching the top URL — invisible to the main settle. Wait
      // for it before the auto-capture, or the snapshot is the pre-click page.
      await waitActiveFrameSettled(tab.id, activeFrameId, activeFrameDocId);
    }

    if (openedTabId != null) {
      const newTab = await chrome.tabs.get(openedTabId).catch(() => null);
      if (newTab) {
        await chrome.tabs.update(newTab.id, { active: true });
        // The agent's working tab moved — re-pin so every later command
        // follows it, and drop any frame scope (a new tab's tree is its own).
        setActiveTabId(newTab.id);
        setActiveFrameId(0);
        // Read the tab's identity only AFTER it leaves about:blank and commits its
        // destination: a slow or redirecting target=_blank popup reports
        // about:blank / its pre-redirect URL the instant it's created, which would
        // describe the agent's newly pinned tab as a page it is not. (The caller's
        // pre-capture settle then no-ops on the already-ready tab.)
        await adoptedDocumentReady(newTab.id, PROBE_MS);
        // Armed monitors follow the agent's working tab onto the adopted popup
        // (headless follows the pin the same way) — after it settles so the hooks
        // land on the real document.
        await rearmMonitors(newTab.id);
        const settled = (await chrome.tabs.get(newTab.id).catch(() => null)) || newTab;
        result.new_tab = {
          id: String(settled.id),
          url: settled.url || "",
          title: settled.title || "",
          active: true,
        };
      }
    }

    return result;
  } catch (e) {
    return { type: "Action", success: false, error: exceptionErr(e) };
  } finally {
    navWatch.dispose();
    chrome.tabs.onCreated.removeListener(onCreated);
    chrome.webNavigation.onCreatedNavigationTarget.removeListener(onNavTarget);
  }
}

async function handleUpload(tabId, action) {
  try {
    await ensureBridge(tabId, activeFrameId);
    // Stash the EXACT snapshot element in the content script (object identity;
    // a stale index is a typed StaleSnapshot here, a non-file element a typed
    // InvalidArgument), then resolve that stored reference to a CDP objectId in
    // the content script's ISOLATED world and set the file on THAT object. No
    // marker attribute and no document-order re-query, so a page can neither
    // observe nor redirect the target between resolve and sink, and the direct
    // object reaches a file input inside an open shadow root. Parity: action.rs.
    try {
      const prep = await sendToContent(tabId, { type: "prepareUpload", index: action.index }, activeFrameId);
      if (prep && prep.success === false) {
        return { type: "Action", success: false, error: prep.error };
      }

      const outcome = await withCdp(tabId, async (tid) => {
        const uniqueContextId = await frameWorldContextId(tid, tabId, activeFrameId, "ISOLATED");
        if (uniqueContextId == null) {
          return { success: false, error: otherErr("upload: could not reach the content-script context") };
        }
        const ev = await cdpSend(tid, "Runtime.evaluate", {
          // `isConnected` recheck, not a bare null check: a detached node keeps a
          // live objectId, so a target the page removed between prepareUpload and
          // here must resolve to null and become a StaleSnapshot, never a silent
          // file-set on an orphaned input.
          expression: "(()=>{const t=window.__webpilot_state.uploadTarget;return t&&t.isConnected?t:null;})()",
          uniqueContextId,
          returnByValue: false,
        });
        const objectId = ev?.result?.objectId;
        if (!objectId) {
          return { success: false, error: err("StaleSnapshot", `[${action.index}] left the DOM before upload`, { index: action.index }) };
        }
        await cdpSend(tid, "DOM.setFileInputFiles", { objectId, files: [action.path] });
        return { success: true };
      });
      if (outcome.success === false) {
        return { type: "Action", success: false, error: outcome.error };
      }
    } finally {
      // Release the stored reference no matter how the attempt ended — even a
      // failed prepare — so a failed upload never pins a detached node.
      await sendToContent(tabId, { type: "clearUpload" }, activeFrameId, 3000).catch(() => {});
    }

    return { type: "Action", success: true };
  } catch (e) {
    return { type: "Action", success: false, error: exceptionErr(e) };
  }
}

async function handleHover(tabId, action) {
  try {
    await ensureBridge(tabId, activeFrameId);
    // Resolve the element centre through the bridge (it owns the index map),
    // then move the real cursor there over CDP — a synthetic mouseover only
    // fires JS listeners, never the browser's internal :hover state, so this
    // mirrors headless `do_hover` for true CSS-hover fidelity.
    const coords = await sendToContent(
      tabId,
      { type: "getElementCoords", source: action.index, target: action.index },
      activeFrameId,
    );
    if (!coords || coords.error) {
      return { type: "Action", success: false, error: coords?.error || otherErr("hover: no coordinates") };
    }
    await withCdp(tabId, async (tid) => {
      await cdpSend(tid, "Input.dispatchMouseEvent", {
        type: "mouseMoved", x: coords.sx, y: coords.sy,
      });
    });
    return { type: "Action", success: true };
  } catch (e) {
    return { type: "Action", success: false, error: exceptionErr(e) };
  }
}

async function handleDrag(tabId, action) {
  try {
    await ensureBridge(tabId, activeFrameId);
    // The bridge resolves both element centres in one call (it knows the index
    // map); the service worker then drives the pointer over CDP exactly as the
    // headless path does.
    const coords = await sendToContent(
      tabId,
      { type: "getElementCoords", source: action.source, target: action.target },
      activeFrameId,
    );
    if (!coords || coords.error) {
      return { type: "Action", success: false, error: coords?.error || otherErr("drag: no coordinates") };
    }

    const { sx, sy, tx, ty } = coords;
    const steps = Math.max(action.steps || 1, 1);
    // `buttons` (held-button bitmask, 1 = left) must ride every event of the
    // gesture: CDP tracks the drag through it, and a move carrying buttons:0
    // resets that state so the release is treated as releasing an un-pressed
    // button — silently ignored, the page never sees mouseup (headless parity;
    // empirically verified there).
    await withCdp(tabId, async (tid) => {
      await cdpSend(tid, "Input.dispatchMouseEvent", {
        type: "mousePressed", x: sx, y: sy, button: "left", buttons: 1, clickCount: 1,
      });
      await sleep(50);
      for (let i = 1; i <= steps; i++) {
        const r = i / steps;
        await cdpSend(tid, "Input.dispatchMouseEvent", {
          type: "mouseMoved", x: sx + (tx - sx) * r, y: sy + (ty - sy) * r, button: "left", buttons: 1,
        });
        await sleep(20);
      }
      await cdpSend(tid, "Input.dispatchMouseEvent", {
        type: "mouseReleased", x: tx, y: ty, button: "left", buttons: 0, clickCount: 1,
      });
    });

    return { type: "Action", success: true };
  } catch (e) {
    return { type: "Action", success: false, error: exceptionErr(e) };
  }
}

export { handleAction };
