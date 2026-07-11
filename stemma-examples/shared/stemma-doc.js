// Shared core for the stemma Word frontend: a rich ProseMirror schema, the
// /rich-projection → ProseMirror mapping, the API client, and decorations.
//
// The editor renders stemma's RICH projection (GET /api/documents/{id}/rich):
// every text run carries `style_props` (font family, size, color, highlight, …)
// and `marks`; every block carries align/numbering/guard; images arrive as
// base64 data-URIs. We map those to a ProseMirror schema with marks/attrs so the
// document LOOKS like Word, while editing still commits as guard-pinned typed
// transactions. Block ids + `guard` come straight from /rich (guard parity with
// `apply` is verified), so editing works off this view.

import { Schema, DOMSerializer, Fragment } from "prosemirror-model";
import { Plugin, TextSelection } from "prosemirror-state";
import { Decoration, DecorationSet } from "prosemirror-view";

// ─── unit + style helpers ───────────────────────────────────────────────────
const emuToPx = (emu) => (emu ? Math.round(emu / 9525) : null); // 914400 EMU = 1in = 96px
const hpToPt = (hp) => (hp ? hp / 2 : null); // half-points → points
const isOn = (v) => v === "On" || v === true; // tri-state OOXML prop is "On"/"Off"/"Inherit"

const HIGHLIGHT_CSS = { darkblue: "#000080", darkcyan: "#008080", darkgreen: "#008000", darkmagenta: "#800080", darkred: "#800000", darkyellow: "#808000", darkgray: "#808080", lightgray: "#d3d3d3" };

// §17.18.99 ST_Underline → CSS text-decoration-style ("" = no underline).
function underlineDecoration(u) {
  if (!u || u === "None") return "";
  const s = String(u);
  if (s === "Double" || s === "WavyDouble") return "double";
  if (s.startsWith("Dotted")) return "dotted";
  if (s.startsWith("Dash") || s.startsWith("DotDash") || s.startsWith("DotDotDash") || s.includes("DashDot")) return "dashed";
  if (s.startsWith("Wav")) return "wavy";
  return "solid"; // Single, Thick, Words, …
}

// Optimistically fold a font/size/color/highlight change into a run mark's CSS
// `style` string (the local, instant update applied before the engine confirms
// the rPrChange via set_format). Mirrors how runStyle renders the same props.
export function applyRunPropsToStyle(style, props) {
  const set = (s, prop, value) => {
    const decls = (s || "")
      .split(";")
      .map((d) => d.trim())
      .filter((d) => d && d.split(":")[0].trim() !== prop);
    if (value) decls.push(`${prop}:${value}`);
    return decls.join(";");
  };
  let s = style || "";
  if (props.color != null) s = set(s, "color", `#${props.color}`);
  if (props.highlight != null) {
    const h = String(props.highlight).toLowerCase();
    s = set(s, "background-color", h === "none" ? null : HIGHLIGHT_CSS[h] || h);
  }
  if (props.font_family != null) s = set(s, "font-family", `"${props.font_family}",serif`);
  if (props.font_size_half_points != null) s = set(s, "font-size", `${props.font_size_half_points / 2}pt`);
  return s;
}

// Compose the inline CSS for a run from its `style_props`.
function runStyle(sp) {
  sp = sp || {};
  const css = [];
  if (sp.font_family) css.push(`font-family:"${sp.font_family}",serif`);
  const pt = hpToPt(sp.font_size);
  if (pt) css.push(`font-size:${pt}pt`);
  if (sp.color && /^[0-9a-fA-F]{6}$/.test(sp.color)) css.push(`color:#${sp.color}`);
  if (sp.highlight) { const h = String(sp.highlight).toLowerCase(); css.push(`background-color:${HIGHLIGHT_CSS[h] || h}`); }
  if (isOn(sp.caps)) css.push("text-transform:uppercase");
  if (isOn(sp.small_caps)) css.push("font-variant:small-caps");
  // Text decoration: underline (with §17.18.99 style) + strike, composed into one
  // declaration (CSS has a single decoration slot). underline_style is the source
  // of truth for underlines — the redundant `Underline` mark is suppressed when
  // it's present (see segToInline) so we don't double-render.
  const lines = [];
  const us = underlineDecoration(sp.underline_style);
  if (us) lines.push("underline");
  if (isOn(sp.strike) || isOn(sp.double_strike)) lines.push("line-through");
  if (lines.length) {
    const style = us && us !== "solid" ? us : isOn(sp.double_strike) ? "double" : null;
    css.push(`text-decoration:${lines.join(" ")}${style ? " " + style : ""}`);
  }
  if (isOn(sp.vanish)) css.push("opacity:0.45");
  // char spacing (w:spacing, twips) and vertical raise/lower (w:position, half-points)
  if (sp.char_spacing) css.push(`letter-spacing:${sp.char_spacing / 20}pt`);
  if (sp.position) css.push(`position:relative;bottom:${sp.position / 2}pt`);
  // run shading (w:shd fill) — a run background, when there's no highlight
  if (!sp.highlight && sp.run_shading && /^[0-9a-fA-F]{6}$/.test(sp.run_shading.fill || "")) css.push(`background-color:#${sp.run_shading.fill}`);
  // Rare OOXML text effects (one text-shadow slot, so first-wins).
  if (isOn(sp.emboss)) css.push("text-shadow:1px 1px 0 rgba(255,255,255,0.6)");
  else if (isOn(sp.imprint)) css.push("text-shadow:-1px -1px 0 rgba(255,255,255,0.6)");
  else if (isOn(sp.outline)) css.push("text-shadow:0 0 1px currentColor");
  else if (isOn(sp.shadow)) css.push("text-shadow:2px 2px 2px rgba(0,0,0,0.3)");
  return css.join(";");
}

// stemma emits image data-URIs as application/octet-stream; sniff + correct so
// the browser renders them.
function fixImageDataUri(uri) {
  if (!uri || !uri.startsWith("data:application/octet-stream")) return uri;
  const b64 = uri.slice(uri.indexOf(",") + 1);
  const mime = b64.startsWith("iVBOR") ? "image/png" : b64.startsWith("/9j/") ? "image/jpeg"
    : b64.startsWith("R0lGOD") ? "image/gif" : (b64.startsWith("PD94") || b64.startsWith("PHN2Zy")) ? "image/svg+xml" : "image/png";
  return `data:${mime};base64,${b64}`;
}

// §17.18.1 ST_Jc. Distribute + the kashida/Thai variants all map to justify;
// numTab is a legacy left alignment.
const ALIGN = {
  Left: "left", Start: "left", Center: "center", Right: "right", End: "right",
  Justify: "justify", Both: "justify", Distribute: "justify",
  HighKashida: "justify", LowKashida: "justify", MediumKashida: "justify", ThaiDistribute: "justify",
  NumTab: "left",
};

// ─── tab resolution ───────────────────────────────────────────────────────────
// The document's default tab interval (twips). 720 (0.5in) is the OOXML default
// and Word's default; set from /rich's section when the engine exposes it.
let DEFAULT_TAB = 720;
export function setDefaultTab(twips) { if (twips && twips > 0) DEFAULT_TAB = twips; }

