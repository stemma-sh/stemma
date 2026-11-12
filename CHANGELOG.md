# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). Being
pre-1.0, minor (`0.x`) releases may include breaking changes; see
[docs/guide/stability.md](docs/guide/stability.md).

## [Unreleased]

## [0.8.0] — 2026-11-12

### Added

- **Comments can be read from the CLI and core MCP.** Comments could be
  created, replied to, resolved and deleted, but no CLI or core MCP read
  returned a comment's author, date, body, resolved state or thread: an
  agent could reply to a thread it could not read. `stemma read` gains a
  `comments` array (additive to `stemma.read.v0`) and core MCP gains
  `inspect_docx query='comments'`. Each row has `id`, `author`, `date`,
  `text`, `resolved` (derived from the thread) and `parent_id`; the raw
  `parent_para_id` is kept, so a reply whose parent is missing stays
  visible instead of reading as a root. Rust callers get the same rows
  from `stemma_diff::comment_readings`.
- **Validator rule I-TC-004: deleted text inside a move's source.** Word
  reports a file whose `w:moveFrom` runs spell their text as `w:delText` as
  corrupted, and its repair loses the move. The rule names the part and the
  run's text and blocks the save, as I-XREF-006 does for a duplicated note
  reference. Such a file still opens, reading the run as the moved text it
  is, with a warning; every move Stemma writes spells the text as `w:t`.
  The same holds for a field instruction spelled `w:delInstrText` there:
  Word writes `w:instrText` in a move's source and reports the other
  spelling as corrupted.
- **Validator rules I-XREF-006 and I-XREF-007.** I-XREF-006 (error): a
  footnote or endnote id referenced more than once; each note belongs to
  one reference, Word reports such a file as corrupted, and the save is
  blocked. I-XREF-007 (warning): a drawing id (`wp:docPr`) used more than
  once; Word opens such a file without repair, so the save goes through.
- **Notes holding a link, field or line break can be edited.** `edit_note`
  refused any note holding an inline object, so a footnote with a hyperlink
  could not be edited at all. The notes read now gives one line per
  paragraph with one U+FFFC per inline object or break, and `edit_note`
  takes the same text back, carrying each object in its place. A line with
  a different placeholder count is refused
  (`NoteBodyObjectCountMismatch`); a table in a note is still refused.
  The read shows a note's accepted text: tracked `edit_note` refuses a note
  with pending changes (`ParagraphContainsTrackedSegments`), and direct
  `edit_note` accepts them and applies the edit.
- **Edits inside another author's inserted paragraph stack on it.** A span
  replace, `set_format`, `delete` and `comment_create` were refused in a
  paragraph that is wholly another author's pending insertion, though the
  same edits inside an inserted run already stacked. They now stack as
  they do in a run.
- **The worklist and `replace_text` match across bookmark, comment and
  permission boundaries**, as Word's Find does, keeping each marker at its
  text offset. A refusal for an occurrence that straddles something now
  names what it crosses (an inline object or break by id, a pending
  deletion by author, or the edge of a pending insertion).
- **Documents with tracked numbering children open.** A paragraph whose
  `w:numPr` carried `w:ins` or `w:numberingChange` was refused at open.
  Word opens both: an inserted `w:numPr` is read as the one paragraph
  property revision Word lists (both resolutions keep the numbering), and a
  `w:numberingChange` is no revision. Each reading records an import
  diagnostic.
- **`RevisionKind::ALL` lists every revision kind**, and the read-model
  page, the HTTP and CLI references, the MCP `by_kind` refusal and the
  Python stub are checked against it (several lacked `format_paragraph_mark`
  or `cell_merge`).

- **Tracked cell merges resolve as Word resolves them.** A `w:cellMerge`
  (§17.13.5.3), which Word writes when it compares documents, is a
  selectable `cell_merge` revision: Accept gives the cell the merge in
  `w:vMerge`, Reject the one in `w:vMergeOrig` (a missing attribute means
  not merged), and a cell left as a continuation loses its content, pending
  changes in it included. The cell merges of one run of cells in one column
  are one revision. Both resolution paths, the census and compare inputs
  handle them, a cell merge inside a textbox included. 0.7 refused every
  resolution while one was pending.

- **Replacements reach text inside links and content controls.** The
  replace-text worklist (MCP `replace_text`, `replace_text_batch`, the CLI
  `apply` worklist) and `replace_all` replace an occurrence in a hyperlink's
  display text or an inline content control's value inside that object, as a
  tracked change there (each occurrence its own change) or directly, through
  the new `EditStep::ReplaceInlineObjectText`. A control bound to a data
  store gets the new value in its store. Occurrences there count toward
  `expected_matches`. Still refused, naming the object and why
  (`ReplaceTextInsideInlineObjects`, `InlineObjectMatch::reason`): a control whose
  content is locked, one whose value is not free text, one showing its
  placeholder, an object holding pending changes, and a replacement that would
  leave one empty. A match running across a link's or control's edge is a
  wall (`FindReplaceBarrierStraddle`, or `skipped_straddles` under `skip`).
  MCP's `unreached_matches` entries carry `reason` in place of `route`.

- **Table-cell paragraphs move, as Word records it.** `move` takes a
  paragraph (or a range) from the body into a cell as one tracked move:
  Accept gives the target, Reject the base, either half resolves the whole
  move, and the same after save and reopen. From inside a cell (within it, to
  another cell, or to the body) Word records no move but a tracked deletion
  plus a tracked insertion, and so does `move`; the MCP receipt says so in
  `cell_paragraph_moves`. A cell's last paragraph keeps its end mark: its text
  moves and an empty paragraph stays, as in Word. A cell's whole content
  (that is `move_row`), a destination after a cell's last paragraph, and a
  table into a cell are refused, each saying why. A move Word wrote into a
  cell now reads as one move (it read as an unpaired insertion and deletion)
  and keeps its markup through a save.

- **Numbering that names a list but no level opens.** A paragraph whose
  `w:numPr` has a `w:numId` but no `w:ilvl`, and the same in a tracked
  paragraph change's previous numbering, was refused at open. It now takes
  the level Word gives it: the nearest `w:ilvl` along its paragraph style's
  `basedOn` chain, otherwise level 0, with Word's one exception (a level in a
  style that names no list, under a style that names one, is ignored). An
  untouched paragraph is saved as written; Reject writes the resolved level,
  as Word does; and every numbering verb writes the level it sets.

### Changed

- **Breaking: the audit tells an edit stacked inside a revision from an
  unexplained change (`RevisionDisposition::EditedInside`).** The audit had
  one `Modified` disposition for two different things: a pre-existing
  revision with another author's new tracked changes stacked inside it, and
  a revision changed without any covering change. `RevisionDisposition`
  gains `EditedInside { after_excerpt }` for the first, proved by
  projection: rejecting exactly the new revisions must give back the
  revision's content; otherwise, or when no new revision can be selected,
  it stays `Modified`. `stemma apply`, `stemma verify`, the MCP save audit
  and verify-task now share one delivery policy, so the same edit can no
  longer pass apply and fail verify. verify's summary gains
  `preexisting_revisions_edited_inside`; Python reports the disposition as
  `edited_inside`. An edit beside another author's tracked paragraph mark
  leaves that revision `Untouched` (it was `Modified`).
- **Bookmark and permission ranges grow at a paragraph edge, as in Word.**
  A paragraph added after the last paragraph a bookmark or permission range
  covers, or before its first, now lands inside the range, in the body and
  in table cells, tracked or direct. A comment range does not grow: new
  paragraphs land outside it, as before.
- **Resolutions follow Word's rules (each checked in Word):**
  - Accepting a move removes the bookmarks that cover nothing but the moved
    text; a bookmark that also covers text that stays is kept, its moved
    end left where the text was. Reject already matched.
  - A resolution that takes a comment's anchor (the insertion holding its
    reference, or everything in its range) removes the comment whole,
    reference and extensions included. The comment stays while anything
    remains in its range: text inside a link or content control, a
    picture, a tab. An insertion holding only the
    reference of a comment on kept text is no revision.
  - Accepting the removal of a whole paragraph drops ranges lying wholly
    inside it, re-anchors a surviving range start at the next paragraph,
    and puts a surviving range end right after the previous paragraph.
  - A pending inserted or deleted final paragraph mark settles to a no-op
    and never leaves an empty tail paragraph, so resolving revisions one at
    a time gives the same document as resolving all.
  - Text one author inserted and another then moved away is a revision of
    its own beside the move, and resolves in Word's order.
- **Font sizes are read as Word reads them**, in runs and in styles alike:
  a decimal size truncates toward zero, a universal measure ("10.5pt")
  converts, and a non-numeric size is 0, a documented default (Word saves 0
  and renders the minimum size). Styles silently dropped such sizes before.
  A negative size is still refused.
- **`set_format` no longer needs `marks` for a value-only change.** An
  omitted `marks` is the empty patch; an op that changes nothing is still
  refused, and the refusal names the fields that would make it meaningful.
- **Audit rows are less noisy.** Deleting one paragraph from a document is
  reported as one deletion, not as every later paragraph modified, and
  revisions in textboxes and tracked link-text edits in table cells are
  accounted for in their host block.
- **The full view shows a comment's range end.** `OpaqueSegmentKind` gains
  `CommentRangeStart` and `CommentRangeEnd`, so a reader no longer pairs a
  range start with the comment's reference.
- **Python.** `from_bytes`, `audit` and `validate` accept `bytearray` and
  `memoryview`; `save(overwrite=True)` keeps the replaced file's
  permissions instead of leaving mode 0600; the type stub matches the
  runtime module (`mypy.stubtest` clean), with the return shapes importable
  from `stemma` and `Document` and `Comparison` final.
- **Docs: ids of lookalike changes can trade places after an outside
  edit.** Among changes that look exactly alike (same kind, author, date
  and text), ids are stable across Stemma's own saves and resolutions, but
  once another editor changes the file, two lookalikes can trade ids. The
  CLI, guide, review guide and MCP reference now say so and tell readers
  to re-read ids after any edit made outside Stemma.
- **Release pipeline.** The release run now publishes the crates to
  crates.io itself, through trusted publishing, with no stored token. npm
  publication waits up to about 15 minutes for each new version to become
  visible, and a timed-out run resumes. The approval gate shows the
  qualification checklist while it waits.

- `NormalizeError::UnmodeledRevision` is replaced by
  `NormalizeError::InvalidCellMerge`: the byte path now refuses only a
  malformed cell merge (a value outside `rest`/`cont`, or two on one cell),
  as import does. Snapshot blobs move to schema 53 (`TableCellNode` gains
  `cell_merge`; a paragraph records a list reference without a level; a
  table-cell paragraph carries its move group).

### Migration

- **Rust callers matching exhaustively on `RevisionDisposition` need an
  `EditedInside` arm.** Code that treated every `Modified` as "needs
  review" should decide what an edit stacked inside a revision means to
  it; Stemma's own delivery policy counts it as an expected change.
- **Rust callers matching exhaustively on `OpaqueSegmentKind` need
  `CommentRangeStart` and `CommentRangeEnd` arms.**
- **Rust callers that build IR values with struct literals, or destructure
  them without `..`, need the new fields:** `ParagraphNode`
  (`direct_numbering_omits_ilvl`, `cell_move_id`), `TableCellNode`
  (`cell_merge`), `TableFormatting` (`cell_margins_pct`), `CellFormatting`
  (`margins_pct`), `OpaqueInlineNode` and `DecorationNode`
  (`formatting_change`), `HyperlinkData` (`flattened_wrappers`) and the
  `InlineChange::Opaque` variant (`object_runs`). The IR is
  engine-version-bound; these types are not `#[non_exhaustive]`.
- **Rust callers matching exhaustively on `EditStep` or `EditError` need
  arms for** `EditStep::ReplaceInlineObjectText` and
  `EditError::{NoteBodyObjectCountMismatch, ContentControlLocked,
  InlineObjectTextTargetNotFound, InlineObjectTextNotEditable}`;
  `EditError::FindReplaceBarrierStraddle` gains `walls`.
- **CLI, MCP and Python consumers of audit dispositions** may see
  `edited_inside` where 0.7.0 reported `modified`.
- **A file with `w:delText` or `w:delInstrText` inside `w:moveFrom` no
  longer saves untouched** (I-TC-004); resolve or rewrite the move first.
  Nor does one whose footnote or endnote id is referenced twice
  (I-XREF-006).
- **Re-read revision ids after any edit made outside Stemma.**

- **Snapshot blobs written by 0.7 are not read by 0.8**: the snapshot
  schema moves to 53; re-import the DOCX instead of loading an old blob.

### Fixed

- **Text typed right before another author's moved text is no longer
  swallowed by the move (regression from 0.6.0).** A whole-paragraph
  replace that inserted text right before (or after) an inline move source
  wrote the new insertion inside that author's `w:moveFrom`, so Accept All
  dropped the new text, Reject All kept it, and rejecting the editor's own
  revision did not remove it. The new text now stays outside the move.
- **Comparison:**
  - Content holding a footnote or endnote reference that moved gave both
    copies the same note, and Word reported the redline as corrupted. Each
    inserted copy now gets a note of its own.
  - A complex field whose code changed (a PAGE field becoming NUMPAGES) was
    kept as unchanged, a silent loss, and a page number field replaced by
    text was deleted as several revisions. A field is now kept only when
    every part is kept, and is otherwise replaced whole as one revision.
  - A table cell paragraph replaced whole keeps its typed list label
    ("1.") inside the change, instead of leaving it behind to join a
    neighbouring paragraph.
  - A paragraph split is one inserted paragraph mark, as Word records it,
    so Reject All gives the one paragraph back instead of joining it to
    the next.
  - A comment the target adds on kept text is carried untracked, as Word's
    own compare does; Reject All left it empty but anchored. A comment
    added on inserted text still goes with the insertion.
  - Text under `w:caps` was written uppercased in the redline, so Reject
    All showed the base's appearance but not its letters. The redline now
    stores the characters each input wrote: kept text that differs only
    in letter case keeps the base's spelling, or the target's when only the
    base has caps, so both readings render right. A heavily edited
    paragraph replaced as one deletion and insertion writes each side's
    own letters.
  - Drawing ids (`wp:docPr`) stay unique in a redline: an inserted drawing
    whose id another drawing already uses gets an unused one.
  - A comment the target adds inside a newly inserted paragraph is
    recorded as inserted with it.
