// // Page capture: DOM snapshot, screenshots, PDF, accessibility tree.
// // Mirrors transport/local/capture.rs.

import { err, exceptionErr, noPageErr, otherErr, topErr } from "./errors.js";
import { activeFrameId, annotationPaintMs, resolveActiveTab, setActiveFrameId, setActiveTabId, sleep } from "./session.js";
import { cdpSend, withCdp } from "./cdp.js";
import { resolveFrameWorld } from "./query.js";
import { ensureBridge, sendToContent } from "./content.js";
import { waitNavigationSettled, watchMainFrameCommit } from "./navigation.js";
import { rearmMonitors } from "./state.js";

// ── Capture ────────────────────────────────────────────────────────────────

// HTTP iframes nested inside the active frame but not shown in its capture — the
// "N iframe(s) not shown" count. Scoped to the active frame from the flat
// `getAllFrames` list via `parentFrameId`: from the main frame (0) it counts
// every HTTP iframe in the tab; from a switched frame it counts that frame's own
// HTTP descendants, so a nested iframe inside a switched frame stays discoverable
// (the headless `count_http_subframes` does the same on the CDP frame tree).
function countHttpSubframes(frames, rootFrameId) {
  const childrenOf = new Map();
  for (const f of frames) {
    if (!childrenOf.has(f.parentFrameId)) childrenOf.set(f.parentFrameId, []);
    childrenOf.get(f.parentFrameId).push(f);
  }
  let count = 0;
  const stack = [...(childrenOf.get(rootFrameId) || [])];
  while (stack.length) {
    const f = stack.pop();
    if (f.url?.startsWith("http")) count += 1;
    const kids = childrenOf.get(f.frameId);
    if (kids) stack.push(...kids);
  }
  return count;
}