// OOXML §17.3.3.32: a tab advances to the next stop strictly past the pen; if
// none is past, to the next default-grid multiple. Bar/Num stops don't catch
// tabs (§17.18.84). Returns the matched stop {p, a(lignment), l(eader)}.
function nextTabStop(penTw, stops) {
  const past = stops.filter((s) => s.a !== "Bar" && s.a !== "Num" && s.p > penTw + 1);
  if (past.length) return past.reduce((m, s) => (s.p < m.p ? s : m));
  return { p: (Math.floor(penTw / DEFAULT_TAB) + 1) * DEFAULT_TAB, a: "Left", l: null };
}

// Width (px) of the run that FOLLOWS a tab, up to the next tab on the same line —
// needed to right/center/decimal-align the following text on the stop. With
// `untilDecimal`, measures only up to the first decimal separator (integer part).
function followingWidthPx(span, untilDecimal) {
  const p = span.parentElement;
  if (!p) return 0;
  const tabs = [...p.querySelectorAll(".pm-tab")];
  const next = tabs[tabs.indexOf(span) + 1] || null;
  // text nodes strictly after this tab span, before the next tab
  const nodes = [];
  const walker = document.createTreeWalker(p, NodeFilter.SHOW_TEXT);
  let n;
  while ((n = walker.nextNode())) {
    if (!(span.compareDocumentPosition(n) & Node.DOCUMENT_POSITION_FOLLOWING)) continue; // at/before this tab
    if (next && !(next.compareDocumentPosition(n) & Node.DOCUMENT_POSITION_PRECEDING)) break; // at/after next tab
    nodes.push(n);
  }
  if (!nodes.length) return 0;
  let endNode = nodes[nodes.length - 1], endOff = endNode.textContent.length;
  if (untilDecimal) {
    for (const nd of nodes) { const di = nd.textContent.search(/[.,]/); if (di >= 0) { endNode = nd; endOff = di; break; } }
  }
  const range = document.createRange();
  range.setStart(nodes[0], 0);
  range.setEnd(endNode, endOff);
  const rects = range.getClientRects();
  return rects.length ? rects[0].width : 0;
}

const LEADER_CLASS = { Dot: "pm-lead-dot", MiddleDot: "pm-lead-dot", Hyphen: "pm-lead-dash", Underscore: "pm-lead-line", Heavy: "pm-lead-line" };

// Lay out every tab spacer to the real tab-stop geometry — honoring stop
// ALIGNMENT (left/right/center/decimal) and LEADER fills. A tab span's LEFT edge
// is the pen position; for right/center/decimal we also measure the following
// run so its trailing/centre/decimal point lands on the stop. One left-to-right
// pass; later tabs re-measure against already-applied earlier widths.
function layoutTabs(view) {
  const host = document.getElementById("editor");
  if (!host) return;
  const cs = getComputedStyle(host);
  const textEdge = host.getBoundingClientRect().left + parseFloat(cs.paddingLeft || "0");
  for (const span of view.dom.querySelectorAll(".pm-tab")) {
    let stops = [];
    try { stops = JSON.parse(span.dataset.stops || "[]"); } catch { /* ignore */ }
    const leftPx = span.getBoundingClientRect().left - textEdge; // forces reflow → reflects prior widths
    const stop = nextTabStop(leftPx * 15, stops);
    const stopPx = stop.p / 15;
    let widthPx = stopPx - leftPx; // Left: following text starts at the stop
    if (stop.a === "Right") widthPx = stopPx - leftPx - followingWidthPx(span, false);
    else if (stop.a === "Center") widthPx = stopPx - leftPx - followingWidthPx(span, false) / 2;
    else if (stop.a === "Decimal") widthPx = stopPx - leftPx - followingWidthPx(span, true);
    span.style.width = `${Math.max(0, widthPx)}px`;
    span.classList.remove("pm-lead-dot", "pm-lead-dash", "pm-lead-line");
    if (stop.l && LEADER_CLASS[stop.l]) span.classList.add(LEADER_CLASS[stop.l]);
  }
}

export const tabLayoutPlugin = new Plugin({
  view(editorView) {
    const run = () => requestAnimationFrame(() => layoutTabs(editorView));
    run();
    return { update: run };
  },
});

// The full per-paragraph layout CSS — indent + spacing + borders.
// All twips → px (1440 twips = 1in = 96px, so /15; equivalent to
// twips/20 pt). Composes with the page margins.
function blockLayoutStyle(b) {
  const px = (t) => `${t / 15}px`;
  const parts = [];

  const ind = b.indent;
  if (ind) {
    if (ind.left) parts.push(`margin-left:${px(ind.left)}`);
    if (ind.right) parts.push(`margin-right:${px(ind.right)}`);
    // First-line origin (negative = hanging). The render projection already
    // resolves this to a SINGLE value — a literal-prefix marker's leading-tab
    // landing is folded into effective_first_line_twips by the engine — so we
    // apply one text-indent with no special-case. (Tab-stop-marked clauses like
    // "(a)" and firstLine-marked ones like "(d)" thus carry the same origin.)
    if (ind.effective_first_line_twips) parts.push(`text-indent:${px(ind.effective_first_line_twips)}`);
  }

  // ── Vertical spacing (§17.3.1.33) ───────────────────────────────────────────
  // b.spacing is the engine's RESOLVED cascade (direct w:spacing → paragraph
  // style → docDefaults), so a paragraph that gets its spacing from its style or
  // the document defaults already carries the effective values here. We therefore
  // emit margins EXPLICITLY rather than only when truthy: a resolved `after:0`
  // (or absent `after`) must produce `margin-bottom:0`, not fall back to a CSS
  // default. (The hardcoded `.ProseMirror p` margin was removed for the same
  // reason — there is no CSS paragraph margin to override Word's spacing.)
  const sp = b.spacing;
  if (sp) {
    // Resolve the line box first: `*_lines` is in hundredths of a line, so one
    // "line" is the effective line height. With an Auto `line` (240ths of a
    // line) that's `line/240` em; otherwise a single line ≈ 1em.
    const lineRule = String(sp.line_rule || "Auto").toLowerCase();
    const lineEm = sp.line && lineRule === "auto" ? sp.line / 240 : 1;

    // margin-top: autospacing defers to the HTML default (don't force a value);
    // beforeLines takes precedence over `before` (§17.3.1.33); otherwise the
    // resolved twip value, defaulting to 0 so absent/zero spacing is explicit.
    if (!sp.before_autospacing) {
      if (sp.before_lines) parts.push(`margin-top:${(sp.before_lines / 100) * lineEm}em`);
      else parts.push(`margin-top:${px(sp.before || 0)}`);
    }
    // margin-bottom: symmetric (afterLines > after).
    if (!sp.after_autospacing) {
      if (sp.after_lines) parts.push(`margin-bottom:${(sp.after_lines / 100) * lineEm}em`);
      else parts.push(`margin-bottom:${px(sp.after || 0)}`);
    }

    if (sp.line) {
      if (lineRule === "auto") parts.push(`line-height:${sp.line / 240}`); // 240 twips = single
      else if (lineRule === "exact") parts.push(`line-height:${px(sp.line)}`);
      else if (lineRule === "atleast") parts.push(`min-height:${px(sp.line)}`);
    }
  }

  const bd = b.borders;
  if (bd) {
    const edge = (e, side) => {
      if (!e || !e.style) return null;
      const st = String(e.style).toLowerCase();
      if (st === "none" || st === "nil") return null;
      const sizePx = Math.max(1, Math.round((e.size ?? 4) / 8)); // size is in 1/8 pt
      const cssStyle = st === "single" ? "solid" : st; // most ST_Border names are valid CSS border-style
      const color = e.color && e.color !== "auto" ? `#${e.color}` : "currentColor";
      return `border-${side}:${sizePx}px ${cssStyle} ${color}`;
    };
    for (const [e, side] of [[bd.top, "top"], [bd.bottom, "bottom"], [bd.left, "left"], [bd.right, "right"]]) {
      const c = edge(e, side); if (c) parts.push(c);
    }
    if (!bd.left) { const bar = edge(bd.bar, "left"); if (bar) parts.push(bar); } // bar → left edge
  }

  return parts.join(";");
}

