/**
 * WebPilot Service Worker
 * Manages Native Messaging connection and routes commands between CLI and content scripts.
 */
console.log("[WebPilot] Service Worker loaded");

const NM_HOST = "com.webpilot.host";
const KEEPALIVE_INTERVAL = 25000;
const CDP_VERSION = "1.3";

let nmPort = null;

// --- CDP Session Manager (inline, self-contained per command) ---
const cdpLocks = new Map(); // tabId → Promise (serialize concurrent CDP operations)

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

// Active frame ID per tab (0 = main frame) — persisted to survive SW restart
let activeFrameId = 0;

// Restore activeFrameId from session storage on SW restart
chrome.storage.session?.get("activeFrameId", (data) => {
  if (data?.activeFrameId != null) activeFrameId = data.activeFrameId;
});

function setActiveFrameId(id) {
  activeFrameId = id;
  chrome.storage.session?.set({ activeFrameId: id });
}

// Track which tabs have console/network monitoring active — persisted to survive SW restart
const monitoringState = { console: new Set(), network: new Set() };

// Restore monitoringState from session storage on SW restart
chrome.storage.session?.get("monitoringTabs", (data) => {
  if (data?.monitoringTabs) {
    (data.monitoringTabs.console || []).forEach(id => monitoringState.console.add(id));
    (data.monitoringTabs.network || []).forEach(id => monitoringState.network.add(id));
  }
});

function saveMonitoringState() {
  chrome.storage.session?.set({
    monitoringTabs: {
      console: [...monitoringState.console],
      network: [...monitoringState.network],
    }
  });
}

// --- Shared injection helpers (DRY — used by command handlers AND webNavigation auto-reinject) ---

