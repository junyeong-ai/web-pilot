// // State-keeping commands: console/network monitors, cookies, session
// // export/import. Mirrors transport/local/state.rs.

import { err, exceptionErr, noPageErr, otherErr, topErr } from "./errors.js";
import { activeFrameId, monitoringState, resolveActiveTab, saveMonitoringState } from "./session.js";
import { ensureBridge, sendToContent } from "./content.js";

// ── Console / network monitoring injection ─────────────────────────────────

// Re-arm any ARMED console/network hooks on `tabId`'s new main document — the
// MAIN-world fetch/console patches, independent of the bridge. Headless calls
// `reinstall_monitors()` the instant a navigation settles; browser must do the
// same right after `waitNavigationSettled`, not wait for the `load`-time
// `webNavigation.onCompleted`, or a fetch/console the new page emits after
// DOMContentLoaded but before a slow `load` is lost from the buffer. A no-op
// unless the tab is actually being monitored, so a plain navigation pays
// nothing.
async function rearmMonitors(tabId) {
  if (monitoringState.console.has(tabId)) {
    try {
      await injectConsoleMonitoring(tabId);
    } catch {}
  }
  if (monitoringState.network.has(tabId)) {
    try {
      await injectNetworkMonitoring(tabId);
    } catch {}
  }
}