// ─── Schema ─────────────────────────────────────────────────────────────────

export const schema = new Schema({
  nodes: {
    doc: { content: "block+" },

    paragraph: {
      group: "block",
      content: "inline*",
      attrs: { blockId: { default: null }, guard: { default: null }, editable: { default: false }, align: { default: null }, bstyle: { default: "" }, numbering: { default: null } },
      parseDOM: [{ tag: "p" }],
      toDOM: (n) => ["p", blockAttrs(n), 0],
    },
    heading: {
      group: "block",
      content: "inline*",
      attrs: { blockId: { default: null }, guard: { default: null }, editable: { default: false }, align: { default: null }, bstyle: { default: "" }, numbering: { default: null }, level: { default: 1 } },
      parseDOM: [1, 2, 3, 4, 5, 6].map((l) => ({ tag: `h${l}`, attrs: { level: l } })),
      toDOM: (n) => [`h${n.attrs.level}`, blockAttrs(n), 0],
    },
    placeholder: {
      group: "block",
      atom: true,
      selectable: true,
      attrs: { blockId: { default: null }, kind: { default: "opaque" }, text: { default: "" } },
      toDOM: (n) => ["div", { class: "pm-placeholder", "data-id": n.attrs.blockId }, ["span", { class: "kind" }, n.attrs.kind], n.attrs.text || "(no extractable text)"],
    },

    text: { group: "inline" },

    image: {
      group: "inline",
      inline: true,
      atom: true,
      selectable: true,
      // drawingId / blockId / drawingGuard thread the engine identity of the
      // drawing through to the editor so a resize can target it:
      //   drawingId    → the Drawing opaque's `opaque_id` (set_image_attrs.drawing_id)
      //   blockId      → the hosting paragraph's block_id (set_image_attrs.target)
      //   drawingGuard → the drawing's own content_hash (set_image_attrs.semantic_hash)
      // width/height are display px (EMU ÷ 9525); they drive both the rendered
      // <img> and the aspect ratio preserved on resize.
      attrs: {
        src: { default: "" }, width: { default: null }, height: { default: null }, alt: { default: "" },
        drawingId: { default: null }, blockId: { default: null }, drawingGuard: { default: null },
      },
      toDOM: (n) => ["img", {
        class: "pm-img", src: n.attrs.src, alt: n.attrs.alt,
        "data-drawing-id": n.attrs.drawingId || "", "data-block-id": n.attrs.blockId || "",
        style: [n.attrs.width && `width:${n.attrs.width}px`, n.attrs.height && `height:${n.attrs.height}px`].filter(Boolean).join(";"),
      }],
    },
    anchor: {
      group: "inline",
      inline: true,
      atom: true,
      attrs: { text: { default: "object" }, cls: { default: "pm-anchor" } },
      toDOM: (n) => ["span", { class: n.attrs.cls }, n.attrs.text || "object"],
    },

    // A real table grid, rendered from the lean view's resolved cells (positions,
    // spans, effective borders/shading, v-align) + table meta (column widths).
    // A table is REAL editor content (table > tableRow > tableCell > paragraph),
    // so cell paragraphs reuse the body paragraph rendering/redline/commit path —
    // selection, redline, and formatting work inside cells like body text.
    table: {
      group: "block",
      content: "tableRow+",
      attrs: { blockId: { default: null }, guard: { default: null }, meta: { default: null } },
      toDOM: (n) => {
        const meta = n.attrs.meta || {};
        const cols = meta.cols || [];
        const head = [["tbody", 0]];
        if (cols.length) head.unshift(["colgroup", ...cols.map((w) => ["col", { style: `width:${w / 15}px` }])]);
        const style = [cols.length ? "table-layout:fixed" : "", meta.align === "center" ? "margin:0 auto" : meta.align === "right" ? "margin-left:auto" : ""].filter(Boolean).join(";");
        return ["table", { class: "pm-table", "data-id": n.attrs.blockId, style }, ...head];
      },
    },
    tableRow: { content: "tableCell+", toDOM: () => ["tr", 0] },
    tableCell: {
      content: "(paragraph | heading)+",
      isolating: true,
      attrs: { row: { default: 0 }, col: { default: 0 }, colSpan: { default: 1 }, rowSpan: { default: 1 }, borders: { default: null }, shading: { default: null }, vAlign: { default: null }, readonly: { default: false } },
      toDOM: (n) => {
        const css = [];
        const b = n.attrs.borders || {};
        for (const [side, e] of [["top", b.top], ["right", b.right], ["bottom", b.bottom], ["left", b.left]]) { const rule = cellBorderCss(side, e); if (rule) css.push(rule); }
        if (n.attrs.shading && /^[0-9a-fA-F]{6}$/.test(n.attrs.shading)) css.push(`background-color:#${n.attrs.shading}`);
        if (n.attrs.vAlign) css.push(`vertical-align:${n.attrs.vAlign}`);
        const attrs = { style: css.join(";"), "data-row": n.attrs.row, "data-col": n.attrs.col };
        if (n.attrs.colSpan > 1) attrs.colspan = n.attrs.colSpan;
        if (n.attrs.rowSpan > 1) attrs.rowspan = n.attrs.rowSpan;
        if (n.attrs.readonly) attrs.contenteditable = "false";
        return ["td", attrs, 0];
      },
    },

    // An equation (OMML). The editor renders it via a node view (OMML → MathML →
    // MathJax); this toDOM is the fallback label.
    equation: {
      group: "inline",
      inline: true,
      atom: true,
      attrs: { omml: { default: "" }, display: { default: false } },
      toDOM: () => ["span", { class: "pm-eq" }, "∑ equation"],
    },

    // A literal tab character. Rendered as an empty inline-block spacer that the
    // tab-layout plugin sizes to the next tab stop. `stops` is this paragraph's
    // custom tab stops, absolute (page-text-margin-relative) twips, comma-joined.
    tab: {
      group: "inline",
      inline: true,
      atom: true,
      attrs: { stops: { default: "" } },
      toDOM: (n) => ["span", { class: "pm-tab", "data-stops": n.attrs.stops }],
    },
  },

  marks: {
    strong: { toDOM: () => ["strong", 0], parseDOM: [{ tag: "strong" }, { tag: "b" }] },
    em: { toDOM: () => ["em", 0], parseDOM: [{ tag: "em" }, { tag: "i" }] },
    underline: { toDOM: () => ["u", 0], parseDOM: [{ tag: "u" }] },
    // User-authored only (the engine's display Mark has no strike; v4 content does).
    strike: { toDOM: () => ["s", 0], parseDOM: [{ tag: "s" }, { tag: "strike" }] },
    subscript: { toDOM: () => ["sub", 0], parseDOM: [{ tag: "sub" }] },
    superscript: { toDOM: () => ["sup", 0], parseDOM: [{ tag: "sup" }] },
    // Run-level CSS (font/size/color/highlight/caps/…), precomposed into one style string.
    run: {
      attrs: { style: { default: "" } },
      toDOM: (m) => ["span", { class: "pm-run", style: m.attrs.style }, 0],
      parseDOM: [{ tag: "span.pm-run", getAttrs: (el) => ({ style: el.getAttribute("style") || "" }) }],
    },
    ins: { inclusive: false, attrs: { author: { default: null }, rev: { default: null } }, toDOM: (m) => ["ins", { "data-author": m.attrs.author, "data-rev": m.attrs.rev }, 0] },
    del: { inclusive: false, attrs: { author: { default: null }, rev: { default: null } }, toDOM: (m) => ["del", { "data-author": m.attrs.author, "data-rev": m.attrs.rev }, 0] },
    // A hyperlink (target from /rich's resolved url; "#anchor" for bookmarks).
    link: {
      inclusive: false,
      // `opaqueId` is the engine's id for an EXISTING hyperlink opaque (null for a
      // link the user just authored). On commit it lets us reference the existing
      // link by id (opaque_ref) so editing the surrounding text preserves it
      // instead of tripping the OpaqueDestroyed guard.
      attrs: { href: { default: "" }, opaqueId: { default: null } },
      toDOM: (m) => ["a", { href: m.attrs.href, class: "pm-link", target: "_blank", rel: "noopener noreferrer", "data-opaque-id": m.attrs.opaqueId || null }, 0],
      parseDOM: [{ tag: "a[href]", getAttrs: (el) => ({ href: el.getAttribute("href") || "", opaqueId: el.getAttribute("data-opaque-id") || null }) }],
    },
    // A commented span (§17.13.4). Display-only, dropped on commit (not in
    // V4_MARK) — it highlights the text the engine bracketed with
    // CommentRangeStart…CommentReference markers (matched by `id` = the
    // CommentPayload.id), so a sidebar card links to its span. `id` is the
    // comment's w:id; `cid` lets clicks/scroll target the span by data attr.
    comment: {
      inclusive: false,
      attrs: { id: { default: "" } },
      toDOM: (m) => ["span", { class: "pm-comment", "data-comment-id": m.attrs.id }, 0],
    },
    // Display-only: a run carrying a tracked formatting change (rPrChange) — bold/
    // italic/underline/color/font added or removed as a tracked change. Flags the
    // text inline (the accept/reject card carries the action). Not a V4 mark, so it
    // never dirties the block or rides a commit; the /rich reconcile re-adds it.
    fmtchange: {
      inclusive: false,
      attrs: { rev: { default: null } },
      toDOM: (m) => ["span", { class: "pm-fmtchange", "data-rev": m.attrs.rev, title: "Formatting changed (tracked)" }, 0],
    },
  },
});

