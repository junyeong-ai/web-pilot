/**
 * WebPilot Service Worker.
 *
 * Bridges Native Messaging Host commands to content scripts and CDP.
 * Wire format matches the Rust `protocol::Command` / `protocol::ResponseData`
 * enums exactly — there is no shape translation layer.
 */
console.log("[WebPilot] Service Worker loaded");

const NM_HOST = "com.webpilot.host";
const KEEPALIVE_INTERVAL = 25000;
const CDP_VERSION = "1.3";

let nmPort = null;
let keepaliveTimer = null;
let connectionRetries = 0;
let activeFrameId = 0;
const monitoringState = { console: new Set(), network: new Set() };

// An MV3 service worker is killed when idle and restarted on the next event,
// losing in-memory state. `activeFrameId` and the monitoring sets are persisted
// to session storage and reloaded here. `RESTORED` resolves once both are back;
// `processCommand` awaits it so a command can never run against un-restored
// state — otherwise the first command after a restart would silently target the
// main frame instead of the iframe the agent had switched to.
const RESTORED = (async () => {
  try {
    const data = await chrome.storage.session?.get(["activeFrameId", "monitoringTabs"]);
    if (data?.activeFrameId != null) activeFrameId = data.activeFrameId;
    if (data?.monitoringTabs) {
      (data.monitoringTabs.console || []).forEach((id) => monitoringState.console.add(id));
      (data.monitoringTabs.network || []).forEach((id) => monitoringState.network.add(id));
    }
  } catch {}
})();

function setActiveFrameId(id) {
  activeFrameId = id;
  chrome.storage.session?.set({ activeFrameId: id });
}

function saveMonitoringState() {
  chrome.storage.session?.set({
    monitoringTabs: {
      console: [...monitoringState.console],
      network: [...monitoringState.network],
    },
  });
}

// ── Error helpers ──────────────────────────────────────────────────────────

function err(code, message, data) {
  return { code, message, ...(data || {}) };
}
const otherErr = (msg) => err("Other", msg);
// Preserve a thrown error's typed `code` (e.g. BridgeUnavailable → exit 3)
// instead of collapsing every exception to Other (exit 1).
const exceptionErr = (e) => (e?.code ? err(e.code, e.message || String(e)) : otherErr(e?.message || String(e)));
const timeoutErr = (kind, elapsed_ms) => err("Timeout", `${kind} timed out`, { kind, elapsed_ms });
const noPageErr = () => err("NoPage", "No web page open");

function topErr(error) {
  return { type: "Error", error };
}

// ── CDP helpers ────────────────────────────────────────────────────────────

const cdpLocks = new Map();

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

async function cdpSend(tabId, method, params = {}) {
  return chrome.debugger.sendCommand({ tabId }, method, params);
}

// ── Console / network monitoring injection ─────────────────────────────────

async function injectConsoleMonitoring(tabId) {
  await chrome.scripting.executeScript({
    target: { tabId, frameIds: [0] },
    world: "MAIN",
    func: () => {
      if (window.__webpilot_console) return;
      window.__webpilot_console = [];
      const orig = { log: console.log, error: console.error, warn: console.warn, info: console.info };
      ["log", "error", "warn", "info"].forEach((m) => {
        console[m] = (...args) => {
          window.__webpilot_console.push({
            level: m,
            message: args.map((a) => { try { return String(a); } catch { return "[object]"; } }).join(" "),
            timestamp: Date.now(),
          });
          if (window.__webpilot_console.length > 500) window.__webpilot_console.shift();
          orig[m].apply(console, args);
        };
      });
    },
  });
}

async function injectNetworkMonitoring(tabId) {
  await chrome.scripting.executeScript({
    target: { tabId, frameIds: [0] },
    world: "MAIN",
    func: () => {
      if (window.__webpilot_network_active) return;
      window.__webpilot_network_active = true;
      window.__webpilot_network = [];
      const origFetch = window.fetch;
      window.fetch = function (...args) {
        const [resource, config] = args;
        const t0 = performance.now();
        return origFetch.apply(this, args).then((response) => {
          window.__webpilot_network.push({
            type: "fetch", url: String(resource), method: config?.method || "GET",
            status: response.status, duration_ms: Math.round(performance.now() - t0),
            timestamp: Date.now(),
          });
          if (window.__webpilot_network.length > 500) window.__webpilot_network.shift();
          return response;
        }).catch((err) => {
          window.__webpilot_network.push({
            type: "fetch", url: String(resource), method: config?.method || "GET",
            error: err.message, duration_ms: Math.round(performance.now() - t0),
            timestamp: Date.now(),
          });
          throw err;
        });
      };
      // Patch the prototype in place: keeps XMLHttpRequest's static constants
      // (UNSENT…DONE) intact, and a once-listener per send records each request
      // exactly once even when an instance is reused.
      const xhrProto = XMLHttpRequest.prototype;
      const origOpen = xhrProto.open;
      const origSend = xhrProto.send;
      const xhrMeta = new WeakMap();
      xhrProto.open = function (m, u, ...a) {
        xhrMeta.set(this, { method: m, url: String(u) });
        return origOpen.apply(this, [m, u, ...a]);
      };
      xhrProto.send = function (...a) {
        const t0 = performance.now();
        this.addEventListener("loadend", () => {
          const meta = xhrMeta.get(this) || {};
          window.__webpilot_network.push({
            type: "xhr", url: meta.url || "", method: meta.method || "GET",
            status: this.status || undefined,
            error: this.status === 0 ? "Network error" : undefined,
            duration_ms: Math.round(performance.now() - t0),
            timestamp: Date.now(),
          });
          if (window.__webpilot_network.length > 500) window.__webpilot_network.shift();
        }, { once: true });
        return origSend.apply(this, a);
      };
    },
  });
}

