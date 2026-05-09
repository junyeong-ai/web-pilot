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

// Restore active frame on SW restart.
chrome.storage.session?.get("activeFrameId", (data) => {
  if (data?.activeFrameId != null) activeFrameId = data.activeFrameId;
});

function setActiveFrameId(id) {
  activeFrameId = id;
  chrome.storage.session?.set({ activeFrameId: id });
}

// Per-tab monitoring state — restored on SW restart.
const monitoringState = { console: new Set(), network: new Set() };
chrome.storage.session?.get("monitoringTabs", (data) => {
  if (data?.monitoringTabs) {
    (data.monitoringTabs.console || []).forEach((id) => monitoringState.console.add(id));
    (data.monitoringTabs.network || []).forEach((id) => monitoringState.network.add(id));
  }
});

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
const timeoutErr = (kind, elapsed_ms) => err("Timeout", `${kind} timed out`, { kind, elapsed_ms });
const noPageErr = () => err("NoPage", "No web page open");
const policyDeniedErr = (action) => err("PolicyDenied", `Action '${action}' denied by policy`, { action });
const elementNotFoundErr = (requested, available) =>
  err("ElementNotFound", `Index ${requested} out of range (1-${available})`, { requested, available });

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
      const OrigXHR = window.XMLHttpRequest;
      window.XMLHttpRequest = function () {
        const xhr = new OrigXHR();
        let method = "GET", url = "";
        const origOpen = xhr.open;
        xhr.open = function (m, u, ...a) { method = m; url = u; return origOpen.apply(this, [m, u, ...a]); };
        const origSend = xhr.send;
        xhr.send = function (...a) {
          const t0 = performance.now();
          xhr.addEventListener("loadend", () => {
            window.__webpilot_network.push({
              type: "xhr", url, method,
              status: xhr.status || undefined,
              error: xhr.status === 0 ? "Network error" : undefined,
              duration_ms: Math.round(performance.now() - t0),
              timestamp: Date.now(),
            });
            if (window.__webpilot_network.length > 500) window.__webpilot_network.shift();
          });
          return origSend.apply(this, a);
        };
        return xhr;
      };
      window.XMLHttpRequest.prototype = OrigXHR.prototype;
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

    nmPort.onMessage.addListener(handleHostMessage);
    nmPort.onDisconnect.addListener(() => {
      const error = chrome.runtime.lastError?.message || "unknown";
      console.log("[WebPilot] Native host disconnected:", error);
      nmPort = null;
      clearInterval(keepaliveTimer);

      connectionRetries++;
      const delay = Math.min(2000 * connectionRetries, 30000);
      setTimeout(connectToHost, delay);
    });

    clearInterval(keepaliveTimer);
    keepaliveTimer = setInterval(() => {
      nmPort?.postMessage({ id: 0, command: { type: "Ping" } });
    }, KEEPALIVE_INTERVAL);
  } catch (e) {
    console.error("[WebPilot] Failed to connect:", e);
    connectionRetries++;
    setTimeout(connectToHost, Math.min(5000 * connectionRetries, 30000));
  }
}

function handleHostMessage(request) {
  const { id, command } = request;
  if (!command) return;
  processCommandWithKeepAlive(id, command);
}

async function processCommandWithKeepAlive(id, command) {
  // Reset 30s idle timer while command is in flight.
  const keepAlive = setInterval(() => {
    chrome.runtime.getPlatformInfo(() => {});
  }, 20000);
  try {
    await processCommand(id, command);
  } finally {
    clearInterval(keepAlive);
  }
}

// ── Command dispatch ───────────────────────────────────────────────────────