async function handleCapture(command) {
  const include = new Set(command.include || ["dom"]);
  const opts = command.opts || {};
  // Annotations are drawn at viewport coordinates, so they cannot combine with a
  // full-page shot — reject the pair, matching headless `CaptureOpts::validate`
  // (the same wording) instead of silently producing a misaligned capture.
  if (opts.annotate && opts.full_page) {
    return topErr(
      err(
        "InvalidArgument",
        "`annotate` and `full_page` cannot be combined; annotations are viewport-only",
      ),
    );
  }
  let tabId;

  try {
    if (command.url) {
      // Navigate the pinned tab (or pin a fresh one) — never whatever tab the
      // user happens to be looking at.
      const existing = await resolveActiveTab();
      let beforeUrl = "";
      let watch;
      if (existing) {
        tabId = existing.id;
        beforeUrl = existing.url || "";
        watch = watchMainFrameCommit(tabId);
        await chrome.tabs.update(tabId, { url: command.url, active: true });
      } else {
        const t = await chrome.tabs.create({ url: command.url, active: true });
        tabId = t.id;
        setActiveTabId(tabId);
        watch = watchMainFrameCommit(tabId);
      }
      await waitNavigationSettled(tabId, beforeUrl, watch, command.url);
      // A fresh document has a new frame tree — drop any stale frame scope, so
      // a capture after `frame switch` + `--url` is main-frame-scoped (matches
      // headless `navigate_reconnect`, and keeps the `--annotate` main-frame
      // guard from firing on a frame id that no longer exists).
      setActiveFrameId(0);
      // Re-arm monitors at settle (headless parity) so a fetch/console the new
      // page fires before `load` is captured, not lost to the `onCompleted` gap.
      await rearmMonitors(tabId);
    } else {
      const t = await resolveActiveTab();
      if (!t) return topErr(noPageErr());
      tabId = t.id;
    }
  } catch (e) {
    // A typed failure (e.g. TabNotFound for a vanished pin) keeps its code;
    // only a raw navigation error becomes NavigationFailed.
    if (e?.code) return topErr(exceptionErr(e));
    return topErr(err("NavigationFailed", e.message, { url: command.url || "", reason: e.message }));
  }

  // Annotation overlays use page-viewport coordinates, so they only line up on
  // the main frame. Refuse `--annotate` while an iframe is active rather than
  // returning an unannotated screenshot with no signal — headless parity
  // (`require_main_frame`); both fail loud, identically.
  if (opts.annotate && activeFrameId !== 0) {
    return topErr(err("InvalidArgument", "'capture --annotate' targets the main frame only and an iframe is active. Switch back first: webpilot frame switch main"));
  }

  const result = {
    type: "Capture",
    dom: null,
    screenshot_path: null,
    page_url: "",
    page_title: "",
  };

  // The frame tree, fetched once — validates the active capture frame here and
  // counts out-of-scope HTTP subframes for the snapshot below. A scoped capture
  // in ANY mode whose target frame has since been removed is a FrameNotFound, not
  // a stale-context success: every pass (DOM, screenshot, PDF, AX) checks the
  // scope through this one tree.
  const frames = await chrome.webNavigation.getAllFrames({ tabId }).catch(() => []);
  if (
    activeFrameId !== 0 &&
    !frames.some((f) => f.frameId === activeFrameId && f.url?.startsWith("http"))
  ) {
    const sel = `frame ${activeFrameId}`;
    return topErr(err("FrameNotFound", `Frame not found: ${sel}`, { selector: sel }));
  }

  // DOM extraction — scoped to the active frame (main by default), exactly
  // like the headless transport. Indices the agent sees resolve against the
  // same frame's bridge snapshot at action time, so every shown index is
  // actionable. Iframe content is reached via `frame switch`, surfaced by
  // the `subframes` hint below. `--annotate` forces a DOM pass even when the
  // caller didn't ask for one: the overlay boxes are positioned from the
  // snapshot's element bounds, so without it there is nothing to draw — matching
  // headless `want_dom = want(Dom) || opts.annotate`.
  if (include.has("dom") || opts.annotate) {
    try {
      await ensureBridge(tabId, activeFrameId);
      const dom = await sendToContent(
        tabId,
        { type: "extractDom", options: { bounds: opts.bounds || opts.annotate, occlusion: opts.occlusion || false } },
        activeFrameId,
        5000,
      );
      // A failed extraction is a typed error, never a fabricated empty page —
      // an agent that reads "0 interactive elements" on a populated page makes
      // catastrophically wrong decisions. Mirrors the headless transport,
      // which propagates the same bridge error.
      if (dom?.success === false && dom.error) return topErr(dom.error);
      if (!dom?.elements) return topErr(otherErr("DOM extraction returned no snapshot"));
      dom.subframes = countHttpSubframes(frames, activeFrameId);
      result.dom = dom;
      result.page_url = dom.page_url || "";
      result.page_title = dom.page_title || "";
    } catch (e) {
      return topErr(exceptionErr(e));
    }
  }

  // Text extraction. A bridge failure is fatal (headless parity); a response
  // without text is merely a page with none.
  if (include.has("text")) {
    try {
      await ensureBridge(tabId, activeFrameId);
      const r = await sendToContent(tabId, { type: "extractText" }, activeFrameId, 5000);
      // `typeof === "string"`, not truthiness: a page with NO text yields `""`,
      // which headless still preserves (`text_content: Some("")` + a snapshot
      // shell). A truthiness check would drop the result entirely, returning no
      // DOM at all for a text capture of a text-empty page.
      if (typeof r?.text === "string") {
        result.dom = result.dom || emptyDom();
        result.dom.text_content = r.text;
        result.dom.text_truncated = r.truncated === true;
        result.page_url = r.url || result.page_url;
        result.page_title = r.title || result.page_title;
      }
    } catch (e) {
      return topErr(exceptionErr(e));
    }
  }

  // Accessibility tree (CDP). Fatal on failure (headless parity).
  if (include.has("accessibility")) {
    try {
      // Stringify the WHOLE CDP response (`{ nodes: [...] }`), pretty-printed,
      // not just the inner `nodes` array — headless serializes the full response
      // with `to_string_pretty`, so an agent parsing the tree must see the same
      // wrapper object and shape in both modes.
      const ax = await withCdp(tabId, async (tid) => {
        // Scope the AX tree to the active frame, like the DOM/screenshot/metadata
        // do: an unscoped getFullAXTree returns the ROOT document's tree while the
        // footer/URL report the iframe. Resolve the active frame's CDP frameId via
        // the same nonce path eval uses (unambiguous for same-URL siblings).
        let params;
        if (activeFrameId !== 0) {
          const resolved = await resolveFrameWorld(tid, tabId, activeFrameId, "MAIN");
          // A cross-origin OOPIF has no context in this tab's session, so its CDP
          // frameId can't be resolved. Fail like eval/find do — an unscoped tree
          // here would be the ROOT under an iframe-scoped envelope: coherent but
          // factually wrong. `null` is the sentinel the caller maps to that error.
          if (!resolved?.frameId) return null;
          params = { frameId: resolved.frameId };
        }
        return cdpSend(tid, "Accessibility.getFullAXTree", params);
      });
      if (ax === null) {
        return topErr(
          err("FrameNotFound", `frame ${activeFrameId} has no reachable execution context`, {
            frame_id: String(activeFrameId),
          }),
        );
      }
      result.dom = result.dom || emptyDom();
      result.dom.accessibility_tree = JSON.stringify(ax, null, 2);
    } catch (e) {
      return topErr(exceptionErr(e));
    }
  }

  // Annotated overlay before screenshot. Overlay coordinates are page-viewport
  // relative, so they only line up when capture is scoped to the main frame.
  // Fatal on failure (headless parity); the overlay is stripped right after
  // the shot below, so an error past this point cannot leave it in the page.
  if (opts.annotate && activeFrameId === 0 && result.dom?.elements) {
    try {
      const annotations = result.dom.elements
        .filter((el) => el.in_viewport && el.bounds && el.bounds.w > 0 && el.bounds.h > 0)
        .map((el) => ({ index: el.index, x: el.bounds.x, y: el.bounds.y, w: el.bounds.w, h: el.bounds.h }));
      if (annotations.length > 0) {
        await ensureBridge(tabId, 0);
        await sendToContent(tabId, { type: "addAnnotations", elements: annotations }, 0);
        await sleep(annotationPaintMs());
      }
    } catch (e) {
      return topErr(exceptionErr(e));
    }
  }

  // Screenshot. `--annotate` forces one even without `--include screenshot`: the
  // overlay boxes were just drawn onto the page and the shot is the only way the
  // agent receives them — matching headless `want_screenshot = want(Screenshot)
  // || opts.annotate`. Without this, `--browser capture --annotate` drew boxes
  // and returned no image.
  if (include.has("screenshot") || opts.annotate) {
    try {
      // CDP captures the target's own surface, so a screenshot never depends
      // on the tab being the active tab of a foreground window. That is what
      // lets a backgrounded workbench tab be captured while the user looks at
      // another window or app — `chrome.tabs.captureVisibleTab` would grab
      // whatever is visible (or fail), and on macOS raising the window can't
      // be forced anyway. It also needs no `<all_urls>` host grant, only the
      // debugger this path already holds. Viewport and full-page differ only
      // by `captureBeyondViewport`; headless takes the identical two shots.
      result.screenshot_b64 = await withCdp(tabId, async (tid) => {
        const params = { format: "png" };
        if (opts.full_page) params.captureBeyondViewport = true;
        const r = await cdpSend(tid, "Page.captureScreenshot", params);
        return r.data;
      });
    } catch (e) {
      // Screenshot failure degrades explicitly (headless parity): the DOM is
      // still useful, and the error rides along in `screenshot_error`.
      result.screenshot_error = e.message;
    }
  }

  // Strip the overlay as soon as the shot is taken — before anything that can
  // fail below — so a capture error never leaves annotations in the live page
  // for the next command (headless parity).
  if (opts.annotate) {
    try {
      await sendToContent(tabId, { type: "removeAnnotations" }, 0, 3000);
    } catch {}
  }

  // PDF. Fatal on failure (headless parity). The base64 bytes ride the wire;
  // the CLI is the single writer and persists them to a file.
  if (include.has("pdf")) {
    try {
      result.pdf_b64 = await withCdp(tabId, async (tid) => {
        const r = await cdpSend(tid, "Page.printToPDF", {
          landscape: false, printBackground: true, preferCSSPageSize: true,
        });
        return r.data;
      });
    } catch (e) {
      return topErr(exceptionErr(e));
    }
  }

  // A capture with no DOM pass (screenshot/pdf/AX-only) still reports where it
  // ran — from the ACTIVE FRAME, not the tab, so a frame-scoped capture shows the
  // frame's own URL and title. Headless derives both via `eval_in_active`; a bare
  // `tab.title` would mislabel an iframe capture with the top page's title.
  if (!result.page_url || !result.page_title) {
    try {
      if (activeFrameId !== 0) {
        const [hit] = await chrome.scripting.executeScript({
          target: { tabId, frameIds: [activeFrameId] },
          func: () => [location.href, document.title],
        });
        const [u, t] = hit?.result || ["", ""];
        result.page_url = result.page_url || u || "";
        result.page_title = result.page_title || t || "";
      } else {
        const tab = await chrome.tabs.get(tabId);
        result.page_url = result.page_url || tab.url || "";
        result.page_title = result.page_title || tab.title || "";
      }
    } catch {}
  }

  // Surface the nested HTTP iframe count on a text/AX-only snapshot shell too —
  // scoped to the active frame, exactly as headless sets `s.subframes` on its
  // shell. Without it a text/AX capture drops the "N iframe(s) not shown" hint
  // the agent needs. A real DOM pass already set it, so the `undefined` guard
  // skips that case.
  if (result.dom && result.dom.subframes === undefined) {
    result.dom.subframes = countHttpSubframes(frames, activeFrameId);
  }

  // A text/AX-only snapshot SHELL (emptyDom) carries blank page_url/title;
  // headless builds its shell with the resolved URL/title (`empty_snapshot(&
  // page_url, &page_title)`), so the snapshot itself — not only the top-level
  // response — reports where it ran. Mirror the resolved metadata onto the shell.
  // A real DOM pass already populated these from the snapshot, so the `||` keeps
  // them.
  if (result.dom) {
    result.dom.page_url = result.dom.page_url || result.page_url || "";
    result.dom.page_title = result.dom.page_title || result.page_title || "";
  }

  return result;
}

function emptyDom() {
  return {
    elements: [], total_nodes: 0, page_url: "", page_title: "",
    scroll: { scroll_x: 0, scroll_y: 0, scroll_width: 0, scroll_height: 0, viewport_width: 0, viewport_height: 0 },
    scroll_percent: 0, extraction_ms: 0,
  };
}

export { handleCapture };