// ── NM connection ──────────────────────────────────────────────────────────

function connectToHost() {
  if (nmPort) return;
  try {
    nmPort = chrome.runtime.connectNative(NM_HOST);
    console.log("[WebPilot] Connected to native host");
    connectionRetries = 0;

    // Bind each connection's messages to that exact port, so a reply always
    // goes back to the host process that sent the request — never to a
    // reconnected host whose fresh id space could match the id to a different
    // pending request.
    const port = nmPort;
    port.onMessage.addListener((request) => handleHostMessage(request, port));
    nmPort.onDisconnect.addListener(() => {
      const error = chrome.runtime.lastError?.message || "unknown";
      console.log("[WebPilot] Native host disconnected:", error);
      nmPort = null;
      clearInterval(keepaliveTimer);

      connectionRetries++;
      const delay = Math.min(2000 * connectionRetries, 30000);
      setTimeout(connectToHost, delay);
    });

    // Every Ping carries our manifest version (connect-time hello + keepalive),
    // so the host can detect a stale install and reject CLI commands loudly.
    const ping = () =>
      nmPort?.postMessage({
        id: 0,
        command: { type: "Ping", extension_version: chrome.runtime.getManifest().version },
      });
    ping();
    clearInterval(keepaliveTimer);
    keepaliveTimer = setInterval(ping, KEEPALIVE_INTERVAL);
  } catch (e) {
    console.error("[WebPilot] Failed to connect:", e);
    connectionRetries++;
    setTimeout(connectToHost, Math.min(5000 * connectionRetries, 30000));
  }
}

function handleHostMessage(request, port) {
  const { id, command } = request;
  if (!command) return;
  processCommandWithKeepAlive(id, command, port);
}

async function processCommandWithKeepAlive(id, command, port) {
  // Reset 30s idle timer while command is in flight.
  const keepAlive = setInterval(() => {
    chrome.runtime.getPlatformInfo(() => {});
  }, 20000);
  try {
    await processCommand(id, command, port);
  } finally {
    clearInterval(keepAlive);
  }
}

// ── Command dispatch ───────────────────────────────────────────────────────

async function processCommand(id, command, port) {
  // Never act on un-restored state after a service-worker restart.
  await RESTORED;
  // Reply to the port that delivered this request, not the mutable global
  // `nmPort`. A disconnected port throws — drop the reply rather than risk
  // landing it on a reconnected host (the originating CLI already failed on its
  // dead socket).
  const send = (result) => {
    try {
      port.postMessage({ id, result });
    } catch {}
  };
  try {
    let result;
    switch (command.type) {
      case "Capture":
        result = await handleCapture(command);
        break;

      case "Action":
        result = await handleAction(command);
        break;

      case "Status":
        result = await handleStatus();
        break;

      case "TabList":
        result = await handleTabList();
        break;

      case "TabSwitch":
        result = await handleTabSwitch(command.tab_id);
        break;

      case "TabNew":
        await chrome.tabs.create({ url: command.url, active: true });
        result = { type: "Action", success: true };
        break;

      case "TabClose":
        try {
          await chrome.tabs.remove(parseInt(command.tab_id, 10));
          result = { type: "Action", success: true };
        } catch (e) {
          result = { type: "Action", success: false, error: exceptionErr(e) };
        }
        break;

      case "Eval":
        result = await handleEval(command);
        break;

      case "Wait":
        result = await handleWait(command);
        break;

      case "DomSet":
        result = await handleDomSet(command);
        break;

      case "DomGet":
        result = await handleDomGet(command);
        break;

      case "Fetch":
        result = await handleFetch(command);
        break;

      case "FrameList":
        result = await handleFrameList();
        break;

      case "FrameSwitch":
        result = await handleFrameSwitch(command.selector);
        break;

      case "CookieList":
        result = await handleCookieList(command.url);
        break;

      case "CookieSet":
        result = await handleCookieSet(command);
        break;

      case "CookieDelete":
        result = await handleCookieDelete(command);
        break;

      case "ConsoleStart":
        result = await handleConsoleStart();
        break;

      case "ConsoleRead":
        result = await handleConsoleRead();
        break;

      case "ConsoleClear":
        result = await handleConsoleClear();
        break;

      case "NetworkStart":
        result = await handleNetworkStart();
        break;

      case "NetworkRead":
        result = await handleNetworkRead(command.since);
        break;

      case "NetworkClear":
        result = await handleNetworkClear();
        break;

      case "SessionExport":
        result = await handleSessionExport();
        break;

      case "SessionImport":
        result = await handleSessionImport(command.data);
        break;

      case "Ping":
        result = { type: "Pong" };
        break;

      default:
        result = topErr(otherErr(`Unknown command: ${command.type}`));
    }

    send(result);
  } catch (e) {
    send(topErr(exceptionErr(e)));
  }
}