function blockAttrs(node) {
  const a = { "data-id": node.attrs.blockId, class: node.attrs.editable ? null : "pm-readonly" };
  const styles = [];
  if (node.attrs.align && ALIGN[node.attrs.align]) styles.push(`text-align:${ALIGN[node.attrs.align]}`);
  if (node.attrs.bstyle) styles.push(node.attrs.bstyle);
  if (styles.length) a.style = styles.join(";");
  return a;
}

const MARK_FOR_MARK = { Bold: schema.marks.strong, Italic: schema.marks.em, Underline: schema.marks.underline, Subscript: schema.marks.subscript, Superscript: schema.marks.superscript };

// Build a <table> DOM spec from flat {row, col, text} cells.
// A border edge {style,size,color} → CSS. size is in 1/8 pt; "auto"/None → sane.
function cellBorderCss(side, e) {
  if (!e || !e.style) return null;
  const st = String(e.style).toLowerCase();
  if (st === "none" || st === "nil") return `border-${side}:none`;
  const px = Math.max(1, Math.round((e.size ?? 4) / 8)); // eighths of a pt → ~px
  const cssStyle = st === "single" ? "solid" : st === "thick" ? "solid" : st;
  const color = e.color && e.color !== "auto" ? `#${e.color}` : "#000";
  return `border-${side}:${px}px ${cssStyle} ${color}`;
}

// Run-mark name → HTML tag, for rendering read-only DOM spans (the header/footer
// bands, which live outside the editable ProseMirror document).
const CELL_MARK_TAG = { Bold: "strong", Italic: "em", Underline: "u", Subscript: "sub", Superscript: "sup" };

// ─── header / footer bands (read-only DOM) ───────────────────────────────────

// Headers and footers live OUTSIDE the editable ProseMirror document: they are
// read-only page furniture. So we render them as real DOM, reusing the same
// run-style + mark + hyperlink decisions as the body (via runStyle / the mark
// tags) so their formatting is faithful — bold, color, fonts, hyperlinks, and
// the tab-separated left/center/right layout of the classic footer.

// Append a /rich segment to `parent` as real DOM nodes, splitting run text on
// tab characters so a tabbed line breaks into segments the caller can lay out.
function appendBandSeg(parent, seg) {
  const d = seg.Unchanged || seg.Inserted || seg.Deleted;
  if (d) {
    if (!d.text) return;
    const style = runStyle(d.style_props);
    const hasUL = d.style_props && d.style_props.underline_style && d.style_props.underline_style !== "None";
    const tags = (d.marks || []).map((m) => (m === "Underline" && hasUL ? null : CELL_MARK_TAG[m])).filter(Boolean);
    // Split on tabs: each chunk is its own (styled) run; the tab boundary itself
    // is left to the band's flex layout (space-between) to spread the runs.
    const chunks = d.text.split("\t");
    chunks.forEach((chunk, i) => {
      if (chunk) {
        let node = document.createTextNode(chunk);
        if (tags.length) { for (const tag of tags) { const w = document.createElement(tag); w.appendChild(node); node = w; } }
        if (style) { const span = document.createElement("span"); span.className = "pm-run"; span.setAttribute("style", style); span.appendChild(node); node = span; }
        parent.appendChild(node);
      }
      if (i < chunks.length - 1) { const t = document.createElement("span"); t.className = "hf-tab"; parent.appendChild(t); }
    });
    return;
  }
  if (seg.Opaque) {
    const o = seg.Opaque;
    if (o.kind === "Hyperlink") {
      if (o.url) { const a = document.createElement("a"); a.href = o.url; a.className = "pm-link"; a.target = "_blank"; a.rel = "noopener noreferrer"; a.textContent = o.text || ""; parent.appendChild(a); }
      else if (o.text) parent.appendChild(document.createTextNode(o.text));
      return;
    }
    if (o.kind === "FootnoteReference" || o.kind === "EndnoteReference") {
      if (o.text) { const sup = document.createElement("sup"); sup.textContent = o.text; parent.appendChild(sup); }
      return;
    }
    // Field (PAGE, NUMPAGES, …), Sym, Ptab → its result text inline.
    if (o.kind === "Field" || o.kind === "Sym" || o.kind === "Ptab") {
      if (o.text) parent.appendChild(document.createTextNode(o.text));
      return;
    }
    if (o.text) parent.appendChild(document.createTextNode(o.text));
  }
}

