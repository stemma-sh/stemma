// stemma · Word frontend.
//
// Opens a .docx, renders it richly from /rich (fonts, sizes, colors, highlights,
// alignment, images, real tables, equations via MathJax, page geometry), and
// edits it in ProseMirror. A formatting toolbar (B/I/U/S) authors run marks; on
// commit a changed block's content is serialized WITH its marks, so bold/italic/
// underline/strike round-trip through stemma's typed `replace`. Edits commit as
// tracked (Suggesting) or direct (Editing) changes, optimistically.

import { EditorState, TextSelection, NodeSelection } from "prosemirror-state";
import { EditorView } from "prosemirror-view";
import { keymap } from "prosemirror-keymap";
import { baseKeymap, toggleMark } from "prosemirror-commands";
import { history, undo, redo } from "prosemirror-history";

import {
  schema, buildDoc, buildBlock, isCommittable,
  makeDirtyHighlight, makePendingHighlight, fetchSample, renderSamples, api,
  tabLayoutPlugin, setDefaultTab, buildHeaderFooterBand, makeActiveCommentHighlight,
  surgicalDiffBounds, liveTrackChangesPlugin, applyRunPropsToStyle, rebuildNodeFromRich,
} from "../shared/stemma-doc.js";
import { renderEquation } from "../shared/math.js";

// PM mark name -> v4 content mark name (the run CSS mark + ins/del are dropped).
const V4_MARK = { strong: "bold", em: "italic", underline: "underline", strike: "strike", subscript: "subscript", superscript: "superscript" };

function equationNodeView(node) {
  const dom = document.createElement("span");
  dom.className = "pm-eq";
  dom.textContent = "∑ equation";
  if (node.attrs.omml) renderEquation(node.attrs.omml, dom);
  return { dom, ignoreMutation: () => true, stopEvent: () => true };
}

// A page-border edge {style,size,color} → CSS (size is in 1/8 pt). "" = none.
function pageBorderCss(e) {
  if (!e || !e.style) return "";
  const st = String(e.style).toLowerCase();
  if (st === "none" || st === "nil") return "";
  const px = Math.max(1, Math.round((e.size ?? 4) / 8));
  const cssStyle = st === "single" || st === "thick" ? "solid" : st;
  const color = e.color && e.color !== "auto" ? `#${e.color}` : "#000";
  return `${px}px ${cssStyle} ${color}`;
}

function applyPageGeometry(section) {
  const el = document.getElementById("editor");
  const tw = (t) => Math.round((t || 0) / 15);
  for (const p of ["maxWidth", "width", "marginLeft", "marginRight", "paddingLeft", "paddingRight", "paddingTop", "paddingBottom", "columnCount", "columnGap", "columnRule", "borderTop", "borderRight", "borderBottom", "borderLeft"]) el.style[p] = "";
  if (!section) return;
  // Page borders (w:pgBorders, §17.6.10) → a border frame on the page.
  const pb = section.page_borders;
  if (pb) {
    for (const [side, cap] of [["top", "Top"], ["right", "Right"], ["bottom", "Bottom"], ["left", "Left"]]) {
      const css = pageBorderCss(pb[side]);
      if (css) el.style[`border${cap}`] = css;
    }
  }
  setDefaultTab(section.default_tab_stop);
  if (section.page_width) { const w = tw(section.page_width); el.style.maxWidth = `${w}px`; el.style.width = `${w}px`; el.style.marginLeft = "auto"; el.style.marginRight = "auto"; }
  if (section.margin_left != null) el.style.paddingLeft = `${tw(section.margin_left)}px`;
  if (section.margin_right != null) el.style.paddingRight = `${tw(section.margin_right)}px`;
  if (section.margin_top != null) el.style.paddingTop = `${tw(section.margin_top)}px`;
  if (section.margin_bottom != null) el.style.paddingBottom = `${tw(section.margin_bottom)}px`;
  // Multi-column body (w:cols, §17.6.4) → CSS multi-column.
  if (section.columns && section.columns > 1) {
    el.style.columnCount = String(section.columns);
    if (section.column_space != null) el.style.columnGap = `${tw(section.column_space)}px`;
    if (section.column_separator) el.style.columnRule = "1px solid #c4ccd6";
  }
}

// Pick the band that applies to the page we render. The editor shows a single
// continuous page, so we render the DEFAULT header/footer (the common case);
// first-page / even-page variants are projected too (carried in `kind`) but not
// selected here. Falls back to whatever single band exists when there is no
// explicit default.
function pickBand(bands) {
  if (!bands || !bands.length) return null;
  return bands.find((b) => (b.kind || "default") === "default") || bands[0];
}

// Render the read-only header/footer bands into the page. The bands sit inside
// `#editor` (the page) but OUTSIDE the ProseMirror-managed content: the header
// before `.ProseMirror`, the footer after it. We remove any previously rendered
// bands first so re-opening a document doesn't stack them.
function renderHeaderFooterBands(headers, footers) {
  const el = document.getElementById("editor");
  el.querySelectorAll(".hf-band").forEach((b) => b.remove());
  const pm = el.querySelector(".ProseMirror");
  const header = pickBand(headers);
  if (header) el.insertBefore(buildHeaderFooterBand(header, "header"), pm || el.firstChild);
  const footer = pickBand(footers);
  if (footer) el.appendChild(buildHeaderFooterBand(footer, "footer"));
}

const state = {
  docId: null,
  committed: new Map(), // blockId -> { sig, text, guard }
  guards: new Map(), // blockId -> guard, for EVERY block (incl. non-committable list items/tables)
  indentLeft: new Map(), // blockId -> current left indent (twips), for relative indent
  listInfo: new Map(), // blockId -> { ilvl } for list items (Word auto-numbering)
  bulletNumId: null, // a num_id of an existing bullet list, for "make this a bullet"
  renderedIds: new Set(), // all block ids from the last /rich render (for delete detection)
  mode: "suggesting",
  pending: new Map(),
  queue: [],
  draining: false,
  generation: 0, // bumped on each open; jobs from a prior doc are dropped when stale
  comments: [], // CommentPayload[] from /rich — drives the sidebar + span links
  activeComment: null, // id of the currently focused comment (card + span highlight)
  suggestions: [], // pending tracked changes (from /revisions) — the review rail
};
let view = null;

// ─── content signature + serialization ──────────────────────────────────────

// A block's editable signature: text + the authored marks (B/I/U/S/sub/sup),
// so a formatting toggle (same text) marks the block dirty.
function blockSig(node) {
  let s = "";
  node.forEach((child) => {
    if (child.isText) {
      // Mirror contentForCommit's accept-all view: deleted text is not part of the
      // block's logical content, so it must not affect the dirty signature.
      if (child.marks.some((x) => x.type.name === "del")) return;
      const m = child.marks.map((x) => V4_MARK[x.type.name]).filter(Boolean).sort().join(",");
      const link = child.marks.find((x) => x.type.name === "link");
      s += `${child.text}${m}${link ? "L:" + link.attrs.href : ""}`;
    } else if (child.type.name === "tab") s += "\t";
    else s += `[${child.type.name}]`;
  });
  return s;
}

// Serialize a committable block to v4 paragraph content, carrying per-run marks.
function contentForCommit(node) {
  const content = [];
  const refdOpaques = new Set();
  node.forEach((child) => {
    if (child.type.name === "tab") { content.push({ type: "text", text: "\t" }); return; }
    if (!child.isText || !child.text) return;
    // Drop text marked as a tracked DELETION: we send the block's accept-all view
    // (the engine flattens existing tracked changes to that base before diffing,
    // so deleted text is already gone). Inserted text carries no special mark here
    // and is kept as ordinary text.
    if (child.marks.some((m) => m.type.name === "del")) return;
    const link = child.marks.find((m) => m.type.name === "link");
    if (link && link.attrs.href) {
      if (link.attrs.opaqueId) {
        // An EXISTING hyperlink opaque: reference it by id so the engine preserves
        // it (its display text lives in the opaque, so we emit no text). Dedup: a
        // run split into pieces shares one opaque id, but opaque_ref must appear
        // once per id.
        if (!refdOpaques.has(link.attrs.opaqueId)) {
          refdOpaques.add(link.attrs.opaqueId);
          content.push({ type: "opaque_ref", attrs: { id: link.attrs.opaqueId } });
        }
        return;
      }
      // A newly-authored link: mint it from the display text + href.
      content.push({ type: "hyperlink", attrs: { href: link.attrs.href }, content: [{ type: "text", text: child.text }] });
      return;
    }
    const marks = [];
    for (const m of child.marks) { const t = V4_MARK[m.type.name]; if (t) marks.push({ type: t }); }
    content.push(marks.length ? { type: "text", text: child.text, marks } : { type: "text", text: child.text });
  });
  return { type: "paragraph", content };
}

function isBlockDirty(node) {
  const id = node.attrs.blockId;
  if (!id || state.pending.has(id) || !state.committed.has(id)) return false;
  return blockSig(node) !== state.committed.get(id).sig;
}