// ── Capture ────────────────────────────────────────────────────────────────

async function handleCapture(command) {
  const include = new Set(command.include || ["dom"]);
  const opts = command.opts || {};
  let tabId;

  try {
    if (command.url) {
      const existing = await findHttpTab();
      if (existing) {
        tabId = existing.id;
        await chrome.tabs.update(tabId, { url: command.url, active: true });
      } else {
        const t = await chrome.tabs.create({ url: command.url, active: true });
        tabId = t.id;
      }
      await waitForTabReady(tabId, 20000);
      await sleep(500);
    } else {
      const t = await findHttpTab();
      if (!t) return topErr(noPageErr());
      tabId = t.id;
    }
  } catch (e) {
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
      if (dom?.elements) {
        if (activeFrameId === 0) {
          dom.subframes = frames.filter((f) => f.frameId !== 0 && f.url?.startsWith("http")).length;
        }
        result.dom = dom;
        result.page_url = dom.page_url || "";
        result.page_title = dom.page_title || "";
      }
    } catch (e) {
      console.error("[WebPilot] DOM error:", e.message);
    }
  }

  // Text extraction.
  if (include.has("text")) {
    try {
      await ensureBridge(tabId, activeFrameId);
      const r = await sendToContent(tabId, { type: "extractText" }, activeFrameId, 5000);
      if (r?.text) {
        result.dom = result.dom || emptyDom();
        result.dom.text_content = r.text.slice(0, 50000);
        result.page_url = r.url || result.page_url;
        result.page_title = r.title || result.page_title;
      }
    } catch (e) {
      console.error("[WebPilot] Text error:", e.message);
    }
  }

  // Accessibility tree (CDP).
  if (include.has("accessibility")) {
    try {
      const ax = await withCdp(tabId, async (tid) => {
        const { nodes } = await cdpSend(tid, "Accessibility.getFullAXTree");
        return nodes;
      });
      result.dom = result.dom || emptyDom();
      result.dom.accessibility_tree = JSON.stringify(ax);
    } catch (e) {
      console.error("[WebPilot] AX error:", e.message);
    }
  }

  // Annotated overlay before screenshot. Overlay coordinates are page-viewport
  // relative, so they only line up when capture is scoped to the main frame.
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
      console.error("[WebPilot] Annotate error:", e.message);
    }
  }

  // Screenshot.
  if (include.has("screenshot")) {
    try {
      const tabInfo = await chrome.tabs.get(tabId);
      await chrome.tabs.update(tabId, { active: true });
      await chrome.windows.update(tabInfo.windowId, { focused: true });
      await sleep(200);

      if (opts.full_page) {
        await captureFullPage(tabId, tabInfo.windowId, result);
      } else {
        result.screenshot_b64 = await captureWithRetry(tabInfo.windowId, 80);
      }
    } catch (e) {
      console.error("[WebPilot] Screenshot failed:", e.message);
      result.screenshot_error = e.message;
    }
  }

  // PDF.
  if (include.has("pdf")) {
    try {
      const pdf = await withCdp(tabId, async (tid) => {
        const r = await cdpSend(tid, "Page.printToPDF", {
          landscape: false, printBackground: true, preferCSSPageSize: true,
        });
        return r.data;
      });
      // Forward the base64 PDF back; host would save to a file. For simplicity
      // here we drop the bytes — full PDF support in browser mode would require
      // host-side persistence.
      result.pdf_b64 = pdf;
    } catch (e) {
      console.error("[WebPilot] PDF failed:", e.message);
    }
  }

  if (opts.annotate) {
    try {
      await sendToContent(tabId, { type: "removeAnnotations" }, 0, 3000);
    } catch {}
  }

  return result;
}

async function captureFullPage(tabId, windowId, result) {
  await ensureBridge(tabId, 0);
  const dims = await sendToContent(tabId, { type: "getPageDims" }, 0, 5000).catch(() => null);
  if (!dims) return;

  const scrollHeight = dims.scrollHeight || 0;
  const viewportHeight = dims.viewportHeight || 0;
  const origSY = dims.scrollY || 0;
  if (scrollHeight <= 0 || viewportHeight <= 0) return;

  const tileCount = Math.min(Math.ceil(scrollHeight / viewportHeight), 20);
  const tiles = [];
  const captureDelay = 750;

  await sendToContent(tabId, { type: "scrollTo", x: 0, y: 0 }, 0, 3000).catch(() => {});
  await sleep(300);

  for (let i = 0; i < tileCount; i++) {
    if (i > 0) {
      await sendToContent(tabId, { type: "scrollTo", x: 0, y: i * viewportHeight }, 0, 3000).catch(() => {});
    }
    await sleep(captureDelay);
    try {
      tiles.push(await captureWithRetry(windowId, 60));
    } catch (e) {
      console.error("[WebPilot] Tile", i + 1, "failed:", e.message);
    }
  }

  await sendToContent(tabId, { type: "scrollTo", x: 0, y: origSY }, 0, 3000).catch(() => {});
  result.screenshot_tiles = tiles;
  result.tile_viewport_height = viewportHeight;
  result.tile_total_height = scrollHeight;
}

