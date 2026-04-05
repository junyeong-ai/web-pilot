/**
 * WebPilot Content Script Bridge v2
 * Auto-injected on all pages via manifest content_scripts.
 * Handles: DOM extraction, action execution, JS evaluation, wait conditions.
 */

// Track previous element set for new-element detection
if (!window.__webpilot_previousKeys) window.__webpilot_previousKeys = new Set();
var previousElementKeys = window.__webpilot_previousKeys;

var INTERACTIVE_SELECTOR =
  'a[href], button, input, select, textarea, ' +
  '[role="button"], [role="link"], [role="tab"], [role="menuitem"], ' +
  '[role="checkbox"], [role="radio"], [role="switch"], [role="combobox"], ' +
  '[role="searchbox"], [role="textbox"], [role="slider"], ' +
  '[contenteditable="true"], details > summary';

// Unified message handler — used by both Extension listener and CDP call_bridge().
// Returns a value (sync) or a Promise (async, e.g. wait).
function handleMessage(msg) {
  switch (msg.type) {
    case "extractDOM":
      return extractDOM(msg.options || {});
    case "extractText":
      return { text: document.body?.innerText || "", url: location.href, title: document.title };
    case "executeAction":
      return executeAction(msg.action);
    case "evaluate":
      try {
        let result;
        try {
          result = new Function("return (" + msg.code + ")")();
        } catch (syntaxErr) {
          if (syntaxErr instanceof SyntaxError) result = new Function(msg.code)();
          else throw syntaxErr;
        }
        return { success: true, result: result !== undefined ? JSON.stringify(result) : null };
      } catch (e) {
        return { success: false, error: { message: e.message, code: "Unknown" } };
      }
    case "wait":
      return new Promise((resolve) => handleWait(msg, resolve));
    case "tagElement": {
      const visible = getVisibleElements();
      const el = msg.index > 0 && msg.index <= visible.length ? visible[msg.index - 1] : null;
      if (el) el.setAttribute(msg.attr, "1");
      return { success: !!el };
    }
    case "untagElement": {
      const tagged = document.querySelector(`[${msg.attr}]`);
      if (tagged) tagged.removeAttribute(msg.attr);
      return { success: true };
    }
    case "getPageDims":
      return {
        scrollHeight: document.documentElement.scrollHeight,
        viewportHeight: window.innerHeight,
        scrollX: window.scrollX,
        scrollY: window.scrollY,
      };
    case "scrollTo":
      window.scrollTo(msg.x ?? 0, msg.y ?? 0);
      return { success: true };
    case "setHtml": {
      const el = document.querySelector(msg.selector);
      if (el) { el.innerHTML = msg.value; return { success: true }; }
      return { success: false, error: { message: `Selector not found: ${msg.selector}`, code: "ElementNotFound" } };
    }
    case "setText": {
      const el = document.querySelector(msg.selector);
      if (el) { el.textContent = msg.value; return { success: true }; }
      return { success: false, error: { message: `Selector not found: ${msg.selector}`, code: "ElementNotFound" } };
    }
    case "setAttr": {
      const el = document.querySelector(msg.selector);
      if (el) { el.setAttribute(msg.attr, msg.value); return { success: true }; }
      return { success: false, error: { message: `Selector not found: ${msg.selector}`, code: "ElementNotFound" } };
    }
    case "getHtml": {
      const el = document.querySelector(msg.selector);
      return el ? { success: true, value: el.innerHTML } : { success: false, error: { message: `Not found: ${msg.selector}`, code: "ElementNotFound" } };
    }
    case "getText": {
      const el = document.querySelector(msg.selector);
      return el ? { success: true, value: el.textContent } : { success: false, error: { message: `Not found: ${msg.selector}`, code: "ElementNotFound" } };
    }
    case "getAttr": {
      const el = document.querySelector(msg.selector);
      return el ? { success: true, value: el.getAttribute(msg.attr) } : { success: false, error: { message: `Not found: ${msg.selector}`, code: "ElementNotFound" } };
    }
    case "exportStorage":
      return {
        localStorage: (() => { const o = {}; for (let i = 0; i < localStorage.length; i++) { const k = localStorage.key(i); o[k] = localStorage.getItem(k); } return o; })(),
        sessionStorage: (() => { const o = {}; for (let i = 0; i < sessionStorage.length; i++) { const k = sessionStorage.key(i); o[k] = sessionStorage.getItem(k); } return o; })(),
      };
    case "importStorage":
      if (msg.localStorage) Object.entries(msg.localStorage).forEach(([k, v]) => localStorage.setItem(k, v));
      if (msg.sessionStorage) Object.entries(msg.sessionStorage).forEach(([k, v]) => sessionStorage.setItem(k, v));
      return { success: true };
    case "addAnnotations": {
      document.getElementById("__webpilot_annotations")?.remove();
      const container = document.createElement("div");
      container.id = "__webpilot_annotations";
      container.style.cssText = "position:fixed;top:0;left:0;width:100%;height:100%;z-index:2147483647;pointer-events:none";
      for (const el of (msg.elements || [])) {
        const box = document.createElement("div");
        box.style.cssText = `position:fixed;left:${el.x}px;top:${el.y}px;width:${el.w}px;height:${el.h}px;border:2px solid rgba(255,0,0,0.8)`;
        const label = document.createElement("div");
        label.textContent = String(el.index);
        label.style.cssText = "position:absolute;top:-16px;left:-2px;background:rgba(255,0,0,0.9);color:#fff;font:bold 11px/14px monospace;padding:0 3px;border-radius:2px";
        box.appendChild(label);
        container.appendChild(box);
      }
      document.documentElement.appendChild(container);
      return { success: true, count: (msg.elements || []).length };
    }
    case "removeAnnotations":
      document.getElementById("__webpilot_annotations")?.remove();
      return { success: true };
    case "getElementCoords": {
        const visible = getVisibleElements();
        const srcEl = msg.source > 0 && msg.source <= visible.length ? visible[msg.source - 1] : null;
        const tgtEl = msg.target > 0 && msg.target <= visible.length ? visible[msg.target - 1] : null;
        if (!srcEl) return { success: false, error: { message: "Source element not found", code: "ElementNotFound" } };
        if (!tgtEl) return { success: false, error: { message: "Target element not found", code: "ElementNotFound" } };
        srcEl.scrollIntoView({ block: "center", behavior: "instant" });
        const sr = srcEl.getBoundingClientRect();
        const tr = tgtEl.getBoundingClientRect();
        return { sx: sr.left + sr.width/2, sy: sr.top + sr.height/2, tx: tr.left + tr.width/2, ty: tr.top + tr.height/2 };
    }
    case "ping":
      return { ok: true, url: location.href, title: document.title };
    default:
      return { success: false, error: { message: "Unknown message type: " + msg.type, code: "Unknown" } };
  }
}