async function processCommand(id, command) {
  try {
    let result;
    switch (command.type) {
      case "Capture":
        result = await handleCapture(command);
        break;

      case "Action":
        result = await handleActionCommand(command);
        break;

      case "Status":
        result = await handleStatus();
        break;

      case "TabList":
        result = await handleListTabs();
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
          result = { type: "Action", success: false, error: otherErr(e.message) };
        }
        break;

      case "Evaluate":
        result = await handleEvaluate(command);
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

      case "PolicySet":
        result = await handlePolicySet(command.action, command.verdict);
        break;

      case "PolicyList":
        result = await handlePolicyList();
        break;

      case "PolicyClear":
        result = await handlePolicyClear();
        break;

      case "Ping":
        result = { type: "Pong" };
        break;

      default:
        result = topErr(otherErr(`Unknown command: ${command.type}`));
    }

    nmPort?.postMessage({ id, result });
  } catch (e) {
    nmPort?.postMessage({ id, result: topErr(otherErr(e.message)) });
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

  // DOM extraction (with iframe merge).
  if (include.has("dom")) {
    try {
      const extractOpts = { bounds: opts.bounds || false, occlusion: opts.occlusion || false };
      const frames = await chrome.webNavigation.getAllFrames({ tabId }).catch(() => [{ frameId: 0 }]);
      const httpFrames = frames.filter((f) => f.url?.startsWith("http"));

      await ensureBridge(tabId, 0);

      const frameResults = await Promise.allSettled(
        httpFrames.map((f) =>
          sendToContent(tabId, { type: "extractDOM", options: extractOpts }, f.frameId, 5000)
            .then((dom) => ({ frameId: f.frameId, url: f.url, dom })),
        ),
      );

      const allElements = [];
      let globalIdx = 1;
      let mainDom = null;

      for (const r of frameResults) {
        if (r.status !== "fulfilled" || !r.value.dom?.elements) continue;
        const { frameId, url, dom } = r.value;
        const frameLabel = frameId === 0 ? null : (url ? new URL(url).hostname : `frame-${frameId}`);
        if (frameId === 0) mainDom = dom;
        for (const el of dom.elements) {
          el.index = globalIdx++;
          if (frameLabel) el.frame = frameLabel;
          allElements.push(el);
        }
      }

      if (allElements.length > 0) {
        const base = mainDom || frameResults.find((r) => r.status === "fulfilled")?.value?.dom || {};
        result.dom = {
          elements: allElements,
          total_nodes: base.total_nodes || 0,
          page_url: base.page_url || "",
          page_title: base.page_title || "",
          scroll: base.scroll || {},
          scroll_percent: base.scroll_percent || 0,
          extraction_ms: base.extraction_ms || 0,
        };
        result.page_url = result.dom.page_url;
        result.page_title = result.dom.page_title;
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

  // Annotated overlay before screenshot.
  if (opts.annotate && result.dom?.elements) {
    try {
      const annotations = result.dom.elements
        .filter((el) => el.in_viewport && el.bounds && el.bounds.w > 0 && el.bounds.h > 0 && !el.frame)
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

async function handleActionCommand(command) {
  const { action } = command;

  // Policy enforcement (action.kind matches stored policy keys).
  try {
    const stored = await chrome.storage.local.get("policies");
    const policies = stored?.policies || {};
    if (policies[action.kind] === "deny") {
      return { type: "Action", success: false, error: policyDeniedErr(action.kind) };
    }
  } catch {}

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

  let result;

  // SW-handled action kinds (navigation + upload).
  switch (action.kind) {
    case "navigate":
      await chrome.tabs.update(tab.id, { url: action.url, active: true });
      await waitForTabReady(tab.id, 15000);
      await sleep(500);
      result = { type: "Action", success: true };
      break;

    case "back":
      await chrome.tabs.goBack(tab.id);
      await sleep(500);
      result = { type: "Action", success: true };
      break;

    case "forward":
      await chrome.tabs.goForward(tab.id);
      await sleep(500);
      result = { type: "Action", success: true };
      break;

    case "reload":
      await chrome.tabs.reload(tab.id);
      await waitForTabReady(tab.id, 15000);
      result = { type: "Action", success: true };
      break;

    case "upload":
      result = await handleUpload(tab.id, action);
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
        const dom = await sendToContent(t.id, { type: "extractDOM", options: {} }, activeFrameId, 5000);
        if (dom) result.dom = dom;
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
    return { type: "Action", success: false, error: otherErr(e.message) };
  }
}

async function handleUpload(tabId, action) {
  try {
    await ensureBridge(tabId, activeFrameId);
    await sendToContent(tabId, { type: "tagElement", index: action.index, attr: "data-wp-upload" }, activeFrameId);

    await withCdp(tabId, async (tid) => {
      const { root } = await cdpSend(tid, "DOM.getDocument");
      const { nodeId } = await cdpSend(tid, "DOM.querySelector", {
        nodeId: root.nodeId,
        selector: "[data-wp-upload]",
      });
      if (!nodeId) throw new Error("File input element not found via CDP");
      await cdpSend(tid, "DOM.setFileInputFiles", { nodeId, files: [action.path] });
    });

    await sendToContent(tabId, { type: "untagElement", attr: "data-wp-upload" }, activeFrameId, 3000)
      .catch(() => {});

    return { type: "Action", success: true };
  } catch (e) {
    return { type: "Action", success: false, error: otherErr(e.message) };
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
    return { type: "Action", success: true };
  } catch (e) {
    return { type: "Action", success: false, error: err("TabNotFound", e.message, { tab_id: tabId }) };
  }
}

async function handleListTabs() {
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

// ── Evaluate ───────────────────────────────────────────────────────────────

async function handleEvaluate(command) {
  const tab = await findHttpTab();
  if (!tab) return { type: "Evaluate", success: false, error: noPageErr() };

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
    return { type: "Evaluate", ...r };
  } catch (e) {
    return { type: "Evaluate", success: false, error: otherErr(e.message) };
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
    return { type: "CommandResult", success: false, error: otherErr(e.message) };
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
    return { type: "CommandResult", success: false, error: otherErr(e.message) };
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
    return { type: "FetchResult", success: false, error: otherErr(e.message) };
  }
}

// ── Frames ─────────────────────────────────────────────────────────────────

async function handleFrameList() {
  const tab = await findHttpTab();
  if (!tab) return { type: "Frames", frames: [], active_frame_id: 0 };

  const all = await chrome.webNavigation.getAllFrames({ tabId: tab.id }).catch(() => []);
  const frames = all.map((f) => ({
    frame_id: f.frameId,
    url: f.url || "",
    name: null,
    parent_frame_id: f.parentFrameId >= 0 ? f.parentFrameId : null,
    is_main: f.frameId === 0,
  }));

  await Promise.allSettled(frames.map(async (f) => {
    if (f.frame_id === 0 || !f.url?.startsWith("http")) return;
    try {
      const r = await sendToContent(tab.id, { type: "evaluate", code: "window.name" }, f.frame_id, 2000);
      if (r?.success && r.result) f.name = JSON.parse(r.result) || null;
    } catch {}
  }));

  return { type: "Frames", frames, active_frame_id: activeFrameId };
}

async function handleFrameSwitch(selector) {
  selector = selector || { by: "main" };

  if (selector.by === "main") {
    setActiveFrameId(0);
    return { type: "FrameSwitched", success: true, frame_id: 0, name: "main", url: null };
  }

  const tab = await findHttpTab();
  if (!tab) {
    return { type: "FrameSwitched", success: false, frame_id: activeFrameId, error: noPageErr() };
  }

  const frames = await chrome.webNavigation.getAllFrames({ tabId: tab.id }).catch(() => []);
  const httpFrames = frames.filter((f) => f.frameId !== 0 && f.url?.startsWith("http"));

  let matched = null;

  if (selector.by === "name") {
    for (const f of httpFrames) {
      try {
        const r = await sendToContent(tab.id, { type: "evaluate", code: "window.name" }, f.frameId, 2000);
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
    for (const f of httpFrames) {
      try {
        const r = await sendToContent(tab.id, { type: "evaluate", code: selector.js }, f.frameId, 2000);
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
      frame_id: matched.frameId,
      url: matched.url,
    };
  }

  const sel = JSON.stringify(selector);
  return {
    type: "FrameSwitched",
    success: false,
    frame_id: activeFrameId,
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
    return { type: "CookieResult", success: false, error: otherErr(e.message) };
  }
}

async function handleCookieDelete(command) {
  try {
    await chrome.cookies.remove({ url: command.url, name: command.name });
    return { type: "CookieResult", success: true };
  } catch (e) {
    return { type: "CookieResult", success: false, error: otherErr(e.message) };
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
    return { type: "CommandResult", success: false, error: otherErr(e.message) };
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
    return { type: "CommandResult", success: false, error: otherErr(e.message) };
  }
}

async function handleNetworkRead(since) {
  const tab = await findHttpTab();
  if (!tab) return { type: "NetworkLog", requests: [] };
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
    return { type: "NetworkLog", requests: r?.[0]?.result || [] };
  } catch {
    return { type: "NetworkLog", requests: [] };
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
    return topErr(otherErr(e.message));
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
          sameSite: c.same_site || "unspecified",
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
    return { type: "SessionResult", success: false, error: otherErr(e.message) };
  }
}

// ── Policy store ───────────────────────────────────────────────────────────
//
// Stored as `{action: "click", verdict: "deny"}` → key by action kind.

async function loadPolicies() {
  const stored = await chrome.storage.local.get("policies");
  return stored?.policies || {};
}

async function handlePolicySet(action, verdict) {
  try {
    const policies = await loadPolicies();
    policies[action] = verdict;
    await chrome.storage.local.set({ policies });
    return { type: "PolicyResult", success: true };
  } catch (e) {
    return { type: "PolicyResult", success: false, error: otherErr(e.message) };
  }
}

async function handlePolicyList() {
  try {
    const policies = await loadPolicies();
    return {
      type: "Policies",
      policies: Object.entries(policies).map(([action, verdict]) => ({ action, verdict })),
    };
  } catch {
    return { type: "Policies", policies: [] };
  }
}

async function handlePolicyClear() {
  try {
    await chrome.storage.local.remove("policies");
    return { type: "PolicyResult", success: true };
  } catch (e) {
    return { type: "PolicyResult", success: false, error: otherErr(e.message) };
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
  await injectBridgeOnly(tabId, 0);
  if (monitoringState.console.has(tabId)) {
    try { await injectConsoleMonitoring(tabId); } catch {}
  }
  if (monitoringState.network.has(tabId)) {
    try { await injectNetworkMonitoring(tabId); } catch {}
  }
});

connectToHost();