function emptyDom() {
  return {
    elements: [], total_nodes: 0, page_url: "", page_title: "",
    scroll: { scroll_x: 0, scroll_y: 0, scroll_width: 0, scroll_height: 0, viewport_width: 0, viewport_height: 0 },
    scroll_percent: 0, extraction_ms: 0,
  };
}

// ── Action ─────────────────────────────────────────────────────────────────

async function handleAction(command) {
  const { action } = command;

  // Policy is enforced CLI-side at the transport boundary before the command
  // is sent, so the service worker never re-checks it.

  // Inject dialog override before any action runs in the page.
  const tab = await findHttpTab();
  if (!tab) {
    return { type: "Action", success: false, error: noPageErr() };
  }
  try {
    await chrome.scripting.executeScript({
      target: { tabId: tab.id, frameIds: [0] },
      world: "MAIN",
      func: () => {
        if (!window.__webpilot_dialogs) {
          window.__webpilot_dialogs = [];
          window.alert = (msg) => { window.__webpilot_dialogs.push({ type: "alert", message: String(msg) }); };
          window.confirm = (msg) => { window.__webpilot_dialogs.push({ type: "confirm", message: String(msg) }); return true; };
          window.prompt = (msg, def) => { window.__webpilot_dialogs.push({ type: "prompt", message: String(msg) }); return def || ""; };
        }
      },
    });
  } catch {}

  // Viewport-coordinate / main-document actions cannot target an iframe: their
  // CDP path uses page-level coordinates or a main-document node lookup. Reject
  // inside a switched frame, matching the headless `require_main_frame` guard,
  // so behaviour is identical across modes.
  if (activeFrameId !== 0 && (action.kind === "hover" || action.kind === "drag" || action.kind === "upload")) {
    return {
      type: "Action",
      success: false,
      error: err(
        "InvalidArgument",
        `'${action.kind}' targets the main frame only and an iframe is active. Switch back first: webpilot frame switch main`,
      ),
    };
  }

  let result;

  // SW-handled action kinds (navigation + upload).
  switch (action.kind) {
    case "navigate": {
      await chrome.tabs.update(tab.id, { url: action.url, active: true });
      await waitForTabReady(tab.id, 15000);
      await sleep(500);
      // A new document invalidates any switched-to frame — reset to main, as
      // the headless transport does on navigation.
      setActiveFrameId(0);
      const landed = await chrome.tabs.get(tab.id).catch(() => null);
      result = { type: "Action", success: true, url_changed: landed?.url || action.url };
      break;
    }

    case "back":
      await chrome.tabs.goBack(tab.id);
      await sleep(500);
      setActiveFrameId(0);
      result = { type: "Action", success: true };
      break;

    case "forward":
      await chrome.tabs.goForward(tab.id);
      await sleep(500);
      setActiveFrameId(0);
      result = { type: "Action", success: true };
      break;

    case "reload":
      await chrome.tabs.reload(tab.id);
      await waitForTabReady(tab.id, 15000);
      setActiveFrameId(0);
      result = { type: "Action", success: true };
      break;

    case "upload":
      result = await handleUpload(tab.id, action);
      break;

    case "drag":
      result = await handleDrag(tab.id, action);
      break;

    default:
      result = await dispatchActionToPage(tab, action);
  }

  // Auto-capture DOM after success if requested.
  if (command.capture && result?.success) {
    await sleep(500);
    try {
      const t = await findHttpTab();
      if (t) {
        await ensureBridge(t.id, activeFrameId);
        const dom = await sendToContent(t.id, { type: "extractDom", options: {} }, activeFrameId, 5000);
        if (dom) {
          // Mirror standalone capture: report out-of-scope http iframes so the
          // agent's subframe logic works the same after an action.
          if (activeFrameId === 0 && dom.elements) {
            const frames = await chrome.webNavigation.getAllFrames({ tabId: t.id }).catch(() => []);
            dom.subframes = frames.filter((f) => f.frameId !== 0 && f.url?.startsWith("http")).length;
          }
          result.dom = dom;
        }
      }
    } catch {}
  }

  return result;
}

async function dispatchActionToPage(tab, action) {
  const tabsBefore = new Set((await chrome.tabs.query({})).map((t) => t.id));
  const urlBefore = tab.url;

  try {
    await ensureBridge(tab.id, activeFrameId);
    const r = await sendToContent(tab.id, { type: "executeAction", action }, activeFrameId);
    const result = { type: "Action", ...r };

    await sleep(300);
    const tabsAfter = await chrome.tabs.query({});
    const newTabs = tabsAfter.filter((t) => !tabsBefore.has(t.id) && t.url?.startsWith("http"));
    if (newTabs.length > 0) {
      const newTab = newTabs[0];
      await chrome.tabs.update(newTab.id, { active: true });
      // Focus moved to a freshly opened tab — its frame tree is its own.
      setActiveFrameId(0);
      result.new_tab = {
        id: String(newTab.id),
        url: newTab.url || "",
        title: newTab.title || "",
        active: true,
      };
    }

    try {
      const current = await chrome.tabs.get(tab.id);
      if (current.url && current.url !== urlBefore) result.url_changed = current.url;
    } catch {}

    return result;
  } catch (e) {
    return { type: "Action", success: false, error: exceptionErr(e) };
  }
}