- **Accepting the deletion of every cell in a row no longer crashes the
  save**: the row goes with its cells.
- **Tracked moves:**
  - A move of text across a paragraph break is one move, not a move plus a
    deletion and an insertion; a moved paragraph's range ends after it, so
    the paragraphs after a moved one are no longer folded into the move,
    and a saved pending move keeps its range and its move pilcrows.
  - Resolving a deletion inside a move's destination no longer panics when
    the deleted words repeat in the moved text.
  - A move keeps its id after a deletion inside its destination is
    accepted or rejected, and after an insertion settles into it; resolving
    a move names the insertions it settles in `cascaded_resolution_ids`.
  - A change nested in an unnamed move container is read as stacked, so
    Reject All no longer writes the deleted words twice; a moved
    document-final paragraph's mark reads untracked, as Word writes it.
- **Tables.** A tracked `merge_cells` is one direct change, as in Word: its
  text relocation was tracked, so Reject All hid text in a continuation
  cell. A whole-table replace gives its cells ids of their own (an edit
  could reach another table's cell). Margins written as `pct` are kept as
  written instead of becoming visible twips margins. One reviewer's cell
  property changes in a table are one revision, and accepting a deleted
  column gives its width away only where Word merges the cell.
- **`replace_text` with a range scope searches table cells**, and a table
  id at either end stands for the whole table. A zero-match diagnosis
  names matches in headers, footers, notes and comments.
- **A tracked link-text edit gives its deletion and insertion their own
  ids**; both carried one `w:id`, which must be unique in a part.
- **Text added beside a proofing bracket stays outside it.**
- **The byte path keeps a note nothing referred to.** `normalize_docx` and
  `reject_all_docx` removed a footnote or endnote that had no reference in
  the input; only a note whose last reference a resolution takes goes now.
- **Import keeps more revisions and formatting.** A tracked formatting
  change on a field part's run, a note mark's run or a lone page-break or
  soft-hyphen run is kept as a revision, and a soft hyphen alone in its
  run keeps the run's formatting; a bookmark inside a body-level insertion
  or deletion is kept; a comment that loses its reference is removed
  whole; a deleted cell
  paragraph's typed label goes with its deletion; a deletion beside moved
  text is its own revision.

- **A re-saved move keeps its paragraph marks in the move.** A move Word
  wrote (whole paragraphs, in the body or into a cell) was written back with
  its range end inside the last moved paragraph, before the paragraph's end,
  so Word read that paragraph's mark on each side as a separate insertion or
  deletion: four revisions instead of two, and resolving the move alone left
  two stray mark revisions and an empty paragraph. The range now ends after
  the paragraph, where Word writes it.
- **A cell join blocked by a pending nested table is refused alone.**
  Accepting (or rejecting) only a cell paragraph's mark, when a nested table
  another pending revision removes stands between it and the next paragraph,
  spent the mark unjoined, so resolving the table afterwards left two
  paragraphs where Accept All joins them. It is now refused like the body
  case. Both refusals name the table's revision to resolve first (then the
  mark joins the paragraphs, as in Word), or the revisions to resolve
  together. Word itself pulls the paragraph into the pending
  table's first cell, after which accepting the table loses the paragraph's
  text.
- **A content control whose contents are locked is not edited.**
  `set_content_control_value`, `opaque_text_edit`, `sdt_text_fill` (inline,
  open block and sealed block controls) and text replacement inside an open
  block control edited a control with `w:lock` `contentLocked` or
  `sdtContentLocked` (§17.5.2.23); they now refuse with
  `ContentControlLocked`, naming the lock. `sdtLocked`, which only keeps the
  control from being deleted, still allows them. A fill of a locked sealed
  block control is refused as an edit, naming the step. Generic paragraph
  edits (`replace`, `delete`, `insert`, formatting, comments, notes,
  bookmarks) inside a locked block control are not yet checked.
- **Selective resolution reports every revision it removes.** A pending
  paragraph mark settled because its paragraph became the document's last
  is now named in `cascaded_revision_ids`.
- **Tracked cell merges resolve on the file path in tables whose cells are
  wrapped in content controls.** `normalize_docx` and `reject_all_docx`
  skipped a cell inside a cell-level `w:sdt`, shifting the columns after it.
- **Replacement inside a link whose text sits in a wrapper is refused.** A
  link holding a simple field, content control, smart tag, custom XML
  element or bidi wrapper was rebuilt from plain runs, turning a field into
  static text; `replace_text` and `replace_all` now refuse it
  (`ReplaceTextInsideInlineObjects`, reason `WrappedText`). Known
  limitation, unchanged from 0.7: any other edit to the paragraph holding
  such a link still flattens the wrapper.
- **No barrier straddle is reported for text that is not there.** A needle
  that only occurs once a link's or control's text is left out no longer
  counts as a straddle.
- **Resolving a change in a data-bound content control updates its store.**
  Reject All (and any resolution) of a tracked edit in a control bound to a
  data store left the store holding the edited text, which Word shows again
  when it refills the control on open; the store now follows the resolved
  text, as it does after an edit.

- **Twips measures are read as Word reads them.** Paragraph, tab, frame,
  section, table, style and numbering measures share one reader: a decimal
  truncates toward zero, `+500` and `.5in` convert, and an empty value is
  absent. A value that is not a number is refused, naming the element and
  attribute, where Word refuses the file, and is read as Word reads it, with
  an import diagnostic, on table and run measures. Before, a decimal tab
  position refused the document, and a decimal in a frame's size or position
  was dropped when the paragraph was edited and saved.

## [0.7.0] — 2026-11-07

### Added

- **Python scripts can edit Word documents (experimental).**
  `pip install stemma-py` (import `stemma`) installs a self-contained wheel
  with no dependencies for Linux, macOS and Windows. It opens a `.docx`,
  reads it, applies v4 transactions, resolves revisions, reviews, compares,
  audits, validates and saves, one call per `api::Document` facade method.
  Engine refusals raise `stemma.StemmaError` subclasses carrying the engine's
  code and structured details. The Python API may change before 1.0; see the
  [Python reference](docs/reference/python.md).

- **A new row can be formatted like another row.** `insert_row` takes
  `format_from_row`: the row to copy, while `ref_row` and `position` still
  place it. A row added at the end of a table whose last row is formatted
  differently (a bullet in its cells) could only copy that last row, and the
  agent's move of its own new row was refused.
- **Two paragraphs can be joined.** `join_paragraphs` (`target` and `with`,
  the paragraph right after it) deletes the paragraph mark between them, as
  Delete at the end of the first does in Word, in the body or within a
  table cell: tracked, Accept gives one paragraph and Reject the two; direct,
  they are joined at once. Before, an
  agent retyped one paragraph's text into the other and lost its run
  formatting.
- **A receipt says what a tracked paragraph reads once accepted.** A
  changed paragraph's `text` is the redline reading (pending deletions
  included); with tracked changes it now also carries `accepted_text`. A
  whole-paragraph replace that kept only a phrase read as the old paragraph
  run into the new phrase, and the agent took it for the intended sentence.
- **A paragraph in a header or footer can be deleted.** `delete` takes
  `header_part` or `footer_part`; before, only its text could be cleared,
  leaving an empty line. The story keeps its last paragraph.
- **List levels are discoverable.** The `set_numbering` entry shows `indent`
  (and names `outdent`, `remove`, `split`, `set_list`, `format`); it offered
  a `join` kind that does not exist. A paragraph moved to a level its list
  does not define is refused: Word shows it unnumbered.
- **A mark may be written as its name.** `"marks": ["bold"]` is read as
  `[{"type": "bold"}]` for the six plain marks; it is the spelling agents
  reach for first, and refusing it cost a retry. Any
  other string is refused with the valid names, and a mark object with an
  unknown key (`{"type": "bold", "x": 1}`, which serde accepted silently)
  is now refused.
- **A column operation can name the column it means.** `delete_column` and
  `move_column` take `expect_header`, the text of the column's top cell; when
  that column is headed otherwise the op is refused with every column's
  index and header. Agents counted columns from the first data column,
  missing a label column, and deleted the column next to the one asked for.
- **A new link looks like a link.** Text an edit makes into a link takes
  the document's Hyperlink character style (the style named "Hyperlink",
  whatever its localized id), as Word styles a link it inserts; before, a new
  link was written with no formatting and read as plain text. Links already
  in the document keep their formatting as authored.
- **Text and links in header and footer tables are editable.** A letterhead
  is often a table in the header, its cells holding the address lines and an
  e-mail link. The read listed those paragraphs, but none could be read by
  id or edited, `find` did not search headers, and a header link's target was
  never read. Now `edit_header` / `edit_footer` reach a paragraph inside a
  table in the story; `replace` and `set_attr` on a link take `header_part` or
  `footer_part`, and a new target is written to that part's own
  relationships; `find` reports header and footer matches under
  `story_matches` with the part to pass; and a header or footer link's target
  is read from its part's relationships.
- **Links in new text are taught where agents look.** A link asked for as
  a `link` mark (the first thing agents try) is refused with the inline
  hyperlink node it should be, and the `insert` shapes show one: an agent
  told only the valid marks concluded links could not be created. The
  `table_op` entry says row and column indices are 0-based.
- **Merged cells can be seen and extended.** A table read gives a merged
  cell its `row_span` / `col_span`; before, a vertical merge read as a cell
  with empty cells below it, and an agent asked to make a new row part of a
  merged group could not tell the group was there. `merge_cells` down one
  column now works on a table that already has merged cells, extending or
  joining existing vertical merges (it refuses to cut one, naming the row to
  start or end at); a merge across columns still needs a table without
  merged cells, and says so instead of describing a column insert. A new row
  joins a merged group by `merge_cells` from the group's top row: neither
  `insert_row` nor Word's Insert Row at the end of a group extends it.
- **`doc_id` may be omitted while one document is open.** An agent that
  opens a file and queries it in the same turn cannot know the id yet, so
  agents routinely spent a call (and a model round trip) on a guessed id such
  as `"x"`. A call that needs a
  document and names none is now about the only open document; with none
  or several open it is refused with `doc_id_required` and the open ids.
- **Form field defaults.** `set_form_field_value` takes an optional
  `default`: the text a FORMTEXT field shows when reset, or a FORMCHECKBOX's
  default state, with or without a new value. No op could set a default, so
  "set both its current value and its default to five" stopped half done.
- **Moving columns and swapping cells.** `table_op` gains `move_column`
  (relocate one column, by grid index, with each cell's content, formatting
  and width; on a merged grid every cell must lie in the moving column, in
  the columns it crosses, outside both, or span both) and `swap_cells`
  (exchange two cells' contents, each keeping its cell formatting, for
  swapping entries in some rows only). Both are direct edits: Word records no
  column or cell moves, and a tracked request is refused with the tracked
  alternative named.
- **List label formats.** `set_numbering` kind `format` gives an item and the
  following items of its list a different label format (`num_fmt`
  `decimal`/`lowerLetter`/`upperLetter`/`lowerRoman`/`upperRoman` and an
  `lvl_text` template such as `(%1)`), tracked like a list split: the items
  move to a new list cloned from theirs with that level's format replaced, so
  earlier items and other lists on the same definition keep their labels.

- **Table row reordering.** `table_op` gains `move_row` (relocate one complete
  row before or after a reference row) and `reorder_rows` (a complete target
  order of source rows in one operation). Both keep each row's content and
  formatting together and refuse tables with vertical merges or table-level
  range boundaries.
- **`unlink_hyperlink`** removes a hyperlink while keeping its text and
  formatting, tracked as the link's deletion plus an ordinary-text insertion.
- **Paragraph insertion inside table cells**, tracked or direct, including
  nested cells, with native list numbering and local formatting retained.
- **Paragraph templates and tab stops on insert.** `attrs.format_from` inserts
  a paragraph formatted like an existing one; `attrs.tab_stops` sets or clears
  direct tab stops.
- **`set_block_form_text`** updates a legacy text form field's cached value.

### Changed

- **Ids read differently from 0.6.0.** Anchor ids restart in each paragraph,
  so after reopen every paragraph after the first shows smaller anchor
  indices; links an edit creates are `{paragraph}_hyperlink{n}` instead of
  `edit_link_N`; and the cell paragraphs of a second inserted table carry
  their own ids. Read ids from the enumerator, as always.

- **The CLI worklist reaches table cells and edits inside another author's
  insertion.** An unscoped item searches every body paragraph, the
  paragraphs in table cells included (nested tables too); a scoped item may
  name a cell paragraph. The receipt's default scope is
  `all_body_and_table_paragraphs` and its coverage lists `body_paragraphs`
  and `table_cell_paragraphs` (the `conditional_detection` field and the
  `unreachable_match` refusal are gone). An approved change inside a pending
  insertion is tracked stacked on it, the insertion staying its author's;
  the receipt counts it under `preexisting_revisions_edited_inside`.
  Resolving a pre-existing revision, or changing one outside the blocks the
  worklist edited, is still refused.
- **A joined paragraph keeps the first paragraph's properties.**
  `join_paragraphs` now gives the joined paragraph the first paragraph's
  paragraph properties, its mark keeping its own run properties, as Word's
  join does; tracked, the second paragraph's old properties are recorded as
  a formatting change. Before, Accept kept the second paragraph's
  properties (a headnote joined across a page marker lost its hanging
  indent).
- **A note of several paragraphs is edited paragraph by paragraph.**
  `edit_note`'s `body` gives one line per note paragraph (lines separated by
  `\n`), each edited in place keeping its properties and list numbering;
  another line count is refused (`NoteBodyLineCountMismatch`) instead of
  being flattened into one paragraph.
- **A run of spaces is as many spaces to the edit diff.** A change in the
  length of a run of spaces keeps the spaces it shares, in their own runs,
  instead of rewriting the whole run (padding runs of different formatting
  collapsed into one, and a header line shifted).
- **Body content controls are open.** A body-level `w:sdt` whose content is
  paragraphs and tables (directly or through nested controls) imports as the
  same `BlockSdtWrap` over ordinary blocks that `wrap_blocks_content_control`
  authors, so every edit reaches its content and an authored control reopens
  as itself. A control holding anything else directly in its content (a
  body-level bookmark, custom XML) stays a preserved opaque block; the
  boundaries of a tracked move into or out of it do not seal it.
  `sdt_text_fill` addresses an open control by its first block's `block_id`
  (no `sdt_id`) and sets its first paragraph with text as an ordinary text
  replace; `semantic_hash` is that paragraph's guard. A control with content
  after that paragraph is not fillable, and filling a control that shows its
  placeholder clears `w:showingPlcHdr`. A sealed control is
  still addressed by `body_index`. `BlockSdtTextTarget` reports either
  address as `BlockSdtAddress`. `set_block_form_text` addresses form fields
  in sealed blocks only; in an open control use `set_form_field_value`.