// Register message listener (Extension content script mode)
// Always re-register on each injection to recover from SPA-induced listener disconnects.
// Previous listener is removed first to avoid duplicates.
if (typeof chrome !== "undefined" && chrome.runtime?.onMessage) {
  if (window.__webpilot_listener_fn) {
    try { chrome.runtime.onMessage.removeListener(window.__webpilot_listener_fn); } catch {}
  }
  window.__webpilot_listener_fn = (msg, sender, sendResponse) => {
    const result = handleMessage(msg);
    if (result && typeof result.then === "function") {
      result.then(sendResponse);
      return true;
    }
    sendResponse(result);
    return false;
  };
  chrome.runtime.onMessage.addListener(window.__webpilot_listener_fn);
}

// ==================== DOM EXTRACTION ====================

function queryAllDeep(selector, root = document, depth = 0) {
  if (depth > 10) return [];
  const results = [...root.querySelectorAll(selector)];
  for (const el of root.querySelectorAll("*")) {
    if (el.shadowRoot) {
      results.push(...queryAllDeep(selector, el.shadowRoot, depth + 1));
    }
  }
  return results;
}

// Shared element collection — used by both extractDOM and getVisibleElements
// to ensure element indices stay consistent between capture and action.
var STANDARD_TAGS = new Set(["a","button","input","select","textarea","summary"]);

