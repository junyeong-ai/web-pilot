// // State-keeping commands: console/network monitors, cookies, session
// // export/import. Mirrors transport/local/state.rs.

import { err, exceptionErr, noPageErr, otherErr, topErr } from "./errors.js";
import { activeFrameId, monitoringState, resolveActiveTab, saveMonitoringState } from "./session.js";
import { ensureBridge, sendToContent } from "./content.js";

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
  const tab = await resolveActiveTab();
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
  const tab = await resolveActiveTab();
  if (!tab) return topErr(noPageErr());
  try {
    const r = await chrome.scripting.executeScript({
      target: { tabId: tab.id, frameIds: [0] },
      world: "MAIN",
      func: () => JSON.parse(JSON.stringify(window.__webpilot_console || [])),
    });
    return { type: "ConsoleEntries", entries: r?.[0]?.result || [] };
  } catch (e) {
    // A scripting failure means we could not read the buffer — surface it
    // typed instead of reporting an empty (but successful) read, which would
    // hide the failure exactly as headless never does (it propagates).
    return topErr(exceptionErr(e));
  }
}

async function handleConsoleClear() {
  const tab = await resolveActiveTab();
  if (!tab) return topErr(noPageErr());
  try {
    await chrome.scripting.executeScript({
      target: { tabId: tab.id, frameIds: [0] },
      world: "MAIN",
      func: () => { window.__webpilot_console = []; },
    });
    return { type: "CommandResult", success: true };
  } catch (e) {
    return topErr(exceptionErr(e));
  }
}

async function handleNetworkStart() {
  const tab = await resolveActiveTab();
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
  const tab = await resolveActiveTab();
  if (!tab) return topErr(noPageErr());
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
  } catch (e) {
    return topErr(exceptionErr(e));
  }
}

async function handleNetworkClear() {
  const tab = await resolveActiveTab();
  if (!tab) return topErr(noPageErr());
  try {
    await chrome.scripting.executeScript({
      target: { tabId: tab.id, frameIds: [0] },
      world: "MAIN",
      func: () => { window.__webpilot_network = []; },
    });
    return { type: "CommandResult", success: true };
  } catch (e) {
    return topErr(exceptionErr(e));
  }
}

// ── Session ────────────────────────────────────────────────────────────────

async function handleSessionExport() {
  try {
    const all = await chrome.cookies.getAll({});
    const tab = await resolveActiveTab();
    let storage = { localStorage: {}, sessionStorage: {} };
    if (tab) {
      // A storage read that fails or comes back as a typed bridge error must
      // fail the export (headless parity): writing empty storage would import
      // back as silent data loss. A valid read carries a `localStorage` object.
      await ensureBridge(tab.id, activeFrameId);
      const s = await sendToContent(tab.id, { type: "exportStorage" }, activeFrameId);
      if (!s || s.success === false || !s.localStorage) {
        return topErr(s && s.error ? s.error : otherErr("storage export failed"));
      }
      storage = s;
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
    // `cookies`, when present, must be an array — a string would iterate
    // character by character, and a null is a malformed present value. Use
    // `hasOwn` (not `!= null`) so a present `null` is rejected too, exactly as
    // headless do_session_import treats `Some(Null)`.
    if (Object.hasOwn(data, "cookies") && !Array.isArray(data.cookies)) {
      return { type: "SessionResult", success: false, error: err("InvalidArgument", "session `cookies` must be an array") };
    }
    let cookiesTotal = 0;
    let cookiesFailed = 0;
    for (const c of data.cookies || []) {
      cookiesTotal++;
      try {
        await chrome.cookies.set({
          url: `http${c.secure ? "s" : ""}://${c.domain.replace(/^\./, "")}${c.path}`,
          name: c.name, value: c.value, domain: c.domain, path: c.path,
          secure: c.secure, httpOnly: c.http_only,
          sameSite: chromeSameSite(c.same_site),
          expirationDate: c.expiration || undefined,
        });
      } catch {
        cookiesFailed++;
      }
    }
    const tab = await resolveActiveTab();
    if (tab && (data.local_storage || data.session_storage)) {
      // A storage import failure is propagated, not swallowed (headless
      // parity): the caller asked to restore it and must know it didn't.
      await ensureBridge(tab.id, activeFrameId);
      const r = await sendToContent(tab.id, {
        type: "importStorage",
        localStorage: data.local_storage || {},
        sessionStorage: data.session_storage || {},
      }, activeFrameId);
      if (r && r.success === false) {
        return { type: "SessionResult", success: false, error: r.error || otherErr("storage import failed") };
      }
    }
    // A well-formed cookie the browser refused to set is a partial failure the
    // agent must see — never a success that imported less than the file held.
    if (cookiesFailed > 0) {
      return {
        type: "SessionResult",
        success: false,
        error: otherErr(`${cookiesFailed} of ${cookiesTotal} cookies failed to set`),
      };
    }
    return { type: "SessionResult", success: true };
  } catch (e) {
    return { type: "SessionResult", success: false, error: exceptionErr(e) };
  }
}

export { handleConsoleClear, handleConsoleRead, handleConsoleStart, handleCookieDelete, handleCookieList, handleCookieSet, handleNetworkClear, handleNetworkRead, handleNetworkStart, handleSessionExport, handleSessionImport, injectConsoleMonitoring, injectNetworkMonitoring };
