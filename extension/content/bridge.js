/**
 * WebPilot content bridge.
 *
 * Sole entry point: `__webpilot_handle(msg)`. Returns either a value or a
 * Promise. Used by the headless-mode CDP injection and the extension content
 * script alike.
 *
 * Error shape: `{ success: false, error: { code, message, ...data } }` where
 * `code` matches the Rust `WebPilotError` discriminator and `...data` are the
 * fields needed to reconstruct the typed Rust variant.
 */

(() => {
  // ── New-element baseline ──────────────────────────────────────────────────
  // Reset whenever the URL changes — otherwise a fresh page would mark every
  // element as "new", which is misleading. Within the same URL, "new" means
  // appeared since the previous capture (e.g., a modal opened).
  if (!window.__webpilot_state) {
    window.__webpilot_state = {
      lastUrl: location.href,
      previousKeys: new Set(),
    };
  }
  const state = window.__webpilot_state;
  if (state.lastUrl !== location.href) {
    state.previousKeys = new Set();
    state.lastUrl = location.href;
  }

  // ── Error helpers ─────────────────────────────────────────────────────────
  const err = (code, message, data) => ({
    success: false,
    error: { code, message, ...(data || {}) },
  });

  const elementNotFound = (requested, available) =>
    err(
      "ElementNotFound",
      `Index ${requested} out of range (1-${available})`,
      { requested, available },
    );

  const selectorNotFound = (selector) =>
    err("SelectorNotFound", `Selector not found: ${selector}`, { selector });

  // ── Selector for interactive elements ─────────────────────────────────────
  const INTERACTIVE_SELECTOR =
    'a[href], button, input, select, textarea, ' +
    '[role="button"], [role="link"], [role="tab"], [role="menuitem"], ' +
    '[role="checkbox"], [role="radio"], [role="switch"], [role="combobox"], ' +
    '[role="searchbox"], [role="textbox"], [role="slider"], ' +
    '[contenteditable="true"], details > summary';

  const STANDARD_TAGS = new Set([
    "a", "button", "input", "select", "textarea", "summary",
  ]);

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

  function collectInteractiveElements() {
    const all = queryAllDeep(INTERACTIVE_SELECTOR);
    const seen = new Set(all);

    const clickableSel = '[onclick],[tabindex],[data-action],[ng-click],' +
      '[v-on\\:click],[\\@click],[data-click],[jsaction]';
    for (const el of document.querySelectorAll(clickableSel)) {
      if (seen.has(el)) continue;
      if (STANDARD_TAGS.has(el.tagName.toLowerCase())) continue;
      if (el.getAttribute("role")) continue;
      const rect = el.getBoundingClientRect();
      if (rect.width > 10 && rect.height > 10) {
        all.push(el);
        seen.add(el);
      }
    }

    const vh = window.innerHeight;
    for (const el of document.querySelectorAll("*")) {
      if (seen.has(el)) continue;
      if (STANDARD_TAGS.has(el.tagName.toLowerCase())) continue;
      if (el.getAttribute("role")) continue;
      const rect = el.getBoundingClientRect();
      if (rect.width <= 10 || rect.height <= 10) continue;
      if (rect.bottom < 0 || rect.top > vh) continue;
      try {
        if (getComputedStyle(el).cursor === "pointer" && !el.closest("a,button")) {
          all.push(el);
          seen.add(el);
        }
      } catch {}
    }
    return all;
  }

  function getVisibleElements() {
    const all = collectInteractiveElements();
    const visible = [];
    for (const el of all) {
      const rect = el.getBoundingClientRect();
      const style = getComputedStyle(el);
      if (
        rect.width > 0 &&
        rect.height > 0 &&
        style.display !== "none" &&
        style.visibility !== "hidden" &&
        parseFloat(style.opacity) > 0
      ) {
        visible.push(el);
      }
    }
    return visible;
  }

  function elementKey(el, text) {
    const tag = el.tagName.toLowerCase();
    const t = (text || "").slice(0, 30);
    return `${tag}|${t}|${el.getAttribute("href") || ""}|${el.getAttribute("role") || ""}`;
  }

  function resolveLabel(el) {
    const labelledBy = el.getAttribute("aria-labelledby");
    if (labelledBy) {
      const parts = labelledBy
        .split(/\s+/)
        .map((id) => document.getElementById(id)?.textContent?.trim())
        .filter(Boolean);
      if (parts.length > 0) return parts.join(" ").slice(0, 80);
    }
    if (el.labels && el.labels.length > 0) {
      return el.labels[0].textContent.trim().slice(0, 80) || null;
    }
    if (el.id) {
      const label = document.querySelector(`label[for="${CSS.escape(el.id)}"]`);
      if (label) return label.textContent.trim().slice(0, 80) || null;
    }
    const parent = el.closest("label");
    if (parent) {
      const text = parent.textContent.trim().replace(/\s+/g, " ").slice(0, 80);
      if (text && text !== el.value) return text;
    }
    return null;
  }

  function findLandmark(el) {
    const landmarks = new Set([
      "nav", "main", "footer", "header", "aside", "banner", "form", "dialog", "search",
    ]);
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

  function extractOptions(el, tag) {
    if (tag === "select") {
      return [...el.options]
        .slice(0, 50)
        .map((o) => ({ value: o.value, text: o.text, selected: o.selected }));
    }
    const role = el.getAttribute("role");
    if (role === "listbox" || role === "menu" || role === "combobox") {
      const opts = el.querySelectorAll('[role="option"], [role="menuitem"]');
      if (opts.length > 0) {
        return [...opts].slice(0, 50).map((o) => ({
          value: o.getAttribute("data-value") || o.textContent.trim().slice(0, 80),
          text: o.textContent.trim().slice(0, 80),
          selected: o.getAttribute("aria-selected") === "true",
        }));
      }
    }
    return undefined;
  }

  function extractDOM(options) {
    try {
      const start = performance.now();
      const all = collectInteractiveElements();
      const totalNodes = document.querySelectorAll("*").length;
      const elements = [];
      const currentKeys = new Set();
      let idx = 1;
      const includeBounds = options.bounds || false;

      for (const el of all) {
        const rect = el.getBoundingClientRect();
        const style = getComputedStyle(el);
        if (
          rect.width <= 0 || rect.height <= 0 ||
          style.display === "none" ||
          style.visibility === "hidden" ||
          parseFloat(style.opacity) === 0
        ) continue;

        const tag = el.tagName.toLowerCase();
        const innerText = (el.innerText || el.textContent || "")
          .trim()
          .replace(/\s+/g, " ");
        const text = (tag === "input" || tag === "textarea")
          ? (el.placeholder || el.getAttribute("aria-label") || "")
          : innerText.slice(0, 300);

        const elemId =
          el.id && el.id.length <= 50 && /^[a-zA-Z0-9_-]+$/.test(el.id)
            ? el.id
            : undefined;

        const entry = {
          index: idx++,
          tag,
          id: elemId,
          role: el.getAttribute("role") || undefined,
          text,
          name: el.getAttribute("aria-label") || el.getAttribute("title") || undefined,
          value: (el.value != null && el.value !== "")
            ? String(el.value).slice(0, 100)
            : undefined,
          placeholder: el.placeholder || undefined,
          href: el.getAttribute("href") || undefined,
          input_type: tag === "input" ? (el.type || undefined) : undefined,
          disabled: el.disabled || el.getAttribute("aria-disabled") === "true" || false,
          focused: document.activeElement === el,
          checked: (el.type === "checkbox" || el.type === "radio") ? el.checked : undefined,
          expanded:
            el.getAttribute("aria-expanded") === "true" ? true :
            el.getAttribute("aria-expanded") === "false" ? false : undefined,
          selected:
            el.getAttribute("aria-selected") === "true" ? true :
            el.selected === true ? true : undefined,
          required: el.required || undefined,
          readonly: el.readOnly || undefined,
          label: resolveLabel(el),
          options: extractOptions(el, tag),
          landmark: findLandmark(el),
          in_viewport:
            rect.top < innerHeight &&
            rect.bottom > 0 &&
            rect.left < innerWidth &&
            rect.right > 0,
          autocomplete: el.getAttribute("autocomplete") || undefined,
        };

        const form = el.closest("form");
        entry.form_id = form?.id || undefined;

        const describedBy = el.getAttribute("aria-describedby");
        if (describedBy) {
          const parts = describedBy
            .split(/\s+/)
            .map((id) => document.getElementById(id)?.textContent?.trim())
            .filter(Boolean);
          entry.description = parts.join(" ").slice(0, 120) || undefined;
        }

        if (options.occlusion) {
          const cx = rect.left + rect.width / 2;
          const cy = rect.top + rect.height / 2;
          if (cx >= 0 && cy >= 0 && cx < innerWidth && cy < innerHeight) {
            const top = document.elementFromPoint(cx, cy);
            entry.occluded =
              !!top && top !== el && !el.contains(top) && !top.contains(el);
          }
        }

        if (includeBounds) {
          entry.bounds = {
            x: Math.round(rect.x),
            y: Math.round(rect.y),
            w: Math.round(rect.width),
            h: Math.round(rect.height),
          };
        }

        const key = elementKey(el, text);
        currentKeys.add(key);
        entry.is_new = !state.previousKeys.has(key);

        for (const k of Object.keys(entry)) {
          if (entry[k] === undefined ||
              (entry[k] === false && k !== "disabled" && k !== "focused")) {
            delete entry[k];
          }
        }
        elements.push(entry);
      }

      state.previousKeys = currentKeys;

      const sh = document.documentElement.scrollHeight;
      const vh = innerHeight;
      const sy = scrollY;

      return {
        elements,
        total_nodes: totalNodes,
        page_url: location.href,
        page_title: document.title,
        scroll: {
          scroll_x: scrollX,
          scroll_y: sy,
          scroll_width: document.documentElement.scrollWidth,
          scroll_height: sh,
          viewport_width: innerWidth,
          viewport_height: vh,
        },
        scroll_percent: sh > vh ? Math.round((sy / (sh - vh)) * 100) : 100,
        extraction_ms: Math.round(performance.now() - start),
      };
    } catch (e) {
      return {
        elements: [],
        total_nodes: 0,
        page_url: location.href,
        page_title: document.title,
        scroll: {},
        extraction_ms: 0,
        error: e.message,
      };
    }
  }

  // ── Action execution ─────────────────────────────────────────────────────

  function resolveTarget(action) {
    const visible = getVisibleElements();
    const idx = action.index;
    if (idx == null) {
      return { error: err("InvalidArgument", "Missing index") };
    }
    if (idx < 1 || idx > visible.length) {
      return { error: elementNotFound(idx, visible.length) };
    }
    return { target: visible[idx - 1] };
  }

  function reliableClick(el) {
    el.scrollIntoView({ block: "center", behavior: "instant" });
    const rect = el.getBoundingClientRect();
    const x = rect.left + rect.width / 2;
    const y = rect.top + rect.height / 2;
    const opts = {
      bubbles: true, cancelable: true, clientX: x, clientY: y, button: 0, view: window,
    };
    el.dispatchEvent(new PointerEvent("pointerdown", opts));
    el.dispatchEvent(new MouseEvent("mousedown", opts));
    el.dispatchEvent(new PointerEvent("pointerup", opts));
    el.dispatchEvent(new MouseEvent("mouseup", opts));
    el.dispatchEvent(new MouseEvent("click", opts));
  }

  function reliableType(el, text, clear) {
    el.scrollIntoView({ block: "center", behavior: "instant" });
    el.focus();

    if (el.isContentEditable) {
      if (clear) el.innerHTML = "";
      document.execCommand("insertText", false, text);
      return;
    }

    const newVal = clear ? text : (el.value || "") + text;

    try {
      const proto = el instanceof HTMLTextAreaElement
        ? HTMLTextAreaElement
        : HTMLInputElement;
      const setter = Object.getOwnPropertyDescriptor(proto.prototype, "value")?.set;
      if (setter) setter.call(el, newVal);
      else el.value = newVal;
    } catch {
      el.value = newVal;
    }

    el.dispatchEvent(new InputEvent("input", {
      bubbles: true, inputType: "insertText", data: text,
    }));
    el.dispatchEvent(new Event("change", { bubbles: true }));
  }

  function keyToCode(key) {
    const map = {
      Enter: "Enter", Tab: "Tab", Escape: "Escape",
      Backspace: "Backspace", Delete: "Delete",
      ArrowUp: "ArrowUp", ArrowDown: "ArrowDown",
      ArrowLeft: "ArrowLeft", ArrowRight: "ArrowRight",
      Home: "Home", End: "End",
      PageUp: "PageUp", PageDown: "PageDown",
      " ": "Space", Space: "Space",
      Insert: "Insert", CapsLock: "CapsLock",
      F1: "F1", F2: "F2", F3: "F3", F4: "F4", F5: "F5", F6: "F6",
      F7: "F7", F8: "F8", F9: "F9", F10: "F10", F11: "F11", F12: "F12",
    };
    if (map[key]) return map[key];
    if (key.length === 1 && /[a-zA-Z]/.test(key)) return `Key${key.toUpperCase()}`;
    if (key.length === 1 && /[0-9]/.test(key)) return `Digit${key}`;
    return key;
  }

  function executeAction(action) {
    try {
      switch (action.kind) {
        case "click": {
          const r = resolveTarget(action);
          if (r.error) return r.error;
          reliableClick(r.target);
          return { success: true };
        }

        case "type": {
          const r = resolveTarget(action);
          if (r.error) return r.error;
          reliableType(r.target, action.text, action.clear);
          return { success: true };
        }

        case "key_press": {
          const m = action.modifiers || {};
          const opts = {
            key: action.key,
            code: keyToCode(action.key),
            bubbles: true,
            cancelable: true,
            ctrlKey: !!m.ctrl,
            shiftKey: !!m.shift,
            altKey: !!m.alt,
            metaKey: !!m.meta,
          };
          const el = document.activeElement || document.body;
          el.dispatchEvent(new KeyboardEvent("keydown", opts));
          el.dispatchEvent(new KeyboardEvent("keypress", opts));
          el.dispatchEvent(new KeyboardEvent("keyup", opts));
          if (action.key === "Enter" && el.form) {
            el.form.requestSubmit?.() || el.form.submit();
          }
          return { success: true };
        }

        case "scroll": {
          const amt = action.amount ?? 600;
          const dy = action.direction === "up" ? -amt : amt;
          window.scrollBy(0, dy);
          return { success: true };
        }

        case "scroll_to": {
          const r = resolveTarget(action);
          if (r.error) return r.error;
          r.target.scrollIntoView({ block: "center", behavior: "instant" });
          return { success: true };
        }

        case "select": {
          const r = resolveTarget(action);
          if (r.error) return r.error;
          r.target.value = action.value;
          r.target.dispatchEvent(new Event("change", { bubbles: true }));
          return { success: true };
        }

        case "hover": {
          const r = resolveTarget(action);
          if (r.error) return r.error;
          r.target.scrollIntoView({ block: "center", behavior: "instant" });
          const rect = r.target.getBoundingClientRect();
          const opts = {
            bubbles: true,
            clientX: rect.left + rect.width / 2,
            clientY: rect.top + rect.height / 2,
          };
          r.target.dispatchEvent(new PointerEvent("pointerover", opts));
          r.target.dispatchEvent(new MouseEvent("mouseover", opts));
          r.target.dispatchEvent(new PointerEvent("pointerenter", { ...opts, bubbles: false }));
          r.target.dispatchEvent(new MouseEvent("mouseenter", { ...opts, bubbles: false }));
          return { success: true };
        }

        case "focus": {
          const r = resolveTarget(action);
          if (r.error) return r.error;
          r.target.focus();
          return { success: true };
        }

        case "back": history.back(); return { success: true };
        case "forward": history.forward(); return { success: true };
        case "reload": location.reload(); return { success: true };

        // navigate / upload / drag are handled in Rust (CDP-side); they will
        // not arrive here. If they do, surface the mismatch.
        case "navigate":
        case "upload":
        case "drag":
          return err("InvalidArgument", `Action '${action.kind}' is dispatched via CDP, not bridge`);

        default:
          return err("Other", `Unknown action kind: ${action.kind}`);
      }
    } catch (e) {
      return err("Other", e.message);
    }
  }

  // ── Wait ─────────────────────────────────────────────────────────────────

  function handleWait(msg, resolve) {
    const timeout = msg.timeout_ms ?? 10000;
    const cond = msg.condition || { until: "idle" };
    let resolved = false;
    let observer = null;
    let idleTimer = null;

    const finish = (result) => {
      if (resolved) return;
      resolved = true;
      if (observer) observer.disconnect();
      if (idleTimer) clearTimeout(idleTimer);
      clearTimeout(timer);
      resolve(result);
    };

    const timer = setTimeout(() => {
      finish(err("Timeout", "Wait timed out", { kind: "wait", elapsed_ms: timeout }));
    }, timeout);

    const root = document.body || document.documentElement;

    switch (cond.until) {
      case "selector":
        if (document.querySelector(cond.value)) return finish({ success: true });
        observer = new MutationObserver(() => {
          if (document.querySelector(cond.value)) finish({ success: true });
        });
        observer.observe(root, { childList: true, subtree: true });
        break;
      case "text":
        if ((document.body?.innerText || "").includes(cond.value)) {
          return finish({ success: true });
        }
        observer = new MutationObserver(() => {
          if ((document.body?.innerText || "").includes(cond.value)) {
            finish({ success: true });
          }
        });
        observer.observe(root, { childList: true, subtree: true, characterData: true });
        break;
      case "navigation":
        // Caller (Rust) listens for Page.loadEventFired; bridge merely waits.
        finish({ success: true });
        break;
      case "idle":
      default:
        observer = new MutationObserver(() => {
          if (idleTimer) clearTimeout(idleTimer);
          idleTimer = setTimeout(() => finish({ success: true }), 500);
        });
        observer.observe(root, { childList: true, subtree: true });
        idleTimer = setTimeout(() => finish({ success: true }), 500);
    }
  }

  // ── Frame switch (delegated to Rust frame ID, but keeps current API stable) ─

  function switchFrame(msg) {
    const sel = msg.selector || { by: "main" };
    if (sel.by === "main") {
      return { success: true, frame_id: 0, url: location.href };
    }
    // Frame switch in headless mode operates at the CDP level; the bridge
    // here only echoes back. The Rust side should not call this for frames
    // beyond the main frame.
    return err("FrameNotFound", `Bridge cannot switch to frame: ${JSON.stringify(sel)}`, {
      selector: JSON.stringify(sel),
    });
  }

  // ── DOM property helpers ─────────────────────────────────────────────────

  function querySelectorOrErr(selector) {
    const el = document.querySelector(selector);
    return el ? { el } : { error: selectorNotFound(selector) };
  }

  // ── Annotations ──────────────────────────────────────────────────────────

  function addAnnotations(msg) {
    document.getElementById("__webpilot_annotations")?.remove();
    const container = document.createElement("div");
    container.id = "__webpilot_annotations";
    container.style.cssText =
      "position:fixed;top:0;left:0;width:100%;height:100%;z-index:2147483647;pointer-events:none";
    for (const el of (msg.elements || [])) {
      const box = document.createElement("div");
      box.style.cssText =
        `position:fixed;left:${el.x}px;top:${el.y}px;width:${el.w}px;height:${el.h}px;border:2px solid rgba(255,0,0,0.8)`;
      const label = document.createElement("div");
      label.textContent = String(el.index);
      label.style.cssText =
        "position:absolute;top:-16px;left:-2px;background:rgba(255,0,0,0.9);color:#fff;font:bold 11px/14px monospace;padding:0 3px;border-radius:2px";
      box.appendChild(label);
      container.appendChild(box);
    }
    document.documentElement.appendChild(container);
    return { success: true, count: (msg.elements || []).length };
  }

  // ── Storage ──────────────────────────────────────────────────────────────

  function exportStorage() {
    const localObj = {};
    for (let i = 0; i < localStorage.length; i++) {
      const k = localStorage.key(i);
      if (k != null) localObj[k] = localStorage.getItem(k);
    }
    const sessionObj = {};
    for (let i = 0; i < sessionStorage.length; i++) {
      const k = sessionStorage.key(i);
      if (k != null) sessionObj[k] = sessionStorage.getItem(k);
    }
    return { localStorage: localObj, sessionStorage: sessionObj };
  }

  function importStorage(msg) {
    if (msg.localStorage) {
      for (const [k, v] of Object.entries(msg.localStorage)) {
        localStorage.setItem(k, v);
      }
    }
    if (msg.sessionStorage) {
      for (const [k, v] of Object.entries(msg.sessionStorage)) {
        sessionStorage.setItem(k, v);
      }
    }
    return { success: true };
  }

  // ── Element coords (drag) ────────────────────────────────────────────────

  function getElementCoords(msg) {
    const visible = getVisibleElements();
    const src = msg.source >= 1 && msg.source <= visible.length
      ? visible[msg.source - 1]
      : null;
    const tgt = msg.target >= 1 && msg.target <= visible.length
      ? visible[msg.target - 1]
      : null;
    if (!src) return elementNotFound(msg.source, visible.length);
    if (!tgt) return elementNotFound(msg.target, visible.length);
    src.scrollIntoView({ block: "center", behavior: "instant" });
    const sr = src.getBoundingClientRect();
    const tr = tgt.getBoundingClientRect();
    return {
      sx: sr.left + sr.width / 2,
      sy: sr.top + sr.height / 2,
      tx: tr.left + tr.width / 2,
      ty: tr.top + tr.height / 2,
    };
  }

  // ── Dispatcher ───────────────────────────────────────────────────────────

  function handle(msg) {
    switch (msg.type) {
      case "extractDOM":
        return extractDOM(msg.options || {});
      case "extractText":
        return {
          text: document.body?.innerText || "",
          url: location.href,
          title: document.title,
        };
      case "executeAction":
        return executeAction(msg.action);
      case "evaluate": {
        try {
          let result;
          try {
            result = new Function("return (" + msg.code + ")")();
          } catch (syntaxErr) {
            if (syntaxErr instanceof SyntaxError) {
              result = new Function(msg.code)();
            } else {
              throw syntaxErr;
            }
          }
          return {
            success: true,
            result: result !== undefined ? JSON.stringify(result) : null,
          };
        } catch (e) {
          return err("Other", e.message);
        }
      }
      case "wait":
        return new Promise((resolve) => handleWait(msg, resolve));
      case "switchFrame":
        return switchFrame(msg);
      case "tagElement": {
        const visible = getVisibleElements();
        const el = msg.index > 0 && msg.index <= visible.length
          ? visible[msg.index - 1]
          : null;
        if (!el) return elementNotFound(msg.index, visible.length);
        el.setAttribute(msg.attr, "1");
        return { success: true };
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
        const r = querySelectorOrErr(msg.selector);
        if (r.error) return r.error;
        r.el.innerHTML = msg.value;
        return { success: true };
      }
      case "setText": {
        const r = querySelectorOrErr(msg.selector);
        if (r.error) return r.error;
        r.el.textContent = msg.value;
        return { success: true };
      }
      case "setAttr": {
        const r = querySelectorOrErr(msg.selector);
        if (r.error) return r.error;
        r.el.setAttribute(msg.attr, msg.value);
        return { success: true };
      }
      case "getHtml": {
        const r = querySelectorOrErr(msg.selector);
        if (r.error) return r.error;
        return { success: true, value: r.el.innerHTML };
      }
      case "getText": {
        const r = querySelectorOrErr(msg.selector);
        if (r.error) return r.error;
        return { success: true, value: r.el.textContent };
      }
      case "getAttr": {
        const r = querySelectorOrErr(msg.selector);
        if (r.error) return r.error;
        return { success: true, value: r.el.getAttribute(msg.attr) };
      }
      case "exportStorage":
        return exportStorage();
      case "importStorage":
        return importStorage(msg);
      case "addAnnotations":
        return addAnnotations(msg);
      case "removeAnnotations":
        document.getElementById("__webpilot_annotations")?.remove();
        return { success: true };
      case "getElementCoords":
        return getElementCoords(msg);
      case "ping":
        return { ok: true, url: location.href, title: document.title };
      default:
        return err("Other", `Unknown message type: ${msg.type}`);
    }
  }

  // ── Public binding ───────────────────────────────────────────────────────

  window.__webpilot_handle = handle;

  // Extension content-script mode: bridge `chrome.runtime.sendMessage` to
  // `__webpilot_handle`. The listener is replaced on every injection so that
  // SPA-induced bfcache/restore cycles cannot leave a stale listener attached.
  if (typeof chrome !== "undefined" && chrome.runtime?.onMessage) {
    if (window.__webpilot_listener) {
      try {
        chrome.runtime.onMessage.removeListener(window.__webpilot_listener);
      } catch {}
    }
    window.__webpilot_listener = (msg, _sender, sendResponse) => {
      const result = handle(msg);
      if (result && typeof result.then === "function") {
        result.then(sendResponse);
        return true;
      }
      sendResponse(result);
      return false;
    };
    chrome.runtime.onMessage.addListener(window.__webpilot_listener);
  }
})();
