// State-keeping commands: console/network monitors, cookies, session
// export/import. Mirrors transport/local/state.rs.

import { err, exceptionErr, noPageErr, otherErr, topErr } from "./errors.js";
import { activeFrameId, monitoringState, resolveActiveTab, saveMonitoringState } from "./session.js";
import { ensureBridge, frameVanishedError, sendToContent } from "./content.js";

// Max entries each MAIN-world monitor ring buffer keeps; the install scripts
// below evict the oldest past this, and a read reports `truncated` when the
// buffer is at this cap, so the literal `500` in those scripts must match.
const MONITOR_BUFFER_CAP = 500;

// Session export schema version, enforced on import (a higher version is rejected
// rather than half-applied). Mirrors headless SESSION_SCHEMA_VERSION.
const SESSION_SCHEMA_VERSION = 1;

// Latest console/network policy verdicts, pushed by the host alongside every
// command (the service worker never reads the policy store — the host is the
// sole sink). A denied monitor is NOT re-armed after a navigation, mirroring
// headless `reinstall_monitors`, which re-checks `enforce(ConsoleStart /
// NetworkStart)` before re-injecting: so an `eval` deny stops the MAIN-world
// hooks in BOTH modes, not just headless. The armed set is kept untouched, so
// re-allowing `eval` re-arms on the next navigation — same as the headless flag.
//
// Default DENY (fail-closed): an MV3 service worker is evicted when idle, and a
// navigation's `onCompleted` can fire after the relaunch but before the host has
// pushed a fresh verdict. Defaulting to allow would re-inject the MAIN-world
// hooks during that warmup window even under an `eval` deny — a fail-OPEN gap
// headless never has (it reads the live store on every re-install). Re-arm stays
// blocked until the first command carries the real verdict.
let monitorPolicy = { console: false, network: false };
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
// DOMContentLoaded but before a slow `load` is lost from the buffer. The armed
// intent is agent-level (headless parity), so this is also the whole of "the
// monitor follows the pin": every pin move (tab switch / new / popup adoption)
// re-arms the new working tab. A no-op unless armed, so a plain navigation
// pays nothing.
async function rearmMonitors(tabId) {
  // `&& monitorPolicy.X`: re-injecting a MAIN-world hook is the same effect
  // `console start` / `network start` are gated on (`eval`), so a deny that
  // landed after arming must stop the re-arm too — exactly as headless
  // `reinstall_monitors` re-checks the gate. The armed flag is left intact.
  if (monitoringState.console && monitorPolicy.console) {
    try {
      await injectConsoleMonitoring(tabId);
    } catch {}
  }
  if (monitoringState.network && monitorPolicy.network) {
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
      // Capture Date.now at install (a page that booby-traps it later can't
      // break the recording) and clip a captured message like the DOM capture;
      // the recording is wrapped so it can never break the page's own console
      // call. Headless CONSOLE_INSTALL_JS parity.
      const nowFn = Date.now;
      const MAX = 4096;
      // CODEPOINT-safe clip via Array.from (a bare slice can split an astral
      // pair into a lone surrogate that breaks the entry's serialization) —
      // headless CONSOLE_INSTALL_JS parity, same bar as bridge.js's clip.
      const clip = (s) => { if (s.length <= MAX) return s; const cps = Array.from(s); return cps.length > MAX ? cps.slice(0, MAX).join("") + "…[" + cps.length + " chars]" : s; };
      const orig = { log: console.log, error: console.error, warn: console.warn, info: console.info, debug: console.debug };
      ["log", "error", "warn", "info", "debug"].forEach((m) => {
        console[m] = (...args) => {
          try {
            const msg = clip(args.map((a) => { try { return String(a); } catch { return "[object]"; } }).join(" "));
            const buf = window.__webpilot_console;
            buf.push({ level: m, message: msg, timestamp: nowFn() });
            // `truncated` is driven by this eviction flag, not `length >= cap`.
            if (buf.length > cap) { buf.shift(); window.__webpilot_console_dropped = true; }
          } catch {}
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
      // Intrinsics captured at install (a page that booby-traps Date.now /
      // performance.now later can't break the recording), a clip like the DOM
      // capture (a giant data: URL must not balloon the buffer/payload), and
      // every recording wrapped so it can never break the page's own fetch/XHR.
      // Headless NETWORK_INSTALL_JS parity.
      const nowFn = Date.now;
      const perfObj = performance;
      const perfNowRaw = perfObj.now;
      const perfNow = () => { try { return perfNowRaw.call(perfObj); } catch { return 0; } };
      const MAX = 4096;
      // CODEPOINT-safe clip (a lone surrogate breaks serialization — see console).
      const clip = (s) => { if (s.length <= MAX) return s; const cps = Array.from(s); return cps.length > MAX ? cps.slice(0, MAX).join("") + "…[" + cps.length + " chars]" : s; };
      const origFetch = window.fetch;
      window.fetch = function (...args) {
        let entry = null, t0 = 0;
        try {
          const [resource, config] = args;
          // `fetch` accepts a string/URL or a Request object. A Request carries its
          // own url and method, which a `config` override can still trump. Reading
          // `String(resource)` would log "[object Request]" and lose the method.
          const isReq = typeof Request !== "undefined" && resource instanceof Request;
          const url = isReq ? resource.url : String(resource);
          const method = config?.method || (isReq ? resource.method : "GET");
          t0 = perfNow();
          // Record the request in-flight immediately (no status, duration 0) so a
          // read DURING a slow request sees it instead of an empty buffer; fill in
          // status/error/duration on completion by mutating this same entry.
          entry = { type: "fetch", url: clip(url), method, duration_ms: 0, timestamp: nowFn() };
          const buf = window.__webpilot_network;
          buf.push(entry);
          if (buf.length > cap) { buf.shift(); window.__webpilot_network_dropped = true; }
        } catch { entry = null; }
        // origFetch can throw SYNCHRONOUSLY (`fetch()` with no args is a
        // TypeError, not a rejected promise). Stamp the entry errored instead of
        // leaving it in-flight forever, then rethrow so the page sees it.
        let p;
        try {
          p = origFetch.apply(this, args);
        } catch (e) {
          if (entry) { try { entry.error = String(e && e.message || e); entry.duration_ms = Math.round(perfNow() - t0); entry.timestamp = nowFn(); } catch {} }
          throw e;
        }
        if (!entry) return p;
        return p.then((response) => {
          // Re-stamp at completion: `--since` polling filters on timestamp, and the
          // start time the entry carried while in-flight sits before a cursor taken
          // after the request began, which would hide the resolved entry from a
          // poller. A plain read (no `since`) shows it either way.
          try { entry.status = response.status; entry.duration_ms = Math.round(perfNow() - t0); entry.timestamp = nowFn(); } catch {}
          return response;
        }).catch((err) => {
          try { entry.error = err.message; entry.duration_ms = Math.round(perfNow() - t0); entry.timestamp = nowFn(); } catch {}
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
        try { xhrMeta.set(this, { method: m, url: String(u) }); } catch {}
        return origOpen.apply(this, [m, u, ...a]);
      };
      xhrProto.send = function (...a) {
        let entry = null, t0 = 0;
        try {
          t0 = perfNow();
          const meta = xhrMeta.get(this) || {};
          // Record in-flight at send (no status, duration 0), updated on loadend —
          // so an in-flight XHR is visible to a read, like fetch.
          entry = { type: "xhr", url: clip(meta.url || ""), method: meta.method || "GET", duration_ms: 0, timestamp: nowFn() };
          const buf = window.__webpilot_network;
          buf.push(entry);
          if (buf.length > cap) { buf.shift(); window.__webpilot_network_dropped = true; }
          // status===0 covers abort, timeout AND network/CORS failure alike, so
          // read the actual terminal event rather than labelling every one a
          // "Network error" — a request the page itself cancelled is not one.
          let terminalError;
          this.addEventListener("abort", () => { terminalError = "aborted"; }, { once: true });
          this.addEventListener("timeout", () => { terminalError = "timeout"; }, { once: true });
          this.addEventListener("error", () => { terminalError = "Network error"; }, { once: true });
          this.addEventListener("loadend", () => {
            try { entry.status = this.status || undefined; entry.error = terminalError; entry.duration_ms = Math.round(perfNow() - t0); entry.timestamp = nowFn(); } catch {}
          }, { once: true });
        } catch { entry = null; }
        return origSend.apply(this, a);
      };
    },
  });
}

// ── Cookies ────────────────────────────────────────────────────────────────

async function handleCookieList(url) {
  // Same guard as `handleCookieSet`: a malformed URL must be the typed
  // InvalidArgument (exit 7) headless CDP returns, not the `Other` (exit 1) a
  // raw chrome.cookies throw would read as — one rejection across commands
  // and modes.
  if (!isHttpUrl(url)) {
    return topErr(err("InvalidArgument", "cookie url must be a valid http or https URL"));
  }
  // `partitionKey: {}` spans partitioned AND unpartitioned cookies — a bare
  // `getAll({url})` silently omits CHIPS partitioned cookies, so the same page
  // would list a partitioned auth cookie in headless and report it absent
  // here. Same form the session export uses.
  const cookies = await chrome.cookies.getAll({ url, partitionKey: {} });
  return {
    type: "Cookies",
    cookies: cookies.map(toCookieInfo),
  };
}

// A well-formed http(s) URL. Parses (not just prefix-matches) so a bare scheme
// like `http://` is rejected, matching what CDP `Network.setCookie` refuses.
function isHttpUrl(url) {
  try {
    const u = new URL(url || "");
    return u.protocol === "http:" || u.protocol === "https:";
  } catch {
    return false;
  }
}

const COOKIE_REFUSED =
  "Chrome refused to set the cookie — common causes: SameSite=None without --secure, a __Host-/__Secure- name that doesn't meet the prefix rules, or an invalid domain/value";

async function handleCookieSet(command) {
  // The cookie URL must be a valid http(s) URL, as the headless CDP
  // `Network.setCookie` enforces — it rejects a malformed URL (`http://` with no
  // host, a bare scheme) as InvalidArgument (exit 7). A prefix-only regex would
  // pass `http://` and let `chrome.cookies.set` throw a generic exception that
  // reads as `Other` (exit 1) in browser mode only. Parse it so the rejection —
  // and its exit code — matches headless for every malformed URL, not just a
  // missing scheme.
  if (!isHttpUrl(command.url)) {
    return {
      type: "CookieResult",
      success: false,
      error: err("InvalidArgument", "cookie url must be a valid http or https URL"),
    };
  }
  try {
    const params = {
      url: command.url,
      name: command.name,
      value: command.value,
      httpOnly: command.http_only || false,
      secure: command.secure || false,
    };
    // `unspecified` (and an omitted flag) means "no SameSite attribute" — leave
    // it off so Chrome applies its default. Mirrors headless do_cookie_set.
    if (command.same_site && command.same_site !== "unspecified") {
      params.sameSite = chromeSameSite(command.same_site);
    }
    // Absolute Unix-epoch expiry; omitted = a session cookie. Mirrors headless.
    if (command.expires != null) params.expirationDate = command.expires;
    const set = await chrome.cookies.set(params);
    // Reporting success when Chrome refused the cookie would hide a
    // silently-unset auth cookie. `chrome.cookies.set` can defensively resolve
    // null on refusal (the throw path is in the catch below). Mirrors the
    // headless `Network.setCookie` `success:false` check.
    if (!set) {
      return {
        type: "CookieResult",
        success: false,
        error: err("InvalidArgument", COOKIE_REFUSED),
      };
    }
    return { type: "CookieResult", success: true };
  } catch {
    // Chrome refuses some cookies by THROWING ("Failed to parse or set
    // cookie") — SameSite=None without Secure, a `__Host-`/`__Secure-` prefix
    // violation, an invalid domain/value. The URL is already validated above,
    // so the set is the only thing that throws here. That is an invalid cookie
    // spec, not an infra fault, so report InvalidArgument (exit 7) to match the
    // headless `Network.setCookie` `success:false` path — never Other (exit 1).
    return { type: "CookieResult", success: false, error: err("InvalidArgument", COOKIE_REFUSED) };
  }
}

async function handleCookieDelete(command) {
  // Same guard as `handleCookieSet` — see handleCookieList.
  if (!isHttpUrl(command.url)) {
    return {
      type: "CookieResult",
      success: false,
      error: err("InvalidArgument", "cookie url must be a valid http or https URL"),
    };
  }
  try {
    // List first (headless parity): a delete of a cookie that does not exist
    // is the typed CookieNotFound, never a silent success — and same-name
    // cookies coexist across scopes (a `.domain` legacy cookie beside a
    // host-only one, different paths), so EVERY matching scope is removed via
    // a per-cookie reconstructed URL (chrome.cookies.remove cannot target
    // domain/path directly) and the count reported.
    // `partitionKey: {}` so a CHIPS partitioned cookie is FOUND (a bare getAll
    // omits it → a false CookieNotFound for a cookie that exists)…
    const matches = await chrome.cookies.getAll({
      url: command.url, name: command.name, partitionKey: {},
    });
    if (matches.length === 0) {
      return {
        type: "CookieResult",
        success: false,
        error: err("CookieNotFound", `Cookie not found: ${command.name}. List: webpilot cookie list URL`, { name: command.name }),
      };
    }
    for (const c of matches) {
      const host = c.domain.startsWith(".") ? c.domain.slice(1) : c.domain;
      const cookieUrl = `${c.secure ? "https" : "http"}://${host}${c.path}`;
      // …and the matched cookie's own key rides the remove: without it Chrome
      // targets the unpartitioned namespace, so the partitioned cookie would
      // survive a reported "Deleted 1" (headless threads the key identically).
      await chrome.cookies.remove({
        url: cookieUrl, name: command.name, storeId: c.storeId,
        ...(c.partitionKey ? { partitionKey: c.partitionKey } : {}),
      });
    }
    return { type: "CookieResult", success: true, deleted: matches.length };
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
  // CHIPS: carry the partition key — it is part of the cookie's IDENTITY, so a
  // round-trip that dropped it would re-import an unpartitioned twin the
  // partitioned (embedded) context never sends. Both fields always written,
  // matching headless CookieInfo's serialization field-for-field.
  if (c.partitionKey && typeof c.partitionKey.topLevelSite === "string") {
    info.partition_key = {
      top_level_site: c.partitionKey.topLevelSite,
      has_cross_site_ancestor: c.partitionKey.hasCrossSiteAncestor === true,
    };
  }
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
    monitoringState.console = true;
    saveMonitoringState();
    return { type: "CommandResult", success: true };
  } catch (e) {
    return { type: "CommandResult", success: false, error: exceptionErr(e) };
  }
}

async function handleConsoleRead(since) {
  const tab = await resolveActiveTab();
  if (!tab) return topErr(noPageErr());
  // Reading before `console start` would return an empty buffer (success) —
  // indistinguishable from "the page logged nothing" — so an agent could
  // conclude there were no messages when the monitor was simply never armed.
  // Fail loud, matching headless do_console_read.
  if (!monitoringState.console) {
    return topErr(
      err("InvalidArgument", "console monitoring is not active — run `webpilot console start` first"),
    );
  }
  try {
    const r = await chrome.scripting.executeScript({
      target: { tabId: tab.id, frameIds: [0] },
      world: "MAIN",
      args: [since || 0, ["log", "warn", "error", "info", "debug"]],
      // Filter by `timestamp >= since` (the incremental cursor) AND sanitize to
      // the same shape headless returns: drop any entry whose `level` is not a
      // known ConsoleLevel (the MAIN-world buffer is page-reachable and only
      // best-effort), and coerce `message` to a string — so the CLI deserializes
      // an identical `Vec<ConsoleEntry>` in both modes and a tampered entry can't
      // break the read or leak a wire shape headless would never emit. `truncated`
      // is the eviction flag (older entries actually dropped), like headless.
      func: (s, levels) => {
        // `undefined` (no hook in THIS document — the re-arm was suppressed by
        // an eval policy deny, headless parity) is distinct from empty.
        const all = window.__webpilot_console;
        if (all === undefined) return { missing: true };
        return {
          entries: all
            .filter((e) => e && levels.includes(e.level) && e.timestamp >= s)
            .map((e) => ({
              level: e.level,
              message: typeof e.message === "string" ? e.message : "",
              // Coerce to 0 anything the CLI's `u64` can't carry — not just a
              // non-number but a NON-INTEGER or negative one (a page-reachable
              // buffer can hold `1.5` / `-1` / `NaN`). Headless does exactly this
              // via `as_u64().unwrap_or(0)`, which yields `None` (→ 0) for a
              // fractional/negative value; a bare `typeof === "number"` check
              // would forward `1.5`, failing the CLI's whole-response decode as a
              // misleading ConnectionLost where headless keeps the entry.
              timestamp: Number.isInteger(e.timestamp) && e.timestamp >= 0 ? e.timestamp : 0,
            })),
          // Driven by the eviction flag, not `length >= cap` (headless parity):
          // a buffer at exactly the cap with nothing dropped isn't truncated.
          truncated: window.__webpilot_console_dropped === true,
        };
      },
    });
    const out = r?.[0]?.result || { entries: [], truncated: false };
    if (out.missing) {
      return topErr(err(
        "InvalidArgument",
        "the console monitor is not installed in this document — an `eval` policy deny suppresses re-arming after navigation; check `webpilot policy list`, then run `webpilot console start`",
      ));
    }
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
    // Sentinel-preserving (headless parity): an unconditional `= []` would
    // CREATE the buffer in a document whose hook was never installed (an
    // `eval` deny suppressed the re-arm), and the read's hook-absent guard —
    // which keys on `undefined` — would then report an empty success while
    // the monitor is in fact off.
    const [hit] = await chrome.scripting.executeScript({
      target: { tabId: tab.id, frameIds: [0] },
      world: "MAIN",
      func: () => {
        if (window.__webpilot_console === undefined) return false;
        window.__webpilot_console = [];
        window.__webpilot_console_dropped = false;
        return true;
      },
    });
    if (hit?.result !== true) {
      return topErr(err(
        "InvalidArgument",
        "the console monitor is not installed in this document — an `eval` policy deny suppresses re-arming after navigation; check `webpilot policy list`, then run `webpilot console start`",
      ));
    }
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
    monitoringState.network = true;
    saveMonitoringState();
    return { type: "CommandResult", success: true };
  } catch (e) {
    return { type: "CommandResult", success: false, error: exceptionErr(e) };
  }
}

async function handleNetworkRead(since) {
  const tab = await resolveActiveTab();
  if (!tab) return topErr(noPageErr());
  // See handleConsoleRead: an empty read before `network start` would read as
  // "no requests" rather than "monitor not armed". Fail loud.
  if (!monitoringState.network) {
    return topErr(
      err("InvalidArgument", "network monitoring is not active — run `webpilot network start` first"),
    );
  }
  try {
    const r = await chrome.scripting.executeScript({
      target: { tabId: tab.id, frameIds: [0] },
      world: "MAIN",
      // Filter by `timestamp >= since` AND sanitize to the same shape headless
      // returns: drop any entry whose fields don't match `NetworkEntry` (the
      // MAIN-world buffer is page-reachable and best-effort), so the CLI
      // deserializes an identical `Vec<NetworkEntry>` in both modes and a tampered
      // entry can't break the read. The OPTIONAL `status`/`error` are type-checked
      // too — a present-but-wrong-typed `status:"200"` (string) or out-of-`u32`
      // value would otherwise pass and fail the CLI's `Option<u32>` decode as a
      // misleading ConnectionLost, where headless's per-entry `.ok()` just drops it.
      func: (s) => {
        // `undefined` (no hook in THIS document) is distinct from empty —
        // headless parity, see handleConsoleRead.
        const all = window.__webpilot_network;
        if (all === undefined) return { missing: true };
        return {
          entries: all.filter(
            (e) =>
              e &&
              e.timestamp >= s &&
              typeof e.type === "string" &&
              typeof e.url === "string" &&
              typeof e.method === "string" &&
              // `duration_ms` decodes to `f64`: a non-FINITE number (NaN /
              // ±Infinity) serializes to JSON `null`, which fails that decode and
              // breaks the whole read — so require finiteness, not bare `number`.
              Number.isFinite(e.duration_ms) &&
              // `timestamp` decodes to `u64`: a fractional/negative `number`
              // (`1.5` / `-1`) passes `typeof` but fails the CLI's whole-response
              // decode as a misleading ConnectionLost, where headless's per-entry
              // `from_value().ok()` just drops it. Match that — drop the entry.
              Number.isInteger(e.timestamp) && e.timestamp >= 0 &&
              (e.status == null ||
                (Number.isInteger(e.status) && e.status >= 0 && e.status <= 0xffffffff)) &&
              (e.error == null || typeof e.error === "string"),
          ),
          // The eviction flag, not `length >= cap` (headless parity).
          truncated: window.__webpilot_network_dropped === true,
        };
      },
      args: [since || 0],
    });
    const out = r?.[0]?.result || { entries: [], truncated: false };
    if (out.missing) {
      return topErr(err(
        "InvalidArgument",
        "the network monitor is not installed in this document — an `eval` policy deny suppresses re-arming after navigation; check `webpilot policy list`, then run `webpilot network start`",
      ));
    }
    return { type: "NetworkEntries", entries: out.entries, truncated: out.truncated };
  } catch (e) {
    return topErr(exceptionErr(e));
  }
}

async function handleNetworkClear() {
  const tab = await resolveActiveTab();
  if (!tab) return topErr(noPageErr());
  try {
    // Sentinel-preserving — see handleConsoleClear.
    const [hit] = await chrome.scripting.executeScript({
      target: { tabId: tab.id, frameIds: [0] },
      world: "MAIN",
      func: () => {
        if (window.__webpilot_network === undefined) return false;
        window.__webpilot_network = [];
        window.__webpilot_network_dropped = false;
        return true;
      },
    });
    if (hit?.result !== true) {
      return topErr(err(
        "InvalidArgument",
        "the network monitor is not installed in this document — an `eval` policy deny suppresses re-arming after navigation; check `webpilot policy list`, then run `webpilot network start`",
      ));
    }
    return { type: "CommandResult", success: true };
  } catch (e) {
    return topErr(exceptionErr(e));
  }
}

// ── Session ────────────────────────────────────────────────────────────────

async function handleSessionExport() {
  try {
    // `partitionKey: {}` matches partitioned AND unpartitioned cookies alike —
    // a bare `getAll({})` returns only unpartitioned ones, so a CHIPS
    // partitioned auth cookie would silently vanish from the export (headless
    // `Storage.getCookies` sees every partition).
    const all = await chrome.cookies.getAll({ partitionKey: {} });
    const tab = await resolveActiveTab();
    // No http page (e.g. only chrome://newtab focused) means Web Storage can't be
    // read at all — fail rather than write a session file with silently empty
    // localStorage/sessionStorage that re-imports as data loss. Headless always
    // has a bound page and so always reads it; this matches that.
    if (!tab) {
      return topErr(noPageErr());
    }
    // The export reads storage through the active frame's bridge; a switched
    // frame that vanished is FrameNotFound (exit 4 → recapture), not the
    // BridgeUnavailable (exit 3 → infra) a failed inject yields — matching
    // capture/wait/dom and headless `bridge_context_id`.
    const frameGone = await frameVanishedError(tab.id, activeFrameId);
    if (frameGone) return topErr(frameGone);
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
      version: SESSION_SCHEMA_VERSION,
      exported_at: Date.now(),
      // Storage is origin-scoped; the bridge records whose it is so the import
      // can refuse to write it into a different origin (headless parity).
      origin: storage.origin ?? null,
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
    // An exported session is a JSON object. An array/string/number reaches every
    // field read as absent and would fall through to `success: true`; a `null`
    // would throw a TypeError the outer catch mislabels `Other`. Reject a
    // non-object root loudly as InvalidArgument, matching headless
    // do_session_import.
    if (data === null || typeof data !== "object" || Array.isArray(data)) {
      return { type: "SessionResult", success: false, error: err("InvalidArgument", "session must be a JSON object") };
    }
    // Honor the export's `version`: a file from a newer schema may carry fields
    // this binary can't apply, so reject it rather than silently drop them and
    // report success. A missing version is a hand-written/legacy file — accepted
    // as the current schema. Mirrors headless do_session_import.
    if (typeof data.version === "number" && data.version > SESSION_SCHEMA_VERSION) {
      return { type: "SessionResult", success: false, error: err("InvalidArgument", `session was exported by a newer WebPilot (schema v${data.version}); this binary supports up to v${SESSION_SCHEMA_VERSION} — upgrade to import it`) };
    }
    // `cookies`, when present, must be an array — a string would iterate
    // character by character, and a null is a malformed present value. Use
    // `hasOwn` (not `!= null`) so a present `null` is rejected too, exactly as
    // headless do_session_import treats `Some(Null)`.
    if (Object.hasOwn(data, "cookies") && !Array.isArray(data.cookies)) {
      return { type: "SessionResult", success: false, error: err("InvalidArgument", "session `cookies` must be an array") };
    }
    const cookies = data.cookies || [];
    const cookiesTotal = cookies.length;
    // A present `local_storage`/`session_storage` must be a plain object or null
    // — the same shape the bridge requires, validated here so a non-object is
    // rejected up front on ANY pin (the bridge needs an http page to run; this
    // check does not) rather than silently dropped. Fail before importing the
    // cookies, so a malformed file never half-applies.
    for (const key of ["local_storage", "session_storage"]) {
      const value = data[key];
      if (!Object.hasOwn(data, key) || value === null) continue;
      if (typeof value !== "object" || Array.isArray(value)) {
        return { type: "SessionResult", success: false, error: err("InvalidArgument", `session \`${key}\` must be an object`) };
      }
      // Web Storage holds only strings; a non-string value would coerce to
      // garbage ("[object Object]"). Reject up front — before any cookie is
      // applied — so a malformed file never half-imports (the bridge re-checks
      // at its sink; this keeps the import atomic).
      for (const v of Object.values(value)) {
        if (typeof v !== "string") {
          return { type: "SessionResult", success: false, error: err("InvalidArgument", `session \`${key}\` values must be strings`) };
        }
      }
    }
    // Only NON-EMPTY storage carries data to import, and only that needs an http
    // page to run the bridge in; an empty, null, or absent field is a no-op.
    // Gating on actual data (not mere presence) keeps an empty storage section
    // from blocking the cookie import — cookies are browser-global and need no
    // page — when the pin is a non-http page.
    const hasStorage = Object.keys(data.local_storage || {}).length > 0
      || Object.keys(data.session_storage || {}).length > 0;
    const tab = await resolveActiveTab();
    if (hasStorage && !tab) {
      return { type: "SessionResult", success: false, error: noPageErr() };
    }
    // Apply storage BEFORE the cookies (headless parity). Storage is the
    // quota-prone, bulky part and runs in the active frame's bridge — so a write
    // the page rejects (a vanished frame → FrameNotFound, or a localStorage
    // quota overflow) must fail up front, before any cookie is committed.
    // Otherwise a half-import leaves the agent an authenticated session (cookies
    // set) on inconsistent app state (storage that couldn't land); storage first
    // leaves no cookies on that failure, merely logged-out. A successful import
    // lands both halves regardless of order.
    if (hasStorage && tab) {
      const frameGone = await frameVanishedError(tab.id, activeFrameId);
      if (frameGone) {
        return { type: "SessionResult", success: false, error: frameGone };
      }
      // A storage import failure is propagated, not swallowed (headless
      // parity): the caller asked to restore it and must know it didn't.
      await ensureBridge(tab.id, activeFrameId);
      const r = await sendToContent(tab.id, {
        type: "importStorage",
        // The bridge enforces the export's origin against the page it is about
        // to write — origin-scoped state must not land on a different origin
        // under a success status (headless parity).
        origin: data.origin ?? null,
        // Shape is validated above; an absent/empty field is `{}` (a no-op),
        // matching headless's `unwrap_or_else(|| json!({}))`. The bridge then
        // validates the values (must be strings) as it imports.
        localStorage: data.local_storage || {},
        sessionStorage: data.session_storage || {},
      }, activeFrameId);
      if (r && r.success === false) {
        return { type: "SessionResult", success: false, error: r.error || otherErr("storage import failed") };
      }
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
      // Match headless `serde_json::from_value::<CookieInfo>` field-for-field: a
      // present `secure`/`http_only`/`host_only` must be a boolean and a present
      // `expiration` a number, exactly as their Rust types demand. Without this a
      // truthy string like `"host_only":"false"` would coerce a domain cookie into
      // a host-only one (dropping `domain`), corrupting scope while reporting
      // success — where headless rejects the same row.
      if (c == null || typeof c.name !== "string" || typeof c.value !== "string"
          || typeof c.domain !== "string" || typeof c.path !== "string"
          || !SAME_SITE.includes(c.same_site)
          || (c.secure !== undefined && typeof c.secure !== "boolean")
          || (c.http_only !== undefined && typeof c.http_only !== "boolean")
          || (c.host_only !== undefined && typeof c.host_only !== "boolean")
          || (c.expiration != null && typeof c.expiration !== "number")
          // A present `partition_key` must carry a string `top_level_site` and
          // (when present) a boolean `has_cross_site_ancestor`, exactly as the
          // Rust `Option<PartitionKey>` demands — a malformed key must count
          // as malformed, never import the cookie UNPARTITIONED (the silent
          // identity change this field exists to prevent). `null` is accepted
          // as absent, matching serde's Option.
          || (c.partition_key != null
            && (typeof c.partition_key !== "object" || Array.isArray(c.partition_key)
              || typeof c.partition_key.top_level_site !== "string"
              || (c.partition_key.has_cross_site_ancestor !== undefined
                && typeof c.partition_key.has_cross_site_ancestor !== "boolean")))) {
        cookiesMalformed++;
        continue;
      }
      try {
        // Count a refusal too, not just a throw: a cookie Chrome rejects comes
        // back as a thrown "Failed to parse or set cookie" (caught below) and
        // can defensively resolve null — either way it didn't restore, so
        // counting it keeps the import from reporting a session silently missing
        // auth cookies. Mirrors the headless `Network.setCookie success:false`
        // check.
        // The scheme is normally implied by `secure` — but a FIRST-PARTY
        // partition (has_cross_site_ancestor=false) means the cookie's site IS
        // the partition's top-level site, and Chrome validates the url ↔
        // topLevelSite pair SCHEMEFULLY: a Secure cookie partitioned on a
        // trustworthy plain-http origin (http://localhost dev setups) would be
        // refused as "not first party" against the https URL the secure flag
        // suggests. Take the partition's own scheme there; a cross-site
        // partition keeps the secure-implied scheme (its url and topLevelSite
        // are different sites by construction).
        let scheme = c.secure ? "https" : "http";
        if (c.partition_key && c.partition_key.has_cross_site_ancestor !== true) {
          const m = /^([a-z][a-z0-9+.-]*):\/\//i.exec(c.partition_key.top_level_site);
          if (m) scheme = m[1];
        }
        const set = await chrome.cookies.set({
          url: `${scheme}://${c.domain.replace(/^\./, "")}${c.path}`,
          name: c.name, value: c.value, path: c.path,
          // A host-only cookie is set by URL with no `domain`, so Chrome scopes
          // it to exactly its host and the round-trip can't widen it to
          // subdomains. A domain cookie keeps its explicit `domain`.
          ...(c.host_only ? {} : { domain: c.domain }),
          secure: c.secure, httpOnly: c.http_only,
          sameSite: chromeSameSite(c.same_site),
          // `== null`, not `|| undefined`: a legitimate `expiration: 0` (the
          // epoch, i.e. already expired) is a real expiry headless forwards as
          // `expires: 0`, not a session cookie — `|| undefined` would drop the
          // zero and keep the cookie instead of expiring it.
          expirationDate: c.expiration == null ? undefined : c.expiration,
          // CHIPS: restore the cookie into its original partition — omitting
          // the key would create an unpartitioned twin instead of the cookie
          // the partitioned (embedded) context actually sends.
          ...(c.partition_key != null ? {
            partitionKey: {
              topLevelSite: c.partition_key.top_level_site,
              hasCrossSiteAncestor: c.partition_key.has_cross_site_ancestor === true,
            },
          } : {}),
        });
        if (!set) cookiesFailed++;
      } catch {
        cookiesFailed++;
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

export { handleConsoleClear, handleConsoleRead, handleConsoleStart, handleCookieDelete, handleCookieList, handleCookieSet, handleNetworkClear, handleNetworkRead, handleNetworkStart, handleSessionExport, handleSessionImport, rearmMonitors, setMonitorPolicy };