- **Comparison keeps content controls both readings agree on.** Two blocks
  align only inside the same chain of controls; a control added, removed or
  changed around shared content makes that content a deletion and an
  insertion. An edit inside an unchanged control is now a revision inside it
  rather than a replacement of the whole control. Controls are matched by
  Word's control id when they have one, so filling a placeholder control is
  an edit inside it; the redline keeps the target's control properties.
- **Snapshot blobs are a same-version cache.** Each blob (schema version 46)
  records the engine release that wrote it, and every other release refuses
  it on load with an explicit error. Blobs persisted by 0.6.0 are refused;
  re-import the source DOCX.
- **MCP `save_docx` reports the session audit instead of refusing on it.**
  Outside a declared task, only a package-gate error refuses the save. When
  the audit finds a change it cannot explain, the file is written and the
  receipt's `verdict.status` is `needs_review`, with the audit counts and a
  `verify_docx` pointer. Declared tasks still refuse a non-deliverable audit
  before creating the output path.
- **One spelling for comment references.** `InlineNode::CommentReference` is
  removed: an engine-authored comment reference is now the same
  `OpaqueInline` (kind `CommentReference`) that import produces, so a live
  edit and its saved form agree. A paragraph replace no longer needs the
  comment reference listed as an `opaque_ref`; like the comment's range
  markers it is re-placed at its text offset (listing it is still accepted).
  In extended markdown, `<comment id=..>` now shows the comment id, like
  `<comment_start>` and `<comment_end>`, instead of the anchor's node id.
- **The MCP transaction schema lists the operation names.** The `op` field of
  the `transaction` parameter is now an enum generated from the engine's
  operation catalog, so a client can validate op names before calling. The
  core profile's startup guidance is shorter and describes the reporting save.

### Migration

- **Re-read ids rather than reusing ids read from 0.6.0.** Anchor ids restart
  in each paragraph, links an edit creates are `{paragraph}_hyperlink{n}`, and
  a saved revision-id part written by 0.6.0 is re-derived once on open.
- **Rust callers matching exhaustively on engine enums need new arms.**
  `EditError` gains `ExpectCrossesInlineObject` and
  `ReplaceTextInsideInlineObjects`; `SelectionError` gains `BlockedJoin`;
  `RevisionKind` gains `CellMerge` (wire name `cell_merge`); `NormalizeError`
  gains `UnmodeledRevision`; `WordIrError` gains
  `InvalidRunPropertyAttribute`.
- **Three model fields changed type.**
  `SectionProperties::doc_grid_char_space` is `Option<i32>` (it may be
  negative); `RunBorder`'s `size` and `space` are `Option<u32>` and `color` is
  `Option<String>`, absent when the document leaves them out;
  `ParaMarkRprOff` gains `vert_align_baseline`.
- **Some documents that opened before are refused, naming what is wrong.** An
  on/off or numeric property no schema form admits (previously dropped), and
  a tracked change whose previous numbering names a list but no level.
- **CLI and MCP consumers switching on revision `kind` must handle
  `cell_merge`.** While one is pending, every resolution refuses. CLI receipts
  gain `withdrawn_revisions` per item, `coverage.searched_not_edited`, and the
  refusal code `matches_inside_inline_objects`.

### Fixed

- **Two edits that crashed are refused instead.** A whole-paragraph replace
  that named a repeated anchor id once panicked (over MCP the request never
  returned), and so did `edit_note` on a note holding a line break. Both now
  refuse `OpaqueDestroyed`, naming the anchor or `hard_break` left out.
- **Resolving a tracked mark on a table cell's last paragraph keeps its
  text.** Accept All (or Reject All, for an inserted mark) deleted the last
  paragraph of a cell, untracked text included, whenever its mark was
  tracked. A cell's last paragraph has nothing to join, so its text now
  stays, as it already did on the byte-level path.
- **Rejecting a move of text keeps the paragraphs it sat in.** Selectively
  rejecting a move of runs inside paragraphs deleted the paragraph when both
  halves sat in one, and merged the destination into the source when they
  sat in two. Only moves whose paragraph mark is part of the move are laid
  out as moved paragraphs now; moved runs resolve in place.
- **A selective paragraph join never moves text into another author's
  pending change.** Accepting a deleted paragraph mark whose next paragraph
  was another author's whole inserted, deleted or moved paragraph made the
  joined text part of that change, so resolving it later deleted the text.
  Joined text now keeps its own status, in any order, equal to Accept All.
- **An edit inside a moved run no longer duplicates text.** A deletion at a
  move's destination, or an insertion later moved away, resolved on its own
  when the move was of runs, so Accept or Reject All produced the text twice.
  These now resolve with their move, as they already did for moved
  paragraphs.
- **A tracked cell merge is reported, and nothing claims to resolve it.** A
  `w:cellMerge` that changes a cell's vertical merge (§17.13.5.3) was
  invisible: the revision list skipped it, the CLI said there was nothing to
  resolve, and Accept or Reject All returned a "resolved" document that still
  carried it. The document still opens and reads, but the change now appears
  in the revision list as kind `cell_merge`, naming its table and cell, and
  every resolution refuses while it is pending. Resolving merge changes is not
  supported yet. A `w:cellMerge` without merge attributes changes nothing and
  is still ignored.
- **Accepting a paragraph join blocked by another change's table is refused
  on its own.** The result depended on the order of resolution. The
  selection now names the revisions to resolve together; resolving both, or
  the table first, gives the Accept All result.
- **Saved revision ids survive an untracked reorder.** Reopening a document
  whose same-author, same-date changes had been reordered with tracking off
  gave each saved id to whichever change took its place. The saved-id check
  now covers each change's kind and text, so such a document derives its ids
  from content. Saved-id parts written by earlier versions are re-derived
  once.
- **An edit elsewhere no longer renumbers anchors on reopen.** Anchor ids
  (`p_N_widget_K`, `_hyperlink_K`, `_brK`) were numbered across the whole
  document, so a change in one paragraph renumbered every later paragraph's
  anchors after save and reopen: an old id silently named a different
  control, and untouched blocks failed their guards. Numbering now restarts
  in each paragraph.
- **Every object an edit creates has its own id.** A second content control,
  image, equation or cross-reference in a paragraph reused the first one's
  id; links authored in separate transactions or from one template shared
  ids; every inserted table's cells were `__edit_table_r0_c0_b0`. Ids are now
  `{paragraph}_{kind}{n}` and unique in the document, and a new control's
  `w:id` no longer repeats.
- **An inserted comment mark keeps its insertion when the comments are
  saved.** A pending insertion wrapping only a comment's annotation mark was
  dropped on save after any body edit, and its revision vanished. Marks that
  Word writes as their own run now stay inside their tracked container.
- **Resolving a change keeps a range end that sat in the removed text.**
  Accepting a deletion or rejecting an insertion that held one end of a
  bookmark, comment or permission range shrank the range to nothing beside
  its other end, even when text inside it survived. The end now stays where
  the removed text was (ECMA-376 §17.13.6), on both resolution paths and in
  selective resolution. A range lying wholly in the removed text still goes.
- **Replacing a paragraph that holds a pending insertion keeps its markers in
  place.** A whole-paragraph replace slid every later bookmark, comment and
  permission range right by the insertion's length, and Reject All no longer
  restored the source.
- **A marker at the edge of a replaced word stays outside its deletion.** A
  whole-paragraph replace or `set_cell_text` put a bookmark touching the
  rewritten text inside the new deletion, sometimes inside another author's.
  Edge markers now stay with the kept text beside them.
- **Reject All leaves no empty paragraph for a bookmark before a table.**
  Rejecting an inserted or moved-in paragraph just before a table kept an
  empty paragraph when it carried a bookmark or comment range. The paragraph
  is removed and its markers move to the paragraph before it, in the body and
  in cells, on both resolution paths.
- **A table whose `bidiVisual` is off stays left to right.**
  `<w:bidiVisual w:val="0"/>` was read as on, so any edit flipped untouched
  tables to right-to-left. Table, row and cell on/off properties now honour
  `w:val` and refuse values outside ST_OnOff.
- **An explicit baseline overrides a style's superscript.** A run or
  paragraph mark set to `vertAlign baseline` in a superscript paragraph style
  lost the override on save and went back to superscript.
- **Section, run-size and border numbers are read, never dropped.** A
  negative document-grid character spacing, a decimal size such as
  `w:sz="21.0"`, and other section and run numbers were silently dropped when
  they didn't parse as unsigned integers. They are now read per their schema
  type (decimals truncate, universal measures convert), and a value no schema
  form admits is refused, naming the element and attribute. A run border
  without colour, size or spacing no longer gains `w:color=""` and zeros.
- **A tracked change's previous numbering without a level is refused, as a
  direct one is.** The model invented level 0 and then dropped the numbering
  on reject, while the byte path wrote a file the engine refused to reopen.
- **Reject All of a compared label change restores the base label in
  memory.** Comparing a paragraph whose typed label changed ("3." to "6.")
  gave a redline whose in-memory Reject All still saved "6."; the reopened
  redline rejected correctly. Both readings now agree.
- **A bookmark end between paragraphs stays outside inserts and deletes.**
  With Word's `_GoBack` bookmark end directly under the body, an insert after
  the bookmarked paragraph took the wrong formatting and landed inside the
  bookmark, and deleting that paragraph made Accept All fail validation.
- **A replacement keeps the formatting of the characters it does not
  change.** Rewriting a word made of differently formatted runs gave all of
  the new word the last run's formatting, in replace, CLI worklists and
  `set_cell_text`. Unchanged characters keep their own run, and new text
  takes the formatting of the first character it replaces, as in Word.
  Compare records a deleted word run by run, so Reject All restores each run.
- **An `expect` copied from a block's `text` matches across its inline
  anchors.** An `expect` equal to the block's read text was refused as stale,
  with identical expected and actual text, whenever the paragraph held a
  comment, link, field, line break, content control or proofing mark. No
  anchor splits a match now. Where a span still can't be addressed,
  `ExpectCrossesInlineObject` names the object it runs across.
- **Comments, notes, bookmarks and other anchored ops reach table cells.**
  `comment_create`, `insert_note`, `insert_bookmark`, `insert_cross_ref`,
  `wrap_content_control`, `insert_image`, `insert_equation`, `apply_style`,
  `set_numbering`, `replace_image` and `set_image_layout` refused a cell
  paragraph id as "block not found". They now reach cell paragraphs, nested
  tables included. `move` refuses a cell paragraph with an error naming
  `move_row`.
- **A CLI worklist item can revise or undo an earlier item's change.** The
  run used to abort with exit 1 and no receipt, even with `--emit-partial`.
  The worklist tracks its revisions as a net set, and each item's receipt
  counts the earlier revisions it replaced or removed as
  `withdrawn_revisions`.
- **MCP block ranges include the revisions inside tables.** `by_range`,
  `by_filter`'s `by_block_range` and the inventory filters skipped every
  revision in a table and still reported the resolution deliverable; a cell
  paragraph id from the inventory was "not found" as an endpoint.
- **Text replacement refuses when the text also stands in a link or
  control.** A worklist item (CLI or MCP, `replace_all` included) replaced
  the plain-text occurrences and reported complete while an occurrence in a
  hyperlink's text or a content control's value stayed. It now refuses,
  naming each object, its paragraph and the op that edits it; the CLI's
  coverage lists link text and control values as searched but not edited.
- **MCP find reaches cell controls, cell links and textbox text**, and a
  cell paragraph's read shows its content control's value.
- **A comment's range start in a table cell has its read id**
  (`comment_<id>_start`), so a cell replace naming it applies.
- **Identical changes made one after another get distinct identities.** A
  second change that looked exactly like an earlier one (same author, date
  and text: "30 days" to "45 days" in clause 3 after clause 7) but landed
  earlier in the document was refused, because the identity its content and
  position derive was already the first change's. It now takes the next
  identity among its lookalikes; a saved document records it, so it
  survives reopen, and each change stays resolvable on its own. A genuine
  digest collision between different revisions is still refused.
- **Rejecting a move leaves no empty paragraph where it was moved to.** A
  clause moved down to just before a table was restored at its origin on
  Reject All, but an empty paragraph stayed at the destination: the move's
  zero-width range brackets counted as content that kept it.
- **A comment's range markers have their own ids.** The read showed a
  comment's range start and end under the comment's id, so a replacement
  could not name both (`DuplicateOpaqueRefInPayload`). They now read as
  `comment_<id>_start` and `comment_<id>_end` (the comment id stays in
  their metadata); a replacement may name them or leave them out, and the
  comment stays on its text and appears once either way.
- **An audit does not report a piece of a split revision as new.** An edit
  inside another author's pending insertion leaves that insertion's text on
  both sides of the edit; the second piece was reported as a new revision of
  its author. A piece of a pre-existing revision now counts as that
  revision, edited inside.
- **A content control inside a table cell can be set.**
  `set_content_control_value` found its paragraph only at the top level of
  the body, so a form's checkboxes in table cells could not be ticked.
- **`opaque_text_edit` on a hyperlink names the route that works**: it is
  refused with the `replace` op it needs.
- **A header or footer part edit changes only what differs.** The text
  `expect` and the content share stays in its own runs; only what differs
  is spliced.
- **A zero table width keeps its declared type.** `w:w="0"` widths were
  rewritten as `auto`; Word saves the declared type, and a layout reader can
  tell them apart (untouched floating tables moved across the page).
- **A section's decimal twips are read, never dropped.** Page margins and
  header/footer distances written as decimals (`566.93`) were dropped, so
  every page's footer moved to Word's default; they are now read as Word
  reads them (truncated), and a value no schema form admits is refused.
- **An extension text effect is not the shadow toggle.** `w14:shadow` was
  read as `w:shadow`, so untouched text gained a legacy shadow and lost its
  effect.