async function handleUpload(tabId, action) {
  try {
    await ensureBridge(tabId, activeFrameId);
    // Tag the chosen element by index — this resolves through the bridge
    // snapshot, so a stale index surfaces a typed `StaleSnapshot` here (exit 4
    // parity with headless) instead of a generic "not found via CDP" later.
    const tag = await sendToContent(tabId, { type: "tagElement", index: action.index, attr: "data-wp-upload" }, activeFrameId);
    if (tag && tag.success === false) {
      return { type: "Action", success: false, error: tag.error };
    }

    try {
      await withCdp(tabId, async (tid) => {
        const { root } = await cdpSend(tid, "DOM.getDocument");
        const { nodeId } = await cdpSend(tid, "DOM.querySelector", {
          nodeId: root.nodeId,
          selector: "[data-wp-upload]",
        });
        if (!nodeId) throw new Error("File input element not found via CDP");
        await cdpSend(tid, "DOM.setFileInputFiles", { nodeId, files: [action.path] });
      });
    } finally {
      // Strip the marker whether or not the assignment succeeded, so a failed
      // upload never leaves a stale attribute on the input.
      await sendToContent(tabId, { type: "untagElement", attr: "data-wp-upload" }, activeFrameId, 3000)
        .catch(() => {});
    }

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
    await withCdp(tabId, async (tid) => {
      await cdpSend(tid, "Input.dispatchMouseEvent", {
        type: "mousePressed", x: sx, y: sy, button: "left", clickCount: 1,
      });
      await sleep(50);
      for (let i = 1; i <= steps; i++) {
        const r = i / steps;
        await cdpSend(tid, "Input.dispatchMouseEvent", {
          type: "mouseMoved", x: sx + (tx - sx) * r, y: sy + (ty - sy) * r, button: "left",
        });
        await sleep(20);
      }
      await cdpSend(tid, "Input.dispatchMouseEvent", {
        type: "mouseReleased", x: tx, y: ty, button: "left", clickCount: 1,
      });
    });

    return { type: "Action", success: true };
  } catch (e) {
    return { type: "Action", success: false, error: exceptionErr(e) };
  }
}

// ── Tabs ───────────────────────────────────────────────────────────────────

async function handleTabSwitch(tabId) {
  try {
    const target = parseInt(tabId, 10);
    await chrome.tabs.update(target, { active: true });
    const tab = await chrome.tabs.get(target);
    if (tab.windowId != null) {
      await chrome.windows.update(tab.windowId, { focused: true });
    }
    // A different tab has its own frame tree — drop any frame scope.
    setActiveFrameId(0);
    return { type: "Action", success: true };
  } catch (e) {
    return { type: "Action", success: false, error: err("TabNotFound", e.message, { tab_id: tabId }) };
  }
}

async function handleTabList() {
  const tabs = await chrome.tabs.query({});
  return {
    type: "Tabs",
    tabs: tabs.map((t) => ({
      id: String(t.id),
      url: t.url || "",
      title: t.title || "",
      active: t.active,
    })),
  };
}

// ── Status ─────────────────────────────────────────────────────────────────

async function handleStatus() {
  // Scope to the focused window so multi-window users don't accidentally see
  // a tab from a different window.
  const [tab] = await chrome.tabs.query({ active: true, lastFocusedWindow: true });
  // Chrome version derives from the user agent (no direct API in MV3 SW).
  const ua = navigator.userAgent || "";
  const m = ua.match(/Chrome\/(\S+)/);
  return {
    type: "Status",
    connected: !!nmPort,
    mode: "browser",
    tab_url: tab?.url || null,
    tab_title: tab?.title || null,
    chrome_version: m ? m[1] : null,
    extension_version: chrome.runtime.getManifest().version,
  };
}

// ── Eval ───────────────────────────────────────────────────────────────────

async function handleEval(command) {
  const tab = await findHttpTab();
  if (!tab) return { type: "Eval", success: false, error: noPageErr() };

  try {
    const r = await withCdp(tab.id, async (tid) => {
      const ev = await cdpSend(tid, "Runtime.evaluate", {
        expression: command.code, returnByValue: true, awaitPromise: true,
      });
      if (ev.exceptionDetails) {
        const msg = ev.exceptionDetails.exception?.description || ev.exceptionDetails.text || "JS exception";
        return { success: false, error: otherErr(msg) };
      }
      const v = ev.result?.value;
      return { success: true, result: v !== undefined ? JSON.stringify(v) : null };
    });
    return { type: "Eval", ...r };
  } catch (e) {
    return { type: "Eval", success: false, error: exceptionErr(e) };
  }
}

// ── Wait ───────────────────────────────────────────────────────────────────