// Paragraph/heading nodes in document order, each with its EFFECTIVE id: the
// first occurrence of a blockId is the original; a SECOND occurrence (PM copies
// the id when it splits a paragraph) is a brand-new block → id null. So a split
// becomes "replace the original + insert the tail", not two replaces of one id.
function classifiedBlocks() {
  const seen = new Set();
  const out = [];
  view.state.doc.forEach((node) => {
    if (node.type !== schema.nodes.paragraph && node.type !== schema.nodes.heading) return;
    const raw = node.attrs.blockId;
    const isNew = !raw || seen.has(raw);
    if (!isNew) seen.add(raw);
    out.push({ node, id: isNew ? null : raw });
  });
  return out;
}

function dirtyBlocks() {
  const dirty = [];
  for (const { node, id } of classifiedBlocks()) {
    if (!id || state.pending.has(id) || !state.committed.has(id)) continue;
    if (blockSig(node) !== state.committed.get(id).sig) {
      const base = state.committed.get(id);
      dirty.push({ blockId: id, guard: base.guard, text: node.textContent, content: contentForCommit(node) });
    }
  }
  return dirty;
}

// Dirty CELL paragraphs (nested in a tableCell). A separate descend that produces
// ONLY plain `replace`s — classifiedBlocks/structuralOps stay strictly top-level,
// so an in-cell edit can never become a body insert/delete (the structural-ops
// guard). Same shape as dirtyBlocks so commit() treats them uniformly.
function cellDirtyBlocks() {
  const dirty = [];
  if (!view) return dirty;
  view.state.doc.descendants((node, _pos, parent) => {
    if (!parent || parent.type !== schema.nodes.tableCell) return;
    if (node.type !== schema.nodes.paragraph && node.type !== schema.nodes.heading) return;
    const id = node.attrs.blockId;
    if (!id || state.pending.has(id) || !state.committed.has(id)) return;
    if (blockSig(node) !== state.committed.get(id).sig) {
      const base = state.committed.get(id);
      dirty.push({ blockId: id, guard: base.guard, text: node.textContent, content: contentForCommit(node) });
    }
  });
  return dirty;
}

// The enclosing tableCell depth for a position, or null if not in a cell.
function cellDepthOf($pos) {
  for (let d = $pos.depth; d >= 1; d--) {
    if ($pos.node(d).type === schema.nodes.tableCell) return d;
  }
  return null;
}
// Structural-ops guard (highest-risk surface): an in-cell edit must NEVER become a
// body structural op. Phase 1 forbids creating in-cell structure — Enter inside a
// cell is a no-op, and Backspace at the start of a cell paragraph is swallowed (it
// would otherwise try to merge across the cell boundary into the body / prev cell).
// In-cell character edits fall through and commit as a plain replace.
function guardCellEnter(state) {
  return cellDepthOf(state.selection.$from) != null;
}
function guardCellBackspace(state) {
  const sel = state.selection;
  if (!sel.empty || cellDepthOf(sel.$from) == null) return false;
  return sel.$from.parentOffset === 0;
}

// Backspace/Delete on a SELECTED image. An image is an opaque the engine won't drop
// from a paragraph via a text edit (OpaqueDestroyed), and image blocks aren't
// committable — so the default (just remove the inline node) would empty the block
// LOCALLY without ever committing, diverging from the server until a later edit
// trips StaleEdit and reverts. Instead, delete the whole paragraph (which removes
// the image WITH it — a structural delete the engine accepts), so it actually
// persists. Only when the image is the sole content of a body paragraph; otherwise
// refuse loudly rather than diverge.
// Backspace/Delete on a SELECTED image → the dedicated delete_image op, which the
// engine applies as a tracked deletion (Suggesting mode: the drawing renders struck,
// accept/reject via its card) or a clean removal (Editing mode). Works for BOTH
// inline (image sharing a line with text) and standalone images. Targets the drawing
// by its own id + content_hash guard (like resize) — never the text-replace path,
// so it can't trip OpaqueDestroyed. No optimistic node removal (that would trip
// structuralOps into a spurious block delete); the /rich reconcile repaints it.
function deleteSelectedImage(state) {
  const sel = state.selection;
  if (!(sel instanceof NodeSelection) || sel.node.type !== schema.nodes.image) return false;
  const { drawingId, blockId, drawingGuard } = sel.node.attrs;
  if (!drawingId || !blockId) {
    setStatus("This image can't be deleted — it's missing its drawing identity.", "error");
    return true;
  }
  const op = { op: "delete_image", target: blockId, drawing_id: drawingId };
  if (drawingGuard) op.semantic_hash = drawingGuard;
  runFormatJob([op], [blockId]);
  return true;
}

// Structural diff vs the last render: blocks removed (Backspace-merge) → Delete;
// runs of NEW blocks (Enter/split, blockId === null) → one Insert anchored on the
// nearest existing block. Free-form editing is local until an explicit Commit;
// this computes the typed ops that reproduce the new structure.
function structuralOps() {
  const cb = classifiedBlocks();
  // Kept ids = effective paragraph/heading ids still present + every non-para
  // block (tables, …), so nothing present is mistaken for deleted.
  const keptIds = new Set(cb.map((b) => b.id).filter(Boolean));
  view.state.doc.forEach((node) => {
    if (node.attrs.blockId && node.type !== schema.nodes.paragraph && node.type !== schema.nodes.heading) keptIds.add(node.attrs.blockId);
  });
  const deletes = [...state.renderedIds]
    .filter((id) => !keptIds.has(id))
    .map((id) => {
      const o = { op: "delete", target: id, expect: state.committed.get(id)?.text ?? "" };
      const g = guardOf(id); if (g) o.semantic_hash = g;
      return o;
    });
  const inserts = [];
  let i = 0;
  while (i < cb.length) {
    if (cb[i].id) { i++; continue; }
    let j = i; const content = [];
    // Inserted paragraphs need an explicit vocabulary role; "body" resolves to
    // the document's default body paragraph role (the engine clones its format).
    while (j < cb.length && !cb[j].id) { content.push({ ...contentForCommit(cb[j].node), role: "body" }); j++; }
    const prev = cb[i - 1]?.id;
    const next = cb[j]?.id;
    if (prev) inserts.push({ op: "insert", target: { anchor: prev, position: "after" }, content });
    else if (next) inserts.push({ op: "insert", target: { anchor: next, position: "before" }, content });
    // No surviving neighbour (e.g. Ctrl+A then type replaces every block with one
    // id-less paragraph): anchor before the FIRST to-be-deleted block, which still
    // exists when inserts run (inserts precede deletes), so the new content lands
    // where the old content was instead of the whole document being wiped.
    else if (deletes.length) inserts.push({ op: "insert", target: { anchor: deletes[0].target, position: "before" }, content });
    i = j;
  }
  return { deletes, inserts, count: deletes.length + inserts.length };
}

// Re-pin committed baselines from the current (rendered) doc + rich guards.
function rememberCommitted(richBlocks, { clear = false } = {}) {
  if (clear) state.committed.clear();
  const info = new Map(richBlocks.map((b) => [b.block_id, b]));
  for (const b of richBlocks) state.indentLeft.set(b.block_id, (b.indent && b.indent.left) || 0);
  if (clear) state.renderedIds = new Set(richBlocks.map((b) => b.block_id));
  // Guards for every block (list items/tables aren't "committed" but still need a guard).
  state.guards = new Map(richBlocks.map((b) => [b.block_id, b.guard]));
  // List membership + a bullet num_id to join (richBlocks is always the full doc).
  state.listInfo = new Map(richBlocks.filter((b) => b.numbering_ilvl != null).map((b) => [b.block_id, { ilvl: b.numbering_ilvl }]));
  const bullet = richBlocks.find((b) => b.numbering_text === "•" && b.numbering_num_id != null);
  state.bulletNumId = bullet ? bullet.numbering_num_id : null;
  view.state.doc.forEach((node) => {
    const id = node.attrs.blockId;
    if (!id || state.pending.has(id)) return;
    const b = info.get(id);
    if (b && isCommittable(b)) state.committed.set(id, { sig: blockSig(node), text: node.textContent, guard: b.guard });
    else state.committed.delete(id);
  });
  // Cell paragraphs (nested in a tableCell) get their committed baseline + guard
  // from their OWN node attrs (block_id/guard threaded by buildCellParagraph), so
  // cellDirtyBlocks + the live plugin can target them. They are NOT added to
  // renderedIds — delete-detection (structuralOps) stays strictly top-level, so a
  // cell paragraph can never be mistaken for a removed body block.
  view.state.doc.descendants((node, _pos, parent) => {
    if (!parent || parent.type !== schema.nodes.tableCell) return;
    if (node.type !== schema.nodes.paragraph && node.type !== schema.nodes.heading) return;
    const id = node.attrs.blockId;
    if (!id || state.pending.has(id)) return;
    state.committed.set(id, { sig: blockSig(node), text: node.textContent, guard: node.attrs.guard });
    state.guards.set(id, node.attrs.guard);
  });
}

// ─── render ──────────────────────────────────────────────────────────────────