- **A header or footer edit can change part of a line.** `edit_header` /
  `edit_footer` treated `content` as the whole new paragraph, so plain
  replacement text for a part ("March 2024 v3" for "November 2023 v2")
  was refused beside page-number fields, and elsewhere replaced the whole
  line with it ("Criminal Appeal" for "Civil Appeal" dropped " No.
  82/12"). Content that is only the replacement (plain text repeating none
  of the paragraph's text around `expect`, which must occur once) now
  replaces just that part; content repeating the rest, or keeping the
  paragraph's objects, rebuilds the paragraph as before.
- **A picture's read shows its size.** A block read gave a drawing's size
  only for a text box, and a cell paragraph's read never; an agent asked to
  scale a picture to 80% scaled from the rounded inches in the request. Every
  drawing's anchor now carries `size_emu`, in the body and in a table cell.
- **An indented list item moves.** `set_numbering` `indent` / `outdent`
  changed the item's level but kept its direct indentation, which in many
  documents equals the old level's, so the item stayed where it was. As
  Word's Increase Indent does (observed with Word: the direct `w:ind` is
  removed into the tracked change, whatever its value), the direct
  indentation is dropped and the new level's indent applies; Reject
  restores both.
- **A typed label in a table cell is read.** A cell paragraph starting
  with a typed label ("b. " before an answer option) read without it, so an
  agent formatting "the option" left the label out; a cell's text and its
  paragraph view (`literal_prefix`) now carry it, as a body read does.
- **New text in a marked paragraph is body text.** A paragraph inserted
  with `format_from` takes the template's formatting by lining its words up
  with the template's. When the caller marks some of its text (a bold italic
  term, an italic title), unmarked new text could still pick up the bold or
  italic of the template words it happened to replace (" means" after a
  defined term). Such content now keeps the template's formatting on
  unchanged words, and its new text is the template's body text plus the
  marks given.
- **Permission ranges ending between table rows survive an edit.** A
  protected form's editable range that starts in a cell and ends between
  rows, between cells or between a cell's blocks lost its end (and a range
  wholly between blocks lost both halves) once the document was edited:
  those positions were read as unknown elements. They are kept like
  bookmarks at the same positions. Text typed into a paragraph holding only
  range markers (an editable cell starting with `w:permStart`) now takes the
  paragraph mark's formatting, as in any empty paragraph.
- **A table indent not in twips is kept as written.** `w:tblInd` with
  `w:type="pct"` was read as twips and rewritten as `dxa`, moving an edited
  table; such an indent is now kept verbatim.
- **A table nested in a cell is read.** A form that lays out its fields in
  a table inside a cell showed that cell empty: the nested table's
  paragraphs were neither listed nor in the cell's text, so they could not be
  found, read or targeted (although an edit addressed to them applied). A
  cell now lists them in document order and carries their text.