async function handleWait(command) {
  const tab = await findHttpTab();
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
    return { type: "Wait", success: false, error: timeoutErr("wait", timeoutMs) };
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
  const tab = await findHttpTab();
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
  const tab = await findHttpTab();
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

async function handleFetch(command) {
  const tab = await findHttpTab();
  if (!tab) return { type: "FetchResult", success: false, error: noPageErr() };
  try {
    const r = await withCdp(tab.id, async (tid) => {
      const code = `
        fetch(${JSON.stringify(command.url)}, {
          method: ${JSON.stringify(command.method || "GET")},
          headers: {"Content-Type": "application/json"},
          credentials: "include",
          ${command.body ? `body: ${JSON.stringify(command.body)},` : ""}
        }).then(r => r.text().then(body => ({status: r.status, body})))
      `;
      const ev = await cdpSend(tid, "Runtime.evaluate", {
        expression: code, awaitPromise: true, returnByValue: true,
      });
      return ev.result?.value;
    });
    if (r) {
      return { type: "FetchResult", success: true, status: r.status, body: r.body };
    }
    return { type: "FetchResult", success: false, error: otherErr("No fetch result") };
  } catch (e) {
    return { type: "FetchResult", success: false, error: exceptionErr(e) };
  }
}

// ── Frames ─────────────────────────────────────────────────────────────────

function activeFrameIdWire() {
  return activeFrameId === 0 ? null : String(activeFrameId);
}

async function handleFrameList() {
  const tab = await findHttpTab();
  if (!tab) return { type: "Frames", frames: [], active_frame_id: null };

  const all = await chrome.webNavigation.getAllFrames({ tabId: tab.id }).catch(() => []);
  const frames = all.map((f) => ({
    frame_id: String(f.frameId),
    url: f.url || "",
    name: null,
    parent_frame_id: f.parentFrameId >= 0 ? String(f.parentFrameId) : null,
    is_main: f.frameId === 0,
  }));

  await Promise.allSettled(all.map(async (f, idx) => {
    if (f.frameId === 0 || !f.url?.startsWith("http")) return;
    try {
      const r = await sendToContent(tab.id, { type: "eval", code: "window.name" }, f.frameId, 2000);
      if (r?.success && r.result) frames[idx].name = JSON.parse(r.result) || null;
    } catch {}
  }));

  return { type: "Frames", frames, active_frame_id: activeFrameIdWire() };
}

async function handleFrameSwitch(selector) {
  selector = selector || { by: "main" };

  if (selector.by === "main") {
    setActiveFrameId(0);
    return { type: "FrameSwitched", success: true, frame_id: null, name: "main", url: null };
  }

  const tab = await findHttpTab();
  if (!tab) {
    return { type: "FrameSwitched", success: false, frame_id: activeFrameIdWire(), error: noPageErr() };
  }

  const frames = await chrome.webNavigation.getAllFrames({ tabId: tab.id }).catch(() => []);
  const httpFrames = frames.filter((f) => f.frameId !== 0 && f.url?.startsWith("http"));

  let matched = null;

  if (selector.by === "name") {
    for (const f of httpFrames) {
      try {
        const r = await sendToContent(tab.id, { type: "eval", code: "window.name" }, f.frameId, 2000);
        if (r?.success && r.result && JSON.parse(r.result) === selector.value) {
          matched = f;
          break;
        }
      } catch {}
    }
    if (!matched) {
      matched = httpFrames.find((f) => f.url?.includes(selector.value));
    }
  } else if (selector.by === "url") {
    const needle = (selector.pattern || "").replace(/\*/g, "");
    matched = httpFrames.find((f) => f.url?.includes(needle));
  } else if (selector.by === "predicate") {
    // A predicate is arbitrary caller JS, gated by the `eval` key — enforced
    // CLI-side before the command is sent.
    for (const f of httpFrames) {
      try {
        const r = await sendToContent(tab.id, { type: "eval", code: selector.js }, f.frameId, 2000);
        if (r?.success && r.result && JSON.parse(r.result) === true) {
          matched = f;
          break;
        }
      } catch {}
    }
  }

  if (matched) {
    setActiveFrameId(matched.frameId);
    return {
      type: "FrameSwitched",
      success: true,
      frame_id: String(matched.frameId),
      url: matched.url,
    };
  }

  const sel = JSON.stringify(selector);
  return {
    type: "FrameSwitched",
    success: false,
    frame_id: activeFrameIdWire(),
    error: err("FrameNotFound", `No matching frame: ${sel}`, { selector: sel }),
  };
}

// ── Cookies ────────────────────────────────────────────────────────────────

async function handleCookieList(url) {
  const cookies = await chrome.cookies.getAll({ url });
  return {
    type: "Cookies",
    cookies: cookies.map(toCookieInfo),
  };
}

async function handleCookieSet(command) {
  try {
    await chrome.cookies.set({
      url: command.url,
      name: command.name,
      value: command.value,
      httpOnly: command.http_only || false,
      secure: command.secure || false,
    });
    return { type: "CookieResult", success: true };
  } catch (e) {
    return { type: "CookieResult", success: false, error: exceptionErr(e) };
  }
}

async function handleCookieDelete(command) {
  try {
    await chrome.cookies.remove({ url: command.url, name: command.name });
    return { type: "CookieResult", success: true };
  } catch (e) {
    return { type: "CookieResult", success: false, error: exceptionErr(e) };
  }
}

function toCookieInfo(c) {
  return {
    name: c.name, value: c.value, domain: c.domain, path: c.path,
    secure: c.secure, http_only: c.httpOnly,
    same_site: c.sameSite === "no_restriction" ? "none" : (c.sameSite || "unspecified").toLowerCase(),
    expiration: c.expirationDate || null,
  };
}

// Inverse of the wire mapping in `toCookieInfo`: the wire carries SameSite=None
// as "none", but Chrome's cookies API expects "no_restriction".
function chromeSameSite(wire) {
  return wire === "none" ? "no_restriction" : wire || "unspecified";
}

// ── Console / network log ──────────────────────────────────────────────────

async function handleConsoleStart() {
  const tab = await findHttpTab();
  if (!tab) return { type: "CommandResult", success: false, error: noPageErr() };
  try {
    await injectConsoleMonitoring(tab.id);
    monitoringState.console.add(tab.id);
    saveMonitoringState();
    return { type: "CommandResult", success: true };
  } catch (e) {
    return { type: "CommandResult", success: false, error: exceptionErr(e) };
  }
}

async function handleConsoleRead() {
  const tab = await findHttpTab();
  if (!tab) return { type: "ConsoleEntries", entries: [] };
  try {
    const r = await chrome.scripting.executeScript({
      target: { tabId: tab.id, frameIds: [0] },
      world: "MAIN",
      func: () => JSON.parse(JSON.stringify(window.__webpilot_console || [])),
    });
    return { type: "ConsoleEntries", entries: r?.[0]?.result || [] };
  } catch {
    return { type: "ConsoleEntries", entries: [] };
  }
}

async function handleConsoleClear() {
  const tab = await findHttpTab();
  if (tab) {
    try {
      await chrome.scripting.executeScript({
        target: { tabId: tab.id, frameIds: [0] },
        world: "MAIN",
        func: () => { window.__webpilot_console = []; },
      });
    } catch {}
  }
  return { type: "CommandResult", success: true };
}

async function handleNetworkStart() {
  const tab = await findHttpTab();
  if (!tab) return { type: "CommandResult", success: false, error: noPageErr() };
  try {
    await injectNetworkMonitoring(tab.id);
    monitoringState.network.add(tab.id);
    saveMonitoringState();
    return { type: "CommandResult", success: true };
  } catch (e) {
    return { type: "CommandResult", success: false, error: exceptionErr(e) };
  }
}

async function handleNetworkRead(since) {
  const tab = await findHttpTab();
  if (!tab) return { type: "NetworkEntries", entries: [] };
  try {
    const r = await chrome.scripting.executeScript({
      target: { tabId: tab.id, frameIds: [0] },
      world: "MAIN",
      func: (s) => {
        const all = window.__webpilot_network || [];
        return s ? all.filter((e) => e.timestamp >= s) : [...all];
      },
      args: [since || 0],
    });
    return { type: "NetworkEntries", entries: r?.[0]?.result || [] };
  } catch {
    return { type: "NetworkEntries", entries: [] };
  }
}

async function handleNetworkClear() {
  const tab = await findHttpTab();
  if (tab) {
    try {
      await chrome.scripting.executeScript({
        target: { tabId: tab.id, frameIds: [0] },
        world: "MAIN",
        func: () => { window.__webpilot_network = []; },
      });
    } catch {}
  }
  return { type: "CommandResult", success: true };
}

// ── Session ────────────────────────────────────────────────────────────────

async function handleSessionExport() {
  try {
    const all = await chrome.cookies.getAll({});
    const tab = await findHttpTab();
    let storage = { localStorage: {}, sessionStorage: {} };
    if (tab) {
      try {
        await ensureBridge(tab.id, activeFrameId);
        storage = await sendToContent(tab.id, { type: "exportStorage" }, activeFrameId);
      } catch {}
    }
    const data = {
      version: 1,
      exported_at: Date.now(),
      cookies: all.map(toCookieInfo),
      local_storage: storage.localStorage || {},
      session_storage: storage.sessionStorage || {},
    };
    return { type: "SessionExport", path: "", session_data: JSON.stringify(data) };
  } catch (e) {
    return topErr(exceptionErr(e));
  }
}

async function handleSessionImport(rawData) {
  try {
    const data = JSON.parse(rawData);
    let cookies = 0;
    for (const c of data.cookies || []) {
      try {
        await chrome.cookies.set({
          url: `http${c.secure ? "s" : ""}://${c.domain.replace(/^\./, "")}${c.path}`,
          name: c.name, value: c.value, domain: c.domain, path: c.path,
          secure: c.secure, httpOnly: c.http_only,
          sameSite: chromeSameSite(c.same_site),
          expirationDate: c.expiration || undefined,
        });
        cookies++;
      } catch {}
    }
    const tab = await findHttpTab();
    if (tab && (data.local_storage || data.session_storage)) {
      try {
        await ensureBridge(tab.id, activeFrameId);
        await sendToContent(tab.id, {
          type: "importStorage",
          localStorage: data.local_storage || {},
          sessionStorage: data.session_storage || {},
        }, activeFrameId);
      } catch {}
    }
    return { type: "SessionResult", success: true };
  } catch (e) {
    return { type: "SessionResult", success: false, error: exceptionErr(e) };
  }
}

// ── Tab / page utilities ───────────────────────────────────────────────────

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function waitForTabReady(tabId, timeoutMs = 15000) {
  return new Promise((resolve) => {
    const timer = setTimeout(() => {
      chrome.tabs.onUpdated.removeListener(listener);
      resolve();
    }, timeoutMs);

    function listener(tid, changeInfo, tab) {
      if (tid !== tabId) return;
      if (changeInfo.status === "complete" && tab.url && tab.url.startsWith("http")) {
        chrome.tabs.onUpdated.removeListener(listener);
        clearTimeout(timer);
        resolve();
      }
    }
    chrome.tabs.onUpdated.addListener(listener);
  });
}

async function findHttpTab() {
  const all = await chrome.tabs.query({});
  return all.find((t) => t.active && t.url?.startsWith("http"))
    || all.find((t) => t.url?.startsWith("http"));
}

async function ensureBridge(tabId, frameId = 0) {
  for (let attempt = 0; attempt < 3; attempt++) {
    try {
      await Promise.race([
        chrome.scripting.executeScript({ target: { tabId, frameIds: [frameId] }, files: ["content/bridge.js"] }),
        new Promise((_, r) => setTimeout(() => r(new Error("inject timeout")), 3000)),
      ]);
    } catch {}

    await sleep(50 + attempt * 100);
    try {
      const pong = await Promise.race([
        chrome.tabs.sendMessage(tabId, { type: "ping" }, { frameId }),
        new Promise((_, r) => setTimeout(() => r(new Error("ping timeout")), 2000)),
      ]);
      if (pong?.ok) return;
    } catch {}
    console.warn(`[WebPilot] Bridge verify failed (attempt ${attempt + 1}/3, tab=${tabId}, frame=${frameId})`);
  }
  const e = new Error("Page is not responding — try reloading the page");
  e.code = "BridgeUnavailable";
  throw e;
}

async function injectBridgeOnly(tabId, frameId = 0) {
  try {
    await Promise.race([
      chrome.scripting.executeScript({ target: { tabId, frameIds: [frameId] }, files: ["content/bridge.js"] }),
      new Promise((_, r) => setTimeout(() => r(new Error("inject timeout")), 3000)),
    ]);
  } catch {}
}

const SEND_TIMEOUT_MSG = "Page did not respond in time";

async function sendToContent(tabId, message, frameId = 0, timeoutMs = 10000) {
  const sendOnce = () => Promise.race([
    chrome.tabs.sendMessage(tabId, message, { frameId }),
    new Promise((_, r) => setTimeout(() => r(new Error(SEND_TIMEOUT_MSG)), timeoutMs)),
  ]);

  try {
    return await sendOnce();
  } catch (firstError) {
    const recoverable = firstError.message.includes("Receiving end") || firstError.message === SEND_TIMEOUT_MSG;
    if (recoverable) {
      console.warn(`[WebPilot] Content script disconnected (${firstError.message}), recovering...`);
      await ensureBridge(tabId, frameId);
      return await sendOnce();
    }
    throw firstError;
  }
}

async function captureWithRetry(windowId, quality = 80, maxAttempts = 3) {
  let delay = 500;
  for (let attempt = 0; attempt < maxAttempts; attempt++) {
    try {
      if (attempt > 0) await sleep(delay);
      const dataUrl = await Promise.race([
        chrome.tabs.captureVisibleTab(windowId, { format: "jpeg", quality }),
        new Promise((_, r) => setTimeout(() => r(new Error("capture timeout")), 10000)),
      ]);
      return dataUrl.replace(/^data:image\/\w+;base64,/, "");
    } catch (e) {
      console.warn(`[WebPilot] Capture attempt ${attempt + 1} failed: ${e.message}`);
      delay *= 2;
      if (attempt === maxAttempts - 1) throw e;
    }
  }
}

// ── Internal popup/sidepanel message handler ──────────────────────────────

chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  if (msg.type === "status") {
    sendResponse({ connected: !!nmPort });
    return false;
  }
});

// ── Lifecycle ──────────────────────────────────────────────────────────────

chrome.runtime.onInstalled.addListener(() => {
  console.log("[WebPilot] Extension installed");
  connectToHost();
});

chrome.runtime.onStartup.addListener(() => {
  console.log("[WebPilot] Chrome started");
  connectToHost();
});

chrome.webNavigation.onCompleted.addListener(async (details) => {
  if (details.frameId !== 0) return;
  if (!details.url?.startsWith("http")) return;
  const tabId = details.tabId;
  // This fires independently of command processing, so it must also wait for
  // the post-restart restore before consulting `monitoringState` — otherwise a
  // navigation right after an SW restart would skip re-injecting a monitor the
  // user had started.
  await RESTORED;
  await injectBridgeOnly(tabId, 0);
  if (monitoringState.console.has(tabId)) {
    try { await injectConsoleMonitoring(tabId); } catch {}
  }
  if (monitoringState.network.has(tabId)) {
    try { await injectNetworkMonitoring(tabId); } catch {}
  }
});

connectToHost();
