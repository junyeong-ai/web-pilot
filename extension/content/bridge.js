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
  if (!window.__webpilot_state) {
    window.__webpilot_state = {
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
      available > 0
        ? `Index ${requested} out of range (1-${available})`
        : `Index ${requested} out of range — the page has no interactive elements`,
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
      // The same `*` scan that finds shadow hosts also counts this root's
      // elements, so `budget.nodes` ends up the DEEP node total — light DOM plus
      // every open shadow root visited — matching the shadow-piercing element
      // index, with no extra traversal.
      const here = r.querySelectorAll("*");
      budget.nodes = (budget.nodes || 0) + here.length;
      for (const host of here) {
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
    const budget = { hosts: SHADOW_HOST_BUDGET, truncated: false, nodes: 0 };

    // Explicit interaction markers. `tabindex` qualifies only when >= 0: a
    // tabindex of -1 is script-only focus (route announcers, modal roots,
    // headings) and is not a click affordance, so it must not mint a phantom
    // target. The marker and tabindex passes pierce open shadow roots like the
    // semantic pass — a design-system custom element whose clickable part
    // lives in its shadow root and carries `onclick`/`jsaction` rather than a
    // semantic tag would otherwise be invisible to the agent.
    // `[draggable="true"]` is an explicit interaction affordance too — the
    // `drag` action addresses elements by index, so a declared drag source
    // must be capturable. (The attribute selector matches only the explicit
    // attribute, never the implicit `draggable` default of images/links.)
    const markerSel = '[onclick],[data-action],[ng-click],' +
      '[v-on\\:click],[\\@click],[data-click],[jsaction],[draggable="true"]';

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

    // The heuristic passes skip an element that already carries a DELIBERATE
    // semantic role (the semantic pass collects the interactive ones). But ARIA
    // `role="none"`/`"presentation"` explicitly STRIP the implicit role — such an
    // element is semantically role-less, so a `role="none"` div WITH an `onclick`
    // (or innermost cursor:pointer) is a real click target the agent must see, not
    // a semantic control to defer. Treat none/presentation (and the first token of
    // a multi-token role) as "no role".
    const hasExplicitRole = (el) => {
      const role = (el.getAttribute("role") || "").trim().split(/\s+/)[0].toLowerCase();
      return role !== "" && role !== "none" && role !== "presentation";
    };

    const markers = new Set(markerEls);
    for (const el of tabindexEls) {
      const ti = parseInt(el.getAttribute("tabindex"), 10);
      if (Number.isFinite(ti) && ti >= 0) markers.add(el);
    }
    for (const el of markers) {
      if (seen.has(el)) continue;
      if (STANDARD_TAGS.has(el.tagName.toLowerCase())) continue;
      if (hasExplicitRole(el)) continue;
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
      if (hasExplicitRole(el)) continue;
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
    return { all, shadowTruncated: budget.truncated, totalNodes: budget.nodes };
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
  // The element directly above `node` in the FLAT tree: its element parent, or —
  // when `node` is the top of an open shadow root — the host that projects it
  // into the outer tree. Returns null at the document root. This is how the a11y
  // tree flattens shadow content into its host's position, so ancestor walks
  // (visibility, landmark) see the outer context a bare `parentElement` would
  // miss at the shadow boundary.
  function flatTreeParent(node) {
    const root = node.getRootNode();
    return node.parentElement || (root instanceof ShadowRoot ? root.host : null);
  }

  // `--include text` source. `document.body.innerText` is rendering-aware
  // (visibility, block newlines, slotted content) and fast, but it stops at open
  // shadow boundaries — so a web component's own labels/prose are silently
  // dropped, while the DOM snapshot (which pierces shadow) shows them. Keep
  // `innerText` as the well-formatted base for the light tree and append the
  // text OWNED by each open shadow root. A `<slot>`'s projected content already
  // lives in the light tree (counted in the base), so the shadow walk skips
  // slots — no double counting, and the light-only common case is unchanged.
  function bodyTextWithShadow() {
    const base = document.body?.innerText || "";
    if (!document.body) return base;
    const extra = [];
    for (const el of document.body.querySelectorAll("*")) {
      if (el.shadowRoot?.mode === "open" && el.checkVisibility?.()) {
        const t = shadowOwnText(el.shadowRoot);
        if (t) extra.push(t);
      }
    }
    return extra.length ? `${base}\n${extra.join("\n")}` : base;
  }

  // Visible text owned by a shadow root: its own nodes' text, descending nested
  // open shadow roots, but SKIPPING `<slot>` — a slot renders light-tree content
  // that the base `innerText` already carries. A shadow host's own light
  // children are unrendered unless slotted (and slotted ones surface via the
  // base), so a host is descended through its shadow root, never its light kids.
  function shadowOwnText(root) {
    let out = "";
    for (const child of root.childNodes) {
      if (child.nodeType === Node.TEXT_NODE) {
        out += `${child.textContent} `;
      } else if (child.nodeType === Node.ELEMENT_NODE) {
        if (child.localName === "slot") continue;
        if (child.checkVisibility && !child.checkVisibility()) continue;
        out +=
          child.shadowRoot?.mode === "open"
            ? `${shadowOwnText(child.shadowRoot)} `
            : `${shadowOwnText(child)} `;
      }
    }
    return out.replace(/\s+/g, " ").trim();
  }

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
      node = flatTreeParent(node);
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
    // Walk the FLAT tree (crossing open shadow boundaries to the host), not just
    // `parentElement`: a control inside a shadow root still sits within whatever
    // landmark wraps its host in the outer tree, but `parentElement` returns null
    // at the shadow boundary — so a bare walk would strip the landmark from every
    // shadow-inner element. Mirrors the shadow-aware `isVisible`/`resolveLabel`.
    let p = flatTreeParent(el);
    while (p && p !== document.body) {
      const role = p.getAttribute("role");
      if (role && landmarks.has(role)) return role;
      const tag = p.tagName.toLowerCase();
      if (landmarks.has(tag)) return tag;
      p = flatTreeParent(p);
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
  // Hit-test that descends into open shadow roots: `document.elementFromPoint`
  // retargets a shadow-interior hit to its HOST, and tree-scoped `contains`
  // cannot relate the host to the element inside its shadow — so without the
  // descent every shadow-root control would read occluded by its own host.
  // Each root's own `elementFromPoint` resolves the real innermost hit.
  function deepElementFromPoint(x, y) {
    let el = document.elementFromPoint(x, y);
    while (el?.shadowRoot) {
      const inner = el.shadowRoot.elementFromPoint(x, y);
      if (!inner || inner === el) break;
      el = inner;
    }
    return el;
  }

  // The next composed-tree boundary above `n`: a slotted ancestor continues at
  // its assigned <slot> (inside the shadow tree that RENDERS it — slotted
  // content paints there, so the hit-test relation must follow it), otherwise
  // the tree's host. Slot forwarding and nested hosts terminate: each hop
  // either follows the finite slot-assignment chain or exits one shadow level.
  function flatTreeHop(n) {
    for (let p = n; p; p = p.parentElement) {
      if (p.assignedSlot) return p.assignedSlot;
    }
    return n.getRootNode().host || null;
  }

  // Composed-tree relatedness: one of the two contains the other once shadow
  // boundaries are hopped — host-by-host, and through slot assignment (a
  // shadow button whose visible content is a slotted light <span> must relate
  // to that span, or every sampled hit on its own label would read as a
  // blocker). `contains` alone is tree-scoped and would call every
  // cross-boundary pair "unrelated".
  function composedRelated(a, b) {
    for (let n = b; n; n = flatTreeHop(n)) {
      if (a.contains(n)) return true;
    }
    for (let n = a; n; n = flatTreeHop(n)) {
      if (b.contains(n)) return true;
    }
    return false;
  }

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
      const top = deepElementFromPoint(px, py);
      if (top && top !== el && !composedRelated(el, top)) blocked++;
    }
    return tested > 0 && blocked * 2 > tested;
  }


  function extractDom(options) {
    try {
      const start = performance.now();
      // New-element baseline by node identity: the previous snapshot holds the
      // exact elements the agent last saw, so "new" means absent from it.
      // Identity is collision-free (no two elements share it) and survives
      // re-renders that keep the node. With no baseline — the first capture in
      // this document — nothing is flagged (`prevNodes = null`): a full
      // navigation gets a fresh isolated-world state (`snapshot: null`), so a new
      // page is never "all new". A same-document change (`pushState`, a hash)
      // keeps the baseline, so the elements it actually adds are correctly `*`-ed.
      const prevNodes = state.snapshot ? new Set(state.snapshot) : null;
      const { all, shadowTruncated, totalNodes } = collectInteractiveElements();
      if (shadowTruncated) {
        console.warn("[WebPilot] shadow-DOM traversal hit its host budget; some controls may be omitted");
      }
      const elements = [];
      const picked = [];
      let idx = 1;
      const includeBounds = options.bounds || false;
      // Resolved once for the whole snapshot: `document.activeElement` names only
      // the outermost shadow HOST, so a focused control inside an open shadow
      // root would otherwise report `focused: false`. `deepActiveElement` pierces
      // shadow roots, matching the focus handling the key-press path already uses.
      const active = deepActiveElement();

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
          // `?? undefined`, not `||`: `href=""` is a real link to the current
          // page (focusable, ARIA role `link`) — collapsing the empty string
          // would strip its implicit role and `find --role link` would miss it.
          href: el.getAttribute("href") ?? undefined,
          input_type: tag === "input" ? (el.type || undefined) : undefined,
          disabled: isDisabled(el),
          focused: active === el,
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
        // they survive alongside `disabled`/`focused`. `in_viewport` is the
        // same: `false` is the signal ("[offscreen]" in the rendered DOM, and
        // the annotation overlay skips it), not an absence — stripping it
        // would erase the offscreen marker entirely.
        for (const k of Object.keys(entry)) {
          if (entry[k] === undefined ||
              (entry[k] === false &&
                k !== "disabled" && k !== "focused" &&
                k !== "checked" && k !== "expanded" &&
                k !== "in_viewport")) {
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

  // True when this click loads a new document IN THE CURRENT FRAME — either a
  // non-prevented self-targeting `a[href]` to a different http(s)/file URL (not a
  // fragment, not a popup target), OR a click on a form's submit control (which
  // submits the form and loads a document with no href). The settle layer needs
  // this because the navigation is queued (HTML spec): its frameStartedLoading
  // can arrive AFTER the click response and miss a one-shot event drain, so it
  // must be derived deterministically at click time. `notCanceled` is the click
  // event's dispatch result, so a preventDefault'd SPA link/button reports no
  // navigation. The frame may be the top frame or a switched iframe; the settle
  // uses it to catch an iframe-internal navigation the top-frame `navigates`
  // signal can't see.
  // The ancestor a (non-cancelled) navigating click targets — "_self" (this
  // frame), "_top", or "_parent" — or null when it loads no document in an
  // existing frame (a popup / named or `_blank` target, a javascript:/mailto:
  // url, or a fragment-only same-document change). Shared by both nav hints so
  // they can never disagree about WHETHER — and WHERE — a click navigates.
  // A non-keyword target NAME navigates an existing frame when it matches one
  // (HTML's browsing-context name lookup) — most commonly THIS frame, a named
  // iframe whose own links target its own name. Map the names this frame can
  // see to their keyword equivalent; cross-origin ancestors throw on `.name`
  // and stay null (conservative: treated as a popup, the pre-existing
  // behaviour). Name matching is case-SENSITIVE per spec — only the keywords
  // are case-insensitive.
  function resolveNamedTarget(name) {
    if (window.name === name) return "_self";
    try {
      if (window.parent !== window && window.parent.name === name) return "_parent";
    } catch {
      /* cross-origin parent — unreadable, not this frame's concern */
    }
    try {
      if (window.top !== window && window.top.name === name) return "_top";
    } catch {
      /* cross-origin top — unreadable */
    }
    return null; // no reachable frame carries the name → a popup
  }

  function navTargetKeyword(el, notCanceled) {
    if (!notCanceled) return null;
    const a = el.closest("a[href]");
    if (a) {
      let target = (a.target || "").trim().toLowerCase();
      // `_blank` (always a new context) and `_unfencedTop` (fenced frames) are
      // reserved keywords — never matched against frame names, so a page that
      // names a frame after one can't trick the lookup into `_self`.
      if (target === "_blank" || target === "_unfencedtop") return null;
      if (target && target !== "_self" && target !== "_top" && target !== "_parent") {
        // Not a keyword: resolve the raw (case-sensitive) name to a frame this
        // click would actually navigate, or bail as a popup.
        const mapped = resolveNamedTarget((a.target || "").trim());
        if (!mapped) return null;
        target = mapped;
      }
      let dest, cur;
      try {
        dest = new URL(a.href, location.href);
        cur = new URL(location.href);
      } catch {
        return null;
      }
      if (dest.protocol !== "http:" && dest.protocol !== "https:" && dest.protocol !== "file:") {
        return null; // javascript:/mailto:/tel:/… never load a document
      }
      // A change confined to the fragment is a same-document nav (no load event):
      // hinting it would burn the settle's whole PROBE waiting for a commit that
      // never comes.
      if (dest.origin === cur.origin && dest.pathname === cur.pathname && dest.search === cur.search) {
        return null;
      }
      return target || "_self";
    }
    // A submit control submits its associated form on click → a new document
    // loads, but it carries no `href` so the link path above misses it. (A form
    // submit always navigates — even to the same URL — so the link's fragment-only
    // exclusion does not apply.) `type=button`/`reset` don't submit; a `<button>`
    // with no type defaults to submit.
    const btn = el.closest('button, input[type="submit"], input[type="image"]');
    if (btn && btn.form && (btn.tagName !== "BUTTON" || btn.type === "submit")) {
      const raw = (btn.getAttribute("formtarget") || btn.form.getAttribute("target") || "").trim();
      let t = raw.toLowerCase();
      // `_blank` / `_unfencedTop` are reserved — never a frame-name match.
      if (t === "_blank" || t === "_unfencedtop") return null;
      if (t && t !== "_self" && t !== "_top" && t !== "_parent") {
        // Same name resolution as the link path: a form targeting an existing
        // frame's name submits INTO that frame, not a popup.
        const mapped = resolveNamedTarget(raw);
        if (!mapped) return null;
        t = mapped;
      }
      return t || "_self";
    }
    return null;
  }

  // Does the navigation land in the ACTIVE frame (where the bridge runs)? Drives
  // the active-frame settle. `_self` always lands here; `_top`/`_parent` resolve
  // to an ancestor and land here only when THIS frame is the top (then they are
  // itself). A nav into an ancestor is not a current-frame load.
  function frameNavigates(el, notCanceled) {
    const t = navTargetKeyword(el, notCanceled);
    if (t === null) return false;
    if (t === "_self") return true;
    return window.top === window; // _top / _parent of the top frame IS the top
  }

  // Does the navigation load a new TOP document? — the `url_changed` signal
  // driving the main-frame settle. `_top` always loads the top; `_self` only from
  // the top frame; `_parent` only from a direct child of the top. A `_top` link
  // clicked inside a switched iframe IS a top navigation — the active frame does
  // not move — so this hint must fire for it, not `frameNavigates`.
  function clickNavigates(el, notCanceled) {
    const t = navTargetKeyword(el, notCanceled);
    if (t === null) return false;
    if (t === "_top") return true;
    if (t === "_parent") return window.parent === window.top;
    return window.top === window; // _self
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
    const mousedownLive = el.dispatchEvent(new MouseEvent("mousedown", opts));
    // A real click focuses the target as mousedown's default action — unless the
    // page cancels mousedown (the toolbar pattern that deliberately prevents
    // focus theft) — firing focus/focusin and, crucially, making the element the
    // browser-focus target a following native key_press lands on (the documented
    // click-then-type contract). A synthetic dispatch does not trigger that
    // default action, so focus explicitly; focus() no-ops on a non-focusable
    // element. The element is already scroll-centered above, so no extra scroll.
    if (mousedownLive) el.focus();
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
      if (clear) {
        // Clear via the editing pipeline (select-all + delete), NOT `innerHTML =
        // ""`: a direct innerHTML wipe clobbers a rich editor's nested structure
        // (<p>/<span>/…) and desyncs a framework that manages its own DOM
        // (Draft/Slate/ProseMirror). selectAll+delete fires beforeinput/input the
        // framework observes, so its model stays consistent. For a plain
        // contenteditable it likewise empties the element.
        document.execCommand("selectAll", false, null);
        document.execCommand("delete", false, null);
      } else {
        // Append at the end, matching the <input>/<textarea> path below: after a
        // programmatic focus() the caret sits at a stale or start position, so a
        // bare insertText would prepend or splice into the middle instead of
        // extending the field. Collapse the selection to the end of the
        // element's contents first.
        const sel = window.getSelection();
        const range = document.createRange();
        range.selectNodeContents(el);
        range.collapse(false);
        sel.removeAllRanges();
        sel.addRange(range);
      }
      document.execCommand("insertText", false, text);
      // execCommand fires input on its own, but an empty `text` (or a framework
      // that swallowed it) fires none, and a contenteditable never fires `change`
      // — so a framework-bound editor (a React onChange) could miss the edit.
      // Dispatch both; a redundant `input` is harmless since listeners re-read the
      // live text.
      el.dispatchEvent(new InputEvent("input", {
        bubbles: true, inputType: "insertText", data: text,
      }));
      el.dispatchEvent(new Event("change", { bubbles: true }));
      return;
    }

    const newVal = clear ? text : (el.value || "") + text;

    // `maxlength` bounds what a USER can type — a real keyboard stops at the
    // cap — but a programmatic value set sails past it, leaving the field
    // holding a value the UI can never produce while the command reports
    // success. Reject typed instead, BEFORE any mutation. Enforced only where
    // the browser itself enforces maxlength (textarea + the textual input
    // types); e.g. `type=number` ignores the attribute, so rejecting there
    // would invent a constraint the page doesn't have.
    const maxlengthApplies =
      el instanceof HTMLTextAreaElement ||
      (el instanceof HTMLInputElement &&
        ["text", "search", "url", "tel", "email", "password"].includes(el.type));
    if (maxlengthApplies && el.maxLength >= 0 && newVal.length > el.maxLength) {
      return err(
        "InvalidArgument",
        `The field caps input at ${el.maxLength} characters (maxlength) but the result would be ${newVal.length} — a real keyboard would stop at the cap; send a shorter value`,
        { requested: text },
      );
    }

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

    // A typed control (number/date/time/…) silently sanitizes a value it can't
    // parse to the empty string — "abc" into `<input type=number>` leaves the
    // field blank. Firing input/change and reporting success would claim a value
    // that never landed. A non-empty target that the control blanked is a
    // rejection, not a no-op: fail typed so the agent retries with a valid
    // format instead of trusting an empty field. (A control that merely
    // normalises a valid value — "3.0" → "3" — keeps a non-empty value and is
    // left alone.)
    if (newVal !== "" && el.value === "") {
      return err(
        "InvalidArgument",
        `The field rejected "${text}" — its input type does not accept that value`,
        { requested: text },
      );
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
          const typeError = reliableType(r.target, action.text, action.clear);
          if (typeError) return typeError;
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
          if (isDisabled(match) || match.hidden) {
            return err(
              "InvalidArgument",
              `<option> "${action.value}" is ${isDisabled(match) ? "disabled" : "hidden"} — a real user can't select it`,
            );
          }
          // On a `<select multiple>`, assigning `.value` deselects every other
          // chosen option and leaves only this one, so an agent could never
          // build a multi-option selection — each call would clobber the last.
          // Add to the selection instead; a single-select still replaces, since
          // only one option can be chosen.
          if (r.target.multiple) {
            match.selected = true;
          } else {
            r.target.value = action.value;
          }
          // A real selection fires `input` THEN `change` (both bubble). The
          // bridge fired only `change`, so a <select> wired to `oninput` — or a
          // framework that observes `input` — silently ignored the choice while
          // the command still reported success. Fire both, like `reliableType`.
          r.target.dispatchEvent(new Event("input", { bubbles: true }));
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
        // Shadow-PIERCING, like the `--include text` capture, `find`, and `wait
        // text`: a selector for an element inside an open shadow root must
        // satisfy the wait, not time out on an element the agent's capture
        // already indexed. The light-DOM query runs first (the common case, and
        // the selector is known-valid past the guard above); the shadow walk
        // runs only on a miss, so a shadow-free page pays nothing extra.
        const matchesDeep = () =>
          !!document.querySelector(cond.value) ||
          queryAllDeepMulti([cond.value], document, { hosts: SHADOW_HOST_BUDGET }).some(
            (m) => m.length,
          );
        if (initial || matchesDeep()) return finish({ success: true });
        observer = new MutationObserver(() => {
          if (matchesDeep()) finish({ success: true });
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
        // `:focus`) would run to the full timeout though it already matches; and
        // the observer watches only the light tree, so a match appearing inside
        // an open shadow root is invisible to it as well. Poll alongside to
        // catch both within one interval (`matchesDeep` pierces shadow); the
        // observer still gives instant response to light-DOM structural and
        // attribute changes.
        pollTimer = setInterval(() => {
          if (matchesDeep()) finish({ success: true });
        }, 100);
        break;
      }
      case "text": {
        // Match like `find --text` and the `--include text` capture:
        // case-INSENSITIVE and shadow-PIERCING, so `wait text submit` matches a
        // "Submit" button and text that lives only inside an open shadow root
        // unblocks the wait. Raw `innerText` alone is case-sensitive and stops at
        // the shadow boundary, diverging from how the agent's other text matching
        // behaves. The fast light-DOM check runs first (the common case); the
        // shadow-aware walk runs only when it misses, so a page without
        // shadow-only text pays nothing extra.
        // Collapse whitespace too, like the DOM snapshot's element text that
        // `find --text` matches (`(el.innerText||…).replace(/\s+/g," ")`): a
        // `<button>Pay<br>now</button>` whose innerText is "Pay\nnow" must match
        // `wait text "pay now"`, exactly as `find --text "pay now"` already does.
        const needle = cond.value.toLowerCase();
        const collapse = (s) => s.replace(/\s+/g, " ").trim().toLowerCase();
        const hasText = () =>
          collapse(document.body?.innerText || "").includes(needle) ||
          collapse(bodyTextWithShadow()).includes(needle);
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

  // One deep, budgeted lookup behind every `dom get-*`/`set-*` selector: the
  // element index and `wait selector` pierce open shadow roots, so the DOM
  // selector surface does too — a web component's field is readable and
  // writable without falling back to eval. Per-root matching (light DOM
  // first, then each open shadow root in document order), the same
  // budget-bounded traversal the capture uses.
  function queryDeepOrErr(selector) {
    const budget = { hosts: SHADOW_HOST_BUDGET, truncated: false };
    try {
      return {
        all: queryAllDeepMulti([selector], document, budget)[0],
        truncated: budget.truncated,
      };
    } catch {
      // An invalid CSS selector throws a SyntaxError. Surface it as a typed
      // InvalidArgument instead of letting it propagate — in browser mode an
      // uncaught throw degrades to a page-response timeout, hiding the real cause.
      return {
        error: err("InvalidArgument", `Invalid CSS selector: ${JSON.stringify(selector)}`),
      };
    }
  }

  function querySelectorOrErr(selector) {
    const r = queryDeepOrErr(selector);
    if (r.error) return r;
    return r.all.length > 0 ? { el: r.all[0] } : { error: selectorNotFound(selector) };
  }

  // The strict-selector contract for WRITES (`frame url`, `tab find`,
  // `find --click`): a `dom set-*` whose selector matches several elements
  // would silently mutate whichever matched first — one row of a hundred,
  // with a bare success and no signal the others existed. Require a unique
  // match ACROSS shadow boundaries (a light-DOM element and a shadow twin
  // sharing the selector are two candidates, not a unique hit) and name the
  // count; reads keep standard first-match semantics (recoverable, and the
  // value identifies what was read).
  function uniqueSelectorOrErr(selector) {
    const r = queryDeepOrErr(selector);
    if (r.error) return r;
    // A budget-clipped traversal proves nothing about uniqueness — an unseen
    // shadow twin may exist past the cap, and "unique so far" would write the
    // wrong element. Writes fail honest (the same truncation the capture
    // surfaces as `shadow_truncated`); reads keep their deterministic
    // light-first first match.
    if (r.truncated) {
      return {
        error: err(
          "InvalidArgument",
          "the shadow-DOM traversal budget was exhausted before the selector's uniqueness could be established — dom set needs a unique match; target the element another way (eval)",
        ),
      };
    }
    if (r.all.length === 0) return { error: selectorNotFound(selector) };
    if (r.all.length > 1) {
      return {
        error: err(
          "InvalidArgument",
          `${r.all.length} elements match ${JSON.stringify(selector)} — dom set writes one element; refine the selector (#id, :nth-of-type(n))`,
        ),
      };
    }
    return { el: r.all[0] };
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
      const text = String(el.index);
      label.textContent = text;
      // Keep the index on-screen at the viewport edges. The default sits just
      // above and slightly left of the box, which renders off-screen — losing
      // the number while the box still shows — for an element flush against the
      // top, left, or right edge. Clamp each axis: flip the label down into the
      // box at the top edge, in from the left, and back left when it would
      // overflow the right (the index width is estimated from the monospace
      // glyph advance plus the horizontal padding).
      const labelW = text.length * 7 + 6;
      const top = Math.max(-16, -el.y);
      let left = Math.max(-2, -el.x);
      const overflow = el.x + left + labelW - window.innerWidth;
      if (overflow > 0) left = Math.max(-el.x, left - overflow);
      label.style.cssText =
        `position:absolute;top:${top}px;left:${left}px;background:rgba(255,0,0,0.9);color:#fff;font:bold 11px/14px monospace;padding:0 3px;border-radius:2px`;
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
    // Storage is origin-scoped state, so the export records WHOSE it is — the
    // import refuses to write it into a page on a different origin.
    return { origin: location.origin, localStorage: localObj, sessionStorage: sessionObj };
  }

  function importStorage(msg) {
    // Storage is origin-scoped: writing an export taken on origin A into a
    // page on origin B would corrupt B's app state under a success status —
    // the agent believes the session is restored while the right origin got
    // nothing. An export always records its origin; enforce it when present
    // (a hand-written file may omit it, the same explicit opt-out the
    // `version` field has). Cookies are unaffected — each carries its own
    // domain and is applied through the cookie API, not the current page.
    if (msg.origin != null) {
      // An OPAQUE origin (file://, a sandboxed frame) serializes as "null" —
      // every such page shares that string while being same-origin with
      // nothing, even itself. Equality between two "null"s would write storage
      // across genuinely unrelated pages, so opaque origins are refused on
      // either side rather than matched by their serialization.
      if (msg.origin === "null" || location.origin === "null") {
        return err(
          "InvalidArgument",
          "session storage cannot be ported to or from an opaque origin (a file:// or sandboxed page)",
        );
      }
      if (msg.origin !== location.origin) {
        return err(
          "InvalidArgument",
          `session storage was exported from ${msg.origin} but the page is on ${location.origin} — navigate there before importing`,
        );
      }
    }
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
        const full = bodyTextWithShadow();
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
        const r = uniqueSelectorOrErr(msg.selector);
        if (r.error) return r.error;
        r.el.innerHTML = msg.value;
        return { success: true };
      }
      case "setText": {
        const r = uniqueSelectorOrErr(msg.selector);
        if (r.error) return r.error;
        r.el.textContent = msg.value;
        return { success: true };
      }
      case "setAttr": {
        const r = uniqueSelectorOrErr(msg.selector);
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