function render(blocks) {
  const editorState = EditorState.create({
    doc: buildDoc(blocks),
    plugins: [
      makePendingHighlight((id) => state.pending.has(id)),
      makeDirtyHighlight(isBlockDirty),
      makeActiveCommentHighlight(() => state.activeComment),
      history(),
      keymap({
        "Mod-z": undo, "Mod-y": redo, "Mod-Shift-z": redo,
        "Mod-b": toggleMark(schema.marks.strong),
        "Mod-i": toggleMark(schema.marks.em),
        "Mod-u": toggleMark(schema.marks.underline),
        "Mod-k": () => { applyLink(); return true; },
        "Mod-Enter": () => { commit(); return true; },
      }),
      // Cell-boundary structural guard — runs BEFORE baseKeymap so an in-cell
      // Enter/Backspace can never produce a body structural op (Phase 1).
      keymap({ Backspace: deleteSelectedImage, Delete: deleteSelectedImage }),
      keymap({ Enter: guardCellEnter, Backspace: guardCellBackspace }),
      keymap(baseKeymap),
      tabLayoutPlugin,
      liveTrackChangesPlugin({
        getMode: () => state.mode,
        getAuthor: () => document.getElementById("author").value.trim() || "Word demo",
        baselineTextFor: (id) => (state.committed.has(id) ? state.committed.get(id).text : null),
      }),
    ],
  });
  if (view) view.updateState(editorState);
  else {
    view = new EditorView(document.getElementById("editor"), {
      state: editorState,
      nodeViews: { equation: equationNodeView },
      dispatchTransaction(tr) { view.updateState(view.state.apply(tr)); refreshDirty(); refreshToolbar(); syncImageBar(); if (tr.docChanged && tr.getMeta("addToHistory") !== false) scheduleAutosave(); },
    });
  }
  rememberCommitted(blocks, { clear: true });
  document.getElementById("empty-state").classList.add("hidden");
  refreshDirty();
  refreshToolbar();
}

// Autosave: edits commit themselves after a short idle (Google-Docs style), so
// there is no explicit "save" gesture — the indicator just reads "Saving…" then
// "All changes saved". Ctrl+Enter / clicking the indicator still saves now.
const AUTOSAVE_MS = 1100;
function scheduleAutosave() {
  clearTimeout(state.autosaveTimer);
  state.autosaveTimer = setTimeout(() => { state.autosaveTimer = null; commit(); }, AUTOSAVE_MS);
}

function refreshDirty() {
  if (!view) return;
  const n = dirtyBlocks().length + cellDirtyBlocks().length + structuralOps().count;
  // "Saving…" whenever there's unsaved work (dirty, queued, or in flight);
  // "All changes saved" once everything has reconciled.
  const busy = n > 0 || state.queue.length > 0 || state.pending.size > 0 || state.draining || state.autosaveTimer;
  const ind = document.getElementById("commit");
  ind.textContent = busy ? "Saving…" : "All changes saved";
  ind.classList.toggle("saved", !busy);
  ind.classList.toggle("busy", !!busy);
  ind.disabled = false; // always clickable as "save now"
}

// ─── formatting toolbar ─────────────────────────────────────────────────────

function markActive(type) {
  const { from, $from, to, empty } = view.state.selection;
  return empty ? !!type.isInSet(view.state.storedMarks || $from.marks()) : view.state.doc.rangeHasMark(from, to, type);
}
function applyFmt(markName) {
  if (!view) return;
  toggleMark(schema.marks[markName])(view.state, view.dispatch);
  view.focus();
}
function refreshToolbar() {
  if (!view) return;
  for (const [id, mark] of [["fmt-bold", "strong"], ["fmt-italic", "em"], ["fmt-underline", "underline"], ["fmt-strike", "strike"]]) {
    const btn = document.getElementById(id);
    if (btn) { btn.disabled = !view; btn.setAttribute("aria-pressed", String(markActive(schema.marks[mark]))); }
  }
  // Run/paragraph controls are enabled whenever a doc is open.
  for (const id of ["fmt-font", "fmt-size", "fmt-color", "fmt-highlight",
    "fmt-link", "fmt-image", "fmt-comment", "fmt-align-left", "fmt-align-center", "fmt-align-right", "fmt-align-justify", "fmt-bullet", "fmt-outdent", "fmt-indent"]) {
    const el = document.getElementById(id); if (el) el.disabled = !view;
  }
  reflectSelectionState();
}

