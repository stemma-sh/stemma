---
name: stemma
description: Edit Microsoft Word (.docx) documents that carry tracked changes (redlines), as tracked changes, via the stemma MCP tools. Use when the user asks to tighten/revise/redline a .docx, make tracked edits, find-and-replace text, accept or reject changes, read a document's revisions, or conform a document to a house style (named heading/body styles, fonts, margins). Encodes the golden path (lead with replace_text for find/replace) and the sharp edges (marks wire format, span semantics, whitespace exactness, style-op field names, layer-vs-resolve policy) so you do not discover them through errors.
version: 1.5.0
---

# Editing a tracked-changes .docx with stemma

The dominant task: **edit a document that already carries a redline, adding your edits as tracked changes layered beside the existing ones.** stemma's engine work is milliseconds. Follow the golden path and the sharp edges below.

## Golden path

1. **`open_docx(path)`** → `doc_id` + the block outline. Every block has a stable `id` (`p_7`), a `role_token`, and visible `text`. Address all later edits by these ids.
2. **See what the redline contains.** `list_revisions(doc_id)` returns one row per tracked change `{revision_id, author, kind, block_id, excerpt}` — the structured index for building accept/reject id lists. `read_redline(doc_id)` returns the full prose with `<ins>`/`<del>` inline, and `read_markdown(doc_id)` the whole document as id-bearing tagged prose — the comprehension surfaces for reading what the redline actually says.
3. **Resolve existing changes only if the task calls for it** (see Policy below). Use `reject_changes` / `accept_changes` with a selector: `{"by":"by_ids","revision_ids":[...]}`, `{"by":"by_author","author":"Opposing Counsel"}`, `{"by":"by_range","from_block_id":"p_4","to_block_id":"p_6"}`, or `{"by":"all"}`. Batch ids in one call. An empty/unmatched selection fails loudly — it never silently no-ops.
4. **Make your own edits as tracked changes.** For the dominant case — find a phrase and replace it, even inside paragraphs that already carry tracked changes — reach for **`replace_text(doc_id, old, new, author, scope?, expected_matches?, match_mode?)`** FIRST: it matches server-side and splices a tracked change through tracked paragraphs in ONE call, with no `read_block`, span handles, or guards. `expected_matches` defaults to 1 and the call fails (`MatchCountMismatch`) listing each match's `{block_id, excerpt}` if the count differs, so you disambiguate in one follow-up; pass `"all"` to replace everywhere. Use `apply_edit` only for the genuinely surgical or structural cases: a **span replace** to target one specific occurrence among several (see below), or a **whole-paragraph replace** for a formatting/mark change. `apply_edit` defaults to tracked (`w:ins`/`w:del`); pass `mode:"direct"` only to bake a change in untracked.
5. **`validate_docx(doc_id)`** → `{ok, issues}`. Run it after your edits; expect `ok:true`, zero issues.
6. **`save_docx(doc_id, path)`** → writes the .docx. Save to a NEW path to leave the original untouched.

`check_edit(doc_id, transaction)` dry-runs a transaction (`{would_apply:true}` or the actionable error apply_edit would give) without mutating — use it before a risky multi-op edit, not before every edit.

## Span replace (the surgical edit)

```json
{"op":"replace","target":"p_27","span":"s_4","guard":"<guard from read_block>",
 "expect":"<the span's exact current text>",
 "content":{"type":"paragraph","content":[{"type":"text","text":"new words"}]}}
```

To get `span` handles (`s_0`, `s_1`, …) and the `guard`, call **`read_block(doc_id, block_id)`** first. A span replace splices: tracked changes elsewhere in the paragraph are carried through untouched, so your change layers beside them. Editing another author's pending insertion **stacks** (their text stays visible as inserted-then-deleted, both independently resolvable).

## Insert a table of contents

`apply_edit`'s `insert` op takes a `toc` content block — no separate op, no internal role token to look up:

```json
{"op":"insert","target":{"anchor":"p_1","position":"before"},"content":[{"type":"toc"}]}
```

`levels` is optional (`{"levels":{"from":1,"to":3}}`, `1 <= from <= to <= 9`); omitted, it defaults to `1-3` — Word's own "Automatic Table of Contents" range, with hyperlinked entries, page numbers hidden in web layout, and outline levels included (`TOC \o "1-3" \h \z \u`). The field has no cached entries yet: Word computes and displays them the next time the document is opened — the same `apply_edit` call also turns on the document's "update fields on open" setting, so this happens automatically with no follow-up op. Like any insert, it is tracked (reject-all removes it cleanly; accept-all keeps it). A `toc` block is insert-only (`replace` refuses it) and top-level only (refused inside a table cell).

## Sharp edges (each one cost a cold agent an error)