function collectInteractiveElements() {
  const allEls = queryAllDeep(INTERACTIVE_SELECTOR);
  for (const el of document.querySelectorAll("*")) {
    if (STANDARD_TAGS.has(el.tagName.toLowerCase())) continue;
    if (el.getAttribute("role")) continue;
    try {
      if (getComputedStyle(el).cursor === "pointer" && !el.closest("a,button")) {
        const rect = el.getBoundingClientRect();
        if (rect.width > 10 && rect.height > 10) allEls.push(el);
      }
    } catch {}
  }
  return allEls;
}

function extractDOM(options) {
  try {
    const start = performance.now();
    const allEls = collectInteractiveElements();
    const totalNodes = document.querySelectorAll("*").length;
    const elements = [];
    let idx = 1;
    const includeBounds = options.bounds || false;

    for (const el of allEls) {
      const rect = el.getBoundingClientRect();
      const style = getComputedStyle(el);
      if (
        rect.width <= 0 || rect.height <= 0 ||
        style.display === "none" ||
        style.visibility === "hidden" ||
        parseFloat(style.opacity) === 0
      ) continue;

      const tag = el.tagName.toLowerCase();
      const innerText = (el.innerText || el.textContent || "").trim().replace(/\s+/g, " ");
      const text = (tag === "input" || tag === "textarea")
        ? (el.placeholder || el.getAttribute("aria-label") || "")
        : innerText.slice(0, 300);

      const elemId = el.id && el.id.length <= 50 && /^[a-zA-Z0-9_-]+$/.test(el.id) ? el.id : undefined;

      const entry = {
        index: idx++,
        tag,
        id: elemId,
        role: el.getAttribute("role") || undefined,
        text,
        name: el.getAttribute("aria-label") || el.getAttribute("title") || undefined,
        value: (el.value != null && el.value !== "") ? String(el.value).slice(0, 100) : undefined,
        placeholder: el.placeholder || undefined,
        href: el.getAttribute("href") || undefined,
        input_type: tag === "input" ? (el.type || undefined) : undefined,
        disabled: el.disabled || el.getAttribute("aria-disabled") === "true" || false,
        focused: document.activeElement === el,
        checked: (el.type === "checkbox" || el.type === "radio") ? el.checked : undefined,
        expanded: el.getAttribute("aria-expanded") === "true" ? true : el.getAttribute("aria-expanded") === "false" ? false : undefined,
        selected: el.getAttribute("aria-selected") === "true" ? true : el.selected === true ? true : undefined,
        required: el.required || undefined,
        readonly: el.readOnly || undefined,
        label: resolveLabel(el),
        options: extractOptions(el, tag),
        landmark: findLandmark(el),
        in_viewport: rect.top < innerHeight && rect.bottom > 0 && rect.left < innerWidth && rect.right > 0,
      };

      const form = el.closest("form");
      entry.form_id = form?.id || undefined;

      const describedBy = el.getAttribute("aria-describedby");
      if (describedBy) {
        const parts = describedBy.split(/\s+/)
          .map(id => document.getElementById(id)?.textContent?.trim())
          .filter(Boolean);
        entry.description = parts.join(" ").slice(0, 120) || undefined;
      }

      entry.autocomplete = el.getAttribute("autocomplete") || undefined;

      // Occlusion detection: is the element's center covered by another element?
      if (options.occlusion) {
        const cx = rect.left + rect.width / 2, cy = rect.top + rect.height / 2;
        if (cx >= 0 && cy >= 0 && cx < innerWidth && cy < innerHeight) {
          const topEl = document.elementFromPoint(cx, cy);
          entry.occluded = topEl && topEl !== el && !el.contains(topEl) && !topEl.contains(el);
        }
      }

      if (includeBounds) {
        entry.bounds = { x: Math.round(rect.x), y: Math.round(rect.y), w: Math.round(rect.width), h: Math.round(rect.height) };
      }

      // New element detection: compare with previous snapshot
      const elemKey = `${tag}|${text?.slice(0,30)}|${el.getAttribute("href")||""}|${el.getAttribute("role")||""}`;
      entry.is_new = !previousElementKeys.has(elemKey);

      // Remove undefined and false fields (keep disabled/focused even when false)
      for (const k of Object.keys(entry)) {
        if (entry[k] === undefined || (entry[k] === false && k !== "disabled" && k !== "focused")) {
          delete entry[k];
        }
      }

      elements.push(entry);
    }

    // Update previous element set for next extraction
    const currentKeys = new Set(elements.map(e => `${e.tag}|${e.text?.slice(0,30)}|${e.href||""}|${e.role||""}`));
    previousElementKeys = currentKeys;
    window.__webpilot_previousKeys = currentKeys;

    const sh = document.documentElement.scrollHeight;
    const vh = innerHeight;
    const sy = scrollY;

    return {
      elements,
      total_nodes: totalNodes,
      page_url: location.href,
      page_title: document.title,
      scroll: {
        scroll_x: scrollX, scroll_y: sy,
        scroll_width: document.documentElement.scrollWidth,
        scroll_height: sh,
        viewport_width: innerWidth, viewport_height: vh,
      },
      scroll_percent: sh > vh ? Math.round((sy / (sh - vh)) * 100) : 100,
      extraction_ms: Math.round(performance.now() - start),
    };
  } catch (e) {
    return { error: e.message, elements: [], total_nodes: 0, page_url: location.href, page_title: document.title, scroll: {}, extraction_ms: 0 };
  }
}