// Build one read-only band (header or footer) from a projected payload
// (`{ kind, segments }`). Paragraph breaks (the "\n" Unchanged segments the
// engine emits between story paragraphs) split the band into lines; within a
// line, tab boundaries become flex gaps so the classic left/center/right footer
// lays out faithfully.
export function buildHeaderFooterBand(payload, role) {
  const band = document.createElement("div");
  band.className = `hf-band hf-${role}`;
  band.setAttribute("data-kind", payload.kind || "default");
  band.setAttribute("contenteditable", "false");
  band.setAttribute("aria-label", `${role} (${payload.kind || "default"})`);

  // One line per paragraph, honoring its alignment (w:jc) — Word centers/right-
  // aligns footer paragraphs, so a flat left-aligned stream is wrong. A hard
  // break inside a paragraph still starts a new line, inheriting its alignment.
  for (const para of payload.paragraphs || []) {
    let line = newBandLine(para);
    band.appendChild(line);
    for (const seg of para.segments || []) {
      const d = seg.Unchanged || seg.Inserted || seg.Deleted;
      if (d && d.text === "\n") {
        line = newBandLine(para);
        band.appendChild(line);
        continue;
      }
      appendBandSeg(line, seg);
    }
  }
  if (!band.childNodes.length) band.appendChild(newBandLine(null));
  return band;
}

// A header/footer line. A line that contains tab characters keeps the flex
// layout (the `.hf-tab` spreaders spread its runs); a plain line aligns its text
// directly via `text-align` from the paragraph's `w:jc` — so a centered footer
// actually centers.
function newBandLine(para) {
  const line = document.createElement("div");
  line.className = "hf-line";
  const hasTab = !!(para && (para.segments || []).some((s) => {
    const d = s.Unchanged || s.Inserted || s.Deleted;
    return d && d.text && d.text.includes("\t");
  }));
  if (hasTab) line.classList.add("has-tab");
  const align = para && para.align && ALIGN[para.align];
  if (align) line.style.textAlign = align;
  return line;
}

// ─── /rich blocks → ProseMirror ─────────────────────────────────────────────

export function buildDoc(blocks) {
  const nodes = (blocks || []).map(buildBlock);
  if (nodes.length === 0) nodes.push(schema.nodes.paragraph.create({ editable: false }));
  return schema.nodes.doc.create(null, nodes);
}

// Build the ProseMirror node for one /rich block. Exported so the editor can
// patch a single block in place (reconcile).
// Cell paragraphs are editable (real PM content), like body paragraphs.
const CELLS_READONLY = false;

// Build a cell paragraph node from a CellParagraphView ({segments, block_id,
// guard}), reusing segToInline so redline/marks render exactly like a body
// paragraph. Cells carry no tab stops/numbering in the projection.
function buildCellParagraph(par, readonly) {
  const openComments = new Set();
  const inline = (par.segments || []).flatMap((seg) => {
    if (seg.Opaque && seg.Opaque.kind === "CommentReference") {
      const cid = seg.Opaque.reference_id;
      if (cid != null) { if (openComments.has(cid)) openComments.delete(cid); else openComments.add(cid); }
      return [];
    }
    return segToInline(seg, "[]", openComments, par.block_id);
  });
  return schema.nodes.paragraph.create(
    { blockId: par.block_id, guard: par.guard, editable: !readonly, align: null, bstyle: "", numbering: null },
    inline,
  );
}

export function buildBlock(b) {
  const type = b.block_type;
  if (type === "Table" && Array.isArray(b.cells) && b.cells.length) {
    const rowsMap = new Map();
    for (const c of b.cells) { if (!rowsMap.has(c.row)) rowsMap.set(c.row, []); rowsMap.get(c.row).push(c); }
    const rows = [...rowsMap.keys()].sort((x, y) => x - y).map((r) =>
      schema.nodes.tableRow.create(null, rowsMap.get(r).sort((x, y) => x.col - y.col).map((c) => {
        // A vMerge continuation cell carries no paragraphs (the engine folds it into
        // the anchor); render a single empty placeholder paragraph so the node is
        // content-valid, never an editable cell.
        const paras = (c.paragraphs && c.paragraphs.length)
          ? c.paragraphs.map((par) => buildCellParagraph(par, CELLS_READONLY))
          : [schema.nodes.paragraph.create({ editable: false }, [])];
        return schema.nodes.tableCell.create({
          row: c.row, col: c.col, colSpan: c.col_span || 1, rowSpan: c.row_span || 1,
          borders: c.borders || null, shading: c.shading || null, vAlign: c.v_align || null,
          readonly: CELLS_READONLY,
        }, paras);
      })),
    );
    return schema.nodes.table.create({ blockId: b.block_id, guard: b.guard, meta: b.table || null }, rows);
  }
  if (type === "Table" || type === "Opaque") {
    return schema.nodes.placeholder.create({ blockId: b.block_id, kind: (b.content_types || [type]).join(","), text: blockText(b) });
  }
  // The paragraph's custom tab stops, absolute (text-margin-relative) twips:
  // the engine gives them relative to body_left = left + first-line origin.
  const ind = b.indent || {};
  const bodyLeft = (ind.left || 0) + (ind.effective_first_line_twips || 0);
  const stopsAttr = JSON.stringify((b.tab_stops || []).map((t) => ({ p: bodyLeft + t.position, a: t.alignment || "Left", l: t.leader || null })));
  // Comment anchors arrive as zero-width `CommentReference` opaque segments that
  // BRACKET the commented text (start + end, same `reference_id`). We toggle an
  // "open comment ids" set as we walk the segments and attach the `comment` mark
  // to the text in between — so the span highlights and links to its sidebar card
  // — then drop the marker segments themselves (they carry no text).
  const openComments = new Set();
  const inline = (b.segments || []).flatMap((seg) => {
    if (seg.Opaque && seg.Opaque.kind === "CommentReference") {
      const cid = seg.Opaque.reference_id;
      if (cid != null) { if (openComments.has(cid)) openComments.delete(cid); else openComments.add(cid); }
      return [];
    }
    return segToInline(seg, stopsAttr, openComments, b.block_id);
  });
  // Numbering marker: a non-editable inline atom + a REAL tab node, so the
  // marker→text gap resolves to the paragraph's actual tab stops (like body
  // tabs) instead of a CSS tab-size grid. text-indent (the resolved origin)
  // places the marker; the tab advances the title to its stop.
  if (b.numbering_text) {
    inline.unshift(
      schema.nodes.anchor.create({ text: b.numbering_text, cls: "pm-num" }),
      schema.nodes.tab.create({ stops: stopsAttr }),
    );
  }
  const attrs = { blockId: b.block_id, guard: b.guard, editable: isCommittable(b), align: b.align || null, bstyle: blockLayoutStyle(b), numbering: b.numbering_text || null };
  if (type === "Heading") return schema.nodes.heading.create({ ...attrs, level: Math.min(6, b.heading_level || 1) }, inline);
  return schema.nodes.paragraph.create(attrs, inline);
}