1. **Marks are objects, not strings.** Bold+italic is `"marks":[{"type":"bold"},{"type":"italic"}]`, never `["bold","italic"]`. The string form fails with an opaque untagged-enum schema error.
2. **Span replaces take PLAIN TEXT only.** A span replace with styled `marks` is refused (`UnsupportedEdit`). For a mark/formatting change, do a **whole-paragraph replace** (omit `span`, or `span:"whole"`) — that path accepts marks.
3. **A write invalidates handles and guards for that paragraph.** `read_block` handles are ordinal and the `guard` is a content hash; any edit to the same paragraph makes both stale. Re-read the block before the next edit to it. A stale guard fails loudly (`StaleEdit`/`AnchorNotFound`) — re-read and retry, never guess.
4. **Whitespace and quotes: the exactness burden is on `content`, not `expect`.** Non-breaking spaces (U+00A0) read as plain spaces but are not, and curly quotes/apostrophes are not the straight ASCII ones — so copy them verbatim into your `content`, or you produce a change that only swaps a character class. `expect` is more forgiving: the engine punctuation-normalizes it (curly/straight, dash and ellipsis glyphs), so an ASCII `expect` still matches a curly-quoted paragraph. A no-effect edit no longer passes silently — it fails loudly (`NoOpEdit`), and a stale `expect` fails `StaleEdit`; if you see those, re-read the block and fix the text. For `replace_text`, `match_mode:"normalize_ws"` folds these classes (NBSP/typographic spaces, curly/straight quotes) for matching and reports what folded in `normalization_applied`.
5. **`span` endpoints `before`/`after`/`between` address OPAQUE ANCHORS** (fields, images, bookmarks), not text runs. To target text, use a `s_n` handle. There is no substring fallback.
6. **`replace_all` refuses paragraphs that already carry tracked changes** (it would fold unrelated history); its refusal message points you to **`replace_text`**, which splices a tracked change through them. So for tracked-paragraph find/replace, use `replace_text` — not a span replace. (`replace_text` matches body text only; it cannot target a structural numbering label, so put your `old` needle in the body text after the label — see edge 8.)
7. **Tabs are literal `\t` in the text** (list markers like `\t(b)\t` are real characters in the run, not layout) — include them in `expect`/`content`.
8. **A typed-in enumeration label is in `text` but is NOT editable text.** When a paragraph carries a hand-typed label (`"1.\t"`, `"(a)\t"`), `read_block` shows it at the FRONT of `text` and again in a separate `literal_prefix` field, but the label is structural: it is not one of the `spans`. When you target this paragraph, work with the BODY text (the part after the label): use it for `expect`, and write your replacement `content` as the new body. Do NOT re-type the leading `literal_prefix` into your `content` — the label stays attached on its own, so re-typing it is at best redundant and may be rejected. (This is also why a label you reject can correctly reappear in `text`: it is restored as structural text, not a tracked change.)
8b. **Moving a heading that a cross-reference (REF field / bookmark) points at:** a tracked MOVE, once accepted in Word, drops the bookmark and orphans the REF ("Error! Reference source not found") — this is Word's own behavior for moves, not something the engine can prevent. If the moved range is a REF target, re-anchor it after the move with `insert_bookmark` at the destination, or tell the user the cross-reference will need re-pointing. Don't claim the references are preserved when they will break on accept.
8c. **Moving a whole section: use `move`'s RANGE form, in ONE op — never chain single-block moves.** `target` takes either one block id or a contiguous `{"from","to"}` range (either doc order):

```json
{"op":"move","target":{"from":"p_22","to":"p_27"},"destination":{"anchor":"p_6","position":"after"}}
```