function extractOptions(el, tag) {
  // Native <select> options
  if (tag === "select") {
    return [...el.options].slice(0, 50).map(o => ({ value: o.value, text: o.text, selected: o.selected }));
  }
  // ARIA listbox/menu/combobox: extract role=option children
  const role = el.getAttribute("role");
  if (role === "listbox" || role === "menu" || role === "combobox") {
    const opts = el.querySelectorAll('[role="option"], [role="menuitem"]');
    if (opts.length > 0) {
      return [...opts].slice(0, 50).map(o => ({
        value: o.getAttribute("data-value") || o.textContent.trim().slice(0, 80),
        text: o.textContent.trim().slice(0, 80),
        selected: o.getAttribute("aria-selected") === "true",
      }));
    }
  }
  return undefined;
}

function resolveLabel(el) {
  // 1. aria-labelledby (computed accessibility name)
  const labelledBy = el.getAttribute("aria-labelledby");
  if (labelledBy) {
    const parts = labelledBy.split(/\s+/).map(id => document.getElementById(id)?.textContent?.trim()).filter(Boolean);
    if (parts.length > 0) return parts.join(" ").slice(0, 80);
  }
  // 2. Native labels collection
  if (el.labels && el.labels.length > 0) {
    return el.labels[0].textContent.trim().slice(0, 80) || null;
  }
  // 3. label[for=id]
  if (el.id) {
    const label = document.querySelector(`label[for="${el.id}"]`);
    if (label) return label.textContent.trim().slice(0, 80) || null;
  }
  // 4. Wrapping label
  const parent = el.closest("label");
  if (parent) {
    const text = parent.textContent.trim().replace(/\s+/g, " ").slice(0, 80);
    if (text && text !== el.value) return text;
  }
  return null;
}