- **A resized picture is drawn at its new size.** Resizing changed only the
  drawing's frame (`wp:extent`) and left the picture's own size
  (`a:xfrm/a:ext`) as it was, so a renderer that draws the picture at its own
  size showed it unchanged; Word writes both. The picture (or shape, or
  group) is now scaled with its frame, on resize and on image replace, and
  the save audit accounts for exactly that scaled size. A picture in a table
  cell (a letterhead's logo) can now be resized: its paragraph was not found.
- **An edit to a data-bound content control is kept by Word.** A control
  bound to a data store (`w:dataBinding`: a header showing the document
  title, a cover page's company, a custom XML field) shows the store's value,
  and Word refills it from the store on open, so an edit to the control alone
  was silently undone. The edit now also writes the control's new text into
  the bound node, as Word does when it edits the control; every other byte
  of the store is kept. Controls sharing a binding given different texts, a
  multi-line value and an XPath beyond plain element steps are refused. A
  control the engine binds (`wrap_content_control` with a binding) now gets
  a store whose root is in the namespace its XPath names, with the wrapped
  text in the bound node; before, a namespaced XPath addressed nothing in
  the authored store and Word opened the control unbound.
- **Find and replace reaches text Word's spell check marked.** Word brackets
  a word its proofing flagged with `w:proofErr` marks, which are not content
  and are recomputed on open. A needle crossing one was refused as spanning
  a wall; agents had to retry or rewrite the whole paragraph. Text across
  proofing marks now matches and is replaced like any other.
- **A tracked column deletion reopens as written.** The table's recorded
  previous properties kept the pre-deletion grid, which the file cannot
  carry there (only in the grid's own change record), so the saved document
  reopened as a different state from the one edited.
- **A table with no table properties gains none.** A table written without
  `w:tblPr` was read as if every table property were set, so its first edit
  wrote a `w:tblLook` it never had (which changes which conditional formats
  of a table style apply). Such a table now comes back with no table
  properties.
- **A new link reads the same after a save.** A link inserted in a
  paragraph built from a template carried the template's inherited font on
  the link itself, which a reopened link never has (its formatting lives in
  its runs).
- **A footnote with a custom mark keeps its mark.** A footnote reference
  whose number is replaced by a symbol (an asterisk written as `w:sym`)
  shares its run with that symbol. Any edit in the document split the two
  into separate runs; Word then attached the reference elsewhere on the
  next save, and the paragraph it landed in read twice. The reference and
  its custom mark are kept in one run, as a custom mark written as text
  already was.
- **A paragraph with tracked text changes can be reformatted.**
  `set_para_format` refused any paragraph holding tracked insertions or
  deletions, so editing a paragraph's words and then its indent (joining a
  broken paragraph, then giving it the body indent) failed on the second
  call. A tracked paragraph-format change records the paragraph's
  properties, independent of the tracked text in it, as Word records it; it
  is refused only on a paragraph that is itself a pending insertion or
  deletion, or that already has one.
- **A field whose instruction continues into the next paragraph stays a
  field.** Word writes table-of-contents entry fields across two paragraphs
  (`TC \l2 "` ends one, the entry's title fills the next, then the field's
  end). The import read the continued instruction as ordinary text, so any
  edit in the document wrote the title into the page as text and left the
  field with an empty entry. A paragraph that closes a field it did not open
  now starts inside that field.
- **Renaming a contents entry keeps its page number a field.** Changing a
  link's text rebuilt every run of the link from plain text, so a table of
  contents entry renamed through `replace` on its link lost its PAGEREF
  field (the page number became typed text Word no longer updates), and any
  edit to a link flattened a field inside it even where the edit did not
  reach. Only the words that differ between the old and new text now
  change; every other run of the link is kept as it was. A change that
  reaches into a field inside the link is refused with the way forward.
- **Merging cells keeps every cell's text, as Word does.** A vertical merge
  left the lower cells' text in continuation cells, where Word does not show
  it, and a horizontal merge dropped the absorbed cells. The merged cell now
  holds the paragraphs of every non-empty cell in reading order and each
  continuation cell one empty paragraph, which is Word's Merge Cells result.
  Like Word with Track Changes on, a merge itself is not tracked.
- **A paragraph built from a template takes its formatting phrase by
  phrase.** New text that replaces a stretch of the template takes the
  formatting most of that stretch has, but the spaces between replaced
  words counted as kept text and split the replacement word by word, so
  each new word took the formatting of the template word it lined up with:
  "for review within 30 days" built from "for a review *ab initio* before"
  came out with "within 30 days" in italics. The spaces between replaced
  words now belong to the replacement.
- **A paragraph inserted before a text label takes none of the label.** A
  label written as text (" 2.\tAim") stays with its paragraph, and the
  paragraph inserted before it took the label's spacing anyway: the spaces
  before the label and the tab after it. Nothing extra was written, but the
  new paragraph read differently before and after a save and reopen.
- **A turned-off underline keeps its colour.** Word turns off an inherited
  underline with `w:u w:val="none"` and often keeps a colour on it. Since
  underline colours are carried in the model, an edit anywhere in the
  document wrote such an underline back without the colour, on runs and on
  paragraph marks, so the document read differently after a save and reopen.
- **A new note takes the document's paragraph alignment.** A footnote,
  endnote or comment the engine creates has no paragraph style, so it takes
  the default paragraph style; the engine read it as left-aligned regardless,
  and in a document whose Normal style is justified the note read left until
  saved and reopened. Every edit now re-derives the alignment of each
  paragraph outside a table from its style chain, as import does. A style
  whose `w:jc` holds a value outside ST_Jc is now refused when the document
  opens, naming the style, as such a paragraph already was.
- **A link's text shows its tabs.** A table-of-contents entry is a link
  whose text is label, tab, title, tab, page; the read dropped the tabs
  ("A.Pecuniary Reparations22"), so an agent renaming the entry rewrote it
  without them. Link text now reads with its tabs, and new link text with
  tabs is written as `w:tab`, as Word writes it.
- **Text can be typed into an empty header or footer line.** `edit_header` /
  `edit_footer` refused an empty `expect`, the only way to name an empty
  paragraph, so an empty footer line could not be filled. An empty `expect`
  now asserts the paragraph is empty (it is not a wildcard: against a line
  with text it is refused).
- **A column can be deleted under a cell that spans it.** Deleting a column
  from a table with a title row spanning all columns was refused. In direct
  mode the spanning cell now shrinks by the column, as Word's Delete Column
  does; inserting a column into a merged grid, or a tracked column change on
  one, is still refused, with that route named.
- **A new paragraph can follow a heading labelled "6.0".** Inserting with
  `format_from` a paragraph whose typed label is not a sequence Stemma
  numbers ("6.0", "1.2.3") was refused outright. The label the caller types
  at the start of the content ("7.0\tNext Steps") now stands as written; with
  no typed label the refusal says how to write one.
- **`format_from` accepts a template that holds a bookmark, field or link.**
  Such a template was refused as "not text-only", though nothing of it is
  copied but its formatting. Its text runs now supply the formatting and the
  rest is passed over: never duplicated, never removed from the template.
- **A negative line spacing no longer stops a document from opening.**
  `w:spacing/@w:line` is a signed measure (ISO 29500-1 §17.3.1.33) and Word
  documents carry values such as -20; the model held it unsigned and refused
  the whole document. Line spacing is now signed in the model, the style
  cascade and the edit wire.
- **A label stays with its text when its paragraph mark is deleted.** In a
  paragraph whose mark is a tracked deletion ("(a)" followed by deleted
  text), import hoisted "(a)" into the paragraph's label. Accepting all then
  lost it, and any resolution elsewhere wrote it inside the neighbouring
  deletion. Such a paragraph is merging into the next one, so its leading
  label now stays ordinary text.
- **A line break in new text is no longer written as a space.** A line feed
  inside `w:t` reads as a space in Word. In a new table row's cell text it
  now starts a new paragraph in the cell, as Enter does in Word; in
  paragraph text it is refused with a message to use separate paragraphs.
- **A link authored in a table cell keeps its destination.** Table cells were
  serialized without the relationship resolver, so a new hyperlink in a cell
  was written as a bare `w:hyperlink` with no `r:id`: the save succeeded and
  the link pointed nowhere. Cells now resolve relationships like body
  paragraphs, and a new link that reaches a part without a resolver is an
  error instead of a link without a target.
- **Edits and saves of long documents are several times faster.** Every
  stored run property was re-parsed inside a wrapper declaring all 42 known
  OOXML namespaces, twenty times the cost of the property itself; a
  91-page document took 30 seconds to reject six changes and 13 to save.
  A fragment now declares only the prefixes it mentions (5.8 and 4.1
  seconds on the same document).
- **Resolving and reviewing skip markers that cannot match.** Projection
  asked every bookmark and opaque run whether it was a customXml range marker
  or held a note reference by parsing its XML; it now checks for the element
  name first (rejecting six changes in a 91-page document: 5.8 to 3.5
  seconds).
- **The open-time validation is computed once per session.** Every save and
  verify re-validated the unchanged source package; the MCP server now
  validates it once, in the background right after `open_docx`, and reuses
  the result (`SimpleRuntime::session_source_validation`).
- **An edit normalizes its package once.** Applying or resolving passed one
  package as both base and target of the merge and normalized it two or
  three times; it is now normalized once (an edit to a 3 MB document: 4.2
  to 3.1 seconds).
- **Continuous sections keep the page setup they declare.** A continuous
  section that left out its page size, margins or header and footer
  distances took the preceding section's on import, and a save wrote them
  out. Real Word reads the omitted values as its defaults (letter portrait,
  1-inch margins, half-inch header and footer), so an untouched section
  could change layout. Sections are now stored and written as authored.
- **`find` locates link text.** A hyperlink's display text and its target
  were invisible to `find`, so "change the link shown as http://…" started
  with zero matches. A match now names the hyperlink (`matched_in:
  "hyperlink"`, `anchor.id`), the id `replace` targets to change it.
- **A typed label's formatting is readable.** The label is not a span, so its
  run formatting was nowhere in the read: agents rewriting a heading with
  styled content gave its label the underline of the text beside it. The
  block view carries `literal_prefix_marks`, shown in the formatting detail.
- **A table an edit creates has a column grid.** Inserted and converted
  tables were written without `w:tblGrid`, which every table requires, so
  renderers sized their columns by their own rules. A new table now gets the
  grid Word writes: its width (twips, a percentage of the section's text
  width, or the text width) split by its cells' widths, or equally. Widths
  set later re-size the grid, as Word does (in direct mode, or while the
  table is still a pending insertion; a tracked re-size of an existing table
  keeps its grid, which a Reject could not restore). A section that states no
  page size leaves the grid as it is rather than assume one.
- **An author can format the content they are proposing.** Bolding a cell of
  a table the same author had just inserted was refused ("has tracked status
  'inserted'"); formatting inside one's own pending insertion now changes the
  proposal itself (accept keeps the formatting, reject removes the insertion).
  Another author's pending insertion is still refused.
- **A role's exemplar has the role's typical formatting.** New text takes the
  role exemplar's run formatting, and the exemplar was simply the role's first
  paragraph: a document whose first body paragraph opened in italics made
  every inserted body paragraph and table cell italic. The exemplar is now the
  earliest paragraph with the role's most common run formatting.
- **Table cells default to body text.** A cell paragraph with neither a role
  nor `format_from` was refused for want of a host paragraph; it is now the
  document's body text, as `blocks_to_table` cells are. The `insert` catalog
  shows a formatted table in one op.
- **Underline colour survives an edit.** The model kept only an underline's
  style, so any edit — even one elsewhere in the document, since edits rewrite
  paragraphs from the model — wrote every coloured underline back without its
  `w:color` / theme colour. The underline's other attributes are now carried
  with its style.
- **Typed labels read as written.** Every read surface joined a typed label
  and its text with a tab, whatever the document had: items reading
  "(c) the design…" read as "(c)\tthe design…", and an agent copying that
  wrote tabs the items never had. The label is now shown with the whitespace
  around it as written (`literal_prefix_text` on the block view).
- **An inserted item modelled on a typed-label item is labelled once,
  correctly.** It takes its label from its position, but the saved file
  replayed the template's label bytes ("(c)" where the model said "(b)"), and
  content that also began with a label read "(b) (b)…". The template's label
  bytes are no longer replayed, and such content is refused with the content
  to send.
- **Roman heading numbers can be relabelled.** "X." reads as a letter label
  and "XI." as no label, so replacing a heading "X.\t…" with "XI.\t…" kept
  "X." and added "XI." to its text. A leading token shaped like the
  paragraph's typed label is that label rewritten. The worklist's zero-match
  diagnosis for a typed label now names whole-paragraph `replace` as the way
  to change it, also when the needle is the label alone.
- **A filled form field keeps its look.** `set_form_field_value` wrote the new
  value as an unformatted run, so a field set in Arial 10pt showed its value in
  the document default. The value now takes the formatting of the result it
  replaces, or, for an empty field, of the field's own runs.
- **`format_from` no longer spreads formatting by position.** A new
  paragraph's words that replace template text took the formatting of the
  template character just before them, so an item modelled on one ending
  "31st of March" set an unrelated word in superscript. Replacing text now
  takes the formatting most of the replaced text has (a bold term replaced by
  another stays bold).
- **Pagination properties can be set.** `set_para_format` gains
  `keep_with_next`, `keep_lines_together` and `page_break_before`, tracked
  like the other paragraph properties; there was no way to keep a new heading
  with the paragraph it introduces.
- **Textboxes in headers and footers can be edited and resized.**
  `opaque_text_edit` and `set_image_attrs` addressed body paragraphs only, so
  a running label in a header textbox (a tab divider's "TAB 2") could be read
  but not changed. Both take an optional `header_part` / `footer_part` (the
  part name, as `edit_header` does). `set_image_attrs` also resizes a legacy
  VML shape (`w:pict`) through its style width/height, which Word reads as the
  shape's size; a drawing with both carriers has both resized.
  `set_textbox_text` takes the same story address.
- **A textbox's text and size are visible where its paragraph is read.** The
  compact block view showed a textbox as an image anchor with no text, so its
  words were invisible without `detail=formatting`; a textbox anchor now
  carries `textbox_text` and `size_emu`. A VML shape's size is read from its
  style (it has no `wp:extent`), so it was reported unknown.
- **A typed list label can be changed.** A label written as text ("1." with no
  list numbering behind it) is the paragraph's own characters, but `replace`
  refused content starting with a different label and `set_numbering` refuses
  typed labels, so it could not be changed at all. A whole-paragraph `replace`
  whose content starts with a different label now rewrites it, tracked as the
  old label's deletion and the new one's insertion. A different label on
  automatic list numbering is still refused, and the refusal now points at
  `set_numbering`.
- **`blocks_to_table` keeps what the paragraphs carry.** Cells were built from
  the visible text only, so run formatting and line breaks were lost and any
  footnote reference made the conversion refuse. Each cell is now cut from its
  source runs. In a tracked conversion each footnote/endnote reference in the
  table points to its own copy of the note, since Word repairs a file that
  references one note twice even when one reference is deleted; accept keeps
  the copies, reject the originals. A direct conversion moves the references.
- **Rows of a table with vertical merges can be moved.** `move_row` and
  `reorder_rows` refused any table holding a vertical merge. They now refuse
  only an order that would separate a merge group (a continuing cell must
  stay directly below the cell it continues); rows outside a group, and whole
  groups, move.
- **A range marker between a content control's paragraphs no longer seals
  it.** A bookmark, comment-range or permission start or end directly inside
  a body control's content (Word writes them there) made the whole control a
  preserved opaque block, so none of its paragraphs could be read, found or
  edited. Such a control is now open: the markers are block-level markers in
  it, written where they were.
- **List labels are current right after an edit.** A list label is a view over
  the list's sequence, but after an edit that changed the sequence (a split, a
  new label format, an added or removed item) the open document still showed
  the labels it was opened with until it was saved and reopened. They are now
  re-derived from the saved numbering definitions after every edit.
- **A text replacement that only finds the text inside a link says how to
  change it.** Text replacement does not reach a hyperlink's visible text, so
  such a needle matched nothing with no explanation. The zero-match diagnosis
  now names the links and the edit that changes their text (`replace` on the
  hyperlink id), and `set_attr`'s description points there too.
- **A negative first-line indent opens.** `w:ind/@w:firstLine` is unsigned in
  the schema, but Word opens a negative value and reads it as a hanging indent
  of that size. Documents carrying one were refused at import; they now read
  as Word reads them.
- **A find match inside a table cell names the paragraph that holds it.**
  `matching_cells[].block_id` was the cell's first paragraph whatever matched,
  so an edit aimed "after the paragraph that begins …" landed after the wrong
  one. It is now the matching paragraph, `null` with
  `match_spans_paragraphs: true` when the match runs across two of the cell's
  paragraphs, and `block_ids` lists all of them.
- **Two refusals now name the way forward.** A call with an unknown `doc_id`
  lists the documents that are open (`open_doc_ids`): an agent that opens a
  file and queries it in the same turn has to guess the id. Deleting a table
  cell's only paragraph explains that a cell always keeps a paragraph, that
  deleting its text is the edit that remains, and that deleting the row also
  deletes the row's other cells.
- **Whitespace at the edge of a text run is read and written as Word does.**
  It is content only under `xml:space="preserve"`, and XML whitespace is
  space, tab, line feed and carriage return alike. Saving marked only text
  that began or ended with a space, so a preserved line feed at a run's edge
  was lost, also in paragraphs an edit made the engine rewrite but did not
  touch; reading kept unpreserved edge whitespace that Word drops. Both now
  follow the one rule, including `xml:space` inherited from an ancestor such
  as the document root (XML 1.0 §2.10). A deletion whose text is only unpreserved whitespace
  deletes nothing, and rejecting it no longer leaves deleted-form text that
  made the save refuse.
- **Resolving a tracked edit inside a text box or content control gives back
  the run it split.** A tracked replacement inside one run splits it around
  the change; after Accept All or Reject All the pieces stayed separate runs,
  so rejecting did not reproduce the source's markup and accepting differed
  from the same edit made directly. Runs that meet where a resolved change
  was, with the same properties, are joined again, in the model's projections
  and the package resolver alike, and a direct edit joins them the same way.
- **`review()` and `audit` no longer certify a changed sealed content control
  as untouched.** The untouched proof skipped the hash that is the model's
  only record of a sealed block's content, so a fill inside one, tracked or
  direct, was reported as no change at all. A tracked change inside one is
  now a new revision in both reports, and a sealed block whose bytes changed
  without one fails the untouched proof. `audit_documents` and
  `audit_session_documents`, which receive models without those bytes,
  report such a change as an untouched-proof failure.
- **Tracked changes inside a sealed block content control are reported and
  resolved.** A block-level control the block model cannot hold (for example
  one with a bookmark directly in its content) keeps its XML outside the
  model, so its tracked changes were missing from `revisions()` and survived
  accept-all and reject-all. That included the change a tracked
  `sdt_text_fill` writes. They are now listed (census-only, `revision_id` 0)
  and resolved by both full projections, as Word resolves them. Selective
  resolution cannot address them and leaves them pending and listed.
- **A value filled into a content control that shows its placeholder is
  formatted as entered text.** It takes the run properties the control keeps
  for entered text (`w:sdtPr/w:rPr`), or none, as ISO 29500-1 §17.5.2.27
  requires. Before, it kept the placeholder's formatting, such as the grey
  Placeholder Text style, so the filled value still looked like a
  placeholder. Filling an inline or sealed block control now also clears its
  placeholder flag, as filling an open one already did, and setting a value
  on a control that shows its value keeps that value's formatting (a
  checkbox's glyph keeps its symbol font).
- **Paragraph roles are the same on every read of a document.** Two font
  sizes used for the same number of characters were tie-broken by map order,
  so a role's name and members, and the `default` role an insert uses, could
  change between reads.
- **A typed number keeps its own formatting when the first word is
  formatted.** In "16. Impossibilité", making "Impossibilité" italic saved the
  "16." in the same italic run, so it reopened italic.
- **Accept All and Reject All resolve every representation of a drawing.**
  A text box saved with a legacy VML copy kept that copy's tracked changes
  after the model resolved everything, while resolving the saved package
  removed them. Both now resolve each copy the same way. Selecting revisions
  by id still reaches the active copy only.
- **Accept and reject on the package read a table grid width as import
  does.** A decimal width such as `3908.5` opened fine but made resolving the
  saved package refuse the document.
- **A document's first footnote or endnote reopens as authored.** The notes
  part it creates opens with Word's reserved separator notes; these were
  added only on save, so the saved document reopened with two notes the
  edited one did not have. The note verb now adds them to the document.
- **A multi-paragraph append at the end of a story is one proposal per
  paragraph: its content plus the break before it.** Before, the anchor's
  displaced paragraph mark was a proposal of its own, so accepting or
  rejecting it alone merged the anchor into the first appended paragraph,
  and rejecting the last paragraph's content alone left an empty final
  paragraph. The appended paragraphs now also reopen exactly as edited.
- **A moved paragraph reopens as the move that saved it.** A paragraph mark's
  own `w:moveFrom`/`w:moveTo` was read as its revision status and also kept as
  an unknown run property, so it could be written twice; it is now read once.
  The run-level move containers the serializer writes around a whole-block
  move are recognized as derived on import, and a move to the end of a story
  keeps the adopted final mark.
- **Edits keep block content controls whole.** Inserting a paragraph inside
  a control pushed the control's last paragraph out of it, because the
  control was recorded as a span on its first block. Every edit now keeps
  each block in its controls; a new block joins the control of the block it
  replaced, else of the block before it (an insertion after a control's last
  paragraph is inside it, one before its first paragraph is outside it, at
  the start of a story too). An edit that would split a control refuses.
- **A control's last paragraph mark is treated like the document-final mark.**
  Word cannot resolve a revision on it (checked against Word 16): deleting a
  control's last paragraph, or inserting after it, now attributes the change
  to the mark before it, as at the end of the document, and resolving never
  joins a paragraph across a control's boundary. A control whose content is
  all inserted or all deleted keeps its last paragraph in both readings, so
  rejecting an added control or accepting a removed one leaves the control,
  emptied, as Word does. A direct edit follows the same rule, so it reads
  as accepting the tracked one.
- **A direct delete through a story's last paragraph reads as accepting the
  tracked delete.** It left an empty final paragraph behind; the paragraph
  before the deleted run now takes over the final mark, and only a delete
  of every paragraph leaves one, emptied.
- **Rejecting a paragraph appended at the end of a story restores the
  original paragraph in Word too.** Word merges the anchor into the appended
  paragraph, which keeps the final mark. That paragraph now carries the
  original final paragraph's `w:rsid*` attributes, and a change to the
  mark's formatting is recorded as the mark's `w:rPrChange` (a `w:pPrChange`
  cannot hold run properties), so Reject on the saved file equals the
  model's Reject.
- **Resolving revisions inside a block content control keeps the control.**
  Accept All of a tracked deletion of a control's first paragraph removed the
  whole control, because the control was recorded on that paragraph; it now
  keeps the paragraphs that remain. A preserved (sealed) control after an
  open one could also be written with the open control's content after a
  projection; each is now matched to its own bytes.
- **Comparing a table-cell paragraph that gains or loses a comment reference
  keeps its other objects in place.** The comment reference was counted
  among the paragraph's fields, images and links but skipped when they were
  placed, so every later object moved one slot and the last was dropped;
  comparison then failed an internal assertion.
- **A tracked hyperlink deletion or insertion reopens as the link.** A
  `w:hyperlink` cannot sit inside `w:ins`/`w:del`, so a whole-link revision
  (from `unlink_hyperlink`, or deleting or inserting text that holds a link)
  is written as a complete HYPERLINK field. Import now reads that exact
  lowering back as the hyperlink, so Reject All after a save restores the
  source's `w:hyperlink` instead of an equivalent field. A HYPERLINK field
  with anything the lowering does not write, Word's own included, stays a
  field.
- **A tracked `blocks_to_table` inserts its table the way `insert(table)`
  does:** one proposal carried on the table's rows, which Word reads as a
  single insertion of the whole table. It used to add a block carrier and mark
  every cell and run as well, which reopened as separate proposals.
- **An inserted table claims only the formatting it carries.** A table built
  by `insert`, `replace` or `blocks_to_table` marked every table and cell
  property as directly authored even when it had no value, and its empty
  cells held an empty text run; it now reads exactly as it does after a save
  and reopen. An inserted empty paragraph likewise has no runs.
- **Deleting an empty paragraph reopens as the same deletion.** Import now
  reads an empty paragraph whose paragraph mark is deleted as a whole-paragraph
  deletion, as an engine-authored deletion of that paragraph is spelled.
- **Blank first-section headers and footers follow section edits.** The blank
  header/footer a first section without one displays (§17.10.5) is now
  recomputed after every edit, so inserting a section break or turning on a
  title page gives the same stories as reopening the saved document.
- **A created header or footer is an empty paragraph, not an empty run.**
  `create_header`/`create_footer` wrote a bare inserted `<w:r/>` into the new
  part; an empty paragraph now has no runs, as Word writes one.
- **Editing a footnote or endnote keeps its paragraph.** A direct
  `edit_note` on a single-paragraph note now edits it in place, keeping its
  style (`FootnoteText`), attributes and bookmarks; it used to rebuild the
  paragraph from scratch. A tracked edit no longer loses a bookmark at the end
  of the note on Reject All.
- **A bookmark or other marker carried through a paragraph edit never joins
  the inserted text.** It used to take the insertion's status when it sat at
  the end of a replaced paragraph, so Reject All dropped it.
- **A direct list split no longer authors tracked changes.** `set_numbering`
  with `split` ignored the transaction mode and always recorded a tracked
  pPrChange on every re-pointed paragraph.
- **Directly deleting through the end of a story keeps its final paragraph.**
  The story's final paragraph mark cannot be deleted; the paragraph is left
  empty with its own properties, as Word and an accepted tracked deletion
  leave it. Before, a direct delete of a document's only paragraph left an
  empty body.
- **An opaque resolved by Accept All or Reject All is hashed by its canonical
  spelling,** like every other opaque, instead of by its raw bytes.
- **A hyperlink's `text` is its visible text on import too.** Runs deleted
  inside the link are left out, as edits already did; before, an imported link
  with a pending deletion read with the deleted words as visible.
- **An inserted field keeps one spelling across save and reopen.** A tracked
  inserted TOC or cross-reference field is written as a complex field (Word
  does not allow `w:fldSimple` inside `w:ins`); import now reads that exact
  form back as the simple field that was authored. Authored fields also carry
  the content hash of their saved form. A complex field with anything more,
  such as Word's `w:dirty`, is read as written.

- **Comparison no longer adds run formatting the inputs never set.** Text whose
  font, size or language comes from styles or document defaults stays
  inherited in the redline. Previously header and footer runs, and deleted body
  runs, received those values as direct formatting, so Reject All and
  re-comparison reported differences that were not in either input.
- **Pictures are compared by the image they show.** Relationship ids are local
  to a package, so the same picture can be `rId4` in one document and `rId8` in
  another. Comparison now resolves each reference to its target: renumbering is
  no longer reported as a change, and replacing a picture's image behind
  unchanged markup is now detected (it was previously missed). An unchanged
  picture keeps a reference that resolves in the redline's package.
- **Comments and bookmarks keep their anchors when nearby text changes.**
  Comparing documents with edits before an unchanged comment emitted its
  markers twice and was refused by validation; edits around a comment could
  also move its reference inside a word on Reject All. A tracked edit before a
  comment no longer drops the comment from the Reject All reading.
- **Comparison no longer reports what Word regenerates as changes:** section
  editing-session ids (`w:rsid*`), the drawing ids Word assigns on save
  (`wp14:anchorId`, `wp14:editId`), and the cached results of `PAGE`,
  `NUMPAGES` and `SECTIONPAGES` fields, which Word recomputes on layout and
  the redline already left untracked.
- **Authored paragraphs and table rows always receive a `w14:paraId`.**
  Without one, Word regenerates every paragraph id in the document on open.
- Revision resolution and serialization preserve run context, ranges, note
  resources, paragraph text versions and property-change dates.

## [0.6.0] — 2026-09-03

### Changed

- **Document comparison is now an opinionated downstream crate.**
  `stemma-diff` owns correspondence, alignment, comparison presentation, and
  diff-to-carrier orchestration and depends one-way on the Word-compatible
  `stemma` engine. The engine no longer exposes comparison methods or models
  and carries no production dependency back to comparison. The product CLI
  exposes one compare path and no presentation selector, proof-planner budget,
  package-census command, or research diagnostics feature. The unreleased
  `stemma::edit_plan` compatibility alias and the old pairwise full-document
  view were removed; accepted-reading normalization, reusable native carriers,
  package operations, resolution, serialization, and validation remain
  engine-owned.
- **Package serialization now follows one engine-owned reconciliation path.**
  The unused comparison-era source-preservation mode was removed, along with
  its conditional story and package handling. Explicit edits and comparison
  plans now reach the same relationship-closure, story, content-type, and
  package-validation logic.
- **Wholly unrelated document bodies no longer acquire Word Compare's
  first-source/last-target paragraph pivot.** Stemma presents their paragraphs
  as honest whole insertions and deletions rather than implying presentation-
  only lineage. Native Word remains authoritative for carrier validity and
  terminal behavior, not for Stemma's review choreography.
- **Existing Word author labels now require attribution confirmation, not an
  impersonation override.** The engine refusal is renamed from
  `AuthorImpersonation` to `AuthorLabelCollision`, and MCP/HTTP responses name
  the label, pending-revision count, open-time scope, non-mutation guarantee,
  and explicit continuation/separation actions. The CLI adds
  `--allow-existing-author`; successful write receipts record the applied
  author-label policy, while direct materialization records the policy as not
  applicable because it creates no Word revisions. Agent guidance no longer
  tells callers to invent a distinct label; a separate reviewer label must
  come from the user.

### Migration

- **Rust callers must move document comparison to `stemma-diff`.** Add a
  dependency on `stemma-diff = "0.6.0"` beside `stemma`, then replace
  `base.diff(&target)` with `stemma_diff::diff(&base, &target)` and
  `base.diff_as(&target, author)` with
  `stemma_diff::diff_as(&base, &target, author)`. The returned value remains a
  `stemma::api::Document`, so validation, resolution, serialization, and
  explicit editing continue through the engine API. Comparison-specific read
  models now come from `stemma_diff`; engine-only callers need no comparison
  dependency. CLI, HTTP, and MCP callers retain their single `compare`
  operation and do not select a presentation or proof mode.

### Fixed

- **Import normalization diagnostics remain visible across product
  boundaries.** Detailed comparison results retain separate base and target
  diagnostics plus the semantic change count and flattened input revision
  censuses. CLI receipts, MCP open/compare responses, and HTTP upload/compare
  responses now disclose those diagnostics instead of letting a target-side
  normalization disappear into a base-derived redline. MCP `change_count`
  continues to mean detected semantic changes; the number of emitted Word
  revision carriers is reported separately as `revision_count`.

- **Word-lenient orphan vertical-merge markers remain readable and lossless.**
  Word renders a `w:vMerge` continuation with no active restart above it as a
  standalone cell and preserves the marker on save. Word can also normalize
  such a cell to a zero-valued `w:gridSpan` sentinel, which it lays out as one
  column. Stemma now mirrors both import behaviors, while valid continuations
  remain folded into their restart cell in the visible read projection. The
  typed authoring API still cannot create orphan or span-mismatched merges.
- **Dangling package thumbnails now follow Word's safe save normalization.**
  Raw validation still reports the missing target, while import discloses the
  repair and serialization removes only the dead package-root thumbnail
  relationship. Missing actively referenced images, stories, embedded objects,
  and other semantic targets remain blocking failures rather than silent data
  loss.
- **Content-control display text no longer includes XML indentation.** The
  structured read model now projects only Word's actual `w:t` and `w:delText`
  carriers, so pretty-print whitespace and CRLF-versus-LF serialization cannot
  change an SDT's visible text.
- **Comparison materialization preserves its error boundary.** A missing or
  unqualified native carrier is reported as `UnsupportedEdit`, while an
  inferred plan that cannot be applied to its validated source is an internal
  correctness failure. The compare/engine split no longer collapses both into
  `ValidationFailed`.
- **Target-only hyperlinks on an empty replaced final boundary now refuse
  before output.** A hyperlink envelope has no tracked presence carrier when
  the source boundary has no content: sharing the required final paragraph lets
  Word Reject retain an empty `w:hyperlink`, while keeping independent source
  and target paragraphs makes Word Accept discard the hyperlink. A
  content-bearing source-final paragraph retains its separately Word-qualified
  replacement carrier.
- **Mixed row and merge-topology changes use exact table replacement.** Row
  insertion/deletion carriers do not preserve previous `gridSpan`, `vMerge`,
  row-property, or structural cell-shell state. When a surviving row changes
  one of those properties, Stemma now replaces the exact table region rather
  than emitting an Accept-correct plan whose Reject reading loses the source
  merge topology.
- **Active same-ID numbering conflicts now refuse before output.** Independently
  authored documents may reuse one package-local `numId` for different list
  definitions. Stemma no longer treats the source definition's mere presence
  as closure for the target reference, which could make native Accept retain
  the source bullet or numbering format. Equivalent definitions with different
  package IDs or Word-generated `durableId`, `nsid`, `tmpl`, and `tplc`
  identity tokens remain compatible; genuinely different active definitions
  require a complete target-side remap or return a typed refusal.
- **Default and inherited style definitions now participate in collision
  safety.** A styleless paragraph still consumes the default paragraph style,
  and an active style consumes its complete `basedOn` chain. Comparison now
  refuses when either reading would require a different active definition in
  the one physical styles part. Direct style-level `w:rsid` provenance remains
  source-owned and does not create a false collision.
- **Table-cell paragraph replacements retain the cell's required final
  boundary.** Stemma previously emitted separate deleted and inserted final
  paragraphs inside a cell; native Word Reject could retain the source
  paragraph and strand the emptied target paragraph beside it. The carrier now
  shares only the required boundary while keeping source and target content in
  distinct deleted and inserted runs.
- **Comparison refuses simultaneous comment insertion and deletion.** The
  individually supported comment carriers do not compose atomically in native
  Word: one terminal can lose the inserted definition while the other retains
  an emptied replacement anchor. Stemma now fails before materialization until
  a native replacement carrier is qualified; addition-only and deletion-only
  comment changes remain supported.
- **Locked inline content-control deletion now refuses before output.** The
  qualified deletion carrier remains available for unlocked controls, but
  native Word can fail to settle the same carrier when `w:lock` protects the
  control or its content. Stemma now reports that unsupported transition
  instead of emitting a document whose Accept All operation may not finish.
- **Comparison no longer double-tracks style-inherited run formatting.** Run
  differences are now classified from their directly authored `w:rPr` state,
  not from the fully resolved style cascade. A paragraph style change can
  therefore own inherited size, strike, language, and other effects without
  redundant `w:rPrChange` records on unchanged text, while simultaneous direct
  run edits remain independently tracked with their authored source state and
  native run-property carrier.
- **Comparison refuses an unsafe document-final table/paragraph carrier
  composition.** Completely replacing or deleting the final source table and
  independently inserting a target-only empty final paragraph can make native
  Word retain an extra sentinel paragraph; the complete-row replacement arm
  can also hang Reject All. Stemma refuses that structural composition before
  materialization. Standalone table changes and partial row changes remain
  supported.
- **Complete table-row replacement now refuses when composed with independent
  story edits.** Each carrier resolves correctly in isolation, but desktop
  Word can hang on Review UI Reject All when the row replacement and separate
  paragraph regions coexist. Standalone complete row replacement and partial
  row changes remain supported.
- **Table comparison no longer uses text similarity to invent row lineage.**
  Exact row content or an independently persisted row identity is required for
  a semantic match. Structurally compatible unrelated rows may share only their
  physical row, cell, and required paragraph shells while every source payload
  is deleted and every target payload is inserted. Tables with active opaque
  content or incompatible shells use exact-region replacement, and a later
  independent story carrier now refuses because native Word cannot reliably
  complete Reject All for that ordering.
- **Comparison refuses oversized paragraph/run property-history plans before
  materialization.** Native Word Review UI resolution is not reliable for the
  witnessed 300-carrier story, even though each property carrier is valid in
  isolation. The opinionated comparer now enforces one fixed internal
  per-story safety limit and returns an explicit refusal rather than emitting
  an artifact whose Accept All or Reject All operation may not complete.
- **Terminal paragraph replacements retain the existing physical document-final
  boundary.** Native Word can otherwise leave the deleted source tail as an
  extra empty paragraph after Accept All when the source-final paragraph is
  deleted and a target-final paragraph is independently inserted. The comparer
  reuses that one boundary while keeping every target inline inserted and every
  source inline deleted, so no unrelated text acquires semantic lineage and
  unrelated complete-body plans are left unchanged.
- **Final-paragraph deletion now anchors across suffix tables that Accept All
  removes.** When a deleted source suffix ended with tracked paragraphs and an
  all-deleted table before the required final paragraph, the final-mark
  normalizer previously stopped at the table and Word retained an extra empty
  paragraph. The native carrier now assigns the deleted break to the preceding
  surviving paragraph and joins across only blocks that the same resolution is
  proved to remove.
- **Inserted tables no longer inherit a paragraph-boundary revision from an
  earlier surviving paragraph.** Paragraph-mark insertion is now limited to
  all-paragraph suffixes; a table keeps its row-level structural carrier
  independent even when inserted paragraphs precede it. This avoids a shape
  that Word opens cleanly but cannot complete Reject All, while preserving the
  exact accepted and rejected readings.
- **Strict OOXML is no longer emitted as an invalid partial Transitional
  conversion.** Untouched Strict packages remain byte-identical through the
  read/export path. Editing, resolving revisions, or comparing a Strict input
  now returns an explicit unsupported-edit error until Stemma has a complete
  lexical as well as namespace conversion; it no longer rewrites namespace
  URIs while leaving Strict-only measures, percentages, and conditional-style
  values under Transitional schemas that Word cannot open.
- **Tracked inline content-control insertion now refuses instead of emitting a
  false native carrier.** A complete `w:sdt` inside `w:ins` is schema-valid,
  but desktop Word Reject All can leave the inserted control content and its
  revision pending. Qualified content-control text edits and the witnessed
  deletion carrier remain supported; whole inserted or replacement control
  envelopes fail explicitly until a reversible native carrier is established.
- **Collapsed text replacements now preserve source formatting on Reject.**
  When unchanged text inside a replacement also changes formatting, the
  comparison compiler gives the deleted/source arm its previous run properties
  and the inserted/target arm its current properties. It no longer copies the
  target formatting into both arms and asks native Word Reject to restore
  source text with target styling.
- **Comparison refuses differing even/odd header selection.** The document-
  global `w:evenAndOddHeaders` selector has no tracked Accept/Reject carrier;
  choosing either input's value makes the other terminal activate the wrong
  header/footer stories. Identical selector state remains supported.
- **Comparison now refuses a native Word terminal-hang composition.** Move
  ranges and a separately inserted table remain supported carriers, but they
  are not composed in the same story: Word can open that combined redline
  without repair yet fail to complete Reject All. The refusal occurs before
  materialization and mints no revision state.
- **Comparison no longer invents note identity from matching prose.** Comments,
  footnotes, and endnotes with different package-local IDs are explicit
  deletions and insertions unless identity was independently proved and
  remapped before diffing. This prevents a similar target comment from losing
  its definition while its body anchor survives. Dangling comment references
  are now a blocking validation failure because Word repairs and discards that
  carrier. Table-cell property revisions also receive distinct revision IDs
  instead of reusing one ID across multiple cells. When otherwise paired
  tables differ in outer table, row, or cell-shell state that cell revisions
  cannot carry, comparison now uses an exact whole-table replacement instead
  of leaving the source table shell active.
- **Target hyperlinks nested inside inserted tables now retain their own
  relationship semantics.** Comparison imports and remaps typed hyperlink
  relationships even though those nodes do not carry raw XML, so a target
  hyperlink cannot accidentally bind to an unrelated source relationship that
  reused the same package-local ID. Validation now rejects any
  `w:hyperlink/@r:id` bound to a non-hyperlink relationship before Word has to
  repair and discard it.
- **Comparison refuses default header/footer binding-topology changes that one
  native redline cannot switch.** A stable authored binding may still carry
  tracked story-content edits, but comparison no longer treats a synthesized
  blank story as an authored relationship: `w:sectPrChange` cannot store a
  previous header/footer reference, so doing so would leave the target binding
  active after Reject.
- **Validation now rejects malformed native revision carriers before they can
  be reported as Word-safe.** Required non-empty `w:id` attributes are checked
  on property, section, table-structure, move, insertion, and deletion
  revisions, and same-type nested move carriers are rejected alongside nested
  insertions and deletions.
- **Document audit now identifies header and footer stories by physical part,
  not only by rendering role.** Multi-section documents may legally contain
  several distinct `default`, `first`, or `even` stories. Reviewing an
  unchanged document no longer reports those siblings as direct edits or
  untouched-state violations, while a role change on one physical part remains
  visible as a conservative story replacement.
- **Body section-property revisions now follow Word's relative previous-state
  semantics.** A tracked `titlePg` transition writes explicit enabled and
  disabled states on both sides of the unresolved carrier, so native Reject
  restores the source's first-page header/footer selection. Other body-section
  transitions and standalone story-binding snapshots now refuse until their
  absent/default directions have controlled native qualification instead of
  relying on an incorrect absolute-snapshot projection.
- **Tracked display-equation insertion now refuses instead of emitting a false
  native carrier.** Native Word does not reliably reject a directly wrapped
  `w:ins > m:oMathPara` object. Direct display-equation editing and tracked
  inline equations remain supported, while comparison refuses side-only block
  equations before materialization until a qualified tracked carrier exists.
- **Tracked inline equations now use Word's direct Office Math carrier.**
  `m:oMath` is written directly inside `w:ins` and `w:del`; the former
  `w:ins|w:del > w:r > m:oMath` shape opened in Word but lost the equation
  payload on Reject. The direct carrier preserves both native terminals.
- **Package reconciliation now fails loudly on unswitchable active style
  state.** A single `styles.xml` cannot give Accept and Reject different
  document defaults or different definitions for the same referenced style.
  Comparison refuses those transitions, and styles-part presence changes,
  instead of silently choosing one side and emitting a false-success redline.

## [0.5.2] — 2026-08-13

### Changed

- **Claude Code projects now become the MCP workspace automatically.** The MCP
  server uses `CLAUDE_PROJECT_DIR` when no explicit root is supplied, so the
  normal project-local setup no longer requires manually configuring
  `STEMMA_MCP_WORKSPACE_ROOT`.
- **Workspace-root configuration is discoverable and fail-loud.** The new
  `--workspace-root PATH` option takes precedence over
  `STEMMA_MCP_WORKSPACE_ROOT`, followed by `CLAUDE_PROJECT_DIR` and the startup
  directory. Invalid higher-priority settings never fall through; the
  startup-directory compatibility fallback warns; and ordinary startup logs
  report the selected source without exposing the absolute path.
- **Ordinary worklists no longer require precomputed document identity.**
  `stemma.worklist.v0` accepts an omitted `input` binding for the direct
  apply path while retaining exact hash-and-size verification whenever the
  binding is supplied. Every apply receipt still records the actual input and
  now reports `input_binding: unbound` or `input_verified` explicitly.

## [0.5.1] — 2026-08-13

### Added

- **The public demo is an executable release artifact.** A synthetic agreement,
  exact MCP instruction, approved worklist, expected native redline, and
  accept/reject projections now live together under `demo/`. The merge gate
  regenerates the redline, compares its document and revision model with the
  checked-in result, and verifies both generated and checked-in accept/reject
  projections. The checked-in Word visual is captured from that synthetic
  workflow before the release commit is frozen; release qualification confirms
  the same behavior against the candidate.
- **CI has one stable required status.** `required CI gate` depends on every
  mandatory job, including the new demo check, so default-branch protection
  can require one status without silently losing coverage when jobs are
  renamed or split.
- **GitHub releases now carry their own usable summary.** Release notes are
  rendered from the exact version's changelog and include installation,
  platforms, pre-1.0 status, product scope, and version-pinned documentation
  links alongside the live documentation site. Creation and recovery both
  refuse an existing draft whose body differs from the source-controlled
  rendering; recovery reads changelog content from the requested release
  commit, not the newer workflow checkout.

### Changed

- **The MCP privacy statement now describes the actual data boundary.** Stemma
  does not upload documents to a Stemma-operated service, and path-based parsing
  and writes remain local; the docs now state that an MCP client or configured
  model provider may receive selected document content through tool calls.
- **The promoted Claude Code setup is project-local and workspace-confined.**
  The exact clean-install-tested command uses `--scope local` and requires an
  explicit `STEMMA_MCP_WORKSPACE_ROOT` instead of registering a user-scoped
  server with an implicit startup-directory boundary.
- **Benchmark summaries name their provenance and version basis.** The 95%
  result is identified as a maintainer-run agent benchmark on pinned
  pre-release v0.1-line builds, the v0.2.0 compact-contract rerun remains
  separate, and the report explicitly says that no v0.5 agent rerun is claimed.
- **The AI-assistance disclosure now sits beside validation evidence.** The
  README names the three-platform tests, specification-focused conformance
  suite, protocol smoke tests, exact-artifact release qualification, and
  published benchmark corrections, then links directly to content-safe
  first-use reporting.

## [0.5.0] — 2026-08-04

### Added

- **Formatting refusals now answer with data instead of prose.** Asking for a
  tracked formatting change on a span that already carries someone else's
  pending formatting revision refuses with `format_revision_conflict`: the
  target and its tracking state, the invariant that would break, the
  conflicting revision and its author, and a list of recovery actions with
  every field the engine can determine already filled in. `expect` matching
  more than one eligible span in the block refuses with
  `ambiguous_format_target` and the occurrence count. Both report
  `mutation: none`, so a refused transaction is guaranteed to have written
  nothing.
- **Boolean formatting has an explicit off state.** `set_format` takes `marks`
  as a tri-state patch object: an omitted property is unchanged, `true` turns
  it on, `false` turns it off. Turning bold off produces one tracked formatting
  revision whose accept keeps it off and whose reject restores the exact prior
  formatting, including whether it was direct or inherited.
- **The first formatting revision inside a pending insertion is supported.**
  Formatting still-pending inserted text authors a separately attributed
  property revision, and the insertion and the formatting stay independently
  resolvable. A second formatting revision on the same span, Direct-mode
  formatting over a pending insertion, and formatting inside a pending deletion
  remain refused, each with a target-specific explanation.

### Fixed

- **A tracked note is now resolved as one operation.** Inserting a footnote or
  endnote writes two carriers, the body reference and the note story. They were
  offered as independently selectable proposals, so a partial resolution could
  accept the reference while leaving its definition pending, leaving a dangling
  reference no further resolution could clear. Both carriers now share one
  review identity, and a resolution that removes a reference reconciles the
  note definitions behind it.
- **A created header or footer is resolved as one operation**, for the same
  reason: the new story and the section reference that names it shared no
  identity, so a partial resolution could leave an orphan story or a dangling
  reference. Selective rejection of a section change now prunes orphans exactly
  as reject-all did.
- **Deleting the last paragraph that references a header or footer now prunes
  the backing story in Direct mode too**, so accepting a tracked deletion and
  performing the same deletion directly reach the same package. Unlinking a
  reference explicitly still keeps the story addressable, so unlink and relink
  round-trips.
- **A move or insertion landing at the end of a document now round-trips.** The
  document-final paragraph mark cannot carry a revision, so it is handed to the
  incoming paragraph and the preceding mark is marked inserted instead. The
  displaced paragraph's own properties were not recorded anywhere, so rejecting
  the change merged the content under the wrong paragraph style and silently
  lost formatting such as all-caps. The paragraph inheriting the final mark now
  records the displaced properties as a tracked properties change, matching
  what Word writes for the same edit: accept keeps the incoming style and
  reject restores the original.
- **Resolving a paragraph mark no longer fails on a numbered paragraph whose
  body became tracked.** An OOXML run cannot cross a revision wrapper, so a
  literal numbering label sharing its source run with newly tracked body text
  had no valid representation; the surviving label now becomes its own run
  before the tracked body.
- **Selective resolution no longer rewrites paragraphs it did not select.**
  Projection normalization ran over every paragraph, so resolving one selection
  could reshape an unrelated pending insertion elsewhere and make accepting it
  differ from making the same edit directly.
- **Restoring a numbering label recomputes its inherited geometry.** A rejected
  paragraph-formatting change that brings a label back now re-resolves its
  indent and tab origin against the document's styles, numbering, and default
  tab stop, instead of only its style table, so the in-memory result matches a
  save and reopen.
- **Rejecting a Word-authored paragraph-formatting change restores numbering
  inherited from the previous style.** The change snapshot records direct
  paragraph properties, so an absent `numPr` must fall through to the restored
  style rather than erase its list label. Full and selective rejection now
  reapply that cascade and agree with a save and reopen.
- **A role-less paragraph inserted before a literal prefix inherits the prefix
  run's formatting.** Import keeps that prefix outside the ordinary body-run
  stream, but it remains the physical run at Word's insertion point; inserted
  text now uses its formatting rather than the following body's formatting.
- **A role-less paragraph inserted before a leading hard break inherits the
  break run's formatting.** A break-only run is still the physical insertion
  point; the engine no longer skips it and accidentally copies formatting from
  the following text run.
- **Pagination-cache markers no longer create false document differences.**
  `w:lastRenderedPageBreak` remains preserved for untouched round-trips, but
  canonical comparison now removes the producer-owned cache marker and rejoins
  equal-format text fragments it split.

- **Full resolution is now move-complete.** `accept-all`/`reject-all` (engine
  `Resolution::AcceptAll`/`RejectAll`, CLI `resolve --accept-all`/
  `--reject-all`) previously settled a tracked move's content but left its
  markup — the `w:moveFrom`/`w:moveTo` containers and the `move*Range*`
  bookmarks — in the package, so a document containing a move came out of a
  bulk resolution still reporting one pending revision that no further
  command could clear. Both projections now remove the move markup as a unit
  with the content, and the resolved output re-imports with zero pending
  revisions.
- Serializing a document that still carries a pending inline move (for
  example after selectively resolving an unrelated revision) no longer
  double-wraps the move content (`w:del` nested inside `w:moveFrom`, `w:ins`
  inside `w:moveTo`). The nested form re-imported as quarantined nested
  tracking, turning a clean pending move into an unresolvable opaque block.
- `resolve`'s author-not-found hint now lists a blank-author group by its
  actual selector token (`"" (empty author)`) instead of the untypeable
  placeholder `<anonymous>`.

### Added

- **Every writing CLI verb now has a machine-readable result.** `resolve`,
  `compare`, and `validate` take `--format json`: `resolve` emits a
  `stemma.resolve_receipt.v0` (the accepted/rejected/remaining partition of
  the input's revision census plus the committed output identity), `compare`
  a `stemma.compare_receipt.v0` (input identities, discovered-revision
  census, output identity), and `validate` a `stemma.validate.v0` result
  that is structured for **invalid** packages too (issues enumerated, exit
  still `1`). Human summaries stay on stderr; stdout carries data.
- `stemma read <file>`: the full structured read model in one call —
  `stemma.read.v0` with the engine's typed block array (per-segment tracked
  status, marks, span handles, guards, table cells) plus the complete
  pending-revision census. One invocation now serves what previously
  required fusing `extract --format json` with `inspect`.
- `resolve --plan <file>`: a `stemma.resolution_plan.v0` JSON file expresses
  a mixed disposition in one call — accept/reject selectors by author and by
  id plus a `rest` disposition (default `leave`), so "accept one party,
  reject the rest" is a single invocation with a single receipt. Unknown
  plan fields, contested ids, unmatched selectors, and plans that resolve
  nothing are all refused loudly.
- `resolve --dry-run`: plan and report the full outcome (receipt included
  under `--format json`) without writing any output.
- `extract --format json` revision rows now carry the change's `w:date`
  (ISO-8601) as `date`, mirroring the read model's `RevisionRecord.date`;
  the field is omitted when the source markup carries no date.
- CLI reference: the full ten-value `kind` vocabulary on `extract`, the
  empty-author selector contract, the import package-part requirements, the
  extended-Markdown projection grammar (including which revision kinds carry
  no inline marker), and a mixed-resolution (multi-pass) recipe.
- **`stemma::api` re-exports every type a `Document` signature names.**
  `Resolution`, `ResolveSelectionAction`, `ExportOptions`, `RuntimeError`,
  `RevisionRecord`, `RevisionKind`, `EditTransaction`, `AuditReport`, and
  the validation report types are now importable from `stemma::api`, next
  to `Document`. Previously `api::Resolution` existed only as a *private*
  import, so the natural `use stemma::api::Resolution;` failed with a
  "private" error that read as selective resolution not being public, while
  the working paths (`stemma::runtime::Resolution`,
  `stemma::tracked_model::ResolveSelectionAction`) were named on no doc
  page.
- Embedding docs: a compile-checked resolve-a-redline walkthrough
  (enumerate via `Document::revisions()`, chain a mixed accept/reject
  triage through `Resolution::Selective { ids: HashSet<u32>, .. }`,
  serialize the resolved `.docx`). The concepts page no longer describes
  projections as read-only text views: `project`/`read_accepted`/
  `read_rejected` return full serializable `Document`s, and
  `Document::serialize` is documented as running the same blocking
  validation gate as `save_docx` (default `ExportOptions`). The guide
  pages' Rust snippets (concepts, revisions) are now doctest-compiled
  against the real facade, closing the drift that let prose name a
  `list_revisions` method the crate spells `revisions()`.

### Changed

- `inspect --format json` is now `stemma.inspect.v1`: the summary integers
  are named `block_count`/`pending_revision_count`. v0 reused the read
  model's `blocks` key for a **count**, inviting consumers to conflate the
  two shapes. The extended-Markdown projection and its `@stemma inspect.v0`
  header line are unchanged.
- The CLI reference's revision-id guidance now states the actual durability
  contract: ids are content-derived engine identities that survive
  serialize/reopen and selective resolution of other changes (the previous
  wording claimed ids never survive a `resolve`, which was wrong).

## [0.4.0] — 2026-07-21

### Added

- MCP task declarations bind every declared target and read-only input by
  SHA-256 before the first task mutation. Schema v1 admits exact-count tracked
  replacement effects, validates each `effect_id` and replacement field before
  mutation, and refuses task-bound operations whose outcome cannot be decided
  later by revision identity.
- The last target save now emits a create-once
  `stemma.task_manifest.v1`. Complete manifests bind every input, target
  baseline, committed output, passing save audit, full effect declaration, and
  minted revision identities. Missing effects or a later write failure produce
  an explicit partial manifest and a failing final call; abandoned tasks write
  no manifest.
- `stemma verify-task <manifest.json> [--root <dir>]` verifies a delivery from
  its files alone. Exit `0` is verified complete, `1` verified partial, `2` an
  artifact/evidence mismatch, and `3` usage, I/O, malformed JSON, or unknown
  schema.

### Changed

- A task-bound `save_docx` no longer treats one valid document as task success.
  Earlier saves report `task_pending` and `deliverable:false`; only the final
  all-effects join can return a deliverable task result.
- Task-manifest claims are intentionally limited: the format is unsigned and
  verifies consistency with declared files and effects, not producer
  authenticity, declaration timing, or effects omitted from the caller's
  declaration.

- Successful CLI `apply`/`execute` now reruns the delivery audit over the exact
  serialized candidate before committing its receipt or DOCX. Complete
  receipts bind `verification.artifact_stage: "serialized_output"` and
  `verification.output_sha256` to the output artifact digest; standalone
  `verify` remains available as an optional producer-neutral recheck.
- MCP `save_docx` now runs a fresh session audit and refuses a non-deliverable
  result before creating the destination path, then applies the serialized
  package gate and create-new commit. The common agent path is therefore
  `open_docx -> inspect_docx -> execute_plan -> save_docx`; `verify_docx`
  remains available for detailed evidence inspection or producer-neutral
  before/after audit. Successful typed accept/reject commands are retained as
  session evidence, so their selected pre-existing revisions and exact ordered
  committed-content effects are reported as expected rather than blocked.
  Missing evidence, an engine-bypass resolution, or a direct mutation before,
  between, or after those effects still fails closed. Producer-neutral audits
  remain conservative because they have no session command evidence.
- The untouched proof now expands a grouped tracked-move census identity to
  every paired source and destination carrier. Verified delivery therefore
  accepts correctly tracked moves while continuing to block any mutation not
  covered by the exact move identity.
- Core `inspect_docx` find now accepts an additive `patterns` array for up to
  eight known phrases. Outcomes preserve input order and duplicates, report
  zero matches explicitly, and reuse the singular result contract with exact
  totals and continuation. Batch pages and their encoded response are capped
  explicitly; singular find remains unchanged.
- Safe-artifact release qualification now selects the explicit `advanced` MCP
  profile required by its persistence-producer cases, verifies the required
  tool set before running any case, and freezes that profile in the candidate
  manifest. This keeps the harness independent of the product-default compact
  surface while failing before mutation if the expected escape-hatch tools are
  unavailable.

## [0.2.0] — 2026-07-18

### Added

- **Engine-minted revision identity.** One user intention now produces one
  engine revision identity across the affected OOXML carriers. Audits,
  untouched-scope proofs, selective resolution, and transport receipts key on
  stable revision identities rather than parser-local counters or reminted
  wire ids.
- **Focused approved-worklist CLI.** `stemma apply INPUT --worklist FILE -o
  OUTPUT` applies the experimental `stemma.worklist.v0` as native tracked
  changes only after the worklist's SHA-256 and byte count match the exact
  input. It audits preservation and untouched scope, then commits an
  authoritative create-new `stemma.apply_receipt.v0` sidecar with exact
  artifact identities and every item outcome before any DOCX. Partial
  worklists exit `3` and create no DOCX by default; `--emit-partial` may create
  an explicitly non-deliverable diagnostic redline without changing that
  status or exit code. Receipts identify the exact running executable and make
  output persistence conditional on actual exit, presence, byte size, and hash
  agreement rather than treating a pre-commit receipt as delivery proof.
- **Publishable `stemma-artifacts` boundary.** The shared MCP/CLI host boundary
  identifies exact input/output bytes with SHA-256, stages output in the
  destination directory, commits create-new without clobbering, and verifies
  the committed bytes before reporting success.
- **MCP workspace confinement.** `STEMMA_MCP_WORKSPACE_ROOT` confines MCP reads
  and writes, defaulting to the canonical startup current directory. Relative
  paths resolve under it and source symlinks may not escape it.
- **Portable path receipts.** Non-UTF-8 supplied or canonical paths fail loudly
  before reads or staging, preventing lossy identities and JSON serialization
  panics.
- **Portable regular-file paths.** Windows alternate-data-stream syntax is
  refused on every platform before reads or staging. Obvious FIFOs, devices,
  and directories are rejected before open and the opened handle is checked
  again, preventing a no-writer FIFO from blocking the transport edge.
- **MCP image resource bounds.** Path-backed image edits default to 20 MiB per
  image and 50 MiB aggregate per transaction, measured before base64 expansion.
  Either limit can be configured or disabled independently; over-limit reads
  fail as `artifact_source_too_large`.
- **Bounded MCP revision workflows.** `inspect_docx` adds
  `revisions_summary`, with exact totals grouped by author and kind, and
  revision resolution adds an AND-combined `by_filter` selector. Resolution
  receipts report exact selected/matched/resolved counts, cap listed
  identities explicitly, and report every truncated list with omitted counts
  and a follow-up route.

### Changed

- `MatchCountMismatch` refusals (CLI worklist and MCP `replacement_worklist`)
  now lead with the safe remediation: narrow the target to the intended site
  using the listed matches; raising `expected_matches` (or `"all"`) is advised
  only after verifying every listed occurrence is intended. The server
  instructions state the same rule. Previously the error suggested passing
  `expected_matches` first, which invited confirming ambiguity instead of
  resolving it.
- Full accept/reject now follows Word-native mixed-move semantics, including
  nested move-range markers, and descends into revision-bearing glossary
  document parts and their related stories. Selective resolution remains
  identity-bound to modeled revisions.
- Compact revision inspection now accepts the same author, kind, and bounded
  block-range filters as the advanced inventory. Verification reports input
  validation separately from newly introduced issues, and the untouched
  comparator correctly treats reminted stacked-revision identifiers as
  ephemeral while still checking their authored metadata. Granular table
  operations may compose structural row/cell changes within one atomic
  transaction; repeated guards on one table are evaluated against the atomic
  transaction's inspected base snapshot, while pre-existing mid-redline tables
  remain refused.
- Claude plugin packaging no longer bundles a separate agent skill. MCP
  initialize instructions and tool descriptions are the single canonical
  guidance source across plugin, npm, MCPB, and direct stdio installs.
- The default MCP profile is now the complete five-stage compact front end:
  `open_docx -> inspect_docx -> execute_plan -> verify_docx -> save_docx`.
  Inspection defaults to the first 16 rows of a paged compact index and multiplexes bounded
  find/window, paged document, block, revision, and style projections;
  block inspection defaults to exact guarded planning detail and exposes the
  complete run-formatting projection through explicit `detail: "formatting"`;
  table finds return only matching cell excerpts and all finds are explicitly
  paged (16 blocks and four matching cells per table by default, each with
  independent continuation metadata); execution handles the
  existing atomic v4 transaction, an explicit non-atomic replacement worklist
  with per-item outcomes, revision resolution, or a two-file comparison
  producer plan with receipts that omit whole-table content; inspection also
  exposes editable note bodies, historical accepted/rejected/redline and
  section projections, and a parser-derived operation catalog that maps all 26
  historical tools onto the five-tool core;
  replacement worklists now support exact throwaway preview, typed match and
  barrier modes, and formatting-preserving table-cell paragraph splices in
  their default whole-body scope; the document projection is paged at 16
  top-level blocks by default instead of returning an unbounded payload;
  verification reuses the session and producer-neutral
  audit kernels while paging every audit section at 16 rows by default (64
  maximum) with totals and continuation metadata. Comment annotations,
  direct OOXML property changes, and committed revision-resolution effects
  are classified explicitly; comment anchors and hyperlink retargeting are
  accounted changes rather than false untouched-scope violations.
  Set `STEMMA_MCP_PROFILE=advanced` to restore the legacy 31-tool surface; this
  is the migration path for callers that still need individual expert verbs.
- Operation failures now return actionable, operation-specific errors with
  target context and review-round guidance instead of generic execution
  failures.
- Worklist receipts retain an outcome for every requested item even when
  evidence details are bounded. Resolution receipts keep exact totals
  authoritative and disclose omitted evidence rows rather than silently
  dropping them.
- MCP `check_edit` and `apply_batch(preview=true)` now execute and discard the
  same package-aware, author-protected snapshot apply used by commit. Preview
  can no longer approve an origin author-label collision or dangling style that
  the persisted path would refuse.
- MCP `open_docx`, save/compare/audit/review render, and persisted image-backed
  edit responses add artifact identity while retaining their existing response
  keys. Image sources register only after mutation applies; registration and
  save/review export are coupled, repeated exact sources deduplicate within the
  session, and source identity expires with the document TTL. Artifact failures
  use the documented `artifact_*` error codes. Every successful object response
  and structured error also reports the exact `server_version` build identity.
- Release-candidate binaries embed `version+g<commit>` and must pass the real
  confined-workspace MCP smoke plus the mandatory safe-artifact wire harness
  on every native release target before upload. Each archive carries the
  machine-readable report, including the exact binary SHA-256. A protected
  environment holds tag creation and publication until qualification approval;
  an aggregate manifest first re-verifies all five binaries, architectures,
  reports, timestamps, build stamps, and stable case sets. Publish jobs verify
  downloaded bytes against that manifest again, refuse cross-SHA reuse through
  SHA-stamped native packages, publish lifecycle-free prepacked npm tarballs
  only at identical measured and registry integrity, require active tag
  update/deletion protection, claim the approved tag before npm publication,
  recheck it immediately before release publication, and
  expose the GitHub release only after its exact draft asset set is complete.
- CLI `compare` and `resolve` now refuse every existing output, not only an input
  alias. Their existing stderr success line appends byte length, SHA-256,
  `collision_policy=create_new`, and `disposition=created`.
- Wave 7 conformance fixes preserve formatting-change revisions with absent or
  populated previous-property payloads; derive move identity from nested range
  markers; re-nest same-name smart tags by wrapper polarity; compare hyperlink
  and decoration identity rather than ephemeral counters; evaluate atomic table
  guards from the base snapshot; re-emit explicit autospacing and underline-off
  tri-state overrides; inject note-reference styles only for authored
  references; coalesce equal-status untouched-proof segments; tolerate two
  Word-verified wildcard emissions; and no longer enforce a deleted-cell final
  mark as a document-state invariant.

The safe artifact boundary reduces ordinary caller mistakes and failed-write
damage. It is not a sandbox against hostile same-user processes, a storage
integrity guarantee, or a power-loss durability promise.

## [0.1.0] — Initial public release

First public release of stemma: a typed-IR DOCX compiler with first-class
tracked-change semantics, and the transports that put it in front of agents and
applications.

### Added

- **`stemma-engine`** — the core crate. Imports `.docx` into a canonical, typed
  IR (`CanonDoc`), diffs and merges with tracked-change semantics, applies typed
  edit transactions, and serializes back to a `.docx` that opens clean in Word.
  A post-serialization OOXML linker checks the output against codified
  ECMA-376 / ISO 29500 structural invariants before bytes leave the engine.
  Opaque content the engine does not model (equations, drawings, embedded
  objects, content controls) round-trips byte-faithfully or fails loud —
  never silently dropped.
- **Tiered public API.** A stable `api::Document` facade (Tier 1) over a
  typed-IR/domain-model tier (Tier 2) and an explicitly-unstable engine API
  (Tier 3); everything else is sealed. See
  [`stemma-engine/README.md`](stemma-engine/README.md) and
  [docs/guide/stability.md](docs/guide/stability.md).
- **`stemma-mcp`** — an MCP server exposing the engine as 28 tools over stdio:
  read/navigation, tracked-change editing, and review (selective accept/reject,
  validate, dry-run). Distributed on npm as `@stemma-sh/mcp` (`npx -y
  @stemma-sh/mcp`): a launcher package over per-platform prebuilt binaries,
  published by the tag-triggered release workflow alongside GitHub-release
  archives (see [RELEASING.md](RELEASING.md)).
- **`stemma-api`** — a demo HTTP/JSON adapter that serves a browser, Word-style
  review editor (`stemma-examples`) from a single `cargo run -p stemma-api`,
  including `POST /api/compare` for producing a redline from two uploaded
  documents. Local-only demo infrastructure (see [SECURITY.md](SECURITY.md)).
- **`stemma-cli`** — the `stemma` command-line tool: `compare` (redline two
  files), `extract` (text or JSON with pending tracked changes), `resolve`
  (accept/reject by id, author, or all), `validate`. See
  [docs/reference/cli.md](docs/reference/cli.md).
- **Conformance suite** — a hermetic daily gate (`just gate`: clippy with
  warnings denied plus the full daily test tier) with ~1,060 spec-compliance
  tests tied to ECMA-376 / ISO 29500 / MS-OI29500 constraints, plus optional
  host-only corpus and stress tiers.
- **Benchmark report** — model sweeps with deterministic gates and disclosed
  losses; see [docs/benchmarks.md](docs/benchmarks.md), every number backed by
  per-cell data.
- Dual-licensed under MIT OR Apache-2.0.

[Unreleased]: https://github.com/stemma-sh/stemma/compare/v0.8.0...HEAD
[0.8.0]: https://github.com/stemma-sh/stemma/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/stemma-sh/stemma/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/stemma-sh/stemma/compare/v0.5.2...v0.6.0
[0.5.2]: https://github.com/stemma-sh/stemma/compare/v0.5.1...v0.5.2
[0.5.1]: https://github.com/stemma-sh/stemma/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/stemma-sh/stemma/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/stemma-sh/stemma/compare/v0.2.0...v0.4.0
[0.2.0]: https://github.com/stemma-sh/stemma/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/stemma-sh/stemma/releases/tag/v0.1.0