Do NOT relocate a section by issuing several single-block moves in one transaction, each anchored on the block the PREVIOUS move just relocated (e.g. "move p_22 after p_6", then "move p_23 after p_22") — once moved, that id becomes a `moveFrom` shadow at its OLD position, and anchoring on it is refused (`AmbiguousAnchorAfterMove`; the error names the moved copy's id to anchor on instead, or use a stable neighbor). Chaining several moves onto the SAME fixed, never-moved anchor (all anchored on `p_6`) is fine — they land in issue order. After a move, `apply_edit`'s receipt carries a `moves` entry — `{move_id, pairs: [{source_id, copy_id}], prev, next}` — naming exactly where the run landed, so you can confirm placement without a follow-up read.
9. **Attribution: set `revision.author` on every transaction** (`"revision":{"author":"YourName"}`); that name is stamped on every `w:ins`/`w:del`. **Never reuse an author already present in the opened redline** — editing under the prior reviewer's identity makes your changes indistinguishable from theirs and defeats layered review. This is *enforced*: an authored write whose author already authors revisions in the document is refused (`AuthorImpersonation`). Pick a name distinct from every author you saw in `list_revisions`. If you genuinely mean to continue an existing author's work, pass `allow_existing_author: true` on that call to opt in deliberately.

## Tracked table row surgery

To insert or delete a whole table row as a tracked change, use `table_op` (`apply_edit`'s `op:"table_op"`, `target` is the table's block id):

- **`insert_row` carries the new row's CONTENT in the SAME op** — give `cells`, one plain-text string per column, left to right:

```json
{"op":"table_op","target":"tbl_1","table_op":{"kind":"insert_row","ref_row":2,"position":"after","cells":["Widget","4","$12.00"]}}
```

  Fewer entries than the table has columns leaves the rest empty; omit `cells` entirely for an all-blank row. MORE entries than columns is refused (naming the actual column count) rather than silently clamped. This is ONE tracked row insertion (`w:trPr/w:ins` + `w:cellIns` per cell) — do not insert a blank row and then call `set_cell_text` per cell to fill it; give the content up front.
- **`delete_row`** marks the whole row (and its cells) as a tracked deletion: `{"kind":"delete_row","row_index":2}`. Deleting the table's last remaining row is refused — delete the whole table block instead.
- **Formatting is preserved.** `insert_row` / `delete_row` / `merge_cells` (and a whole-table `replace`) work on a fully-formatted table — borders, shading, cell widths, row heights, table style all round-trip; untouched cells are byte-identical. The one refusal is a table that already carries an UNRESOLVED tracked change (accept/reject it first). To *change* a cell's or table's formatting as a tracked change, use `set_cell_format` / `set_row_format` / `set_table_format`.
- **Building/inserting a formatted table.** When you `insert` (or `replace` with `mode:"direct"`) a table, you can set its look inline via `attrs`: the table object takes `attrs:{style,borders,width,cell_margins}`, each row `attrs:{header,height,height_rule}`, and each cell `attrs:{grid_span,v_merge,borders,shading,width,v_align,margins}` (same shapes as the `set_*_format` ops). On a `replace`, any formatting you set wins and everything you omit is inherited from the base table. Caller-set table/row/cell formatting on a *tracked* `replace` is refused (it can't be a reversible tracked change) — use `mode:"direct"`, or the `set_*_format` verbs to author it tracked.
- **If you DO need to fill a row inserted earlier IN THE SAME `apply_edit` call** (e.g. you inserted it blank, or a prior step already committed you to two ops), `set_cell_text` on that row's own cells is allowed — the text becomes part of the same pending insertion, not a second tracked layer. But `set_cell_text` on a cell carrying a PRE-EXISTING tracked change (from an earlier `apply_edit` call, or imported from Word) is still refused: accept/reject that revision first, or address the cell's own paragraph `block_id` (from `read_block`'s `cells` — each entry now carries `{row, col, text, block_id}`) with a tracked `replace` instead of the grid address.
- After a row insert/delete, `apply_edit`'s receipt carries a `table_receipts` entry — `{table_id, rows: [{row_index, status, cell_texts, prev_row_texts, next_row_texts}]}` — naming exactly which row(s) were freshly marked and their neighbors, so you confirm placement without a follow-up read (the same idiom `moves` uses for block moves).

## Styling and house style (named styles, tracked)

To conform a document to a house style — real heading styles, a body font, margins — author **named styles** and apply them; don't hand-set direct formatting on every run. Three ops, all via `apply_edit`:

- **`create_style`** / **`modify_style`** — define or redefine a style in `styles.xml`. `create_style` for a style id the doc lacks, `modify_style` for one it already has (e.g. redefining `Normal`). Exact wire shape (the field names are unforgiving — a misnamed field is now rejected with a "did you mean…" error, but get them right the first time):

```json
{"op":"create_style","style_id":"Heading1","style_type":"para","name":"Heading 1",
 "run_props":{"font_family":"Georgia","font_size_half_points":32,"color":"1F3864","bold":true},
 "para_props":{"spacing_before":240,"spacing_after":120,"line_spacing":276}}
```

  - `style_type` is `"para"` (NOT `"paragraph"`), or `"char"`/`"table"`/`"numbering"`.
  - `run_props`: `font_family`, `font_size_half_points` (HALF-points — 24 = 12pt, 32 = 16pt), `color` (6-hex RGB, no `#`), `bold`/`italic`/`underline`. NOT `font`/`size`/`run_format`.
  - `para_props`: `alignment` (`"left"`/`"center"`/`"right"`/`"justify"`), `spacing_before`/`spacing_after` (twips, 20/pt), `line_spacing` (240ths of a line — 276 ≈ 1.15), `indent_left`/`indent_right`/`indent_first_line`.
- **`apply_style`** — set a paragraph's style as a tracked `w:pPrChange`: `{"op":"apply_style","target":"p_3","style_id":"Heading1","expect":"<the paragraph's visible text>"}`. Applying a style now carries its font/size — you do NOT need a separate run-formatting op to make the font take effect.

**House-style worked path:** (1) `create_style` each heading style (Heading1/Heading2 with the house font/size/color); (2) `modify_style` `Normal` to the house body font once — every body paragraph that uses Normal inherits it, no per-paragraph op needed; (3) `apply_style` each heading paragraph to its heading style; (4) `set_page_setup` for margins. Redefining `Normal` is one untracked style-table edit (OOXML has no `w:styleChange`); applying styles to paragraphs IS tracked. If the task says "track the changes," prefer `apply_style` per paragraph (tracked) and disclose any untracked style-table redefinition in your summary.

**Re-skinning a LARGE / unfamiliar document you can't read in full** (e.g. "change the body font of this 150-page doc to Georgia"): do NOT read the content. **`read_styles(doc_id)` FIRST** — it returns the style table (every style's id/name/font/size/`based_on`/`is_default`) plus the document `doc_default` run font, with `font_family_is_theme` flagging a theme reference (e.g. `minorHAnsi`) vs a literal font. Then:
- If the body inherits its font from the document default (the common case — `read_styles` shows body styles with no own `font_family`, or a `font_family_is_theme` default), set it in ONE op: **`set_doc_defaults(font_family, font_size_half_points?)`** — this pins a literal `w:rFonts` on `w:docDefaults`, re-skinning every paragraph that inherits, without touching the content.
- For specific named styles that set their own font, `modify_style` each (you now know their ids from `read_styles`).
Batch these style edits into ONE `apply_edit` transaction — each `apply_edit` reserializes the whole document, so on a big doc do it once, not per style.

## Footnotes and endnotes (tracked note editing)

`read_index(doc_id)` returns a `notes` array — one row per footnote/endnote: `{note_id, kind, text}` (`kind` is `"footnote"` | `"endnote"`). This is the only read surface for note ids/bodies; `read_block`/`read_outline`/`read_markdown` are body-only and never show note content.

Three ops, all via `apply_edit`, all default to tracked (`w:ins`/`w:del`):

- **`insert_note`** — splice a reference run after `expect` in `target`, plus a new story: `{"op":"insert_note","target":"<block_id>","expect":"<substring in the block>","note_kind":"footnote","body":"<note text>"}`. Tracked: BOTH the reference run and the whole story body show as inserted; reject-all removes the note entirely, accept-all keeps it.
- **`edit_note`** — replace an existing note's body by its `note_id` (from `read_index`'s `notes` or from `list_revisions`): `{"op":"edit_note","note_id":"<note_id>","note_kind":"footnote","body":"<new body text>"}`. Tracked mode is a SURGICAL word-diff (like a whole-paragraph `replace`) — only the changed words become `w:ins`/`w:del`, not the whole body. Refused (`NoteBodyMultiParagraph`) if the note's body is more than one paragraph — v1 scope is single-paragraph note bodies.
- **`delete_note`** — remove a note and its reference run by `note_id`: `{"op":"delete_note","note_id":"<note_id>","note_kind":"footnote"}`. Tracked mode marks both the reference and the story `Deleted` (not physically removed) — accept-all removes the note, reject-all restores it fully.

**No stacking.** `edit_note`/`delete_note` in tracked mode refuse (`BlockHasTrackedStatus`) if the target note's story already carries a pending tracked change (e.g. you just `insert_note`d it and haven't accepted/rejected yet, or a prior `delete_note` on it is still pending) — resolve the existing change first, same rule as editing an already-tracked body paragraph. This is why a note you just inserted in the SAME session can't be `edit_note`d again until it's committed (`accept_changes`/`reject_changes`) or the document is reopened with it already resolved.

Both note verbs are enumerable via `list_revisions` (their `location` names the footnote/endnote id) and resolvable via `accept_changes`/`reject_changes`, same as any other tracked change.

## Policy: layer beside, don't resolve, unless asked

**By default, layer your tracked changes BESIDE the other authors' pending changes — do not accept or reject their changes.** Resolving (accepting/rejecting) someone else's pending change is allowed only when the user's instruction calls for it: a cleanup/tighten-class task ("tighten this redline", "clean up the noise", "strip the junk edits") authorizes you to reject changes you judge to be noise or errors. When you do resolve other authors' changes, **report it distinctly in your final summary** (which revisions you rejected/accepted and why), separately from the new tracked edits you authored. When in doubt, layer and flag the question for the user rather than resolving unilaterally.
