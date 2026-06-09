// // State-keeping commands: console/network monitors, cookies, session
// // export/import. Mirrors transport/local/state.rs.

import { err, exceptionErr, noPageErr, otherErr, topErr } from "./errors.js";
import { activeFrameId, monitoringState, resolveActiveTab, saveMonitoringState } from "./session.js";
import { ensureBridge, sendToContent } from "./content.js";

// Max entries each MAIN-world monitor ring buffer keeps; the install scripts
// below evict the oldest past this, and a read reports `truncated` when the
// buffer is at this cap, so the literal `500` in those scripts must match.
const MONITOR_BUFFER_CAP = 500;

// Latest console/network policy verdicts, pushed by the host alongside every
// command (the service worker never reads the policy store — the host is the
// sole sink). A denied monitor is NOT re-armed after a navigation, mirroring
// headless `reinstall_monitors`, which re-checks `enforce(ConsoleStart /
// NetworkStart)` before re-injecting: so an `eval` deny stops the MAIN-world
// hooks in BOTH modes, not just headless. The armed set is kept untouched, so
// re-allowing `eval` re-arms on the next navigation — same as the headless flag.
let monitorPolicy = { console: true, network: true };
function setMonitorPolicy(mp) {
  monitorPolicy = {
    console: mp?.console !== false,
    network: mp?.network !== false,
  };
}

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
  // `&& monitorPolicy.X`: re-injecting a MAIN-world hook is the same effect
  // `console start` / `network start` are gated on (`eval`), so a deny that
  // landed after arming must stop the re-arm too — exactly as headless
  // `reinstall_monitors` re-checks the gate. The armed set is left intact.
  if (monitoringState.console.has(tabId) && monitorPolicy.console) {
    try {
      await injectConsoleMonitoring(tabId);
    } catch {}
  }
  if (monitoringState.network.has(tabId) && monitorPolicy.network) {
    try {
      await injectNetworkMonitoring(tabId);
    } catch {}
  }
}