function findLandmark(el) {
  const landmarks = new Set(["nav", "main", "footer", "header", "aside", "banner", "form", "dialog", "search"]);
  let p = el.parentElement;
  while (p && p !== document.body) {
    const role = p.getAttribute("role");
    if (role && landmarks.has(role)) return role;
    const tag = p.tagName.toLowerCase();
    if (landmarks.has(tag)) return tag;
    p = p.parentElement;
  }
  return null;
}

// ==================== ACTION EXECUTION ====================

function getVisibleElements() {
  const allEls = collectInteractiveElements();
  const visible = [];
  for (const el of allEls) {
    const rect = el.getBoundingClientRect();
    const style = getComputedStyle(el);
    if (rect.width > 0 && rect.height > 0 &&
        style.display !== "none" && style.visibility !== "hidden" &&
        parseFloat(style.opacity) > 0) {
      visible.push(el);
    }
  }
  return visible;
}

function resolveTarget(action) {
  const index = action.index;
  if (index == null) return { target: null, error: { message: "No index provided", code: "ElementNotFound" } };
  const visible = getVisibleElements();
  if (index < 1 || index > visible.length) {
    return { target: null, error: { message: `Index ${index} out of range (1-${visible.length})`, code: "ElementNotFound" } };
  }
  return { target: visible[index - 1] };
}

function reliableClick(el) {
  el.scrollIntoView({ block: "center", behavior: "instant" });
  const rect = el.getBoundingClientRect();
  const x = rect.left + rect.width / 2;
  const y = rect.top + rect.height / 2;
  const opts = { bubbles: true, cancelable: true, clientX: x, clientY: y, button: 0, view: window };
  el.dispatchEvent(new PointerEvent("pointerdown", opts));
  el.dispatchEvent(new MouseEvent("mousedown", opts));
  el.dispatchEvent(new PointerEvent("pointerup", opts));
  el.dispatchEvent(new MouseEvent("mouseup", opts));
  el.dispatchEvent(new MouseEvent("click", opts));
}

function reliableType(el, text, clear) {
  el.scrollIntoView({ block: "center", behavior: "instant" });
  el.focus();

  // contentEditable support (WYSIWYG editors like CJ World CrossEditor)
  if (el.isContentEditable) {
    if (clear) {
      el.innerHTML = "";
    }
    document.execCommand("insertText", false, text);
    return;
  }

  // Standard input/textarea with React/Vue compatibility
  const newVal = clear ? text : (el.value || "") + text;

  // Native setter trick — bypasses React's synthetic event system
  try {
    const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement : HTMLInputElement;
    const setter = Object.getOwnPropertyDescriptor(proto.prototype, "value")?.set;
    if (setter) {
      setter.call(el, newVal);
    } else {
      el.value = newVal;
    }
  } catch {
    // Fallback for cross-origin or restricted elements
    el.value = newVal;
  }

  // InputEvent (not Event) for modern framework compatibility
  el.dispatchEvent(new InputEvent("input", { bubbles: true, inputType: "insertText", data: text }));
  el.dispatchEvent(new Event("change", { bubbles: true }));
}