// Rebuild the PM node for a block id from /rich — a top-level block OR a cell
// paragraph nested in a table. The reconcile (commit/accept-reject) uses this so a
// cell paragraph's provisional redline (rev:null) swaps to the authoritative one
// (real rev) in place, exactly like a body paragraph — a cell-paragraph id is not
// a top-level block, so a plain blocks.find() would miss it.
export function rebuildNodeFromRich(blocks, id) {
  const top = (blocks || []).find((b) => b.block_id === id);
  if (top) return buildBlock(top);
  for (const b of blocks || []) {
    if (b.block_type !== "Table" || !Array.isArray(b.cells)) continue;
    for (const c of b.cells) {
      for (const par of c.paragraphs || []) {
        if (par.block_id === id) return buildCellParagraph(par, CELLS_READONLY);
      }
    }
  }
  return null;
}

// Split run text on tab characters into text nodes + `tab` spacer nodes.
function splitTabs(text, marks, stopsAttr) {
  if (!text.includes("\t")) return [schema.text(text, marks)];
  const out = [];
  const parts = text.split("\t");
  parts.forEach((part, i) => {
    if (part) out.push(schema.text(part, marks));
    if (i < parts.length - 1) out.push(schema.nodes.tab.create({ stops: stopsAttr }));
  });
  return out;
}

function segToInline(seg, stopsAttr = "", openComments = null, blockId = null) {
  for (const variant of ["Unchanged", "Inserted", "Deleted"]) {
    if (seg[variant]) {
      const d = seg[variant];
      if (!d.text) return [];
      const marks = [];
      // Underline is rendered from style_props.underline_style in runStyle (which
      // also carries the line style); drop the plain `Underline` mark when that's
      // present so we don't render two underlines.
      const hasUnderlineStyle = d.style_props && d.style_props.underline_style && d.style_props.underline_style !== "None";
      for (const m of d.marks || []) {
        if (m === "Underline" && hasUnderlineStyle) continue;
        const mk = MARK_FOR_MARK[m];
        if (mk) marks.push(mk.create());
      }
      const style = runStyle(d.style_props);
      if (style) marks.push(schema.marks.run.create({ style }));
      if (variant === "Inserted") marks.push(schema.marks.ins.create({ rev: d.rev_id ?? null }));
      if (variant === "Deleted") marks.push(schema.marks.del.create({ rev: d.rev_id ?? null }));
      // A tracked run formatting change (rPrChange) → flag the text inline.
      if (d.formatting_change) marks.push(schema.marks.fmtchange.create({ rev: d.formatting_change.revision_id ?? null }));
      // Highlight text that sits inside an open comment range. We mark with the
      // FIRST open id (one highlight per span keeps the DOM simple; overlapping
      // comments still each light up their own non-overlapping stretch).
      if (openComments && openComments.size) {
        const cid = openComments.values().next().value;
        marks.push(schema.marks.comment.create({ id: String(cid) }));
      }
      return splitTabs(d.text, marks, stopsAttr);
    }
  }
  if (seg.Opaque) {
    const o = seg.Opaque;
    // Comment anchor markers are zero-width and consumed by buildBlock (which
    // toggles the open-comment set); they render nothing on their own.
    if (o.kind === "CommentReference") return [];
    if (o.kind === "Drawing" && o.asset_ref && o.asset_ref.startsWith("data:")) {
      // Thread the drawing's engine identity (opaque_id → drawingId, the hosting
      // block → blockId, the drawing's own content_hash → drawingGuard) so the
      // editor can target a resize (set_image_attrs) / delete (delete_image) at
      // this exact drawing. A tracked deletion/insertion of the drawing rides the
      // opaque's segment_type → carry it as a del/ins mark so the image renders
      // struck / marked (the same redline vocabulary as text).
      const imgMarks =
        o.segment_type === "Delete" ? [schema.marks.del.create({ rev: null })] :
        o.segment_type === "Insert" ? [schema.marks.ins.create({ rev: null })] : null;
      return [schema.nodes.image.create({
        src: fixImageDataUri(o.asset_ref), width: emuToPx(o.asset_width_emu), height: emuToPx(o.asset_height_emu), alt: o.alt_text || "",
        drawingId: o.opaque_id || null, blockId, drawingGuard: o.content_hash || null,
      }, null, imgMarks)];
    }
    if (o.kind === "Omml" && o.asset_ref) {
      return [schema.nodes.equation.create({ omml: o.asset_ref })];
    }
    // Hyperlink → a real <a> on the display text, carrying the engine's opaque id
    // so a commit can preserve THIS link (opaque_ref) when editing around it.
    if (o.kind === "Hyperlink") {
      const marks = o.url ? [schema.marks.link.create({ href: o.url, opaqueId: o.opaque_id || null })] : [];
      return o.text ? splitTabs(o.text, marks, stopsAttr) : [];
    }
    // Footnote/endnote reference → a superscript marker number, not a chip.
    if (o.kind === "FootnoteReference" || o.kind === "EndnoteReference") {
      return o.text ? [schema.text(o.text, [schema.marks.superscript.create()])] : [];
    }
    // Field → render its result text inline; structural plumbing (no result) drops.
    if (o.kind === "Field" || o.kind === "Sym" || o.kind === "Ptab") {
      return o.text ? splitTabs(o.text, [], stopsAttr) : [];
    }
    return [schema.nodes.anchor.create({ text: o.text || `[${String(o.kind || "object").toLowerCase()}]` })];
  }
  return [];
}

function blockText(b) {
  return (b.segments || []).map((s) => (s.Unchanged || s.Inserted || s.Deleted || {}).text || (s.Opaque || {}).text || "").join("");
}

// The visible text of a committable block — what ProseMirror's textContent
// yields, and the baseline for "did it change".
export function segmentText(b) {
  return (b.segments || []).map((s) => (s.Unchanged || s.Inserted || s.Deleted || {}).text || "").join("");
}