async function injectConsoleMonitoring(tabId) {
  await chrome.scripting.executeScript({
    target: { tabId, frameIds: [0] },
    world: "MAIN",
    args: [MONITOR_BUFFER_CAP],
    func: (cap) => {
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
          if (window.__webpilot_console.length > cap) window.__webpilot_console.shift();
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
    args: [MONITOR_BUFFER_CAP],
    func: (cap) => {
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
        // Record the request in-flight immediately (no status, duration 0) so a
        // read DURING a slow request sees it instead of an empty buffer; fill in
        // status/error/duration on completion by mutating this same entry.
        const entry = { type: "fetch", url, method, duration_ms: 0, timestamp: Date.now() };
        window.__webpilot_network.push(entry);
        if (window.__webpilot_network.length > cap) window.__webpilot_network.shift();
        return origFetch.apply(this, args).then((response) => {
          entry.status = response.status;
          entry.duration_ms = Math.round(performance.now() - t0);
          // Re-stamp at completion: `--since` polling filters on timestamp, and the
          // start time the entry carried while in-flight sits before a cursor taken
          // after the request began, which would hide the resolved entry from a
          // poller. A plain read (no `since`) shows it either way.
          entry.timestamp = Date.now();
          return response;
        }).catch((err) => {
          entry.error = err.message;
          entry.duration_ms = Math.round(performance.now() - t0);
          entry.timestamp = Date.now();
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
        const meta = xhrMeta.get(this) || {};
        // Record in-flight at send (no status, duration 0), updated on loadend —
        // so an in-flight XHR is visible to a read, like fetch.
        const entry = { type: "xhr", url: meta.url || "", method: meta.method || "GET", duration_ms: 0, timestamp: Date.now() };
        window.__webpilot_network.push(entry);
        if (window.__webpilot_network.length > cap) window.__webpilot_network.shift();
        // status===0 covers abort, timeout AND network/CORS failure alike, so
        // read the actual terminal event rather than labelling every one a
        // "Network error" — a request the page itself cancelled is not one.
        let terminalError;
        this.addEventListener("abort", () => { terminalError = "aborted"; }, { once: true });
        this.addEventListener("timeout", () => { terminalError = "timeout"; }, { once: true });
        this.addEventListener("error", () => { terminalError = "Network error"; }, { once: true });
        this.addEventListener("loadend", () => {
          entry.status = this.status || undefined;
          entry.error = terminalError;
          entry.duration_ms = Math.round(performance.now() - t0);
          entry.timestamp = Date.now();
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
  // The cookie URL must be http(s), as the headless CDP `Network.setCookie`
  // enforces — surface it as a typed InvalidArgument (exit 7), not the less
  // specific `chrome.cookies.set` exception (which would read as a generic
  // failure with a different code in browser mode only).
  if (!/^https?:\/\//i.test(command.url || "")) {
    return {
      type: "CookieResult",
      success: false,
      error: err("InvalidArgument", "cookie url must have scheme http or https"),
    };
  }
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
      args: [since || 0, ["log", "warn", "error", "info", "debug"], MONITOR_BUFFER_CAP],
      // Filter by `timestamp >= since` (the incremental cursor) AND sanitize to
      // the same shape headless returns: drop any entry whose `level` is not a
      // known ConsoleLevel (the MAIN-world buffer is page-reachable and only
      // best-effort), and coerce `message` to a string — so the CLI deserializes
      // an identical `Vec<ConsoleEntry>` in both modes and a tampered entry can't
      // break the read or leak a wire shape headless would never emit. `truncated`
      // reports a full buffer (older entries possibly evicted), like headless.
      func: (s, levels, cap) => {
        const all = window.__webpilot_console || [];
        return {
          entries: all
            .filter((e) => e && levels.includes(e.level) && e.timestamp >= s)
            .map((e) => ({
              level: e.level,
              message: typeof e.message === "string" ? e.message : "",
              // Coerce a non-numeric timestamp to 0 rather than forward a string
              // the CLI can't deserialize into `u64` — headless does the same via
              // `as_u64().unwrap_or(0)`, so a tampered entry yields 0, not a
              // malformed-reply error.
              timestamp: typeof e.timestamp === "number" ? e.timestamp : 0,
            })),
          truncated: all.length >= cap,
        };
      },
    });
    const out = r?.[0]?.result || { entries: [], truncated: false };
    return { type: "ConsoleEntries", entries: out.entries, truncated: out.truncated };
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
      // Filter by `timestamp >= since` AND sanitize to the same shape headless
      // returns: drop any entry missing a required NetworkEntry field (the
      // MAIN-world buffer is page-reachable and best-effort), so the CLI
      // deserializes an identical `Vec<NetworkEntry>` in both modes and a tampered
      // entry can't break the read. `status`/`error` stay optional.
      func: (s, cap) => {
        const all = window.__webpilot_network || [];
        return {
          entries: all.filter(
            (e) =>
              e &&
              e.timestamp >= s &&
              typeof e.type === "string" &&
              typeof e.url === "string" &&
              typeof e.method === "string" &&
              typeof e.duration_ms === "number" &&
              typeof e.timestamp === "number",
          ),
          truncated: all.length >= cap,
        };
      },
      args: [since || 0, MONITOR_BUFFER_CAP],
    });
    const out = r?.[0]?.result || { entries: [], truncated: false };
    return { type: "NetworkEntries", entries: out.entries, truncated: out.truncated };
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
    // No http page (e.g. only chrome://newtab focused) means Web Storage can't be
    // read at all — fail rather than write a session file with silently empty
    // localStorage/sessionStorage that re-imports as data loss. Headless always
    // has a bound page and so always reads it; this matches that.
    if (!tab) {
      return topErr(noPageErr());
    }
    // A storage read that fails or comes back as a typed bridge error must
    // fail the export (headless parity): writing empty storage would import
    // back as silent data loss. A valid read carries a `localStorage` object.
    await ensureBridge(tab.id, activeFrameId);
    const s = await sendToContent(tab.id, { type: "exportStorage" }, activeFrameId);
    if (!s || s.success === false || !s.localStorage) {
      return topErr(s && s.error ? s.error : otherErr("storage export failed"));
    }
    const storage = s;
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
    // Web Storage import needs an http page to run in. If the file carries
    // storage but no such page is active (e.g. a chrome:// pin), fail up front —
    // don't import the cookies and then silently drop the storage. The NoPage
    // sibling of session export's own guard.
    const hasStorage = Object.keys(data.local_storage || {}).length > 0
      || Object.keys(data.session_storage || {}).length > 0;
    const tab = await resolveActiveTab();
    if (hasStorage && !tab) {
      return { type: "SessionResult", success: false, error: noPageErr() };
    }
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
    if (tab && hasStorage) {
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

export { handleConsoleClear, handleConsoleRead, handleConsoleStart, handleCookieDelete, handleCookieList, handleCookieSet, handleNetworkClear, handleNetworkRead, handleNetworkStart, handleSessionExport, handleSessionImport, injectConsoleMonitoring, injectNetworkMonitoring, rearmMonitors, setMonitorPolicy };
