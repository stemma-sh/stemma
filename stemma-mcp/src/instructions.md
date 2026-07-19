
# Editing a tracked-changes .docx with stemma

The default task path is a compact compiler front end over the complete typed
engine. Follow it first; the individual advanced tools below are expert escape
hatches over the same kernels.

## Golden path

1. **`open_docx(path)`** returns `doc_id`, exact input identity, and the first 16
   rows of a paged compact block index. Prefer find over walking every page.
   Paths stay under `STEMMA_MCP_WORKSPACE_ROOT`.
2. **`inspect_docx(doc_id, ...)`** returns the first compact index page by default.
   Prefer `query:"find"` plus `pattern`, then inspect one exact block. Block
   inspection is compact by default: paragraphs keep exact text, guards,
   list/role identity, and durable opaque anchors. A table block returns eight
   bounded cell locators by default and omits its aggregate body; page with
   `cell_offset`/`cell_limit`, then inspect a locator's `block_ids` for exact
   cell paragraphs. Pass `detail:"formatting"` only when
   complete run marks and style properties are needed. Use a
   returned `next_offset` to page beyond the default 16 find matches. Use a
   returned table's `matching_cells_next_offset` as `cell_offset` when more than
   the default four matching cells exist; this nested page is separate from
   the top-level find page. Non-table find results use a match-centered excerpt
   of at most 240 characters; inspect the returned block id for exact text. Use a
   bounded `query:"window"` when nearby context is needed. The
   `query:"document"` projection returns 16 top-level blocks by default and
   exposes `has_more`/`next_offset` for deliberate paging. Prose is exact;
   tables are explicit summaries with four cell previews and route back to
   paged block inspection, so a large table cannot silently defeat the bound. Query
   revisions or styles when the plan needs them; for review rounds start with
   `query:"revisions_summary"` (exact counts by author × kind, no rows).