// A block is committable as a whole-block text replace only when that round-
// trips: a plain paragraph/heading, no list prefix, no existing tracked change,
// and no opaque EXCEPT hyperlinks (which round-trip via the v4 `hyperlink`
// content type — so a linked paragraph stays editable and links are authorable)
// and zero-width comment anchors (display-only, dropped on commit — a commented
// paragraph stays editable). Images/equations/fields still block, since a text
// replace can't carry them.
export function isCommittable(b) {
  if (b.block_type !== "Paragraph" && b.block_type !== "Heading") return false;
  if (b.numbering_text) return false;
  const segs = b.segments || [];
  if (segs.some((s) => s.Opaque && s.Opaque.kind !== "Hyperlink" && s.Opaque.kind !== "CommentReference")) return false;
  // Blocks that already carry a tracked change (Inserted/Deleted) ARE committable
  // again: the engine re-edits them via flatten-then-diff (accepts the prior
  // change into the base, then tracks the new edit), so a reviewer can refine a
  // sentence they just suggested. The client serializes the block's accept-all
  // view (insertions kept, deletions dropped) as the replace content to match
  // that base — see contentForCommit / blockSig. (Images/equations/fields still
  // block: a text replace can't carry them.)
  return true;
}

// ─── decorations ─────────────────────────────────────────────────────────────

// `isDirty(node) -> bool` decides which blocks are flagged (text OR formatting).
export function makeDirtyHighlight(isDirty) {
  return new Plugin({ props: { decorations(s) {
    const decos = [];
    s.doc.forEach((node, offset) => {
      if (node.attrs.blockId && isDirty(node)) decos.push(Decoration.node(offset, offset + node.nodeSize, { class: "pm-dirty" }));
    });
    return DecorationSet.create(s.doc, decos);
  } } });
}

// Highlight the ACTIVE comment's span via a ProseMirror Decoration (not a DOM
// classList mutation — that makes PM re-parse the node and drop the display-only
// comment mark, wiping every comment highlight). `getId()` returns the focused
// comment id; the editor dispatches a meta-only transaction to recompute.
export function makeActiveCommentHighlight(getId) {
  return new Plugin({ props: { decorations(s) {
    const id = getId();
    if (id == null || id === "") return DecorationSet.empty;
    const decos = [];
    s.doc.descendants((node, pos) => {
      if (node.isText && node.marks.some((m) => m.type === schema.marks.comment && m.attrs.id === id))
        decos.push(Decoration.inline(pos, pos + node.nodeSize, { class: "pm-comment-active" }));
    });
    return DecorationSet.create(s.doc, decos);
  } } });
}

export function makePendingHighlight(isPending) {
  return new Plugin({ props: { decorations(s) {
    const decos = [];
    s.doc.forEach((node, offset) => {
      if (node.attrs.blockId && isPending(node.attrs.blockId)) decos.push(Decoration.node(offset, offset + node.nodeSize, { class: "pm-pending" }));
    });
    return DecorationSet.create(s.doc, decos);
  } } });
}

// ─── live tracked changes (as-you-type redline) ──────────────────────────────
// In Suggesting mode, render each block's redline the instant the user types —
// a deletion shows struck (it never disappears), an insertion green — instead of
// only at commit. The engine commit then swaps the provisional marks (rev:null)
// for authoritative ones (real rev_id) seamlessly.

// Common leading/trailing char counts of two strings — the surgical diff bounds
// shared by the commit-time optimistic redline (redlineNode) and the live one,
// so both produce the same minimal del(old-middle)+ins(new-middle) shape.
export function surgicalDiffBounds(oldText, newText) {
  const oLen = oldText.length, nLen = newText.length;
  const maxMid = Math.min(oLen, nLen);
  let p = 0;
  while (p < maxMid && oldText[p] === newText[p]) p++;
  let s = 0;
  while (s < maxMid - p && oldText[oLen - 1 - s] === newText[nLen - 1 - s]) s++;
  return { p, s };
}

const isRedlineMark = (m) => m.type === schema.marks.ins || m.type === schema.marks.del;
const isLiveBlock = (node) => node.type === schema.nodes.paragraph || node.type === schema.nodes.heading;

// A block's accept-all inline content: del-marked text dropped, ins/del marks
// stripped, but ALL other marks (font, bold, link, …) preserved — i.e. the text
// the user intends, with its formatting. Returns an array of text nodes.
function acceptAllInlines(node) {
  const out = [];
  node.forEach((child) => {
    if (!child.isText || !child.text) return;
    if (child.marks.some((m) => m.type === schema.marks.del)) return;
    out.push(schema.text(child.text, child.marks.filter((m) => !isRedlineMark(m))));
  });
  return out;
}

// Slice an array of text nodes to the char range [from, to), preserving marks.
function sliceInlineNodes(nodes, from, to) {
  const out = [];
  let pos = 0;
  for (const n of nodes) {
    const len = n.text.length;
    const a = Math.max(from, pos), b = Math.min(to, pos + len);
    if (a < b) out.push(schema.text(n.text.slice(a - pos, b - pos), n.marks));
    pos += len;
  }
  return out;
}

// The provisional redline for a block: kept text (prefix/suffix) and inserted
// text keep the user's formatting; deleted text is shown struck (plain — it is
// going away). Every ins/del mark is provisional (rev:null).
function liveRedlineContent(baseText, userNodes, author) {
  const userText = userNodes.map((n) => n.text).join("");
  if (userText === baseText) return userNodes; // unchanged → no marks
  const { p, s } = surgicalDiffBounds(baseText, userText);
  const delMid = baseText.slice(p, baseText.length - s);
  const insMark = schema.marks.ins.create({ author, rev: null });
  const out = [];
  out.push(...sliceInlineNodes(userNodes, 0, p));
  if (delMid) out.push(schema.text(delMid, [schema.marks.del.create({ author, rev: null })]));
  for (const n of sliceInlineNodes(userNodes, p, userText.length - s)) {
    out.push(schema.text(n.text, insMark.addToSet(n.marks)));
  }
  out.push(...sliceInlineNodes(userNodes, userText.length - s, userText.length));
  return out;
}

// The caret's offset in ACCEPT-ALL coordinates within its block (counting only
// non-deleted chars), so it can be restored after the block is rebuilt with
// re-inserted struck text shifting the raw positions.
function captureCaret(state) {
  const sel = state.selection;
  if (!sel.empty) return null;
  // Nearest paragraph/heading ancestor at ANY depth (top-level OR inside a cell).
  let depth = sel.$from.depth;
  while (depth >= 1 && !isLiveBlock(sel.$from.node(depth))) depth--;
  if (depth < 1 || !isLiveBlock(sel.$from.node(depth))) return null;
  const block = sel.$from.node(depth);
  const caretInBlock = sel.$from.pos - sel.$from.start(depth);
  let acc = 0, pos = 0;
  for (let i = 0; i < block.childCount && pos < caretInBlock; i++) {
    const child = block.child(i);
    const size = child.isText ? child.text.length : child.nodeSize;
    const take = Math.min(size, caretInBlock - pos);
    if (child.isText && !child.marks.some((m) => m.type === schema.marks.del)) acc += take;
    pos += take;
  }
  // By blockId (not index/position): rebuilding redline shifts positions, and a
  // cell paragraph has no top-level index — the id locates it at any depth.
  return { blockId: block.attrs.blockId, acceptOffset: acc };
}