async function injectConsoleMonitoring(tabId) {
  await chrome.scripting.executeScript({
    target: { tabId, frameIds: [0] },
    world: "MAIN",
    func: () => {
      // Gating on `__webpilot_console` alone fails after `console clear` (an
      // empty array is truthy, so the patch wouldn't reinstall) and double-wraps
      // if the buffer is cleared out of band. A separate sentinel keeps `start`
      // idempotent without that hazard — the headless CONSOLE_INSTALL_JS design.
      if (!Array.isArray(window.__webpilot_console)) window.__webpilot_console = [];
      if (window.__webpilot_console_patched) return;
      window.__webpilot_console_patched = true;
      const orig = { log: console.log, error: console.error, warn: console.warn, info: console.info, debug: console.debug };
      ["log", "error", "warn", "info", "debug"].forEach((m) => {
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
        // `fetch` accepts a string/URL or a Request object. A Request carries its
        // own url and method, which a `config` override can still trump. Reading
        // `String(resource)` would log "[object Request]" and lose the method.
        const isReq = typeof Request !== "undefined" && resource instanceof Request;
        const url = isReq ? resource.url : String(resource);
        const method = config?.method || (isReq ? resource.method : "GET");
        const t0 = performance.now();
        return origFetch.apply(this, args).then((response) => {
          window.__webpilot_network.push({
            type: "fetch", url, method,
            status: response.status, duration_ms: Math.round(performance.now() - t0),
            timestamp: Date.now(),
          });
          if (window.__webpilot_network.length > 500) window.__webpilot_network.shift();
          return response;
        }).catch((err) => {
          window.__webpilot_network.push({
            type: "fetch", url, method,
            error: err.message, duration_ms: Math.round(performance.now() - t0),
            timestamp: Date.now(),
          });
          if (window.__webpilot_network.length > 500) window.__webpilot_network.shift();
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
  const info = {
    name: c.name, value: c.value, domain: c.domain, path: c.path,
    secure: c.secure, http_only: c.httpOnly,
    same_site: c.sameSite === "no_restriction" ? "none" : (c.sameSite || "unspecified").toLowerCase(),
    // Carry host-only scope so a round-trip can't widen the cookie to
    // subdomains. chrome.cookies exposes it directly (unlike CDP).
    host_only: c.hostOnly === true,
  };
  // Omit `expiration` for a session cookie rather than writing null, so an
  // exported session file is byte-identical across modes (headless's
  // CookieInfo skips the field when absent).
  if (c.expirationDate != null) info.expiration = c.expirationDate;
  return info;
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

async function handleConsoleRead(since) {
  const tab = await resolveActiveTab();
  if (!tab) return topErr(noPageErr());
  try {
    const r = await chrome.scripting.executeScript({
      target: { tabId: tab.id, frameIds: [0] },
      world: "MAIN",
      args: [since || 0],
      // Filter by `timestamp >= since` in-page (the incremental cursor), exactly
      // as the network read does, then deep-clone for structured transfer.
      func: (s) => JSON.parse(JSON.stringify((window.__webpilot_console || []).filter((e) => e.timestamp >= s))),
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
  let data;
  try {
    data = JSON.parse(rawData);
  } catch (e) {
    // A malformed session file is bad input, not an internal fault — return
    // InvalidArgument so the code matches headless do_session_import (a serde
    // parse error maps to InvalidArgument, exit 7); an agent keys its retry off
    // the one code. Without this the outer catch would mislabel it `Other`.
    return { type: "SessionResult", success: false, error: err("InvalidArgument", `session JSON parse error: ${e.message}`) };
  }
  try {
    // `cookies`, when present, must be an array — a string would iterate
    // character by character, and a null is a malformed present value. Use
    // `hasOwn` (not `!= null`) so a present `null` is rejected too, exactly as
    // headless do_session_import treats `Some(Null)`.
    if (Object.hasOwn(data, "cookies") && !Array.isArray(data.cookies)) {
      return { type: "SessionResult", success: false, error: err("InvalidArgument", "session `cookies` must be an array") };
    }
    const cookies = data.cookies || [];
    const cookiesTotal = cookies.length;
    let cookiesFailed = 0;
    let cookiesMalformed = 0;
    for (const c of cookies) {
      // A row headless would drop when deserializing CookieInfo is counted, not
      // silently skipped — losing a cookie while reporting success hands the
      // agent a session quietly missing part of what the file held. CookieInfo
      // requires name/value/domain/path strings AND a same_site that parses as
      // the SameSite enum — match both, exactly. (`cookiesFailed` is separate: a
      // well-formed cookie the browser actually refused.)
      const SAME_SITE = ["strict", "lax", "none", "no_restriction", "unspecified"];
      if (c == null || typeof c.name !== "string" || typeof c.value !== "string"
          || typeof c.domain !== "string" || typeof c.path !== "string"
          || !SAME_SITE.includes(c.same_site)) {
        cookiesMalformed++;
        continue;
      }
      try {
        await chrome.cookies.set({
          url: `http${c.secure ? "s" : ""}://${c.domain.replace(/^\./, "")}${c.path}`,
          name: c.name, value: c.value, path: c.path,
          // A host-only cookie is set by URL with no `domain`, so Chrome scopes
          // it to exactly its host and the round-trip can't widen it to
          // subdomains. A domain cookie keeps its explicit `domain`.
          ...(c.host_only ? {} : { domain: c.domain }),
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
    // A cookie the browser refused, or a malformed row that couldn't be parsed,
    // is a partial failure the agent must see — never a success that imported
    // less than the file held. Mirrors headless do_session_import exactly.
    if (cookiesFailed > 0 || cookiesMalformed > 0) {
      const reasons = [];
      if (cookiesFailed > 0) reasons.push(`${cookiesFailed} refused by the browser`);
      if (cookiesMalformed > 0) reasons.push(`${cookiesMalformed} malformed`);
      return {
        type: "SessionResult",
        success: false,
        error: otherErr(
          `${cookiesFailed + cookiesMalformed} of ${cookiesTotal} cookies not imported (${reasons.join(", ")})`,
        ),
      };
    }
    return { type: "SessionResult", success: true };
  } catch (e) {
    return { type: "SessionResult", success: false, error: exceptionErr(e) };
  }
}

export { handleConsoleClear, handleConsoleRead, handleConsoleStart, handleCookieDelete, handleCookieList, handleCookieSet, handleNetworkClear, handleNetworkRead, handleNetworkStart, handleSessionExport, handleSessionImport, injectConsoleMonitoring, injectNetworkMonitoring, rearmMonitors };