async function injectConsoleMonitoring(tabId) {
  await chrome.scripting.executeScript({
    target: { tabId, frameIds: [0] },
    world: "MAIN",
    func: () => {
      if (window.__webpilot_console) return;
      window.__webpilot_console = [];
      const orig = { log: console.log, error: console.error, warn: console.warn, info: console.info };
      ["log", "error", "warn", "info"].forEach(m => {
        console[m] = (...args) => {
          window.__webpilot_console.push({
            level: m,
            message: args.map(a => { try { return String(a); } catch { return "[object]"; } }).join(" "),
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
      window.fetch = function(...args) {
        const [resource, config] = args;
        const t0 = performance.now();
        return origFetch.apply(this, args).then(response => {
          window.__webpilot_network.push({
            type: "fetch", url: String(resource),
            method: config?.method || "GET", status: response.status,
            duration_ms: Math.round(performance.now() - t0),
            timestamp: Date.now(),
          });
          if (window.__webpilot_network.length > 500) window.__webpilot_network.shift();
          return response;
        }).catch(err => {
          window.__webpilot_network.push({
            type: "fetch", url: String(resource),
            method: config?.method || "GET", error: err.message,
            duration_ms: Math.round(performance.now() - t0),
            timestamp: Date.now(),
          });
          throw err;
        });
      };
      const OrigXHR = window.XMLHttpRequest;
      window.XMLHttpRequest = function() {
        const xhr = new OrigXHR();
        let method = "GET", url = "";
        const origOpen = xhr.open;
        xhr.open = function(m, u, ...a) { method = m; url = u; return origOpen.apply(this, [m, u, ...a]); };
        const origSend = xhr.send;
        xhr.send = function(...a) {
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

async function cdpSend(tabId, method, params = {}) {
  return chrome.debugger.sendCommand({ tabId }, method, params);
}
let keepaliveTimer = null;
let connectionRetries = 0;

// Connect to Native Messaging Host
function connectToHost() {
  if (nmPort) return;

  try {
    nmPort = chrome.runtime.connectNative(NM_HOST);
    console.log("[WebPilot] Connected to native host");
    connectionRetries = 0; // Reset on successful connection

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

    // Start keepalive pings (clear any stale timer first)
    clearInterval(keepaliveTimer);
    keepaliveTimer = setInterval(() => {
      if (nmPort) {
        nmPort.postMessage({ id: 0, command: { type: "Ping" } });
      }
    }, KEEPALIVE_INTERVAL);
  } catch (e) {
    console.error("[WebPilot] Failed to connect:", e);
    connectionRetries++;
    const delay = Math.min(5000 * connectionRetries, 30000);
    setTimeout(connectToHost, delay);
  }
}

// Handle messages from Native Host (forwarded CLI requests)
function handleHostMessage(request) {

  const { id, command } = request;
  if (!command) return;

  // Process the command with a keep-alive wrapper.
  // navigator.locks does NOT prevent service worker termination — it is a Web API,
  // not a Chrome extension API. Instead, we periodically call a Chrome extension API
  // to reset the 30-second idle timer while async work is in progress.
  processCommandWithKeepAlive(id, command);
}

async function processCommandWithKeepAlive(id, command) {
  // Start a periodic Chrome extension API call to reset the idle timer.
  // Any chrome.* API call counts as extension activity and resets the 30s idle timer.
  // chrome.runtime.getPlatformInfo() requires no extra permissions and is lightweight.
  const keepAlive = setInterval(() => {
    chrome.runtime.getPlatformInfo(() => {});
  }, 20000); // every 20s, well within the 30s idle timeout

  try {
    await processCommand(id, command);
  } finally {
    clearInterval(keepAlive);
  }
}

async function processCommand(id, command) {
  try {
    let result;
    switch (command.type) {
      case "Capture":
        result = await handleCapture(command);
        break;
      case "Action": {
        // Inject dialog override before action (prevents alert/confirm from blocking)
        const actionTab = await findHttpTab();
        if (actionTab) {
          try {
            await chrome.scripting.executeScript({
              target: { tabId: actionTab.id, frameIds: [0] },
              world: "MAIN",
              func: () => {
                if (!window.__webpilot_dialogs) {
                  window.__webpilot_dialogs = [];
                  window.alert = (msg) => { window.__webpilot_dialogs.push({type:'alert',message:String(msg)}); };
                  window.confirm = (msg) => { window.__webpilot_dialogs.push({type:'confirm',message:String(msg)}); return true; };
                  window.prompt = (msg, def) => { window.__webpilot_dialogs.push({type:'prompt',message:String(msg)}); return def || ''; };
                }
              },
            });
          } catch {}
        }
        result = await handleAction(command.action);
        // Auto-capture DOM after action if requested
        if (command.capture && result?.success) {
          await sleep(500);
          const tab = await findHttpTab();
          if (tab) {
            await injectBridge(tab.id);
            try {
              const dom = await Promise.race([
                chrome.tabs.sendMessage(tab.id, { type: "extractDOM", options: {} }, { frameId: activeFrameId }),
                new Promise((_, r) => setTimeout(() => r(), 5000)),
              ]);
              if (dom) result.dom = dom;
            } catch {}
          }
        }
        break;
      }
      case "Status":
        result = await handleStatus();
        break;
      case "ListTabs":
        result = await handleListTabs();
        break;
      case "SwitchTab":
        result = await handleSwitchTab(command.tab_id);
        break;
      case "NewTab": {
        const t = await chrome.tabs.create({ url: command.url, active: true });
        result = { type: "Action", success: true, code: null, dom: null, error: null };
        break;
      }
      case "CloseTab":
        await chrome.tabs.remove(command.tab_id);
        result = { type: "Action", success: true, code: null, dom: null, error: null };
        break;
      case "Evaluate": {
        const tab = await findHttpTab();
        if (!tab) { result = { type: "Error", message: "No web page tab" }; break; }
        try {
          // Execute JS in MAIN world for access to page variables
          // Note: Sites with strict CSP (unsafe-eval blocked) may reject eval().
          // We use Function constructor as primary, eval as fallback.
          const results = await Promise.race([
            chrome.scripting.executeScript({
              target: { tabId: tab.id },
              world: "MAIN",
              func: (code) => {
                try {
                  // Try Function constructor first (works on most sites)
                  const fn = new Function("return (" + code + ")");
                  return { success: true, result: JSON.stringify(fn()) };
                } catch (e1) {
                  if (e1.message.includes("Content Security Policy")) {
                    // CSP blocks eval — try indirect approach via script element
                    try {
                      const s = document.createElement("script");
                      s.textContent = `window.__webpilot_result = JSON.stringify(${code})`;
                      document.documentElement.appendChild(s);
                      s.remove();
                      const r = window.__webpilot_result;
                      delete window.__webpilot_result;
                      return { success: true, result: r };
                    } catch (e2) {
                      return { success: false, error: "CSP blocks eval. Try a different page or simpler expression." };
                    }
                  }
                  return { success: false, error: e1.message };
                }
              },
              args: [command.code],
            }),
            new Promise((_, r) => setTimeout(() => r(new Error("eval timeout")), 10000)),
          ]);
          const r = results?.[0]?.result || { success: false, error: "No result" };
          result = { type: "Evaluate", ...r };
        } catch (e) {
          result = { type: "Evaluate", success: false, error: e.message };
        }
        break;
      }
      case "Wait": {
        const tab = await findHttpTab();
        if (!tab) { result = { type: "Error", message: "No web page tab" }; break; }

        if (command.navigation) {
          // Wait for tab navigation (URL change + complete status)
          try {
            await Promise.race([
              new Promise((resolve) => {
                function listener(tabId, changeInfo, updatedTab) {
                  if (tabId === tab.id && changeInfo.status === "complete" && updatedTab.url?.startsWith("http")) {
                    chrome.tabs.onUpdated.removeListener(listener);
                    resolve();
                  }
                }
                chrome.tabs.onUpdated.addListener(listener);
              }),
              new Promise((_, rej) => setTimeout(() => rej(new Error("Navigation wait timed out")), command.timeout_ms || 10000)),
            ]);
            result = { type: "Wait", success: true, error: null, code: null };
          } catch (e) {
            result = { type: "Wait", success: false, error: e.message, code: "TIMEOUT" };
          }
        } else {
          // Wait for selector/text/DOM idle via content script
          await injectBridge(tab.id);
          try {
            const r = await Promise.race([
              chrome.tabs.sendMessage(tab.id, { type: "wait", selector: command.selector, text: command.text, timeout_ms: command.timeout_ms }, { frameId: activeFrameId }),
              new Promise((_, rej) => setTimeout(() => rej(new Error("Wait timed out")), (command.timeout_ms || 10000) + 2000)),
            ]);
            result = { type: "Wait", success: r.success, error: r.error || null, code: r.code || null };
          } catch (e) {
            result = { type: "Wait", success: false, error: e.message, code: "TIMEOUT" };
          }
        }
        break;
      }
      case "SetDom": {
        const tab = await findHttpTab();
        if (!tab) { result = { type: "Error", message: "No web page tab" }; break; }
        await injectBridge(tab.id);
        const msgType = command.property === "html" ? "setHtml" : command.property === "text" ? "setText" : "setAttr";
        const msg = { type: msgType, selector: command.selector, value: command.value, attr: command.attr };
        try {
          const r = await chrome.tabs.sendMessage(tab.id, msg, { frameId: activeFrameId });
          result = { type: "CommandResult", success: r.success, value: null, error: r.error || null };
        } catch (e) {
          result = { type: "CommandResult", success: false, value: null, error: e.message };
        }
        break;
      }
      case "GetDom": {
        const tab = await findHttpTab();
        if (!tab) { result = { type: "Error", message: "No web page tab" }; break; }
        await injectBridge(tab.id);
        const msgType = command.property === "html" ? "getHtml" : command.property === "text" ? "getText" : "getAttr";
        const msg = { type: msgType, selector: command.selector, attr: command.attr };
        try {
          const r = await chrome.tabs.sendMessage(tab.id, msg, { frameId: activeFrameId });
          result = { type: "CommandResult", success: r.success, value: r.value || null, error: r.error || null };
        } catch (e) {
          result = { type: "CommandResult", success: false, value: null, error: e.message };
        }
        break;
      }
      case "Fetch": {
        const tab = await findHttpTab();
        if (!tab) { result = { type: "Error", message: "No web page tab" }; break; }
        try {
          const fetchResult = await withCdp(tab.id, async (tid) => {
            const code = `
              fetch(${JSON.stringify(command.url)}, {
                method: ${JSON.stringify(command.method || "GET")},
                headers: {"Content-Type": "application/json"},
                credentials: "include",
                ${command.body ? `body: ${JSON.stringify(command.body)},` : ""}
              }).then(r => r.text().then(body => ({status: r.status, body})))
            `;
            const { result: evalResult } = await cdpSend(tid, "Runtime.evaluate", {
              expression: code, awaitPromise: true, returnByValue: true,
            });
            return evalResult?.value;
          });
          if (fetchResult) {
            result = { type: "FetchResult", success: true, status: fetchResult.status, body: fetchResult.body, error: null };
          } else {
            result = { type: "FetchResult", success: false, status: null, body: null, error: "No result" };
          }
        } catch (e) {
          result = { type: "FetchResult", success: false, status: null, body: null, error: e.message };
        }
        break;
      }
      case "ListFrames": {
        const tab = await findHttpTab();
        if (!tab) { result = { type: "Error", message: "No web page tab" }; break; }
        const allFrames = await chrome.webNavigation.getAllFrames({ tabId: tab.id }).catch(() => []);
        result = {
          type: "Frames",
          frames: allFrames.map(f => ({
            frame_id: f.frameId,
            url: f.url || "",
            name: null, // Chrome doesn't expose frame name in webNavigation
            parent_frame_id: f.parentFrameId >= 0 ? f.parentFrameId : null,
            is_main: f.frameId === 0,
          })),
          active_frame_id: activeFrameId,
        };
        // Enrich with frame names via content script
        for (const frame of result.frames) {
          if (frame.frame_id !== 0 && frame.url?.startsWith("http")) {
            try {
              const info = await Promise.race([
                chrome.tabs.sendMessage(tab.id, { type: "ping" }, { frameId: frame.frame_id }),
                new Promise((_, r) => setTimeout(() => r(), 1000)),
              ]);
              // Try to get frame name from parent
            } catch {}
          }
        }
        break;
      }
      case "SwitchFrame": {
        if (command.main) {
          setActiveFrameId(0);
          result = { type: "FrameSwitched", success: true, frame_id: 0, name: "main", url: null, error: null };
          break;
        }
        const tab = await findHttpTab();
        if (!tab) { result = { type: "Error", message: "No web page tab" }; break; }
        const frames = await chrome.webNavigation.getAllFrames({ tabId: tab.id }).catch(() => []);
        let matched = null;

        if (command.name) {
          // Search by frame name — eval in each frame to check window.name
          for (const f of frames) {
            if (f.frameId === 0 || !f.url?.startsWith("http")) continue;
            try {
              const r = await Promise.race([
                chrome.tabs.sendMessage(tab.id, { type: "evaluate", code: "window.name" }, { frameId: f.frameId }),
                new Promise((_, rej) => setTimeout(() => rej(), 2000)),
              ]);
              if (r?.success && r.result && JSON.parse(r.result) === command.name) {
                matched = f;
                break;
              }
            } catch {}
          }
          // Fallback: check URL contains name
          if (!matched) {
            matched = frames.find(f => f.url?.includes(command.name) && f.frameId !== 0);
          }
        } else if (command.url_pattern) {
          const pattern = command.url_pattern.replace(/\*/g, "");
          matched = frames.find(f => f.url?.includes(pattern) && f.frameId !== 0);
        } else if (command.predicate) {
          // Evaluate predicate in each frame
          for (const f of frames) {
            if (f.frameId === 0 || !f.url?.startsWith("http")) continue;
            try {
              const r = await Promise.race([
                chrome.tabs.sendMessage(tab.id, { type: "evaluate", code: command.predicate }, { frameId: f.frameId }),
                new Promise((_, rej) => setTimeout(() => rej(), 2000)),
              ]);
              if (r?.success && r.result && JSON.parse(r.result) === true) {
                matched = f;
                break;
              }
            } catch {}
          }
        }

        if (matched) {
          setActiveFrameId(matched.frameId);
          result = { type: "FrameSwitched", success: true, frame_id: matched.frameId, name: null, url: matched.url, error: null };
        } else {
          result = { type: "FrameSwitched", success: false, frame_id: activeFrameId, name: null, url: null, error: "No matching frame found" };
        }
        break;
      }
      case "GetCookies": {
        const cookies = await chrome.cookies.getAll({ url: command.url });
        result = {
          type: "Cookies",
          cookies: cookies.map(c => ({
            name: c.name, value: c.value, domain: c.domain,
            path: c.path, secure: c.secure, http_only: c.httpOnly,
            same_site: c.sameSite || "unspecified",
            expiration: c.expirationDate || null,
          })),
        };
        break;
      }
      case "SetCookie": {
        try {
          await chrome.cookies.set({
            url: command.url, name: command.name, value: command.value,
            httpOnly: command.http_only || false,
            secure: command.secure || false,
          });
          result = { type: "CookieResult", success: true };
        } catch (e) {
          result = { type: "CookieResult", success: false, error: e.message };
        }
        break;
      }
      case "DeleteCookie": {
        try {
          await chrome.cookies.remove({ url: command.url, name: command.name });
          result = { type: "CookieResult", success: true };
        } catch (e) {
          result = { type: "CookieResult", success: false, error: e.message };
        }
        break;
      }
      case "ConsoleStart": {
        const tab = await findHttpTab();
        if (!tab) { result = { type: "Error", message: "No web page tab" }; break; }
        try {
          await injectConsoleMonitoring(tab.id);
          monitoringState.console.add(tab.id);
          saveMonitoringState();
          result = { type: "CommandResult", success: true, value: null, error: null };
        } catch (e) {
          result = { type: "CommandResult", success: false, value: null, error: e.message };
        }
        break;
      }
      case "ConsoleRead": {
        const tab = await findHttpTab();
        if (!tab) { result = { type: "ConsoleEntries", entries: [] }; break; }
        try {
          const r = await chrome.scripting.executeScript({
            target: { tabId: tab.id, frameIds: [0] },
            world: "MAIN",
            func: () => {
              const logs = window.__webpilot_console || [];
              return JSON.parse(JSON.stringify(logs));
            },
          });
          result = { type: "ConsoleEntries", entries: r?.[0]?.result || [] };
        } catch (e) {
          result = { type: "ConsoleEntries", entries: [] };
        }
        break;
      }
      case "ConsoleClear": {
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
        result = { type: "CommandResult", success: true, value: null, error: null };
        break;
      }
      case "NetworkStart": {
        const tab = await findHttpTab();
        if (!tab) { result = { type: "Error", message: "No web page tab" }; break; }
        try {
          await injectNetworkMonitoring(tab.id);
          monitoringState.network.add(tab.id);
          saveMonitoringState();
          result = { type: "CommandResult", success: true, value: null, error: null };
        } catch (e) {
          result = { type: "CommandResult", success: false, value: null, error: e.message };
        }
        break;
      }
      case "NetworkRead": {
        const tab = await findHttpTab();
        if (!tab) { result = { type: "NetworkLog", requests: [] }; break; }
        try {
          const r = await chrome.scripting.executeScript({
            target: { tabId: tab.id, frameIds: [0] },
            world: "MAIN",
            func: (since) => {
              const all = window.__webpilot_network || [];
              return since ? all.filter(e => e.timestamp >= since) : [...all];
            },
            args: [command.since || 0],
          });
          result = { type: "NetworkLog", requests: r?.[0]?.result || [] };
        } catch (e) {
          result = { type: "NetworkLog", requests: [] };
        }
        break;
      }
      case "NetworkClear": {
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
        result = { type: "CommandResult", success: true, value: null, error: null };
        break;
      }
      case "ExportSession": {
        try {
          const allCookies = await chrome.cookies.getAll({});
          const tab = await findHttpTab();
          let storage = { localStorage: {}, sessionStorage: {} };
          if (tab) {
            await injectBridge(tab.id);
            try {
              storage = await chrome.tabs.sendMessage(tab.id, { type: "exportStorage" }, { frameId: activeFrameId });
            } catch {}
          }
          const sessionData = {
            version: 1,
            exported_at: Date.now(),
            cookies: allCookies.map(c => ({
              name: c.name, value: c.value, domain: c.domain, path: c.path,
              secure: c.secure, http_only: c.httpOnly, same_site: c.sameSite,
              expiration: c.expirationDate || null,
            })),
            local_storage: storage.localStorage || {},
            session_storage: storage.sessionStorage || {},
          };
          // Send back as session_data (host will save to file)
          result = { type: "SessionExport", path: "", session_data: JSON.stringify(sessionData) };
        } catch (e) {
          result = { type: "Error", message: e.message };
        }
        break;
      }
      case "ImportSession": {
        try {
          const data = JSON.parse(command.data);
          // Restore cookies
          let cookieCount = 0;
          for (const c of (data.cookies || [])) {
            try {
              await chrome.cookies.set({
                url: `http${c.secure ? "s" : ""}://${c.domain.replace(/^\./, "")}${c.path}`,
                name: c.name, value: c.value,
                domain: c.domain, path: c.path,
                secure: c.secure, httpOnly: c.http_only,
                sameSite: c.same_site || "unspecified",
                expirationDate: c.expiration || undefined,
              });
              cookieCount++;
            } catch {}
          }
          // Restore storage
          const tab = await findHttpTab();
          if (tab && (data.local_storage || data.session_storage)) {
            await injectBridge(tab.id);
            try {
              await chrome.tabs.sendMessage(tab.id, {
                type: "importStorage",
                localStorage: data.local_storage || {},
                sessionStorage: data.session_storage || {},
              }, { frameId: activeFrameId });
            } catch {}
          }
          result = { type: "SessionResult", success: true, error: null };
        } catch (e) {
          result = { type: "SessionResult", success: false, error: e.message };
        }
        break;
      }
      case "SetPolicy": {
        try {
          const policies = (await chrome.storage.local.get("policies"))?.policies || {};
          policies[command.action_type] = command.verdict;
          await chrome.storage.local.set({ policies });
          result = { type: "PolicyResult", success: true, error: null };
        } catch (e) {
          result = { type: "PolicyResult", success: false, error: e.message };
        }
        break;
      }
      case "GetPolicies": {
        try {
          const policies = (await chrome.storage.local.get("policies"))?.policies || {};
          result = {
            type: "Policies",
            policies: Object.entries(policies).map(([k, v]) => ({ action_type: k, verdict: v })),
          };
        } catch (e) {
          result = { type: "Policies", policies: [] };
        }
        break;
      }
      case "ClearPolicies": {
        try {
          await chrome.storage.local.remove("policies");
          result = { type: "PolicyResult", success: true, error: null };
        } catch (e) {
          result = { type: "PolicyResult", success: false, error: e.message };
        }
        break;
      }
      case "Ping":
        result = { type: "Pong" };
        break;
      default:
        result = { type: "Error", message: `Unknown command: ${command.type}` };
    }

    nmPort?.postMessage({ id, result });
  } catch (e) {
    nmPort?.postMessage({ id, result: { type: "Error", message: e.message } });
  }
}

// --- Command Handlers ---

async function handleCapture(command) {
  let tabId;

  try {
    if (command.url) {
      // Find any tab to navigate, preferring existing http tabs
      const allTabs = await chrome.tabs.query({});
      let existingTab = allTabs.find(t => t.active && t.url?.startsWith("http"));
      if (!existingTab) existingTab = allTabs.find(t => t.url?.startsWith("http"));

      if (existingTab) {
        tabId = existingTab.id;
        await chrome.tabs.update(tabId, { url: command.url, active: true });
      } else {
        const newTab = await chrome.tabs.create({ url: command.url, active: true });
        tabId = newTab.id;
      }

      // Wait for page load — use onUpdated with URL check to skip about:blank
      await waitForTabReady(tabId, 20000);
      await sleep(500);
    } else {
      const allTabs = await chrome.tabs.query({});
      const httpTab = allTabs.find(t => t.active && t.url?.startsWith("http"))
        || allTabs.find(t => t.url?.startsWith("http"));
      if (!httpTab) {
        return { type: "Error", message: "No web page tab found. Open a web page or use --url." };
      }
      tabId = httpTab.id;
    }
  } catch (e) {
    console.error("[WebPilot] Navigation error:", e);
    return { type: "Error", message: "Navigation failed: " + e.message };
  }

  const result = {
    type: "Capture",
    dom: null,
    screenshot_path: null,
    page_url: "",
    page_title: "",
  };

  // DOM extraction via content script message passing
  if (command.dom) {
    // Force-inject the content script first (manifest auto-injection can be unreliable)
    try {
      // Inject content script
      await Promise.race([
        chrome.scripting.executeScript({
          target: { tabId },
          files: ["content/bridge.js"],
        }),
        new Promise((_, reject) => setTimeout(() => reject(new Error("inject timeout")), 5000)),
      ]);
      // injected
    } catch (e) {
      // bridge.js may already be loaded — continue
    }

    // Small delay for listener registration
    await sleep(100);

    // Extract DOM from ALL frames (main + iframes)
    try {
      const opts = { bounds: command.bounds || false, occlusion: command.occlusion || false };
      const frames = await chrome.webNavigation.getAllFrames({ tabId }).catch(() => [{ frameId: 0 }]);
      const httpFrames = frames.filter(f => f.url?.startsWith("http"));

      // Collect DOM from each frame in parallel
      const frameResults = await Promise.allSettled(
        httpFrames.map(f =>
          Promise.race([
            chrome.tabs.sendMessage(tabId, { type: "extractDOM", options: opts }, { frameId: f.frameId }),
            new Promise((_, r) => setTimeout(() => r(new Error("timeout")), 5000)),
          ]).then(dom => ({ frameId: f.frameId, url: f.url, dom }))
        )
      );

      // Merge: re-index elements across all frames
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
        const base = mainDom || frameResults.find(r => r.status === "fulfilled")?.value?.dom || {};
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

  // Text extraction
  if (command.text) {
    try {
      // Ensure bridge is injected for text extraction
      try {
        await Promise.race([
          chrome.scripting.executeScript({ target: { tabId, frameIds: [0] }, files: ["content/bridge.js"] }),
          new Promise((_, r) => setTimeout(() => r(), 3000)),
        ]);
      } catch {}
      await sleep(50);

      const textResult = await Promise.race([
        chrome.tabs.sendMessage(tabId, { type: "extractText" }, { frameId: activeFrameId }),
        new Promise((_, r) => setTimeout(() => r(new Error("timeout")), 5000)),
      ]);
      if (textResult?.text) {
        result.dom = result.dom || { elements: [], total_nodes: 0, page_url: "", page_title: "", scroll: {}, scroll_percent: 0, extraction_ms: 0 };
        result.dom.text_content = textResult.text.slice(0, 50000); // 50KB max
        result.page_url = textResult.url || result.page_url;
        result.page_title = textResult.title || result.page_title;
      }
    } catch (e) {
      console.error("[WebPilot] Text extraction error:", e.message);
    }
  }

  // Accessibility tree (CDP — shows debugger banner)
  if (command.accessibility) {
    try {
      const axResult = await withCdp(tabId, async (tid) => {
        const { nodes } = await cdpSend(tid, "Accessibility.getFullAXTree");
        return nodes;
      });
      // Save AX tree as JSON string — host will save to file
      if (result.dom) {
        result.dom.accessibility_tree = JSON.stringify(axResult);
      } else {
        result.dom = {
          elements: [], total_nodes: 0,
          page_url: result.page_url || "", page_title: result.page_title || "",
          scroll: { scroll_x: 0, scroll_y: 0, scroll_width: 0, scroll_height: 0, viewport_width: 0, viewport_height: 0 }, scroll_percent: 0, extraction_ms: 0,
          accessibility_tree: JSON.stringify(axResult),
        };
      }
    } catch (e) {
      console.error("[WebPilot] Accessibility tree error:", e.message);
    }
  }

  // Annotated screenshot: inject numbered overlays before capture
  if (command.annotate && result.dom?.elements) {
    try {
      await injectBridge(tabId);
      // Only annotate main-frame elements (iframe bounds are relative to iframe viewport, not main)
      const annotations = result.dom.elements
        .filter(el => el.in_viewport && el.bounds && el.bounds.w > 0 && el.bounds.h > 0 && !el.frame)
        .map(el => ({ index: el.index, x: el.bounds.x, y: el.bounds.y, w: el.bounds.w, h: el.bounds.h }));
      if (annotations.length > 0) {
        await chrome.tabs.sendMessage(tabId, {
          type: "addAnnotations", elements: annotations
        }, { frameId: 0 });
        await sleep(300); // Render annotations + rate limit buffer
      }
    } catch (e) {
      console.error("[WebPilot] Annotation inject error:", e.message);
    }
  }

  // Screenshot capture
  if (command.screenshot) {
    try {
      const tabInfo = await chrome.tabs.get(tabId);
      await chrome.tabs.update(tabId, { active: true });
      await chrome.windows.update(tabInfo.windowId, { focused: true });
      await sleep(200);

      console.log("[WebPilot] Screenshot mode:", command.full_page ? "fullpage" : "viewport");
      if (command.full_page) {
        // Tile-and-stitch via bridge.js messaging (not executeScript — avoids hang issues)
        // 1. Get page dimensions
        // Inject bridge
        await injectBridge(tabId);

        // Get page dimensions via bridge message (no eval — CSP safe)
        const dims = await chrome.tabs.sendMessage(tabId, { type: "getPageDims" }, { frameId: 0 })
          .catch((e) => { console.error("[WebPilot] dims error:", e.message); return null; });

        const scrollHeight = dims?.scrollHeight || 0;
        const viewportHeight = dims?.viewportHeight || 0;
        const origSX = dims?.scrollX || 0;
        const origSY = dims?.scrollY || 0;

        if (scrollHeight > 0 && viewportHeight > 0) {
          console.log("[WebPilot] Fullpage:", scrollHeight, "px,", Math.ceil(scrollHeight/viewportHeight), "tiles");
          const tiles = [];
          const tileCount = Math.ceil(scrollHeight / viewportHeight);
          const rateLimit = 2;
          const captureDelay = Math.ceil(1000 / rateLimit) + 100;

          // 2. Scroll to top
          await chrome.tabs.sendMessage(tabId, { type: "scrollTo", x: 0, y: 0 }, { frameId: 0 }).catch(() => {});
          await sleep(300);

          // 3. Capture tiles (with per-tile timeout and rate limit handling)
          for (let i = 0; i < tileCount && i < 20; i++) {
            if (i > 0) {
              await chrome.tabs.sendMessage(tabId, { type: "scrollTo", x: 0, y: i * viewportHeight }, { frameId: 0 }).catch(() => {});
            }
            // Always wait captureDelay before capture (rate limit)
            await sleep(captureDelay);
            try {
              const dataUrl = await Promise.race([
                chrome.tabs.captureVisibleTab(tabInfo.windowId, { format: "jpeg", quality: 60 }),
                new Promise((_, r) => setTimeout(() => r(new Error("capture timeout")), 5000)),
              ]);
              tiles.push(dataUrl.replace(/^data:image\/\w+;base64,/, ""));
              console.log("[WebPilot] Tile", i + 1, "/", tileCount, "captured");
            } catch (e) {
              console.error("[WebPilot] Tile", i + 1, "failed:", e.message);
              // On rate limit, wait longer and retry once
              if (e.message.includes("quota") || e.message.includes("CAPTURE")) {
                await sleep(1500);
                try {
                  const retry = await chrome.tabs.captureVisibleTab(tabInfo.windowId, { format: "jpeg", quality: 60 });
                  tiles.push(retry.replace(/^data:image\/\w+;base64,/, ""));
                  console.log("[WebPilot] Tile", i + 1, "retry OK");
                } catch (e2) {
                  console.error("[WebPilot] Tile", i + 1, "retry failed:", e2.message);
                }
              }
            }
          }

          // 4. Restore scroll
          await chrome.tabs.sendMessage(tabId, { type: "scrollTo", x: origSX, y: origSY }, { frameId: 0 }).catch(() => {});

          // Send tiles array (host will stitch)
          result.screenshot_tiles = tiles;
          result.tile_viewport_height = viewportHeight;
          result.tile_total_height = scrollHeight;
        }
      } else {
        // Single viewport screenshot
        const dataUrl = await chrome.tabs.captureVisibleTab(tabInfo.windowId, {
          format: "jpeg", quality: 80,
        });
        result.screenshot_b64 = dataUrl.replace(/^data:image\/\w+;base64,/, "");
      }
    } catch (e) {
      console.error("[WebPilot] Screenshot failed:", e.message);
      result.screenshot_error = e.message;
    }
  }

  // Clean up annotations (try/finally guarantees removal even on screenshot failure)
  if (command.annotate) {
    try {
      await chrome.tabs.sendMessage(tabId, { type: "removeAnnotations" }, { frameId: 0 });
    } catch {}
  }

  return result;
}

async function handleAction(action) {
  // Policy enforcement
  try {
    const stored = await chrome.storage.local.get("policies");
    const policies = stored?.policies || {};
    const verdict = policies[action.action];
    if (verdict === "deny") {
      return { type: "Action", success: false, error: `Action '${action.action}' denied by policy`, code: "POLICY_DENIED" };
    }
  } catch {}

  const allTabs = await chrome.tabs.query({});
  const tab = allTabs.find(t => t.active && t.url?.startsWith("http"))
    || allTabs.find(t => t.url?.startsWith("http"));
  if (!tab) return { type: "Action", success: false, error: "No web page tab found" };

  // Navigation actions handled directly by service worker
  switch (action.action) {
    case "Navigate":
      await chrome.tabs.update(tab.id, { url: action.url, active: true });
      await waitForTabReady(tab.id, 15000);
      await sleep(500);
      return { type: "Action", success: true };
    case "Back":
      await chrome.tabs.goBack(tab.id);
      await sleep(500);
      return { type: "Action", success: true };
    case "Forward":
      await chrome.tabs.goForward(tab.id);
      await sleep(500);
      return { type: "Action", success: true };
    case "Reload":
      await chrome.tabs.reload(tab.id);
      await waitForTabReady(tab.id, 15000);
      return { type: "Action", success: true };
    case "Upload":
      // File upload via CDP DOM.setFileInputFiles
      try {
        // Tag the target element with a temporary attribute
        await chrome.tabs.sendMessage(tab.id, {
          type: "tagElement", index: action.index, attr: "data-wp-upload"
        }, { frameId: activeFrameId });

        await withCdp(tab.id, async (tid) => {
          const { root } = await cdpSend(tid, "DOM.getDocument");
          const { nodeId } = await cdpSend(tid, "DOM.querySelector", {
            nodeId: root.nodeId,
            selector: "[data-wp-upload]",
          });
          if (!nodeId) throw new Error("File input element not found via CDP");
          await cdpSend(tid, "DOM.setFileInputFiles", {
            nodeId, files: [action.path],
          });
        });

        // Clean up temporary attribute
        await chrome.tabs.sendMessage(tab.id, {
          type: "untagElement", attr: "data-wp-upload"
        }, { frameId: activeFrameId }).catch(() => {});

        return { type: "Action", success: true };
      } catch (e) {
        return { type: "Action", success: false, error: e.message };
      }
  }

  // Snapshot state before action (for change detection)
  const tabsBefore = new Set((await chrome.tabs.query({})).map(t => t.id));
  const urlBefore = tab.url;

  // Ensure content script is injected, then send action
  try {
    await Promise.race([
      chrome.scripting.executeScript({ target: { tabId: tab.id }, files: ["content/bridge.js"] }),
      new Promise((_, r) => setTimeout(() => r(new Error("inject timeout")), 3000)),
    ]);
  } catch (e) { /* ignore injection issues */ }
  await sleep(100);

  try {
    const actionResult = await chrome.tabs.sendMessage(tab.id, { type: "executeAction", action }, { frameId: activeFrameId });
    const result = { type: "Action", ...actionResult };

    // Post-action: detect new tabs and URL changes
    await sleep(300);
    const tabsAfter = await chrome.tabs.query({});
    const newTabs = tabsAfter.filter(t => !tabsBefore.has(t.id) && t.url?.startsWith("http"));
    if (newTabs.length > 0) {
      // Auto-switch to new tab
      const newTab = newTabs[0];
      await chrome.tabs.update(newTab.id, { active: true });
      result.new_tab = { id: newTab.id, url: newTab.url || "", title: newTab.title || "" };
    }

    // Detect URL change (navigation triggered by action)
    try {
      const currentTab = await chrome.tabs.get(tab.id);
      if (currentTab.url && currentTab.url !== urlBefore) {
        result.url_changed = currentTab.url;
      }
    } catch {}

    return result;
  } catch (e) {
    return { type: "Action", success: false, error: e.message };
  }
}

async function handleStatus() {
  const [tab] = await chrome.tabs.query({ active: true });
  return {
    type: "Status",
    connected: true,
    tab_url: tab?.url || null,
    tab_title: tab?.title || null,
    extension_version: chrome.runtime.getManifest().version,
  };
}

async function handleListTabs() {
  const tabs = await chrome.tabs.query({});
  return {
    type: "Tabs",
    tabs: tabs.map((t) => ({
      id: t.id,
      url: t.url || "",
      title: t.title || "",
      active: t.active,
    })),
  };
}

async function handleSwitchTab(tabId) {
  await chrome.tabs.update(tabId, { active: true });
  return { type: "Action", success: true };
}

// --- Utilities ---

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function waitForTabReady(tabId, timeoutMs = 15000) {
  return new Promise((resolve) => {
    const timer = setTimeout(() => {
      chrome.tabs.onUpdated.removeListener(listener);
      resolve(); // proceed anyway
    }, timeoutMs);

    function listener(tid, changeInfo, tab) {
      if (tid !== tabId) return;
      // Wait for complete status with a real http URL (skip about:blank)
      if (changeInfo.status === "complete" && tab.url && tab.url.startsWith("http")) {
        chrome.tabs.onUpdated.removeListener(listener);
        clearTimeout(timer);
        resolve();
      }
    }
    chrome.tabs.onUpdated.addListener(listener);
  });
}

// --- Helpers ---

async function findHttpTab() {
  const allTabs = await chrome.tabs.query({});
  return allTabs.find(t => t.active && t.url?.startsWith("http"))
    || allTabs.find(t => t.url?.startsWith("http"));
}

async function injectBridge(tabId) {
  try {
    await Promise.race([
      chrome.scripting.executeScript({ target: { tabId, frameIds: [0] }, files: ["content/bridge.js"] }),
      new Promise((_, r) => setTimeout(() => r(new Error("inject timeout")), 3000)),
    ]);
  } catch (e) { /* bridge may already be loaded */ }
  await sleep(50);
}

// --- Internal message handler (from popup/sidepanel) ---
chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  if (msg.type === "status") {
    sendResponse({ connected: !!nmPort });
    return false;
  }
});

// --- Initialize ---
chrome.runtime.onInstalled.addListener(() => {
  console.log("[WebPilot] Extension installed");
  connectToHost();
});

chrome.runtime.onStartup.addListener(() => {
  console.log("[WebPilot] Chrome started");
  connectToHost();
});

// Auto-reinject console/network monitoring after page navigation
chrome.webNavigation.onCompleted.addListener(async (details) => {
  if (details.frameId !== 0) return;
  if (!details.url?.startsWith("http")) return;
  const tabId = details.tabId;
  if (monitoringState.console.has(tabId)) {
    try { await injectConsoleMonitoring(tabId); } catch {}
  }
  if (monitoringState.network.has(tabId)) {
    try { await injectNetworkMonitoring(tabId); } catch {}
  }
});

// Connect immediately
connectToHost();