// Map an accept-all offset to a raw position inside a (rebuilt) block, skipping
// past deleted (struck) text so the caret lands after it, not before.
function acceptOffsetToPos(block, target) {
  let acc = 0, pos = 0;
  for (let i = 0; i < block.childCount; i++) {
    const child = block.child(i);
    if (!child.isText) { pos += child.nodeSize; continue; }
    const len = child.text.length;
    if (child.marks.some((m) => m.type === schema.marks.del)) { pos += len; continue; }
    if (acc + len >= target) return pos + (target - acc);
    acc += len; pos += len;
  }
  return pos;
}

// The plugin. `getMode()` → "suggesting"|"editing"; `getAuthor()` → string;
// `baselineTextFor(id)` → the block's committed accept-all text (or null).
export function liveTrackChangesPlugin({ getMode, getAuthor, baselineTextFor }) {
  return new Plugin({
    appendTransaction(trs, oldState, newState) {
      if (getMode() !== "suggesting") return null;
      if (!trs.some((tr) => tr.docChanged)) return null;
      // Our own (commit/reconcile/resolve) transactions set addToHistory:false —
      // never re-mark those, only genuine user edits.
      if (trs.every((tr) => tr.getMeta("addToHistory") === false)) return null;
      // Structural change (a block added/removed, e.g. Enter/merge) → defer to the
      // existing commit full-resync path; live marking is text-within-block only.
      if (oldState.doc.childCount !== newState.doc.childCount) return null;

      const caret = captureCaret(newState);
      const author = getAuthor();
      const repls = [];
      // Old paragraphs/headings by id (top-level AND inside cells) — compare by id
      // so a cell paragraph (which has no top-level index) is matched to its prior
      // version. Descend into tables; don't recurse into a paragraph's inlines.
      const oldById = new Map();
      oldState.doc.descendants((n) => {
        if (isLiveBlock(n)) { if (n.attrs.blockId != null) oldById.set(n.attrs.blockId, n); return false; }
        return undefined;
      });
      newState.doc.descendants((node, pos) => {
        if (!isLiveBlock(node)) return undefined; // recurse into tables/rows/cells
        const id = node.attrs.blockId;
        // Only blocks the user actually changed this transaction (matched by id).
        if (id == null || (oldById.has(id) && node.eq(oldById.get(id)))) return false;
        // Defer to the commit-time path when the block carries an opaque (the live
        // string-diff can't carry opaques) OR an already-COMMITTED tracked change —
        // an ins/del redline OR a run formatting change (rPrChange) — since
        // re-deriving the live redline from the accept-all text would DROP it. Same
        // rule for cells and body.
        let defer = false;
        node.forEach((c) => {
          if (!c.isText) { defer = true; return; }
          if (c.marks.some((m) => (m.type === schema.marks.ins || m.type === schema.marks.del || m.type === schema.marks.fmtchange) && m.attrs.rev != null)) defer = true;
        });
        if (defer) return false;
        const baseText = baselineTextFor(id);
        if (baseText == null) return false; // no committed baseline → skip
        const want = Fragment.fromArray(liveRedlineContent(baseText, acceptAllInlines(node), author));
        if (!node.content.eq(want)) repls.push({ from: pos + 1, to: pos + node.nodeSize - 1, want });
        return false; // a paragraph's inlines aren't blocks — don't recurse
      });
      if (!repls.length) return null;

      const tr = newState.tr;
      for (const r of repls.sort((a, b) => b.from - a.from)) tr.replaceWith(r.from, r.to, r.want);
      if (caret && caret.blockId != null) {
        let bpos = null, bnode = null;
        tr.doc.descendants((node, pos) => {
          if (bnode) return false;
          if (isLiveBlock(node) && node.attrs.blockId === caret.blockId) { bpos = pos; bnode = node; return false; }
          return undefined;
        });
        if (bnode != null && bpos != null) {
          const raw = bpos + 1 + acceptOffsetToPos(bnode, caret.acceptOffset);
          const clamped = Math.max(0, Math.min(raw, tr.doc.content.size - 1));
          try { tr.setSelection(TextSelection.near(tr.doc.resolve(clamped))); } catch { /* leave default */ }
        }
      }
      tr.setMeta("addToHistory", false);
      tr.setMeta("liveTrack", true);
      return tr;
    },
  });
}

// ─── samples ─────────────────────────────────────────────────────────────────

export const SAMPLES = [
  { file: "safe-agreement.docx", label: "SAFE agreement", desc: "Numbered legal clauses and [placeholders] to fill" },
  { file: "simple-text.docx", label: "Simple text", desc: "Plain paragraphs — a clean first edit" },
  { file: "table.docx", label: "Table", desc: "A real table, rendered from the cell grid" },
  { file: "images.docx", label: "Images", desc: "Embedded images, rendered inline" },
  { file: "equations.docx", label: "Equations", desc: "OMML math, rendered via MathJax" },
];

export async function fetchSample(file) {
  const res = await fetch(`/samples/${file}`);
  if (!res.ok) throw new Error(`sample not found: ${file} (HTTP ${res.status})`);
  return res.blob();
}

export function renderSamples(container, onPick) {
  container.replaceChildren();
  const label = document.createElement("span");
  label.className = "samples-label";
  label.textContent = "or try:";
  container.appendChild(label);
  for (const s of SAMPLES) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "ghost sample";
    btn.textContent = s.label;
    btn.title = s.desc;
    btn.addEventListener("click", () => onPick(s));
    container.appendChild(btn);
  }
}

// ─── API client ─────────────────────────────────────────────────────────────

async function parseError(res) {
  let body = {};
  try { body = await res.json(); } catch { /* non-JSON */ }
  const err = new Error(body.error || res.statusText);
  err.code = body.code || `HTTP_${res.status}`;
  err.status = res.status;
  return err;
}

export const api = {
  async upload(file) {
    const res = await fetch("/api/documents", { method: "POST", headers: { "Content-Type": "application/octet-stream" }, body: await file.arrayBuffer() });
    if (!res.ok) throw await parseError(res);
    return res.json(); // { doc_id, document }
  },
  async rich(id) {
    const res = await fetch(`/api/documents/${id}/rich`);
    if (!res.ok) throw await parseError(res);
    return res.json(); // { blocks, section }
  },
  async apply(id, transaction) {
    const res = await fetch(`/api/documents/${id}/apply`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(transaction) });
    if (!res.ok) throw await parseError(res);
    return (await res.json()).document;
  },
  async revisions(id) {
    const res = await fetch(`/api/documents/${id}/revisions`);
    if (!res.ok) throw await parseError(res);
    return (await res.json()).revisions;
  },
  async resolve(id, revisionIds, action) {
    const res = await fetch(`/api/documents/${id}/resolve`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ revision_ids: revisionIds, action }) });
    if (!res.ok) throw await parseError(res);
    return (await res.json()).document;
  },
  exportUrl(id, mode) { return `/api/documents/${id}/export?mode=${mode}`; },
};
