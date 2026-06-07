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
  // ── Per-document state ────────────────────────────────────────────────────
  // `snapshot` — the exact element references the last `extractDom` emitted,
  // in index order. Index-addressed actions resolve against this list, so an
  // index always targets the element the agent actually saw. A null snapshot
  // (no capture in this document yet) or an element that has left the DOM /
  // become invisible is a typed `StaleSnapshot` error — never a silent
  // re-resolution against the live DOM. The same list is the new-element
  // baseline: an element is "new" when its node identity was absent from the
  // previous snapshot (see `extractDom`), so detection needs no separate
  // bookkeeping and cannot drift from what the agent actually saw.
  // `lastUrl` — page identity at the last extraction; a change means the first
  // capture of a new page, where "everything is new" would be noise.
  if (!window.__webpilot_state) {
    window.__webpilot_state = {
      lastUrl: location.href,
      snapshot: null,
    };
  }
  const state = window.__webpilot_state;

  // Codepoint-safe truncation: slice on Unicode scalar values, never on a
  // UTF-16 surrogate half. A raw `String.slice` can cut an emoji/astral
  // character in two and emit a lone surrogate, which then fails JSON
  // serialization over CDP / native messaging and sinks the whole capture.
  const clip = (s, n) => {
    if (s == null) return s;
    const cps = Array.from(s);
    return cps.length > n ? cps.slice(0, n).join("") : s;
  };

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

  const staleSnapshot = (index) =>
    err(
      "StaleSnapshot",
      `Element [${index}] is from a stale or missing snapshot — the page changed since the last capture. Re-capture: webpilot capture --include dom`,
      { index },
    );

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

  // Shadow-DOM traversal is bounded by the number of shadow hosts visited, not
  // an arbitrary nesting depth. A depth cap silently drops controls in
  // component libraries that legitimately nest a dozen-plus shadow roots; a
  // host budget bounds only a pathological tree and records when it clipped.
  const SHADOW_HOST_BUDGET = 5000;

  function queryAllDeep(selector, root, budget) {
    const results = [...root.querySelectorAll(selector)];
    for (const host of root.querySelectorAll("*")) {
      if (!host.shadowRoot) continue;
      if (budget.hosts <= 0) {
        budget.truncated = true;
        break;
      }
      budget.hosts -= 1;
      results.push(...queryAllDeep(selector, host.shadowRoot, budget));
    }
    return results;
  }

  function collectInteractiveElements() {
    const budget = { hosts: SHADOW_HOST_BUDGET, truncated: false };
    const all = queryAllDeep(INTERACTIVE_SELECTOR, document, budget);
    const seen = new Set(all);
    const add = (el) => {
      all.push(el);
      seen.add(el);
    };

    // Degenerate-size floor for the HEURISTIC passes only (markers and
    // cursor:pointer): 1px telemetry/spacer nodes — jsaction tracking pixels
    // especially — must not mint phantom targets, while real small controls
    // (8px icon buttons) still qualify. The semantic allowlist above has no
    // size gate: a real <button> is interactive regardless of size.
    const isDegenerate = (rect) => rect.width < 5 || rect.height < 5;

    // Explicit interaction markers. `tabindex` qualifies only when >= 0: a
    // tabindex of -1 is script-only focus (route announcers, modal roots,
    // headings) and is not a click affordance, so it must not mint a phantom
    // target.
    const markerSel = '[onclick],[data-action],[ng-click],' +
      '[v-on\\:click],[\\@click],[data-click],[jsaction]';
    const markers = new Set(document.querySelectorAll(markerSel));
    for (const el of document.querySelectorAll("[tabindex]")) {
      const ti = parseInt(el.getAttribute("tabindex"), 10);
      if (Number.isFinite(ti) && ti >= 0) markers.add(el);
    }
    for (const el of markers) {
      if (seen.has(el)) continue;
      if (STANDARD_TAGS.has(el.tagName.toLowerCase())) continue;
      if (el.getAttribute("role")) continue;
      if (isDegenerate(el.getBoundingClientRect())) continue;
      if (!isVisible(el)) continue;
      add(el);
    }

    // cursor:pointer signals a target only on the INNERMOST such element.
    // Cards, rows and banners set pointer on a wrapper that merely contains the
    // real control; surfacing the wrapper hands the agent a giant phantom
    // overlapping the actual button. Iterate in REVERSE document order so every
    // descendant is visited before its ancestor — a pointer child is collected
    // first, and the ancestor is then skipped for wrapping it. The scan is
    // viewport-bounded before any style read: computing style for every node in
    // a long document is a real cost, and off-screen pointer-only elements
    // reappear in the next capture after the agent scrolls.
    const everything = document.querySelectorAll("*");
    for (let i = everything.length - 1; i >= 0; i--) {
      const el = everything[i];
      if (seen.has(el)) continue;
      if (STANDARD_TAGS.has(el.tagName.toLowerCase())) continue;
      if (el.getAttribute("role")) continue;
      const rect = el.getBoundingClientRect();
      if (rect.bottom < 0 || rect.top > innerHeight) continue;
      if (isDegenerate(rect)) continue;
      let cursor;
      try {
        cursor = getComputedStyle(el).cursor;
      } catch {
        continue;
      }
      if (cursor !== "pointer") continue;
      if (!isVisible(el)) continue;
      let wrapsCollected = false;
      for (const c of seen) {
        if (el.contains(c)) {
          wrapsCollected = true;
          break;
        }
      }
      if (wrapsCollected) continue;
      add(el);
    }
    return { all, shadowTruncated: budget.truncated };
  }

  // Single visibility predicate shared by extraction and action-time
  // revalidation, so the two can never drift apart. `checkVisibility()` is
  // called WITHOUT options: the option keys are newer than some installed
  // Chromes and unknown dictionary keys are silently dropped, which would
  // disable the very checks they name. The bare call covers display:none /
  // display:contents ancestors and content-visibility:hidden; the explicit
  // style reads cover visibility and the element's own opacity on every
  // Chrome version. A zero-area box is never actionable, so real layout is
  // still required.
  function isVisible(el) {
    const rect = el.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return false;
    if (!el.checkVisibility()) return false;
    const style = getComputedStyle(el);
    return (
      style.visibility !== "hidden" &&
      style.visibility !== "collapse" &&
      parseFloat(style.opacity) > 0
    );
  }

  // Resolve an element index against the stored snapshot.
  //
  // The snapshot holds *direct element references*, so an index can never
  // silently resolve to a different node than the one captured — a reference is
  // stable identity. Revalidation therefore only confirms the captured node is
  // still live (`isConnected`) and visible; that is the complete, correct, and
  // false-positive-free staleness signal:
  //   - a full navigation builds a new document with a fresh `window`, so this
  //     whole state object is gone and `snapshot` is null → StaleSnapshot;
  //   - an SPA navigation that unmounts the old DOM detaches the captured nodes
  //     → `isConnected === false` → StaleSnapshot;
  //   - a same-document change that keeps the node (hash change, content update,
  //     re-render that preserves the element) leaves it connected → still valid,
  //     which is correct: it is the exact element the agent saw.
  // We deliberately do NOT invalidate on `location.href` change: that would
  // falsely reject a valid index after a hash/anchor navigation or for an
  // element that persists across an SPA route — exactly the kind of heuristic
  // false positive this design exists to avoid. The agent re-captures after an
  // action it knows changed the page (the documented capture→act→capture loop).
  function resolveIndex(index) {
    if (index == null) {
      return { error: err("InvalidArgument", "Missing index") };
    }
    const snap = state.snapshot;
    if (!snap) return { error: staleSnapshot(index) };
    if (index < 1 || index > snap.length) {
      return { error: elementNotFound(index, snap.length) };
    }
    const el = snap[index - 1];
    if (!el.isConnected || !isVisible(el)) {
      return { error: staleSnapshot(index) };
    }
    return { el };
  }

  function resolveLabel(el) {
    const labelledBy = el.getAttribute("aria-labelledby");
    if (labelledBy) {
      const parts = labelledBy
        .split(/\s+/)
        .map((id) => document.getElementById(id)?.textContent?.trim())
        .filter(Boolean);
      if (parts.length > 0) return clip(parts.join(" "), 80);
    }
    // aria-label is an explicit label for the field, ranked by ARIA right after
    // aria-labelledby and above a native <label>. Without it, `find --label`
    // can't match inputs labelled only with aria-label.
    const ariaLabel = el.getAttribute("aria-label")?.trim();
    if (ariaLabel) return clip(ariaLabel, 80);
    if (el.labels && el.labels.length > 0) {
      return clip(el.labels[0].textContent.trim(), 80) || null;
    }
    if (el.id) {
      const label = document.querySelector(`label[for="${CSS.escape(el.id)}"]`);
      if (label) return clip(label.textContent.trim(), 80) || null;
    }
    const parent = el.closest("label");
    if (parent) {
      const text = clip(parent.textContent.trim().replace(/\s+/g, " "), 80);
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
          value: o.getAttribute("data-value") || clip(o.textContent.trim(), 80),
          text: clip(o.textContent.trim(), 80),
          selected: o.getAttribute("aria-selected") === "true",
        }));
      }
    }
    return undefined;
  }

  // Occlusion by multi-point hit-test. A single centre probe both over-reports
  // (a sticky header or tooltip covering only the middle marks a clickable
  // control occluded) and under-reports (a transparent gap at the exact centre
  // pixel hides a real overlay). Sample the centre plus four inset corners and
  // judge by majority of the points that actually fall inside the viewport.
  function isOccluded(el, rect) {
    const inset = 0.15;
    const xs = [0.5, inset, 1 - inset, inset, 1 - inset];
    const ys = [0.5, inset, inset, 1 - inset, 1 - inset];
    let tested = 0;
    let blocked = 0;
    for (let i = 0; i < xs.length; i++) {
      const px = rect.left + rect.width * xs[i];
      const py = rect.top + rect.height * ys[i];
      if (px < 0 || py < 0 || px >= innerWidth || py >= innerHeight) continue;
      tested++;
      const top = document.elementFromPoint(px, py);
      if (top && top !== el && !el.contains(top) && !top.contains(el)) blocked++;
    }
    return tested > 0 && blocked * 2 > tested;
  }


  function extractDom(options) {
    try {
      const start = performance.now();
      const urlChanged = state.lastUrl !== location.href;
      state.lastUrl = location.href;
      // New-element baseline by node identity: the previous snapshot holds the
      // exact elements the agent last saw, so "new" means absent from it.
      // Identity is collision-free (no two elements share it) and survives
      // re-renders that keep the node. With no usable baseline — the first
      // capture in this document, or the first after the URL changed — nothing
      // is flagged (`prevNodes = null`): a fresh page is not "all new".
      const prevNodes =
        !urlChanged && state.snapshot ? new Set(state.snapshot) : null;
      const { all, shadowTruncated } = collectInteractiveElements();
      if (shadowTruncated) {
        console.warn("[WebPilot] shadow-DOM traversal hit its host budget; some controls may be omitted");
      }
      const totalNodes = document.querySelectorAll("*").length;
      const elements = [];
      const picked = [];
      let idx = 1;
      const includeBounds = options.bounds || false;

      for (const el of all) {
        if (!isVisible(el)) continue;
        const rect = el.getBoundingClientRect();

        const tag = el.tagName.toLowerCase();
        const innerText = (el.innerText || el.textContent || "")
          .trim()
          .replace(/\s+/g, " ");
        const text = (tag === "input" || tag === "textarea")
          ? (el.placeholder || el.getAttribute("aria-label") || "")
          : clip(innerText, 300);

        // Display-only identifier — the actionable handle is the snapshot
        // index, never this. Emit any non-empty id (codepoint-clipped); modern
        // frameworks mint ids like ":r1:" (React useId) that a character
        // allowlist would wrongly drop, and it is never used to build a
        // selector here.
        const elemId = el.id ? clip(el.id, 50) : undefined;

        const entry = {
          index: idx++,
          tag,
          id: elemId,
          role: el.getAttribute("role") || undefined,
          text,
          name: el.getAttribute("aria-label") || el.getAttribute("title") || undefined,
          value: (el.value != null && el.value !== "")
            ? clip(String(el.value), 100)
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
          entry.description = clip(parts.join(" "), 120) || undefined;
        }

        if (options.occlusion) {
          entry.occluded = isOccluded(el, rect);
        }

        if (includeBounds) {
          entry.bounds = {
            x: Math.round(rect.x),
            y: Math.round(rect.y),
            w: Math.round(rect.width),
            h: Math.round(rect.height),
          };
        }

        entry.is_new = prevNodes ? !prevNodes.has(el) : false;

        // Strip absent (undefined) fields, and `false` only where it is the
        // mere absence of a property. `checked` and `expanded` are genuine
        // tri-states — `false` means "unchecked checkbox" / "collapsed
        // disclosure", distinct from "not a checkbox" / "not expandable" — so
        // they survive alongside `disabled`/`focused`.
        for (const k of Object.keys(entry)) {
          if (entry[k] === undefined ||
              (entry[k] === false &&
                k !== "disabled" && k !== "focused" &&
                k !== "checked" && k !== "expanded")) {
            delete entry[k];
          }
        }
        elements.push(entry);
        picked.push(el);
      }

      state.snapshot = picked;

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
        scroll_percent: sh > vh ? Math.round((sy / (sh - vh)) * 100) : 0,
        extraction_ms: Math.round(performance.now() - start),
      };
    } catch (e) {
      // A genuine extraction failure must surface as a typed error, not a
      // fabricated empty snapshot — an agent that reads "0 interactive
      // elements" on a populated page makes catastrophically wrong decisions.
      return err("Other", `DOM extraction failed: ${e.message}`);
    }
  }

  // ── Action execution ─────────────────────────────────────────────────────

  function resolveTarget(action) {
    const r = resolveIndex(action.index);
    return r.error ? { error: r.error } : { target: r.el };
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
            // requestSubmit() fires native validation and the submit handler
            // (so a preventDefault-based AJAX form stays put); submit() bypasses
            // both and force-navigates. They are alternatives, never both — the
            // old `requestSubmit?.() || submit()` ran submit() every time
            // because requestSubmit returns undefined, double-submitting.
            if (el.form.requestSubmit) el.form.requestSubmit();
            else el.form.submit();
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

        // navigate / upload / drag are dispatched via CDP (headless: Rust;
        // browser: the service worker), never through the bridge. If one
        // arrives here it is a routing mismatch.
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
    const src = resolveIndex(msg.source);
    if (src.error) return src.error;
    const tgt = resolveIndex(msg.target);
    if (tgt.error) return tgt.error;
    src.el.scrollIntoView({ block: "center", behavior: "instant" });
    const sr = src.el.getBoundingClientRect();
    const tr = tgt.el.getBoundingClientRect();
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
      case "extractDom":
        return extractDom(msg.options || {});
      case "extractText":
        // Capped here, in the one place both modes share, so a giant page
        // costs the same bounded tokens everywhere (codepoint-safe).
        return {
          text: clip(document.body?.innerText || "", 50000),
          url: location.href,
          title: document.title,
        };
      case "executeAction":
        return executeAction(msg.action);
      case "eval": {
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
      case "tagElement": {
        const r = resolveIndex(msg.index);
        if (r.error) return r.error;
        r.el.setAttribute(msg.attr, "1");
        return { success: true };
      }
      case "untagElement": {
        const tagged = document.querySelector(`[${msg.attr}]`);
        if (tagged) tagged.removeAttribute(msg.attr);
        return { success: true };
      }
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