3. **`execute_plan(..., preview:true)`** validates one explicit atomic v4
   transaction or accept/reject selection without mutation. Fix every refusal,
   then execute the same plan with `preview:false`. A complete successful
   preview reports `apply_ready:true`; apply that identical plan once it covers
   all intended changes rather than re-reading or reformulating it. Both paths return only the
   touched blocks, not the full document. For several literal substitutions,
   preview `replacement_worklist` with `preview:true`, then apply it with
   `preview:false`; it runs the ordered tracked worklist on a throwaway snapshot
   or live state and reports each item independently. Give each item an integer
   `expected_matches`, or `replace_all:true` when every occurrence is intended;
   do not stringify the count. On a `MatchCountMismatch` refusal, the listed
   matches are disambiguation data: when one site is intended, narrow the
   target (longer old text, or the listed match's cell `scope`) — never raise
   `expected_matches` to absorb ambiguity you have not verified occurrence by
   occurrence. `match_mode` is exactly `exact` or
   `normalize_ws`. Omitted scope includes top-level and table-cell paragraphs;
   pass a matching cell paragraph `block_id` as `scope:{block_id}` only when
   disambiguation is needed. Do not flatten the cell.

When replacing a paragraph that contains an opaque span, preserve that anchor
inside `content.content` as
`{"type":"opaque_ref","attrs":{"id":"<opaque id from block segments>"}}`.
Do not flatten, omit, or invent the anchor.
4. **`verify_docx({doc_id})`** must report zero unexpected direct changes, zero
   untouched-scope violations, intended pre-existing-revision dispositions, and
   `validator.ok:true`. It also accepts a producer-neutral before/after path pair.
   Audit lists return 16 rows by default with totals and continuation metadata;
   retrieve remaining rows with `detail`, `offset`, and `limit` (maximum 64).
5. **`save_docx(doc_id, path)`** commits only a complete, verified result to a
   NEW unused path. Existing destinations and input aliases are refused.

Use the individual read, edit, resolution, and audit tools only when an expert
workflow needs separate steps or narrower receipts. Never silently broaden an
approved plan.

`check_edit(doc_id, transaction)` dry-runs the same package-aware,
author-protected path as `apply_edit` and discards the result. Use it before a
risky advanced transaction, not before the focused batch.

## Review rounds and dense documents (triage → bulk resolve → save early)

For "accept/reject <author>'s changes" tasks, especially on long documents:

1. **Triage with counts, not rows.** `inspect_docx(query:"revisions_summary")`
   returns exact pending counts by author × kind and composes with `filter`.
   Do not page the full inventory just to learn who changed what and how much.
2. **Resolve with a bulk selector, never a hand-built id list.** The
   resolution selector takes `{"by":"by_author",...}`, `{"by":"by_range",...}`,
   `{"by":"by_filter", by_author?, by_kind?, by_block_range?}` (AND-combined —
   "author X's changes in Section Y" is ONE call), or `{"by":"all"}`.
   Enumerating the inventory to assemble `{"by":"by_ids"}` wastes turns and
   context; reserve explicit ids for cherry-picks the axes cannot express.
   For a section named in the instruction: `query:"find"` the heading, take
   the section's block range via `query:"section"` on the heading id, and
   pass those endpoints as `by_block_range`.
3. **Trust bounded receipts.** Bulk writes and resolutions return exact counts
   beside capped evidence lists carrying `omitted` and `set_sha256` commitments
   to the complete set. Submitted worklist items and transaction operations are
   never capped: each has an inline outcome. Verify with `verify_docx`;
   do not re-derive counts by re-reading the inventory.
4. **Persist before polishing.** Once the requested changes are complete and
   `verify_docx` is clean, `save_docx` IMMEDIATELY — completed-but-unsaved
   work is worthless if the session ends. Extra spot-checks and summary
   material come after the artifact exists, never before.

## Filesystem boundary

- Every server-side read and output path stays inside
  `STEMMA_MCP_WORKSPACE_ROOT`, which defaults to the canonical directory where
  the server started. Relative tool paths resolve under that root. Do not retry
  a path that returns `artifact_outside_workspace`; choose a path inside the
  configured root.
- A read symlink that resolves outside the root is refused. Image `path` inputs
  follow the same rule as DOCX inputs.
- Image `path` inputs default to 20 MiB each
  (`STEMMA_MCP_MAX_IMAGE_BYTES`) and 50 MiB aggregate per transaction
  (`STEMMA_MCP_MAX_IMAGE_TOTAL_BYTES`), measured before base64 expansion; `0`
  disables the corresponding host limit. `artifact_source_too_large` means use
  a smaller image, split the transaction, or ask the host operator to raise the
  relevant limit.
- Outputs are create-new only. Always choose a path that does not exist; do not
  delete or overwrite another artifact to make a call succeed.
- Successful save, compare, and render calls preserve their existing receipt
  fields and add SHA-256-qualified artifact identity. Treat the output as
  committed only after the tool returns success.
- An image path becomes a registered session source if the runtime mutation
  applies; edits rejected before mutation, `check_edit`, and previews register nothing. Mutation,
  registration, and save/review export are coupled; exact repeated sources
  deduplicate, and their identities expire with the document session TTL.
- Stemma stages and verifies output before a no-clobber commit. This guards
  ordinary caller mistakes and failed writes, not a hostile same-user local
  process, storage corruption, or power-loss durability.

## Transaction op quick reference

The everyday ops, exact wire shapes — for anything beyond these,
`inspect_docx(query:"operations", pattern:"<op_name>")` returns the full
catalog entry; you rarely need the whole catalog:

```json
{"op":"replace","target":"p_7","expect":"<current text>","content":{"type":"paragraph","content":[{"type":"text","text":"new text"}]}}
{"op":"insert","target":{"anchor":"p_7","position":"after"},"content":[{"type":"paragraph","content":[{"type":"text","text":"new paragraph"}]}]}
{"op":"delete","target":"p_7","expect":"<current text>"}
```

Every transaction carries `"revision":{"author":"<distinct name>"}`. There is
no `toc` or `field` op — a table of contents is an `insert` with a
`{"type":"toc"}` content block (see below); images use `insert_image`/
`replace_image`/`set_image_attrs`. Formatting is `set_format` (run marks),
`set_para_format`, or `apply_style`; tables are `table_op` (see the table
section).

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