// Show the CURRENT selection's font / size / color and its list/alignment state,
// so the toolbar reads like Word's (the active value, not a placeholder).
function reflectSelectionState() {
  const setVal = (id, v) => { const el = document.getElementById(id); if (el && el.value !== v) el.value = v; };
  // Run style lives in the `run` mark's CSS string at the selection head.
  const $h = view.state.selection.$head;
  const runMark = $h.marks().concat(view.state.storedMarks || []).find((m) => m.type === schema.marks.run);
  const style = runMark ? runMark.attrs.style : "";
  const font = (style.match(/font-family:\s*"?([^",;]+)/) || [])[1] || "";
  const size = (style.match(/font-size:\s*([\d.]+)pt/) || [])[1];
  const color = (style.match(/(?:^|;)\s*color:\s*#?([0-9a-fA-F]{6})/) || [])[1];
  const fontEl = document.getElementById("fmt-font");
  setVal("fmt-font", fontEl && [...fontEl.options].some((o) => o.value === font) ? font : "");
  setVal("fmt-size", size ? String(Math.round(Number(size))) : "");
  if (color) setVal("fmt-color", "#" + color);
  // List + alignment active state.
  const blocks = selectionBlocks();
  const isList = blocks.length > 0 && blocks.every((id) => state.listInfo.has(id));
  document.getElementById("fmt-bullet")?.setAttribute("aria-pressed", String(isList));
  const blk = blockAt($h);
  const al = (blk && blk.attrs.align) || "Left";
  const isAlign = { Left: ["Left", "Start"], Center: ["Center"], Right: ["Right", "End"], Justify: ["Justify", "Both", "Distribute"] };
  for (const [id, key] of [["fmt-align-left", "Left"], ["fmt-align-center", "Center"], ["fmt-align-right", "Right"], ["fmt-align-justify", "Justify"]]) {
    document.getElementById(id)?.setAttribute("aria-pressed", String(isAlign[key].includes(al)));
  }
}

// ─── value-bearing formatting (color/font/highlight, align/indent) ──────────────
// These ride the engine's typed SetFormat / SetParaFormat ops — the v4 `replace`
// content can't carry color/font. We fire the op and reconcile the affected
// block(s) from a fresh /rich read (correct over optimistic, since SetFormat
// addresses a span by text and the server is local).

// The paragraph block + its committed guard at the selection head.
function blockAt($pos) {
  for (let d = $pos.depth; d > 0; d--) {
    const node = $pos.node(d);
    if (node.type === schema.nodes.paragraph || node.type === schema.nodes.heading) return node;
  }
  return $pos.parent;
}
function selectionBlocks() {
  const { $from, $to } = view.state.selection;
  // Depth-guard: a whole-doc selection (Ctrl+A) or a node selection has depth 0,
  // where $from.before()/$to.after() would throw "no position before the
  // top-level node". Clamp to the document bounds instead.
  const from = $from.depth ? $from.before($from.depth) : 0;
  const to = $to.depth ? $to.after($to.depth) : view.state.doc.content.size;
  const ids = new Set();
  view.state.doc.nodesBetween(from, to, (node) => {
    if ((node.type === schema.nodes.paragraph || node.type === schema.nodes.heading) && node.attrs.blockId) ids.add(node.attrs.blockId);
  });
  if (!ids.size) { const b = blockAt($from); if (b && b.attrs.blockId) ids.add(b.attrs.blockId); }
  return [...ids];
}
function guardOf(id) { return state.guards.get(id) || state.committed.get(id)?.guard || null; }
function txMode() { return state.mode === "suggesting" ? "tracked_change" : "direct"; }

// Apply a run-formatting op (color/highlight/font/size) to the selected span.
function applyRunFormat(props) {
  if (!view) return;
  const { from, to, empty } = view.state.selection;
  if (empty) { setStatus("Select some text first, then apply a font or color.", "error"); return; }
  // set_format locates its target by finding `expect` text WITHIN A SINGLE RUN,
  // so a selection that crosses a formatting boundary (e.g. a bold word) can't be
  // sent as one span. Split the selection at run boundaries and emit one op per
  // run, each tagged with that run's OWN containing block (a Ctrl+A selection
  // spans many blocks — tagging all ops with the first block would 422), and
  // carrying each run's EXISTING marks so changing only the font/color preserves
  // its bold/italic/etc. (No semantic_hash: the per-run expect is the
  // precondition, and a shared guard would go stale across ops.)
  const ops = [];
  const blockIds = new Set();
  view.state.doc.nodesBetween(from, to, (node, pos) => {
    if (!node.isText || !node.text) return;
    const text = node.text.slice(Math.max(from, pos) - pos, Math.min(to, pos + node.nodeSize) - pos);
    if (!text) return;
    const block = blockAt(view.state.doc.resolve(pos));
    const id = block && block.attrs.blockId;
    if (!id) return;
    blockIds.add(id);
    const marks = [];
    for (const m of node.marks) { const t = V4_MARK[m.type.name]; if (t) marks.push({ type: t }); }
    ops.push({ op: "set_format", target: id, expect: text, marks, ...props });
  });
  if (ops.length) {
    // Optimistic: fold the font/color/highlight change into the selected runs'
    // `run` mark NOW, so it shows instantly; the engine confirms it via the
    // set_format rPrChange in the background. (blockSig ignores the run mark, so
    // this doesn't double-fire an autosave commit.)
    const otr = view.state.tr;
    view.state.doc.nodesBetween(from, to, (node, pos) => {
      if (!node.isText || !node.text) return;
      const a = Math.max(from, pos), b = Math.min(to, pos + node.nodeSize);
      if (a >= b) return;
      const cur = node.marks.find((m) => m.type === schema.marks.run);
      const style = applyRunPropsToStyle(cur ? cur.attrs.style : "", props);
      otr.removeMark(a, b, schema.marks.run);
      if (style) otr.addMark(a, b, schema.marks.run.create({ style }));
    });
    otr.setMeta("addToHistory", false);
    view.dispatch(otr);
    runFormatJob(ops, [...blockIds]);
  }
}

// Author a hyperlink on the selection: toggle off if already linked, else prompt
// for a URL and apply the `link` mark. On commit the linked run serializes as the
// engine's `hyperlink` content type (see contentForCommit), so it round-trips.
// `urlForTest` bypasses the prompt for automated checks.
// An inline popover that reads like part of the app (vs the jarring native
// window.prompt). Anchors to the current selection, resolves on OK/Enter with the
// value or null on Cancel/Esc. `multiline` uses a textarea (Ctrl/Cmd+Enter = OK).
let popoverResolve = null;
function promptInline({ label, initial = "", multiline = false }) {
  return new Promise((resolve) => {
    closePopover(null); // dismiss any open one first
    popoverResolve = resolve;
    document.getElementById("popover-label").textContent = label;
    const input = document.getElementById("popover-input");
    const ta = document.getElementById("popover-textarea");
    input.classList.toggle("hidden", multiline);
    ta.classList.toggle("hidden", !multiline);
    const field = multiline ? ta : input;
    field.value = initial;
    const pop = document.getElementById("inline-popover");
    pop.classList.remove("hidden");
    const sel = window.getSelection();
    let rect = sel && sel.rangeCount ? sel.getRangeAt(0).getBoundingClientRect() : null;
    if (!rect || (!rect.width && !rect.height)) rect = document.getElementById("editor").getBoundingClientRect();
    pop.style.top = `${window.scrollY + rect.bottom + 6}px`;
    pop.style.left = `${window.scrollX + rect.left}px`;
    field.focus(); field.select && field.select();
  });
}
function closePopover(value) {
  document.getElementById("inline-popover").classList.add("hidden");
  const r = popoverResolve; popoverResolve = null;
  if (r) r(value);
}

// The link mark touching the current selection/caret (existing link), if any.
function linkMarkAt() {
  const { $from, from, to } = view.state.selection;
  const atCaret = ($from.marks() || []).find((m) => m.type === schema.marks.link);
  if (atCaret) return atCaret;
  let found = null;
  view.state.doc.nodesBetween(from, Math.max(to, from + 1), (node) => {
    if (found) return false;
    const lm = (node.marks || []).find((m) => m.type === schema.marks.link);
    if (lm) found = lm;
  });
  return found;
}

async function applyLink() {
  if (!view) return;
  const existing = linkMarkAt();
  // An EXISTING (committed) hyperlink carries the engine's opaque id — editing it
  // changes the link's TARGET via set_attr (the engine re-resolves the relation),
  // rather than re-minting it. A link the user just authored (no opaqueId) toggles
  // off as before.
  if (existing && existing.attrs.opaqueId) {
    const id = selectionBlocks()[0];
    const current = existing.attrs.href || "";
    const url = await promptInline({ label: "Link URL", initial: current });
    view.focus();
    if (!url || url === current || !id) return;
    runFormatJob([{ op: "set_attr", target: existing.attrs.opaqueId, attrs: { href: url }, expect_href: current }], [id]);
    return;
  }
  const { from, to, empty } = view.state.selection;
  if (existing) { view.dispatch(view.state.tr.removeMark(from, to, schema.marks.link)); view.focus(); return; }
  if (empty) { setStatus("Select text to turn into a link.", "error"); return; }
  const url = await promptInline({ label: "Link URL", initial: "https://" });
  if (!url) { view.focus(); return; }
  view.dispatch(view.state.tr.addMark(from, to, schema.marks.link.create({ href: url })));
  view.focus();
}

// Insert an image (from a picked file) into the current block via the engine's
// insert_image op. Reads the file as base64, scales to a sane display width, and
// converts px → EMU (914400 EMU = 1in = 96px → ×9525).
async function insertImageFile(file) {
  if (!view || !file) return;
  const dataUrl = await new Promise((res) => { const r = new FileReader(); r.onload = () => res(r.result); r.readAsDataURL(file); });
  const comma = dataUrl.indexOf(",");
  const meta = dataUrl.slice(0, comma), b64 = dataUrl.slice(comma + 1);
  const format = (meta.match(/image\/([a-z0-9]+)/i) || [, "png"])[1].toLowerCase();
  const dims = await new Promise((res) => { const im = new Image(); im.onload = () => res({ w: im.naturalWidth || 200, h: im.naturalHeight || 200 }); im.onerror = () => res({ w: 200, h: 200 }); im.src = dataUrl; });
  const maxW = 400, scale = dims.w > maxW ? maxW / dims.w : 1;
  const cx = Math.round(dims.w * scale * 9525), cy = Math.round(dims.h * scale * 9525);
  const id = selectionBlocks()[0];
  if (!id) { setStatus("Place the cursor in a paragraph, then insert an image.", "error"); return; }
  const op = { op: "insert_image", target: id, bytes_base64: b64, format, cx, cy, alt_text: file.name };
  const g = guardOf(id); if (g) op.semantic_hash = g;
  runFormatJob([op], [id]);
}

// ─── comments (redline review) ──────────────────────────────────────────────
// Author a comment on the selected text → comment_create {target, expect, body,
// author}. The engine brackets the span with CommentRangeStart…CommentReference
// markers (surfaced as zero-width CommentReference opaque segments carrying the
// comment's reference_id); buildBlock turns the text between them into a `comment`
// mark, so the span highlights and links to its sidebar card. After the op we
// re-read /rich fully so the sidebar + highlights stay read-consistent.

// Flatten a CommentPayload's inline segments to its body text.
function commentBodyText(c) {
  return (c.segments || []).map((s) => (s.Unchanged || s.Inserted || s.Deleted || {}).text || "").join("");
}

// Author a comment on the current selection. `bodyForTest` bypasses the prompt.
async function addComment() {
  if (!view) return;
  const { from, to, empty } = view.state.selection;
  if (empty) { setStatus("Select the text you want to comment on first.", "error"); return; }
  const text = view.state.doc.textBetween(from, to, "", "");
  if (!text) return;
  const id = selectionBlocks()[0];
  if (!id) return;
  const body = await promptInline({ label: "Comment", multiline: true });
  if (!body || !body.trim()) { view.focus(); return; }
  const author = document.getElementById("author").value.trim() || "Word demo";
  const op = { op: "comment_create", target: id, expect: text, body: body.trim(), author };
  const g = guardOf(id); if (g) op.semantic_hash = g;
  const tempId = `pending-${commentSeq++}`;
  const optimistic = () => {
    const tr = view.state.tr.addMark(from, to, schema.marks.comment.create({ id: tempId }));
    tr.setMeta("addToHistory", false);
    view.dispatch(tr);
    state.comments = [...(state.comments || []), { id: tempId, author, date: null, resolved: false, parent_para_id: null, segments: [{ Unchanged: { text: body.trim() } }] }];
    renderComments();
  };
  runCommentJob([op], `comment on “${text.slice(0, 24)}${text.length > 24 ? "…" : ""}”`, optimistic);
}

// Resolve / reopen a comment → comment_resolve {comment_id, done}.
function resolveComment(commentId, done) {
  const optimistic = () => {
    state.comments = (state.comments || []).map((c) => (c.id === commentId ? { ...c, resolved: done } : c));
    renderComments();
  };
  runCommentJob([{ op: "comment_resolve", comment_id: commentId, done }], done ? "resolve comment" : "reopen comment", optimistic);
}

// Reply to a comment → comment_reply {parent_comment_id, body, author}.
function replyToComment(commentId, body) {
  if (!body || !body.trim()) return;
  const author = document.getElementById("author").value.trim() || "Word demo";
  const optimistic = () => {
    state.comments = [...(state.comments || []), { id: `pending-${commentSeq++}`, author, date: null, resolved: false, parent_para_id: "pending", segments: [{ Unchanged: { text: body.trim() } }] }];
    renderComments();
  };
  runCommentJob([{ op: "comment_reply", parent_comment_id: commentId, body: body.trim(), author }], "reply to comment", optimistic);
}

// Apply a comment transaction, then FULLY re-read /rich and re-render. Comment ops
// rewrite block segments (anchor markers) and the comment store, so a whole-doc
// refresh is the read-consistent choice: highlights + sidebar update together.
let commentSeq = 0;

async function runCommentJob(ops, summary, optimistic) {
  if (!state.docId) return;
  const author = document.getElementById("author").value.trim() || "Word demo";
  // Optimistic: show the highlight/card locally NOW (the `optimistic` callback),
  // then replicate in the background — no "Syncing…" interruption, no whole-doc
  // re-render. Comments are annotations, applied directly (not redline-gated).
  if (optimistic) optimistic();
  const transaction = { ops, revision: { author }, summary, materialization_mode: "direct" };
  try {
    await api.apply(state.docId, transaction);
    const { blocks, comments } = await api.rich(state.docId);
    state.comments = comments || [];
    // Targeted reconcile: patch only the blocks that actually changed (the
    // commented span gains its range markers; the provisional id → the real one),
    // so the caret and scroll position are preserved.
    patchNodes([...state.renderedIds], (id) => rebuildNodeFromRich(blocks, id));
    rememberCommitted(blocks);
    renderComments();
  } catch (err) {
    // Replicate failed — revert the optimistic change to server truth.
    setStatus(friendlyError(err), "error");
    await reloadDoc();
  }
}

// Render the comments sidebar from state.comments. Top-level comments are cards;
// replies (parent_para_id set) nest under their parent. A click scrolls to the
// span and highlights it; Resolve/Reopen toggles w15:done; a reply box threads.
function renderComments() {
  const list = document.getElementById("comments-list");
  if (!list) return;
  list.innerHTML = "";
  const all = state.comments || [];
  const top = all.filter((c) => !c.parent_para_id);
  if (!top.length) {
    list.innerHTML = `<p class="empty">No comments yet. Select text and click 💬 to add one.</p>`;
    updateRailLayout();
    return;
  }
  // Replies link to their parent by w14:paraId; we don't have the parent's paraId
  // in the payload, so render replies inline by author order after top-level cards
  // they follow. (Reply threading by paraId is an engine detail; see report.)
  const replies = all.filter((c) => c.parent_para_id);
  for (const c of top) {
    list.appendChild(commentCard(c, replies));
  }
  updateRailLayout();
}

// ─── suggestions (tracked-change review rail) ───────────────────────────────

// Fetch the pending tracked changes and repaint the review rail. Called on open
// and after every reconcile, so a tracked edit shows up as a reviewable card.
async function loadRevisions() {
  if (!state.docId) return;
  try { state.suggestions = await api.revisions(state.docId); }
  catch { state.suggestions = []; }
  renderSuggestions();
  updateRailLayout();
}

// Group the engine's flat revision list into the logical edits a reviewer thinks
// in. A replace is a delete immediately followed by an insert at the same spot
// (same block + author + apply date) — the engine lists those as two rows and
// leaves grouping to the client, so we fold them into ONE "old → new" item that
// resolves both ids together. Everything else stays a standalone insert/delete.
function groupRevisions(revs) {
  const out = [];
  for (let i = 0; i < revs.length; i++) {
    const r = revs[i], n = revs[i + 1];
    if (r.kind === "delete" && n && n.kind === "insert"
        && n.block_id === r.block_id && n.author === r.author && n.date === r.date) {
      out.push({ kind: "replace", block_id: r.block_id, author: r.author,
                 oldText: r.excerpt, newText: n.excerpt, revision_ids: [r.revision_id, n.revision_id] });
      i++; // consumed the paired insert
    } else {
      out.push({ kind: r.kind, block_id: r.block_id, author: r.author,
                 excerpt: r.excerpt, revision_ids: [r.revision_id] });
    }
  }
  return out;
}

function renderSuggestions() {
  const list = document.getElementById("suggestions-list");
  const section = document.getElementById("suggestions");
  if (!list || !section) return;
  const groups = groupRevisions(state.suggestions || []);
  section.classList.toggle("hidden", groups.length === 0);
  list.innerHTML = "";
  for (const g of groups) list.appendChild(suggestionCard(g));
}

const clipExcerpt = (t) => { t = (t || "").trim(); return t.length > 60 ? t.slice(0, 60) + "…" : t; };

function suggestionCard(g) {
  const card = document.createElement("div");
  card.className = "suggest-card";
  const verb = { insert: "Insert", delete: "Delete", replace: "Replace" }[g.kind] || "Format";
  const head = document.createElement("div");
  head.className = "suggest-head";
  const who = document.createElement("span"); who.className = "suggest-author"; who.textContent = g.author || "Someone";
  const kind = document.createElement("span"); kind.className = `suggest-kind k-${g.kind}`; kind.textContent = verb;
  head.append(who, kind);
  const body = document.createElement("div");
  body.className = "suggest-body";
  if (g.kind === "replace") {
    // One change, shown as "old → new" (not a separate delete + insert).
    const oldS = document.createElement("span"); oldS.className = "suggest-old"; oldS.textContent = clipExcerpt(g.oldText);
    const arrow = document.createElement("span"); arrow.className = "suggest-arrow"; arrow.textContent = " → ";
    const newS = document.createElement("span"); newS.className = "suggest-new"; newS.textContent = clipExcerpt(g.newText);
    body.append(oldS, arrow, newS);
  } else {
    const ex = clipExcerpt(g.excerpt);
    body.textContent = ex ? `“${ex}”` : "(formatting change)";
  }
  const actions = document.createElement("div");
  actions.className = "suggest-actions";
  const accept = document.createElement("button"); accept.className = "accept"; accept.textContent = "✓ Accept";
  accept.addEventListener("click", () => resolveSuggestion(g.revision_ids, g.block_id, "accept"));
  const reject = document.createElement("button"); reject.className = "decline"; reject.textContent = "✕ Reject";
  reject.addEventListener("click", () => resolveSuggestion(g.revision_ids, g.block_id, "reject"));
  actions.append(accept, reject);
  card.append(head, body, actions);
  return card;
}

// Apply an accept/reject LOCALLY (preemptively) by transforming this revision's
// redline spans, then replicate to the backend. accept: keep inserted text
// (drop the ins mark), drop deleted text. reject: drop inserted text, restore
// deleted text (drop the del mark). Returns true if anything was transformed.
function applyResolveOptimistic(revisionIds, action) {
  if (!view) return false;
  const ids = new Set(revisionIds);
  const dels = []; // ranges of text to remove
  const unmarks = []; // ranges to strip an ins/del mark from (kept text)
  view.state.doc.descendants((node, pos) => {
    if (!node.isText) return;
    const ins = node.marks.find((m) => m.type === schema.marks.ins && ids.has(m.attrs.rev));
    const del = node.marks.find((m) => m.type === schema.marks.del && ids.has(m.attrs.rev));
    const range = { from: pos, to: pos + node.nodeSize };
    if (ins) (action === "accept" ? unmarks.push({ ...range, mark: schema.marks.ins }) : dels.push(range));
    else if (del) (action === "accept" ? dels.push(range) : unmarks.push({ ...range, mark: schema.marks.del }));
  });
  if (!dels.length && !unmarks.length) return false;
  const tr = view.state.tr;
  for (const u of unmarks) tr.removeMark(u.from, u.to, u.mark);
  // delete high→low so earlier positions stay valid
  for (const d of dels.sort((a, b) => b.from - a.from)) tr.delete(d.from, d.to);
  tr.setMeta("addToHistory", false);
  view.dispatch(tr);
  return true;
}

// `revisionIds` is the group's ids — a replace resolves its delete + insert
// together (both accepted or both rejected), so "old → new" never splits.
async function resolveSuggestion(revisionIds, blockId, action) {
  if (!state.docId) return;
  const ids = Array.isArray(revisionIds) ? revisionIds : [revisionIds];
  // Optimistic: apply locally + drop the card immediately, so it feels instant.
  applyResolveOptimistic(ids, action);
  state.suggestions = (state.suggestions || []).filter((s) => !ids.includes(s.revision_id));
  renderSuggestions();
  updateRailLayout();
  try {
    await api.resolve(state.docId, ids, action);
    // Replicate succeeded — sync metadata (guards/baselines) and re-pin the
    // affected block to authoritative server state (corrects any optimistic drift
    // without a full re-render), then refresh the worklist.
    const { blocks } = await api.rich(state.docId);
    if (blockId) patchNodes([blockId], (id) => rebuildNodeFromRich(blocks, id));
    rememberCommitted(blocks);
    loadRevisions();
  } catch (err) {
    // Replicate failed — revert to server truth.
    setStatus(friendlyError(err), "error");
    await reloadDoc();
  }
}

// Re-read the CURRENT doc (after a server-side change like resolve) and repaint.
async function reloadDoc() {
  const { blocks, section, headers, footers, comments } = await api.rich(state.docId);
  state.comments = comments || [];
  applyPageGeometry(section);
  render(blocks);
  renderHeaderFooterBands(headers, footers);
  renderComments();
  await loadRevisions();
}

// Show the review rail only when it has content (suggestions or comments); when
// empty, the page sits in a centered single column (Google-Docs style).
function updateRailLayout() {
  const hasComments = (state.comments || []).some((c) => !c.parent_para_id);
  const hasRail = (state.suggestions || []).length > 0 || hasComments;
  const main = document.querySelector("main");
  if (main) { main.classList.toggle("split", hasRail); main.classList.toggle("single", !hasRail); }
  document.getElementById("comments")?.classList.toggle("hidden", !hasRail);
}

function commentCard(c, replies) {
  const card = document.createElement("div");
  card.className = `comment-card${c.resolved ? " resolved" : ""}${state.activeComment === c.id ? " active" : ""}`;
  card.dataset.commentId = c.id;
  const date = c.date ? new Date(c.date).toLocaleString() : "";
  const meta = document.createElement("div");
  meta.className = "meta";
  meta.innerHTML = `<span class="author"></span><span class="date"></span>${c.resolved ? `<span class="badge">Resolved</span>` : ""}`;
  meta.querySelector(".author").textContent = c.author || "Anonymous";
  meta.querySelector(".date").textContent = date;
  card.appendChild(meta);
  const body = document.createElement("div");
  body.className = "body";
  body.textContent = commentBodyText(c);
  card.appendChild(body);
  // Replies addressed to this comment's id (best-effort: payload exposes
  // parent_para_id, not parent comment id; replies still render as a flat thread).
  for (const r of replies) {
    const rep = document.createElement("div");
    rep.className = "reply";
    rep.textContent = `${r.author || "Anonymous"}: ${commentBodyText(r)}`;
    card.appendChild(rep);
  }
  const actions = document.createElement("div");
  actions.className = "actions";
  const resolveBtn = document.createElement("button");
  resolveBtn.textContent = c.resolved ? "Reopen" : "Resolve";
  resolveBtn.addEventListener("click", (e) => { e.stopPropagation(); resolveComment(c.id, !c.resolved); });
  actions.appendChild(resolveBtn);
  const replyBox = document.createElement("input");
  replyBox.className = "reply-box";
  replyBox.placeholder = "Reply…";
  replyBox.addEventListener("click", (e) => e.stopPropagation());
  replyBox.addEventListener("keydown", (e) => { if (e.key === "Enter") { replyToComment(c.id, replyBox.value); replyBox.value = ""; } });
  actions.appendChild(replyBox);
  card.appendChild(actions);
  card.addEventListener("click", () => focusComment(c.id));
  return card;
}

// Click a card → highlight its span, scroll it into view, and mark the card active.
function focusComment(commentId) {
  state.activeComment = commentId;
  // Drive the span highlight through the decoration plugin (NOT a DOM classList
  // mutation, which makes PM re-parse and drop the comment mark, wiping all
  // highlights) — dispatch a meta-only transaction to recompute decorations.
  if (view) view.dispatch(view.state.tr.setMeta("activeComment", commentId));
  // The card class is on a non-PM element, so DOM mutation there is safe.
  document.querySelectorAll(".comment-card.active").forEach((el) => el.classList.remove("active"));
  const card = document.querySelector(`.comment-card[data-comment-id="${CSS.escape(commentId)}"]`);
  if (card) card.classList.add("active");
  // Scroll the (decorated) span into view — reading the DOM is fine.
  const span = document.querySelector(`.pm-comment[data-comment-id="${CSS.escape(commentId)}"]`);
  if (span) span.scrollIntoView({ behavior: "smooth", block: "center" });
}

// The image node currently selected (a NodeSelection on an image atom), or null.
function selectedImage() {
  const sel = view && view.state.selection;
  // A tracked-deleted (struck) image isn't resizable — hide the bar / disable resize
  // for it (you're proposing to remove it, not reshape it).
  if (sel instanceof NodeSelection && sel.node.type === schema.nodes.image
    && !sel.node.marks.some((m) => m.type === schema.marks.del)) return sel.node;
  return null;
}

// Resize the currently-selected image to `newWidthPx`, preserving its aspect
// ratio, via the engine's set_image_attrs op (px → EMU, ×9525). The drawing is
// targeted by the identity threaded onto the node (blockId = hosting paragraph,
// drawingId = the Drawing opaque's id) and guarded by the drawing's own
// content_hash (drawingGuard) — NOT the block guard, which is a different hash
// the engine would reject. After apply we reconcile the block from /rich, so the
// <img> re-renders at the engine's authoritative new size.
function resizeImageTo(newWidthPx) {
  const node = selectedImage();
  if (!node) return;
  const { drawingId, blockId, drawingGuard, width, height } = node.attrs;
  if (!drawingId || !blockId) { setStatus("This image can't be resized — it's missing its drawing identity.", "error"); return; }
  const w = Math.round(newWidthPx);
  if (!(w > 0)) { setStatus("Width must be a positive number of pixels.", "error"); return; }
  // Preserve aspect ratio from the current display dimensions; if height is
  // unknown, fall back to a square (cy = cx) rather than guessing.
  const ratio = width && height ? height / width : 1;
  const cx = w * 9525, cy = Math.round(w * ratio) * 9525;
  const op = { op: "set_image_attrs", target: blockId, drawing_id: drawingId, resize: { cx, cy } };
  if (drawingGuard) op.semantic_hash = drawingGuard;
  // Optimistic: snap the image to the new size locally; the engine confirms the
  // set_image_attrs in the background.
  const sel = view.state.selection;
  if (sel instanceof NodeSelection && sel.node === node) {
    const otr = view.state.tr.setNodeMarkup(sel.from, undefined, { ...node.attrs, width: w, height: Math.round(w * ratio) });
    otr.setMeta("addToHistory", false);
    view.dispatch(otr);
  }
  runFormatJob([op], [blockId]);
  hideImageBar();
}

// ─── image resize bar ───────────────────────────────────────────────────────
// A small floating control shown when an image atom is selected (click it).
// Width input + −/+ step buttons emit set_image_attrs; height follows the
// aspect ratio. The bar lives in #image-bar (see index.html) and is positioned
// over the selected <img>.
function imageBar() { return document.getElementById("image-bar"); }
function imageWidthInput() { return document.getElementById("image-width"); }

function showImageBar(node) {
  const bar = imageBar();
  if (!bar) return;
  imageWidthInput().value = node.attrs.width || "";
  bar.classList.remove("hidden");
  // Anchor the bar just above the selected image's DOM node.
  const dom = view.nodeDOM(view.state.selection.from);
  const img = dom && dom.nodeName === "IMG" ? dom : dom && dom.querySelector && dom.querySelector("img");
  if (img && img.getBoundingClientRect) {
    const r = img.getBoundingClientRect();
    bar.style.top = `${window.scrollY + r.top - bar.offsetHeight - 6}px`;
    bar.style.left = `${window.scrollX + r.left}px`;
  }
}
function hideImageBar() { const bar = imageBar(); if (bar) bar.classList.add("hidden"); }

// Reflect the current selection: show the resize bar for a selected image, hide
// it otherwise. Called from dispatchTransaction.
function syncImageBar() {
  const node = selectedImage();
  if (node) showImageBar(node); else hideImageBar();
}

// Step the selected image's width by a delta (px), preserving aspect ratio.
function stepImageWidth(deltaPx) {
  const node = selectedImage();
  if (!node) return;
  resizeImageTo(Math.max(16, (node.attrs.width || 100) + deltaPx));
}

// Apply a paragraph-formatting op (align/indent) to every selected block.
function applyParaFormat(patch) {
  if (!view) return;
  const ids = selectionBlocks();
  if (!ids.length) return;
  // The engine's ST_Jc accepts "both"/"distribute", not "justify" (the CSS name);
  // map it so the Justify button works instead of 400-ing with a raw AdapterError.
  if (patch.align === "justify") patch = { ...patch, align: "both" };
  const ops = ids.map((id) => { const op = { op: "set_para_format", target: id, ...patch }; const g = guardOf(id); if (g) op.semantic_hash = g; return op; });
  // Optimistic: set the alignment on the affected blocks NOW (the node `align`
  // attr → text-align), so it shows instantly; the engine confirms in the
  // background. The attr value matches /rich's (capitalized Alignment), so the
  // reconcile is seamless.
  if (patch.align) {
    const attr = patch.align.charAt(0).toUpperCase() + patch.align.slice(1);
    const otr = view.state.tr;
    let changed = false;
    view.state.doc.forEach((node, offset) => {
      if (ids.includes(node.attrs.blockId)
        && (node.type === schema.nodes.paragraph || node.type === schema.nodes.heading)
        && node.attrs.align !== attr) {
        otr.setNodeMarkup(offset, undefined, { ...node.attrs, align: attr });
        changed = true;
      }
    });
    if (changed) { otr.setMeta("addToHistory", false); view.dispatch(otr); }
  }
  runFormatJob(ops, ids);
}

// Indent/outdent: list-aware (like Word's Tab / Shift+Tab). A list item changes
// its LIST LEVEL (set_numbering indent/outdent); a plain paragraph shifts its
// left margin by ±360 twips (0.25in) via set_para_format.
function adjustIndent(deltaTwips) {
  if (!view) return;
  const ids = selectionBlocks();
  const ops = ids.map((id) => {
    let op;
    if (state.listInfo.has(id)) {
      op = { op: "set_numbering", target: id, change: { kind: deltaTwips > 0 ? "indent" : "outdent" } };
    } else {
      const left = Math.max(0, (state.indentLeft.get(id) || 0) + deltaTwips);
      op = { op: "set_para_format", target: id, indent: { left } };
    }
    const g = guardOf(id); if (g) op.semantic_hash = g;
    return op;
  });
  runFormatJob(ops, ids);
}

// Toggle a bullet list on the selected paragraphs: a list item becomes plain
// (Remove); a plain paragraph joins the document's existing bullet list (SetList
// at its num_id). Creating a NEW list definition from scratch isn't supported by
// the engine, so toggling a plain paragraph on requires an existing bullet list.
function toggleBulletList() {
  if (!view) return;
  const ids = selectionBlocks();
  const anyPlain = ids.some((id) => !state.listInfo.has(id));
  if (anyPlain && state.bulletNumId == null) {
    setStatus("This document has no bullet list to join — creating a new list definition isn't supported yet.", "error");
    return;
  }
  const ops = ids.map((id) => {
    const change = state.listInfo.has(id)
      ? { kind: "remove" }
      : { kind: "set_list", num_id: state.bulletNumId, ilvl: 0, synthesized_text: "•", is_bullet: true };
    const op = { op: "set_numbering", target: id, change };
    const g = guardOf(id); if (g) op.semantic_hash = g;
    return op;
  });
  runFormatJob(ops, ids);
}

// Emit a table structural op (SetCellText, InsertRow, …) and reconcile the
// table from /rich. The table guard comes from the node (tables aren't in
// state.committed). Fired by the table NodeView's editable cells.
// Fire a formatting transaction and reconcile the affected blocks from /rich.
function runFormatJob(ops, blockIds) {
  const author = document.getElementById("author").value.trim() || "Word demo";
  for (const id of blockIds) state.pending.set(id, true);
  state.queue.push({
    docId: state.docId, generation: state.generation,
    blockIds,
    baseline: new Map(blockIds.map((id) => [id, (node) => node])), // rollback: keep node (no optimistic change)
    tracked: state.mode === "suggesting",
    author,
    transaction: { ops, revision: { author }, summary: `format ${blockIds.length} block${blockIds.length === 1 ? "" : "s"}`, materialization_mode: txMode() },
  });
  refreshDirty();
  syncStatus();
  drain();
}

// ─── node builders for optimistic / rollback / reconcile ────────────────────────

function blockNode(ref, content, editable) {
  const attrs = { blockId: ref.attrs.blockId, guard: ref.attrs.guard, editable, align: ref.attrs.align, bstyle: ref.attrs.bstyle, numbering: ref.attrs.numbering };
  if (ref.type === schema.nodes.heading) return schema.nodes.heading.create({ ...attrs, level: ref.attrs.level }, content);
  return schema.nodes.paragraph.create(attrs, content);
}
const plainNode = (ref, text) => blockNode(ref, text ? [schema.text(text)] : [], true);
// Optimistic redline for a committed text edit. SURGICAL: keep the common
// leading/trailing text as normal and redline only the changed middle, so it
// matches the engine's word-diff and the reconcile from /rich is visually
// seamless — instead of flashing a whole-paragraph delete+insert that the server
// then corrects to a minimal change.
function redlineNode(ref, oldText, newText, author) {
  const { p, s } = surgicalDiffBounds(oldText, newText);
  const prefix = oldText.slice(0, p);
  const oldMid = oldText.slice(p, oldText.length - s);
  const newMid = newText.slice(p, newText.length - s);
  const suffix = oldText.slice(oldText.length - s);
  const content = [];
  if (prefix) content.push(schema.text(prefix));
  if (oldMid) content.push(schema.text(oldMid, [schema.marks.del.create({ author })]));
  if (newMid) content.push(schema.text(newMid, [schema.marks.ins.create({ author })]));
  if (suffix) content.push(schema.text(suffix));
  return blockNode(ref, content, false);
}

// Does this block already carry a redline (ins/del marks)? When the live
// track-changes plugin has marked an edit as-you-type, commit() must NOT re-run
// redlineNode over it — node.textContent now includes struck text, so a second
// surgical pass would double-count. Leave the provisional redline in place; the
// /rich reconcile swaps it for the authoritative one.
function hasRedlineMarks(node) {
  let found = false;
  node.descendants((n) => {
    if (n.isText && n.marks.some((m) => m.type === schema.marks.ins || m.type === schema.marks.del)) found = true;
  });
  return found;
}
function visiblyDiffers(a, b) {
  if (a.type !== b.type) return true;
  // (Tables are now real content nodes — they compare by content like any block.
  // Paragraphs must NOT compare guard: it changes on every commit, which would
  // force a re-render and reset the caret in the block you just edited.)
  return a.attrs.level !== b.attrs.level || a.attrs.editable !== b.attrs.editable
    || a.attrs.align !== b.attrs.align || a.attrs.bstyle !== b.attrs.bstyle || a.attrs.numbering !== b.attrs.numbering || !a.content.eq(b.content);
}
function patchNodes(ids, nodeFor) {
  const want = new Set(ids);
  const patches = [];
  // Descend (not just top-level): a cell paragraph nested in table > row > cell is
  // matched + patched in place by its blockId, so a cell edit reconciles its own
  // paragraph node without rebuilding the whole table. Only BLOCK-level nodes are
  // reconcile targets — an inline image atom carries its hosting paragraph's blockId
  // (for resize targeting), so without this guard it would also match and get
  // replaced by the rebuilt paragraph, duplicating the image.
  view.state.doc.descendants((node, pos) => {
    if (!node.isBlock || !node.attrs || !want.has(node.attrs.blockId)) return;
    const next = nodeFor(node.attrs.blockId, node);
    if (next && visiblyDiffers(node, next)) patches.push({ from: pos, to: pos + node.nodeSize, node: next });
  });
  const tr = view.state.tr;
  for (const p of patches.sort((a, b) => b.from - a.from)) tr.replaceWith(p.from, p.to, p.node);
  tr.setMeta("addToHistory", false);
  view.dispatch(tr);
}

// ─── optimistic commit ──────────────────────────────────────────────────────────

function commit() {
  clearTimeout(state.autosaveTimer); state.autosaveTimer = null;
  if (!state.docId || !view) return;
  // Top-level dirty blocks + dirty cell paragraphs — both commit as plain replaces.
  const fresh = [...dirtyBlocks(), ...cellDirtyBlocks()];
  const struct = structuralOps();
  if (fresh.length === 0 && struct.count === 0) { refreshDirty(); return; }
  const tracked = state.mode === "suggesting";
  const author = document.getElementById("author").value.trim() || "Word demo";

  const baseline = new Map();
  for (const d of fresh) {
    baseline.set(d.blockId, (ref) => plainNode(ref, state.committed.get(d.blockId)?.text ?? ""));
    state.pending.set(d.blockId, true);
  }
  // Structural commits (insert/delete blocks) can't be optimistically redlined or
  // patched per-block — the block ids/structure change — so they full-resync from
  // /rich on confirm. Pure text/format edits keep the optimistic redline path.
  const structural = struct.count > 0;
  if (!structural) {
    const freshById = new Map(fresh.map((d) => [d.blockId, d]));
    patchNodes(fresh.map((d) => d.blockId), (id, node) => {
      if (!tracked) return null;
      // The live track-changes plugin already redlined this block as the user
      // typed — leave it untouched (re-running redlineNode would double-count,
      // since textContent now includes the struck text). Only blocks with no
      // live redline yet (e.g. a programmatic edit) need the commit-time redline.
      if (hasRedlineMarks(node)) return null;
      const oldText = state.committed.get(id)?.text ?? "";
      if (node.textContent === oldText) return null;
      return redlineNode(node, oldText, freshById.get(id).text, author);
    });
  }

  // Order: replaces (edit existing) → inserts (new blocks anchored on existing) →
  // deletes (remove merged-away blocks). Anchors reference kept blocks.
  const ops = [
    ...fresh.map((d) => ({ op: "replace", target: d.blockId, guard: d.guard, content: d.content })),
    ...struct.inserts,
    ...struct.deletes,
  ];
  state.queue.push({
    docId: state.docId, generation: state.generation,
    blockIds: fresh.map((d) => d.blockId), baseline, tracked, author, structural,
    transaction: {
      ops,
      revision: { author },
      summary: `edit ${ops.length} op${ops.length === 1 ? "" : "s"}`,
      materialization_mode: tracked ? "tracked_change" : "direct",
    },
  });
  refreshDirty();
  syncStatus();
  drain();
}

async function drain() {
  if (state.draining) return;
  state.draining = true;
  while (state.queue.length) {
    const job = state.queue[0];
    // Apply/read against the job's OWN doc, and drop the job if the document was
    // switched out from under it — so a slow commit never lands on another doc.
    const stale = () => job.generation !== state.generation || job.docId !== state.docId;
    try {
      await api.apply(job.docId, job.transaction);
      const { blocks } = await api.rich(job.docId);
      state.queue.shift();
      if (stale()) { for (const id of job.blockIds) state.pending.delete(id); refreshDirty(); }
      else confirmJob(job, blocks);
    } catch (err) {
      state.queue.shift();
      if (stale()) { for (const id of job.blockIds) state.pending.delete(id); refreshDirty(); }
      else await rollbackJob(job, err);
    }
  }
  state.draining = false;
  refreshDirty(); // reflect the now-idle state ("All changes saved")
}

function confirmJob(job, richBlocks) {
  for (const id of job.blockIds) state.pending.delete(id);
  if (job.structural) {
    // Structure changed (blocks inserted/deleted) — rebuild the whole doc from
    // /rich so new blocks get their real ids and order. Capture the caret first
    // and restore it to the nearest valid spot afterwards (the reconciled doc is
    // structurally what the user just produced, so the same absolute position
    // lands them where they were) + refocus, so a split/merge doesn't dump the
    // caret to the top of the document.
    const caretPos = view.state.selection.from;
    render(richBlocks);
    try {
      const pos = Math.min(caretPos, view.state.doc.content.size - 1);
      view.dispatch(view.state.tr.setSelection(TextSelection.near(view.state.doc.resolve(pos))));
    } catch { /* selection couldn't be restored — leave it at the default */ }
    view.focus();
  } else {
    patchNodes(job.blockIds, (id) => rebuildNodeFromRich(richBlocks, id));
    rememberCommitted(richBlocks);
    refocusEditor();
  }
  refreshDirty();
  syncStatus(job);
  loadRevisions();
}

// A button-triggered commit/rollback moved focus to #commit, which then disables
// (0 changes) and blurs to <body>, silently eating the next keystrokes. Reclaim
// focus to the editor — but ONLY when it was dropped to a non-editing place
// (body or the commit button), never stealing it from the author field, the
// inline popover, or another control the user is using.
function refocusEditor() {
  const a = document.activeElement;
  if (view && (a === document.body || a === null || a === document.getElementById("commit"))) view.focus();
}

async function rollbackJob(job, err) {
  for (const id of job.blockIds) state.pending.delete(id);
  // A rejected edit leaves the SERVER unchanged, so the authoritative revert is a
  // fresh /rich read: it reverts optimistic text redlines, structural inserts/
  // deletes, AND table-cell edits (which the local baseline can't reach). Fall
  // back to the local baseline only if the re-read itself fails.
  try {
    const { blocks } = await api.rich(state.docId);
    if (job.structural) render(blocks);
    else {
      patchNodes(job.blockIds, (id) => rebuildNodeFromRich(blocks, id));
      rememberCommitted(blocks);
    }
  } catch {
    patchNodes(job.blockIds, (id, node) => job.baseline.get(id)?.(node) ?? null);
  }
  refocusEditor();
  refreshDirty();
  setStatus(friendlyError(err), "error");
}

// Map known engine rejections to actionable messages instead of raw "step 0:"
// engine prose.
function friendlyError(err) {
  const code = err.code || "";
  const map = {
    StaleEdit: "That edit was based on an out-of-date version — your change was reverted; try again.",
    UnsupportedEdit: "That edit isn't supported here yet — reverted.",
    OpaqueDestroyed: "That edit would have removed a link or image, so it was reverted.",
  };
  return map[code] || (code ? `${code}: ${err.message}` : err.message);
}

// Save feedback lives in the "All changes saved / Saving…" indicator now, so the
// status line stays quiet for routine autosaves (it's reserved for open messages
// + errors). Just keep the indicator in sync.
function syncStatus() {
  refreshDirty();
}

// ─── flows ──────────────────────────────────────────────────────────────────

function setStatus(message, kind = "") {
  const el = document.getElementById("status");
  el.className = `status ${kind}`.trim();
  el.innerHTML = message;
}
function setExportEnabled(enabled) { document.getElementById("export").disabled = !enabled; }

// Count uncommitted local edits (text + structural) for the unsaved-changes guard.
function pendingChanges() {
  if (!view) return 0;
  return dirtyBlocks().length + structuralOps().count;
}

async function openDoc(source, name) {
  const dirty = pendingChanges();
  // Dismissing the confirm must return focus to the editor — otherwise focus is
  // stranded on the sample button, dropping the next keystrokes, and a stray
  // Enter re-fires the sample and discards the very edits the confirm protected.
  if (dirty > 0 && !window.confirm(`Discard ${dirty} uncommitted change${dirty === 1 ? "" : "s"}?`)) { view?.focus(); return; }
  // Bump the doc generation: any in-flight commit for the previous doc becomes
  // stale and is dropped on resolve, so its reconcile never lands on this doc.
  state.generation = (state.generation || 0) + 1;
  setStatus(`Parsing <code>${name}</code> …`);
  state.pending.clear();
  state.queue = [];
  const { doc_id } = await api.upload(source);
  state.docId = doc_id;
  const { blocks, section, headers, footers, comments } = await api.rich(doc_id);
  state.comments = comments || [];
  state.activeComment = null;
  applyPageGeometry(section);
  render(blocks);
  renderHeaderFooterBands(headers, footers);
  renderComments();
  await loadRevisions();
  setExportEnabled(true);
  setStatus(`Opened <code>${name}</code> — ${blocks.length} blocks, full formatting. Select text and use the toolbar, or edit + Commit.`, "ok");
}

function exportDocx() { if (state.docId) window.location.href = api.exportUrl(state.docId, "redline"); }

function setMode(mode) {
  state.mode = mode;
  document.getElementById("mode-suggesting").setAttribute("aria-pressed", String(mode === "suggesting"));
  document.getElementById("mode-editing").setAttribute("aria-pressed", String(mode === "editing"));
  document.getElementById("author-wrap").style.opacity = mode === "suggesting" ? "1" : "0.4";
}

const fail = (err) => setStatus(`${err.code ? err.code + ": " : ""}${err.message}`, "error");

document.getElementById("file").addEventListener("change", (e) => {
  const file = e.target.files[0];
  if (file) openDoc(file, file.name).catch(fail);
  e.target.value = "";
});
// Warn before leaving (tab close / reload) with uncommitted edits.
window.addEventListener("beforeunload", (e) => { if (pendingChanges() > 0) { e.preventDefault(); e.returnValue = ""; } });
renderSamples(document.getElementById("samples"), (s) => fetchSample(s.file).then((blob) => openDoc(blob, s.label)).catch(fail));
document.getElementById("fmt-bold").addEventListener("click", () => applyFmt("strong"));
document.getElementById("fmt-italic").addEventListener("click", () => applyFmt("em"));
document.getElementById("fmt-underline").addEventListener("click", () => applyFmt("underline"));
document.getElementById("fmt-strike").addEventListener("click", () => applyFmt("strike"));
// Run formatting → SetFormat (color is RRGGBB; highlight is a named ST_HighlightColor).
document.getElementById("fmt-color").addEventListener("input", (e) => applyRunFormat({ color: e.target.value.replace("#", "").toUpperCase() }));
document.getElementById("fmt-highlight").addEventListener("change", (e) => { if (e.target.value) applyRunFormat({ highlight: e.target.value }); e.target.value = ""; });
document.getElementById("fmt-font").addEventListener("change", (e) => { if (e.target.value) applyRunFormat({ font_family: e.target.value }); });
document.getElementById("fmt-size").addEventListener("change", (e) => { if (e.target.value) applyRunFormat({ font_size_half_points: Math.round(Number(e.target.value) * 2) }); });
// Paragraph formatting → SetParaFormat.
document.getElementById("fmt-align-left").addEventListener("click", () => applyParaFormat({ align: "left" }));
document.getElementById("fmt-align-center").addEventListener("click", () => applyParaFormat({ align: "center" }));
document.getElementById("fmt-align-right").addEventListener("click", () => applyParaFormat({ align: "right" }));
document.getElementById("fmt-align-justify").addEventListener("click", () => applyParaFormat({ align: "justify" }));
document.getElementById("fmt-indent").addEventListener("click", () => adjustIndent(360));
document.getElementById("fmt-outdent").addEventListener("click", () => adjustIndent(-360));
document.getElementById("fmt-bullet").addEventListener("click", () => toggleBulletList());
document.getElementById("fmt-link").addEventListener("click", () => applyLink());
document.getElementById("fmt-image").addEventListener("click", () => document.getElementById("image-file").click());
// Inline popover (link/comment) controls.
function popoverValue() { const ml = !document.getElementById("popover-textarea").classList.contains("hidden"); return document.getElementById(ml ? "popover-textarea" : "popover-input").value; }
document.getElementById("popover-ok").addEventListener("click", () => closePopover(popoverValue()));
document.getElementById("popover-cancel").addEventListener("click", () => closePopover(null));
for (const id of ["popover-input", "popover-textarea"]) {
  document.getElementById(id).addEventListener("keydown", (e) => {
    if (e.key === "Escape") { e.preventDefault(); closePopover(null); }
    else if (e.key === "Enter" && (id === "popover-input" || e.metaKey || e.ctrlKey)) { e.preventDefault(); closePopover(popoverValue()); }
  });
}
document.getElementById("image-file").addEventListener("change", (e) => { const f = e.target.files[0]; if (f) insertImageFile(f); e.target.value = ""; });
document.getElementById("fmt-comment").addEventListener("click", () => addComment());
// Image resize bar: the width input commits via its `change` event, which fires
// on both blur and Enter — so Enter just blurs the field (one commit, not two).
// −/+ step the width by 20px.
document.getElementById("image-width").addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); e.target.blur(); } });
document.getElementById("image-width").addEventListener("change", (e) => resizeImageTo(Number(e.target.value)));
document.getElementById("image-smaller").addEventListener("click", () => stepImageWidth(-20));
document.getElementById("image-bigger").addEventListener("click", () => stepImageWidth(20));
document.getElementById("commit").addEventListener("click", () => commit());
document.getElementById("mode-suggesting").addEventListener("click", () => setMode("suggesting"));
document.getElementById("mode-editing").addEventListener("click", () => setMode("editing"));
document.getElementById("export").addEventListener("click", () => exportDocx());

// Test hooks: drive comment authoring/resolve without the native prompt (which
// headless browsers can't answer). Mirrors applyLink's `urlForTest` convention.
window.__stemmaComment = {
  add: (body) => addComment(body),
  resolve: (id, done) => resolveComment(id, done),
  focus: (id) => focusComment(id),
  state: () => state.comments,
  // Set the PM selection to the first occurrence of `substring` (so a headless
  // test can select text without simulating drag). Returns true on a hit.
  selectText: (substring) => {
    if (!view) return false;
    let found = null;
    view.state.doc.descendants((node, pos) => {
      if (found || !node.isText) return;
      const i = node.text.indexOf(substring);
      if (i >= 0) found = { from: pos + i, to: pos + i + substring.length };
    });
    if (!found) return false;
    const sel = TextSelection.create(view.state.doc, found.from, found.to);
    view.dispatch(view.state.tr.setSelection(sel));
    view.focus();
    return true;
  },
};
