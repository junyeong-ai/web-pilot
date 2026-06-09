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
    // Any editable host, not just `contenteditable="true"`: a bare
    // `contenteditable` (empty value) and `contenteditable="plaintext-only"` are
    // both editable, so the literal-"true" match dropped real comment boxes and
    // rich-text editors. Exclude only an explicit `false` (case-insensitive).
    '[contenteditable]:not([contenteditable="false" i]), details > summary';

  const STANDARD_TAGS = new Set([
    "a", "button", "input", "select", "textarea", "summary",
  ]);

  // Shadow-DOM traversal is bounded by the number of shadow hosts visited, not
  // an arbitrary nesting depth. A depth cap silently drops controls in
  // component libraries that legitimately nest a dozen-plus shadow roots; a
  // host budget bounds only a pathological tree and records when it clipped.
  const SHADOW_HOST_BUDGET = 5000;

  // Run several selectors over the document AND every open shadow root in a
  // SINGLE host walk, returning one match array per selector (parallel to the
  // input). One traversal and one shared budget for all selectors — running
  // each selector through its own `queryAllDeepMulti` would walk the shadow-host
  // tree once per selector and split the budget unevenly.
  function queryAllDeepMulti(selectors, root, budget) {
    const results = selectors.map(() => []);
    const visit = (r) => {
      selectors.forEach((sel, i) => {
        for (const el of r.querySelectorAll(sel)) results[i].push(el);
      });
      for (const host of r.querySelectorAll("*")) {
        if (!host.shadowRoot) continue;
        if (budget.hosts <= 0) {
          budget.truncated = true;
          return;
        }
        budget.hosts -= 1;
        visit(host.shadowRoot);
      }
    };
    visit(root);
    return results;
  }

  function collectInteractiveElements() {
    const budget = { hosts: SHADOW_HOST_BUDGET, truncated: false };

    // Explicit interaction markers. `tabindex` qualifies only when >= 0: a
    // tabindex of -1 is script-only focus (route announcers, modal roots,
    // headings) and is not a click affordance, so it must not mint a phantom
    // target. The marker and tabindex passes pierce open shadow roots like the
    // semantic pass — a design-system custom element whose clickable part
    // lives in its shadow root and carries `onclick`/`jsaction` rather than a
    // semantic tag would otherwise be invisible to the agent.
    const markerSel = '[onclick],[data-action],[ng-click],' +
      '[v-on\\:click],[\\@click],[data-click],[jsaction]';

    // One shadow-DOM walk gathers all three candidate sets under one budget.
    const [all, markerEls, tabindexEls] = queryAllDeepMulti(
      [INTERACTIVE_SELECTOR, markerSel, "[tabindex]"],
      document,
      budget,
    );
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

    const markers = new Set(markerEls);
    for (const el of tabindexEls) {
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

    // cursor:pointer discovery stays light-DOM only: `el.contains()` does not
    // cross shadow boundaries, so the innermost/wrapping logic below cannot be
    // computed correctly across a shadow root — and a cursor:pointer element
    // carrying no marker, role, or semantic tag inside a shadow root is exotic.
    // Semantic and marker controls in shadow DOM are already covered above.
    //
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
        // Only a VISIBLE collected descendant makes this a mere wrapper. A hidden
        // one (e.g. a `display:none` input) is dropped from the snapshot, so
        // letting it mark the wrapper "not innermost" would leave a real
        // cursor:pointer click target unindexed and unaddressable.
        if (el.contains(c) && isVisible(c)) {
          wrapsCollected = true;
          break;
        }
      }
      if (wrapsCollected) continue;
      add(el);
    }
    // Indices must follow document (reading) order, not the order the three
    // passes ran — semantic, then markers, then cursor:pointer. Otherwise a
    // `<div onclick>` sitting ABOVE a `<button>` would be indexed AFTER it, so
    // the agent's `[N]` no longer tracks top-to-bottom layout.
    // `compareDocumentPosition` orders light-DOM nodes exactly and cross-tree
    // (shadow) nodes consistently.
    all.sort((a, b) => {
      const pos = a.compareDocumentPosition(b);
      if (pos & Node.DOCUMENT_POSITION_FOLLOWING) return -1;
      if (pos & Node.DOCUMENT_POSITION_PRECEDING) return 1;
      return 0;
    });
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
    const own = getComputedStyle(el);
    // `visibility` is inherited, so the element's own computed value already
    // accounts for an ancestor's `hidden`/`collapse`.
    if (own.visibility === "hidden" || own.visibility === "collapse") return false;
    // `opacity` is NOT inherited: an `opacity:0` ANCESTOR paints the whole
    // subtree transparent without lowering the child's own opacity, so a bare
    // own-opacity check would emit an invisible control as actionable. Walk up —
    // across open shadow boundaries to the host, since extraction pierces shadow
    // roots — and reject if any box is fully transparent. Done manually rather
    // than via `checkVisibility`'s `opacityProperty` option, which is silently
    // dropped on Chromes that predate it.
    for (let node = el; node && node.nodeType === 1; ) {
      if (parseFloat(getComputedStyle(node).opacity) <= 0) return false;
      const root = node.getRootNode();
      node = node.parentElement || (root instanceof ShadowRoot ? root.host : null);
    }
    return true;
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
      // An `aria-labelledby` IDREF is scoped to the element's own tree, so for a
      // control inside a shadow root the label element lives in that SAME shadow
      // root — `document.getElementById` returns null, the name comes back empty,
      // and `find --label` can't match the component. Resolve through the
      // element's root (the ShadowRoot or the document); both expose
      // getElementById, and an IDREF never crosses the shadow boundary anyway.
      const root = el.getRootNode();
      const scope = typeof root.getElementById === "function" ? root : document;
      const parts = labelledBy
        .split(/\s+/)
        .map((id) => scope.getElementById(id)?.textContent?.trim())
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
      // A `label[for]` IDREF is tree-scoped, so a control inside a shadow root
      // pairs with a label in that SAME root — query the element's root, not
      // `document` (which would miss it), mirroring aria-labelledby/describedby.
      // `el.labels` already covers standard labelable controls; this reaches a
      // custom labelable element a shadow root can hold.
      const label = el.getRootNode().querySelector(`label[for="${CSS.escape(el.id)}"]`);
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

  // The option list is capped so a giant `<select>` (countries, timezones) costs
  // bounded tokens — but `truncated` flags the cut so the agent never reads the
  // shown slice as the whole list (the same honesty as `shadow_truncated` and the
  // console/network caps). Returns `{ list, truncated }`, or `undefined` for a
  // non-option element. One walk per element: the truncation flag comes from the
  // same collected set the list is sliced from, never a second query.
  const OPTION_CAP = 50;
  function extractOptions(el, tag) {
    let all;
    let mapper;
    if (tag === "select") {
      all = [...el.options];
      mapper = (o) => ({ value: o.value, text: o.text, selected: o.selected });
    } else {
      const role = el.getAttribute("role");
      if (role === "listbox" || role === "menu" || role === "combobox") {
        const opts = el.querySelectorAll('[role="option"], [role="menuitem"]');
        if (opts.length > 0) {
          all = [...opts];
          mapper = (o) => ({
            // `?? clip(...)`, not `|| clip(...)`: an option with an explicit
            // `data-value=""` (a "none"/placeholder choice) has the empty string
            // as its real value — `||` would discard it for the visible text and
            // mis-report what `action select` must send. `getAttribute` returns
            // null only when the attribute is absent, which correctly falls back.
            value: o.getAttribute("data-value") ?? clip(o.textContent.trim(), 80),
            text: clip(o.textContent.trim(), 80),
            selected: o.getAttribute("aria-selected") === "true",
          });
        }
      }
    }
    if (!all) return undefined;
    return {
      list: all.slice(0, OPTION_CAP).map(mapper),
      truncated: all.length > OPTION_CAP,
    };
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
        // An input's VISIBLE LABEL lives in a different field per type: a
        // button-type carries it in `value` (`<input type=submit value="Search">`
        // reads "Search"), an image button in `alt`, while a text field's `text`
        // is its placeholder hint. Emitting the right one as `text` makes the
        // control findable by its label (`find --text "Search"`) and shows it in
        // the snapshot, instead of an empty `text` next to a `value`/`alt`
        // `find --text` never searches.
        const inputText = (e) => {
          if (["submit", "button", "reset"].includes(e.type)) return clip(String(e.value || ""), 300);
          if (e.type === "image") return clip(e.getAttribute("alt") || "", 300);
          return e.placeholder || e.getAttribute("aria-label") || "";
        };
        const text =
          tag === "input"
            ? inputText(el)
            : tag === "textarea"
              ? el.placeholder || el.getAttribute("aria-label") || ""
              : clip(innerText, 300);

        // Display-only identifier — the actionable handle is the snapshot
        // index, never this. Emit any non-empty id (codepoint-clipped); modern
        // frameworks mint ids like ":r1:" (React useId) that a character
        // allowlist would wrongly drop, and it is never used to build a
        // selector here.
        const elemId = el.id ? clip(el.id, 50) : undefined;

        const optionData = extractOptions(el, tag);
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
          options: optionData?.list,
          options_truncated: optionData?.truncated || undefined,
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
          // An `aria-describedby` IDREF is scoped to the element's own tree, so a
          // control inside a shadow root references a description element in that
          // SAME shadow root — `document.getElementById` would miss it and the
          // agent would lose the field's help/constraint/error text. Resolve
          // through the element's root, exactly as `aria-labelledby` does.
          const root = el.getRootNode();
          const scope = typeof root.getElementById === "function" ? root : document;
          const parts = describedBy
            .split(/\s+/)
            .map((id) => scope.getElementById(id)?.textContent?.trim())
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
        // Surface the shadow-host budget clip to the agent — a page-console
        // warn alone is invisible to it, and a silently short index leads to
        // index actions that can't resolve a control that was never emitted.
        shadow_truncated: shadowTruncated,
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

  // Whether a click on `el` will queue a top-level document navigation — the
  // signal the settle logic needs because a link click's navigation is queued
  // (HTML spec), so its frameStartedLoading can arrive AFTER the click response
  // and miss a one-shot event drain. Deterministic and derived at click time:
  // a non-prevented click on a self-targeting `a[href]` whose http(s)/file
  // destination differs from the current document (a pure fragment change loads
  // nothing). `notCanceled` is the click event's dispatch result, so a
  // preventDefault'd SPA link correctly reports no navigation.
  // True when this click loads a new document IN THE CURRENT FRAME — a
  // non-prevented link to a different http(s)/file URL (not a fragment, not a
  // popup target). The frame may be the top frame or a switched iframe; the
  // settle layer uses this to wait for an iframe-internal navigation the
  // top-frame `navigates` signal can't see.
  function frameNavigates(el, notCanceled) {
    if (!notCanceled) return false;
    const a = el.closest("a[href]");
    if (!a) return false;
    const target = (a.target || "").trim().toLowerCase();
    if (target && target !== "_self" && target !== "_top" && target !== "_parent") {
      return false; // opens a new context (a popup), not a same-frame load
    }
    let dest, cur;
    try {
      dest = new URL(a.href, location.href);
      cur = new URL(location.href);
    } catch {
      return false;
    }
    if (dest.protocol !== "http:" && dest.protocol !== "https:" && dest.protocol !== "file:") {
      return false; // javascript:/mailto:/tel:/… never load a document
    }
    // A change confined to the fragment is a same-document nav (no load event):
    // hinting it would burn the settle's whole PROBE waiting for a commit that
    // never comes.
    if (dest.origin === cur.origin && dest.pathname === cur.pathname && dest.search === cur.search) {
      return false;
    }
    return true;
  }

  // `navigates` is the TOP-frame subset of `frameNavigates`: only a top-level
  // navigation is `url_changed`, the signal driving the main-frame settle.
  function clickNavigates(el, notCanceled) {
    return window.top === window && frameNavigates(el, notCanceled);
  }

  function reliableClick(el) {
    el.scrollIntoView({ block: "center", behavior: "instant" });
    const rect = el.getBoundingClientRect();
    const x = rect.left + rect.width / 2;
    const y = rect.top + rect.height / 2;
    const opts = {
      // `composed: true` so the event crosses shadow boundaries: capture indexes
      // controls inside open shadow roots (queryAllDeep), and a non-composed event
      // dispatched on one stops at its shadow root — a host/document delegated
      // click listener would never fire, making the click a silent no-op.
      bubbles: true, composed: true, cancelable: true, clientX: x, clientY: y, button: 0, view: window,
    };
    el.dispatchEvent(new PointerEvent("pointerdown", opts));
    el.dispatchEvent(new MouseEvent("mousedown", opts));
    el.dispatchEvent(new PointerEvent("pointerup", opts));
    el.dispatchEvent(new MouseEvent("mouseup", opts));
    const notCanceled = el.dispatchEvent(new MouseEvent("click", opts));
    return {
      navigates: clickNavigates(el, notCanceled),
      frameNavigates: frameNavigates(el, notCanceled),
    };
  }

  function reliableType(el, text, clear) {
    el.scrollIntoView({ block: "center", behavior: "instant" });
    el.focus();

    if (el.isContentEditable) {
      if (clear) el.innerHTML = "";
      document.execCommand("insertText", false, text);
      // The `innerHTML` clear and an empty `text` fire no native event, and a
      // contenteditable never fires `change` — so a framework-bound editor
      // (Draft/Slate/ProseMirror, a React onChange) would miss the edit. Mirror
      // the input path and dispatch both; a redundant `input` from a non-empty
      // execCommand insert is harmless, since listeners re-read the live text.
      el.dispatchEvent(new InputEvent("input", {
        bubbles: true, inputType: "insertText", data: text,
      }));
      el.dispatchEvent(new Event("change", { bubbles: true }));
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

  // `<input>` types that hold user-typed text. Excludes the toggles/pickers
  // (checkbox/radio/color/range), buttons (button/submit/reset/image), and
  // file/hidden — typing into those just stamps a meaningless expando `.value`.
  const TEXT_INPUT_TYPES = new Set([
    "text", "search", "email", "url", "tel", "password", "number",
    "date", "time", "datetime-local", "month", "week",
  ]);

  // Whether `action type` can meaningfully enter text here: a contenteditable, a
  // textarea, or a text-admitting input. Anything else (a link, button, div,
  // checkbox, select) would silently no-op while reporting success, so the caller
  // rejects it instead.
  function isTextEditable(el) {
    if (el.isContentEditable) return true;
    if (el.tagName === "TEXTAREA") return true;
    if (el.tagName === "INPUT") return TEXT_INPUT_TYPES.has((el.type || "text").toLowerCase());
    return false;
  }

  // Whether the control can't be activated by a real user. A synthetic
  // `dispatchEvent` (click) or `.value` setter (type) would still fire on a
  // disabled control, so both actions reject one — the agent must learn the
  // control is inert, not get a phantom success with a handler firing in a state
  // the page never allows. `:disabled` also catches a control disabled by an
  // ancestor `<fieldset disabled>` (which the bare `.disabled` property misses);
  // `aria-disabled` covers custom/ARIA controls.
  function isDisabled(el) {
    return el.matches(":disabled") || el.getAttribute("aria-disabled") === "true";
  }

  // The genuinely-focused element, descending through open shadow roots:
  // `document.activeElement` only ever names the outermost shadow HOST, so a
  // focused element INSIDE a shadow root is invisible to it. Walk the chain so a
  // focus check can recognise a shadow-hosted control as focused.
  function deepActiveElement() {
    let el = document.activeElement;
    while (el?.shadowRoot?.activeElement) el = el.shadowRoot.activeElement;
    return el;
  }

  function executeAction(action) {
    try {
      switch (action.kind) {
        case "click": {
          const r = resolveTarget(action);
          if (r.error) return r.error;
          // A disabled control can't be activated by a real user, but the
          // synthetic click below would fire its handlers anyway — reject rather
          // than report a success the page would never produce.
          if (isDisabled(r.target)) {
            return err(
              "InvalidArgument",
              "Cannot click a disabled element — a real user can't activate it",
            );
          }
          // `navigates` tells the settle layer a TOP-level navigation is coming
          // (drives `url_changed`); `frame_navigates` tells it the CURRENT frame
          // will load a new document — the only signal for an iframe-internal
          // navigation under a switched frame — even before the queued
          // frameStartedLoading is observable.
          const nav = reliableClick(r.target);
          return { success: true, navigates: nav.navigates, frame_navigates: nav.frameNavigates };
        }

        case "type": {
          const r = resolveTarget(action);
          if (r.error) return r.error;
          if (!isTextEditable(r.target)) {
            return err(
              "InvalidArgument",
              `Cannot type into <${r.target.tagName.toLowerCase()}${
                r.target.tagName === "INPUT" ? ` type=${r.target.type}` : ""
              }>: not a text field — use action click for buttons/links, action select for dropdowns`,
            );
          }
          // A `.value` setter succeeds on a disabled/read-only field via JS even
          // though a real user can't edit it — and the page then never submits a
          // disabled value, and resets or ignores a read-only one. Reject loudly
          // rather than report a success the page won't honor.
          if (isDisabled(r.target)) {
            return err(
              "InvalidArgument",
              "Cannot type into a disabled field — its value is never submitted",
            );
          }
          if (r.target.readOnly) {
            return err(
              "InvalidArgument",
              "Cannot type into a read-only field — the page rejects edits to it",
            );
          }
          reliableType(r.target, action.text, action.clear);
          return { success: true };
        }

        case "scroll": {
          // `amount` is optional (absent → 600). An explicit 0 is a no-op the
          // tool schema forbids (minimum 1), so reject it rather than report a
          // scroll that moved nothing.
          if (action.amount === 0) {
            return err("InvalidArgument", "scroll amount must be at least 1 pixel");
          }
          const amt = action.amount ?? 600;
          const dy = action.direction === "up" ? -amt : amt;
          // `behavior: "instant"` (not the bare positional form, which inherits the
          // page's `scroll-behavior: smooth`): a CSS-animated scroll returns before
          // it finishes, so the auto-capture would report a mid-animation scroll_y
          // that doesn't match where the page lands. `scroll_to` already forces it.
          window.scrollBy({ top: dy, left: 0, behavior: "instant" });
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
          // Only a native `<select>` has a meaningful `.value`/`change` for this
          // path. Other elements expose `.options` too (a `<datalist>`), and
          // setting `.value` on them does nothing — so without this guard
          // `action select` would report success while selecting nothing, the
          // same silent-wrong-success class as `action type`/`focus` on a wrong
          // element. A custom dropdown is driven with `action click`.
          if (!(r.target instanceof HTMLSelectElement)) {
            return err(
              "InvalidArgument",
              `<${r.target.tagName.toLowerCase()}> is not a <select> — use action click to open a custom dropdown, then click the option`,
            );
          }
          // A disabled <select> can't be changed by a real user; the `.value`
          // setter below would change it anyway and fire `change` — reject, like
          // click/type, rather than report a selection the page disallows.
          if (isDisabled(r.target)) {
            return err(
              "InvalidArgument",
              "Cannot select in a disabled <select> — a real user can't change it",
            );
          }
          // Setting `.value` to a value no <option> carries silently leaves a
          // <select> at "" (selectedIndex -1). Firing `change` and returning
          // success then would report a selection that did not happen — so
          // verify the option exists and fail typed instead.
          const opts = [...r.target.options];
          const match = opts.find((o) => o.value === action.value);
          if (!match) {
            // Put the valid values IN the message, not only the data: the Rust
            // `InvalidArgument` variant carries just a string, so a structured
            // `available` field is dropped on the way to JSON/MCP. A
            // self-contained message keeps the retry guidance in every surface.
            const available = opts.map((o) => o.value);
            const shown = available
              .slice(0, 12)
              .map((v) => JSON.stringify(v))
              .join(", ");
            const more =
              available.length > 12 ? `, … (${available.length} total)` : "";
            return err(
              "InvalidArgument",
              `No <option> with value "${action.value}" in this <select>. Available: ${shown}${more}`,
              { value: action.value, available },
            );
          }
          // The option exists but a real user can't pick a disabled or hidden one;
          // assigning `.value` to it would select it anyway — reject instead of
          // reporting a choice the page forbids.
          if (match.disabled || match.hidden) {
            return err(
              "InvalidArgument",
              `<option> "${action.value}" is ${match.disabled ? "disabled" : "hidden"} — a real user can't select it`,
            );
          }
          r.target.value = action.value;
          r.target.dispatchEvent(new Event("change", { bubbles: true }));
          return { success: true };
        }

        case "focus": {
          const r = resolveTarget(action);
          if (r.error) return r.error;
          r.target.focus();
          // `focus()` on a non-focusable element (a static div/span with no
          // tabindex) is a silent no-op. Verify focus actually landed before
          // reporting success, accepting both shapes: the target IS the focused
          // element (`document.activeElement`, which for a delegatesFocus host is
          // the host itself), OR the target is a control inside a shadow root that
          // took focus (where `document.activeElement` only names the host, so we
          // descend the shadow-active chain to find it).
          if (r.target !== document.activeElement && r.target !== deepActiveElement()) {
            return err(
              "InvalidArgument",
              `<${r.target.tagName.toLowerCase()}> took no focus — it is not a form control and has no tabindex`,
            );
          }
          return { success: true };
        }

        case "back": history.back(); return { success: true };
        case "forward": history.forward(); return { success: true };
        case "reload": location.reload(); return { success: true };

        // navigate / upload / drag / hover / key_press are dispatched via CDP
        // (headless: Rust; browser: the service worker) for native input
        // fidelity, never through the bridge. If one arrives here it is a
        // routing mismatch.
        case "navigate":
        case "upload":
        case "drag":
        case "hover":
        case "key_press":
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
    let pollTimer = null;

    const finish = (result) => {
      if (resolved) return;
      resolved = true;
      if (observer) observer.disconnect();
      if (idleTimer) clearTimeout(idleTimer);
      if (pollTimer) clearInterval(pollTimer);
      clearTimeout(timer);
      resolve(result);
    };

    const timer = setTimeout(() => {
      finish(err("Timeout", "Wait timed out", { kind: "wait", elapsed_ms: timeout }));
    }, timeout);

    const root = document.body || document.documentElement;

    switch (cond.until) {
      case "selector": {
        // Validate the selector once: an invalid one throws a SyntaxError, which
        // must be a typed InvalidArgument, not a wait that runs to its full
        // timeout. Past this guard the selector is known-valid, so the observer's
        // re-query cannot throw.
        let initial;
        try {
          initial = document.querySelector(cond.value);
        } catch {
          // `finish` resolves the wait with this value as-is — the Rust side
          // reads it like the `Timeout` branch does (`finish(err(...))`), a bare
          // error object, NOT wrapped in `{ error }`. Wrapping it made the
          // unrecognized shape parse as success, so an invalid selector reported
          // the wait satisfied instead of a typed InvalidArgument.
          return finish(err("InvalidArgument", `Invalid CSS selector: ${JSON.stringify(cond.value)}`));
        }
        if (initial) return finish({ success: true });
        observer = new MutationObserver(() => {
          if (document.querySelector(cond.value)) finish({ success: true });
        });
        // `attributes: true` as well as `childList`: a selector can start
        // matching not only when a node is inserted but when an existing node
        // gains a class/attribute (`.active`, `[aria-expanded=true]`, …) — an
        // attribute mutation the childList-only observer would never see, timing
        // the wait out even though the element now matches.
        observer.observe(root, { childList: true, subtree: true, attributes: true });
        // A MutationObserver cannot see a property/state change — `el.checked =
        // true`, `el.disabled = false`, a `.value` edit — because those fire no
        // mutation, so a state pseudo-class (`:checked`, `:disabled`, `:valid`,
        // `:focus`) would run to the full timeout though it already matches. Poll
        // alongside the observer to catch them within one interval; the observer
        // still gives instant response to structural and attribute changes.
        pollTimer = setInterval(() => {
          if (document.querySelector(cond.value)) finish({ success: true });
        }, 100);
        break;
      }
      case "text": {
        const hasText = () => (document.body?.innerText || "").includes(cond.value);
        if (hasText()) {
          return finish({ success: true });
        }
        observer = new MutationObserver(() => {
          if (hasText()) finish({ success: true });
        });
        // `attributes` too: `innerText` gains the text when an element stops being
        // `display:none` via a style/class change — an attribute mutation, not a
        // childList/characterData one. The poll alongside catches visibility driven
        // by a stylesheet rule, which fires no mutation at all — the same
        // belt-and-suspenders the selector wait uses for state it can't observe.
        observer.observe(root, {
          childList: true,
          subtree: true,
          characterData: true,
          attributes: true,
        });
        pollTimer = setInterval(() => {
          if (hasText()) finish({ success: true });
        }, 100);
        break;
      }
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
        // Watch attributes and character data as well as node insertion: a page
        // still mutating only class/attribute (a spinner toggling `aria-busy`) or
        // text (a live counter) is NOT idle, and a childList-only observer would
        // declare it settled after the first 500ms quiet window.
        observer.observe(root, {
          childList: true,
          subtree: true,
          attributes: true,
          characterData: true,
        });
        idleTimer = setTimeout(() => finish({ success: true }), 500);
    }
  }

  // ── DOM property helpers ─────────────────────────────────────────────────

  function querySelectorOrErr(selector) {
    let el;
    try {
      el = document.querySelector(selector);
    } catch {
      // An invalid CSS selector throws a SyntaxError. Surface it as a typed
      // InvalidArgument instead of letting it propagate — in browser mode an
      // uncaught throw degrades to a page-response timeout, hiding the real cause.
      return {
        error: err("InvalidArgument", `Invalid CSS selector: ${JSON.stringify(selector)}`),
      };
    }
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
    // setItem throws on a full quota; count the rejects and report them typed
    // rather than letting one throw abort the rest and surface as a generic
    // exception — the agent learns it imported less than the file held.
    let total = 0;
    let failed = 0;
    // A storage map must be a plain object of string→string. A string (e.g.
    // `"abc"`) would otherwise iterate as character index keys (`0`→`"a"`),
    // silently importing garbage; a non-object is rejected outright.
    const isPlainObject = (o) => o != null && typeof o === "object" && !Array.isArray(o);
    for (const store of ["localStorage", "sessionStorage"]) {
      const o = msg[store];
      if (o != null && !isPlainObject(o)) {
        return err("InvalidArgument", `${store} must be an object of string keys and values`);
      }
    }
    // Storage values are always strings (that's all the Web Storage API holds);
    // a non-string in the file would coerce to garbage like "[object Object]",
    // so reject it rather than import a silent lie.
    for (const store of ["localStorage", "sessionStorage"]) {
      for (const v of Object.values(msg[store] || {})) {
        if (typeof v !== "string") {
          return err("InvalidArgument", `${store} values must be strings`);
        }
      }
    }
    const restore = (store, obj) => {
      for (const [k, v] of Object.entries(obj || {})) {
        total++;
        try {
          store.setItem(k, v);
        } catch {
          failed++;
        }
      }
    };
    restore(localStorage, msg.localStorage);
    restore(sessionStorage, msg.sessionStorage);
    if (failed > 0) {
      return err("Other", `${failed} of ${total} storage entries failed to set (quota?)`);
    }
    return { success: true };
  }

  // ── Element coords (drag) ────────────────────────────────────────────────

  function getElementCoords(msg) {
    const src = resolveIndex(msg.source);
    if (src.error) return src.error;
    const tgt = resolveIndex(msg.target);
    if (tgt.error) return tgt.error;
    // Bring BOTH endpoints into view — scrolling only the source would leave a
    // far-down or differently-scrolled target off-screen, and the CDP release
    // would then land in empty space while the command still "succeeded".
    // Scroll the target last (it's where the drop must register), then read
    // both rects in the final scroll position.
    src.el.scrollIntoView({ block: "center", behavior: "instant" });
    tgt.el.scrollIntoView({ block: "center", behavior: "instant" });
    const sr = src.el.getBoundingClientRect();
    const tr = tgt.el.getBoundingClientRect();
    const sx = sr.left + sr.width / 2;
    const sy = sr.top + sr.height / 2;
    const tx = tr.left + tr.width / 2;
    const ty = tr.top + tr.height / 2;
    // If the two centres can't share the viewport (different scroll containers,
    // or too far apart for one gesture), a coordinate drag would miss. Fail
    // loud instead of reporting a success that did nothing.
    // Half-open bounds: the viewport spans pixels [0, innerWidth) × [0,
    // innerHeight), so a centre exactly on `innerWidth`/`innerHeight` (an element
    // straddling the right/bottom edge) is OUTSIDE the hit-test region — CDP would
    // dispatch the drag onto nothing. Reject it here rather than miss silently.
    const inView = (x, y) => x >= 0 && y >= 0 && x < innerWidth && y < innerHeight;
    if (!inView(sx, sy) || !inView(tx, ty)) {
      return err(
        "InvalidArgument",
        "drag source and target can't share the viewport — they are in different scroll containers or too far apart to drag in one gesture",
      );
    }
    return { sx, sy, tx, ty };
  }

  // ── Dispatcher ───────────────────────────────────────────────────────────

  function handle(msg) {
    switch (msg.type) {
      case "extractDom":
        return extractDom(msg.options || {});
      case "extractText": {
        // Capped here, in the one place both modes share, so a giant page
        // costs the same bounded tokens everywhere (codepoint-safe). `truncated`
        // tells the agent the page has more text than shown — without it a clip
        // is silent and the visible prefix reads as the whole page.
        const full = document.body?.innerText || "";
        const text = clip(full, 50000);
        return {
          text,
          truncated: text !== full,
          url: location.href,
          title: document.title,
        };
      }
      case "executeAction":
        return executeAction(msg.action);
      case "wait":
        return new Promise((resolve) => handleWait(msg, resolve));
      case "prepareUpload": {
        // Stash the EXACT snapshot element (resolveIndex → object identity, so a
        // stale index is a typed StaleSnapshot here) for a CDP objectId handoff.
        // No DOM-visible marker and no document-order re-query, so a page can
        // neither observe nor redirect the target between resolve and the
        // file-set sink; the direct reference also reaches a file input inside
        // an open shadow root, which a document-root selector cannot.
        const r = resolveIndex(msg.index);
        if (r.error) return r.error;
        const el = r.el;
        if (!(el instanceof HTMLInputElement) || el.type !== "file") {
          return err(
            "InvalidArgument",
            `[${msg.index}] is not a file input — upload targets <input type=file>`,
            { index: msg.index },
          );
        }
        state.uploadTarget = el;
        return { success: true };
      }
      case "clearUpload": {
        state.uploadTarget = null;
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
