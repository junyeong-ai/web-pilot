// // Page capture: DOM snapshot, screenshots, PDF, accessibility tree.
// // Mirrors transport/local/capture.rs.

import { err, exceptionErr, noPageErr, otherErr, topErr } from "./errors.js";
import { activeFrameId, resolveActiveTab, setActiveTabId, sleep } from "./session.js";
import { cdpSend, withCdp } from "./cdp.js";
import { ensureBridge, sendToContent } from "./content.js";
import { waitNavigationSettled, watchMainFrameCommit } from "./navigation.js";

// ── Capture ────────────────────────────────────────────────────────────────

async function handleCapture(command) {
  const include = new Set(command.include || ["dom"]);
  const opts = command.opts || {};
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

  const result = {
    type: "Capture",
    dom: null,
    screenshot_path: null,
    page_url: "",
    page_title: "",
  };

  // DOM extraction — scoped to the active frame (main by default), exactly
  // like the headless transport. Indices the agent sees resolve against the
  // same frame's bridge snapshot at action time, so every shown index is
  // actionable. Iframe content is reached via `frame switch`, surfaced by
  // the `subframes` hint below.
  if (include.has("dom")) {
    try {
      const frames = await chrome.webNavigation.getAllFrames({ tabId }).catch(() => [{ frameId: 0 }]);

      // A scoped capture whose target frame has since gone is a not-found
      // error, not an empty success — mirror the headless FrameNotFound.
      if (
        activeFrameId !== 0 &&
        !frames.some((f) => f.frameId === activeFrameId && f.url?.startsWith("http"))
      ) {
        const sel = `frame ${activeFrameId}`;
        return topErr(err("FrameNotFound", `Frame not found: ${sel}`, { selector: sel }));
      }

      await ensureBridge(tabId, activeFrameId);
      const dom = await sendToContent(
        tabId,
        { type: "extractDom", options: { bounds: opts.bounds || false, occlusion: opts.occlusion || false } },
        activeFrameId,
        5000,
      );
      // A failed extraction is a typed error, never a fabricated empty page —
      // an agent that reads "0 interactive elements" on a populated page makes
      // catastrophically wrong decisions. Mirrors the headless transport,
      // which propagates the same bridge error.
      if (dom?.success === false && dom.error) return topErr(dom.error);
      if (!dom?.elements) return topErr(otherErr("DOM extraction returned no snapshot"));
      if (activeFrameId === 0) {
        dom.subframes = frames.filter((f) => f.frameId !== 0 && f.url?.startsWith("http")).length;
      }
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
      if (r?.text) {
        result.dom = result.dom || emptyDom();
        result.dom.text_content = r.text;
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
      const ax = await withCdp(tabId, async (tid) => {
        const { nodes } = await cdpSend(tid, "Accessibility.getFullAXTree");
        return nodes;
      });
      result.dom = result.dom || emptyDom();
      result.dom.accessibility_tree = JSON.stringify(ax);
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
        await sleep(300);
      }
    } catch (e) {
      return topErr(exceptionErr(e));
    }
  }

  // Screenshot.
  if (include.has("screenshot")) {
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

  // A capture with no DOM (screenshot/pdf-only) still reports where it ran:
  // the active frame's URL and the tab title (headless fills these via
  // active-frame evals; the frames list serves the same scope here).
  if (!result.page_url) {
    try {
      const tab = await chrome.tabs.get(tabId);
      if (activeFrameId !== 0) {
        const frames = await chrome.webNavigation.getAllFrames({ tabId }).catch(() => []);
        result.page_url = frames.find((f) => f.frameId === activeFrameId)?.url || tab.url || "";
      } else {
        result.page_url = tab.url || "";
      }
      result.page_title = result.page_title || tab.title || "";
    } catch {}
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