function executeAction(action) {
  try {
    switch (action.action) {
      case "Click": {
        const { target, error } = resolveTarget(action);
        if (!target) return { success: false, error };
        reliableClick(target);
        return { success: true };
      }

      case "Type": {
        const { target, error } = resolveTarget(action);
        if (!target) return { success: false, error };
        reliableType(target, action.text, action.clear);
        return { success: true };
      }

      case "KeyPress": {
        const opts = { key: action.key, code: `Key${action.key.toUpperCase()}`, bubbles: true, cancelable: true };
        if (action.modifiers?.includes("ctrl")) opts.ctrlKey = true;
        if (action.modifiers?.includes("shift")) opts.shiftKey = true;
        if (action.modifiers?.includes("alt")) opts.altKey = true;
        if (action.modifiers?.includes("meta")) opts.metaKey = true;
        const el = document.activeElement || document.body;
        el.dispatchEvent(new KeyboardEvent("keydown", opts));
        el.dispatchEvent(new KeyboardEvent("keypress", opts));
        el.dispatchEvent(new KeyboardEvent("keyup", opts));
        if (action.key === "Enter" && el.form) {
          el.form.requestSubmit?.() || el.form.submit();
        }
        return { success: true };
      }

      case "ScrollDown":
        window.scrollBy(0, action.amount || 600);
        return { success: true };

      case "ScrollUp":
        window.scrollBy(0, -(action.amount || 600));
        return { success: true };

      case "ScrollToElement": {
        const { target, error } = resolveTarget(action);
        if (!target) return { success: false, error };
        target.scrollIntoView({ block: "center", behavior: "instant" });
        return { success: true };
      }

      case "Select": {
        const { target, error } = resolveTarget(action);
        if (!target) return { success: false, error };
        target.value = action.value;
        target.dispatchEvent(new Event("change", { bubbles: true }));
        return { success: true };
      }

      case "Hover": {
        const { target, error } = resolveTarget(action);
        if (!target) return { success: false, error };
        target.scrollIntoView({ block: "center", behavior: "instant" });
        const rect = target.getBoundingClientRect();
        const opts = { bubbles: true, clientX: rect.left + rect.width / 2, clientY: rect.top + rect.height / 2 };
        target.dispatchEvent(new PointerEvent("pointerover", opts));
        target.dispatchEvent(new MouseEvent("mouseover", opts));
        target.dispatchEvent(new PointerEvent("pointerenter", { ...opts, bubbles: false }));
        target.dispatchEvent(new MouseEvent("mouseenter", { ...opts, bubbles: false }));
        return { success: true };
      }

      case "Focus": {
        const { target, error } = resolveTarget(action);
        if (!target) return { success: false, error };
        target.focus();
        return { success: true };
      }

      case "Back": history.back(); return { success: true };
      case "Forward": history.forward(); return { success: true };
      case "Reload": location.reload(); return { success: true };

      default:
        return { success: false, error: { message: `Unknown action: ${action.action}`, code: "Unknown" } };
    }
  } catch (e) {
    return { success: false, error: { message: e.message, code: "Unknown" } };
  }
}

// ==================== WAIT ====================

function handleWait(msg, sendResponse) {
  const timeout = msg.timeout_ms || 10000;
  let resolved = false;
  let observer = null;

  // Single resolve function — prevents double sendResponse
  function finish(result) {
    if (resolved) return;
    resolved = true;
    if (observer) observer.disconnect();
    clearTimeout(timer);
    sendResponse(result);
  }

  const timer = setTimeout(() => {
    finish({ success: false, error: { message: "Wait timed out", code: "Timeout" } });
  }, timeout);

  if (msg.selector) {
    if (document.querySelector(msg.selector)) {
      finish({ success: true });
      return;
    }
    observer = new MutationObserver(() => {
      if (document.querySelector(msg.selector)) finish({ success: true });
    });
    observer.observe(document.body || document.documentElement, { childList: true, subtree: true });
  } else if (msg.text) {
    if ((document.body?.innerText || "").includes(msg.text)) {
      finish({ success: true });
      return;
    }
    observer = new MutationObserver(() => {
      if ((document.body?.innerText || "").includes(msg.text)) finish({ success: true });
    });
    observer.observe(document.body || document.documentElement, { childList: true, subtree: true, characterData: true });
  } else {
    // Default: DOM idle (no changes for 500ms)
    let idleTimer;
    observer = new MutationObserver(() => {
      clearTimeout(idleTimer);
      idleTimer = setTimeout(() => finish({ success: true }), 500);
    });
    observer.observe(document.body || document.documentElement, { childList: true, subtree: true });
    idleTimer = setTimeout(() => finish({ success: true }), 500);
  }
}
