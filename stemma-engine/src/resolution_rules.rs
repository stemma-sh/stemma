//! Shared paragraph-mark JOIN semantics for tracked-change resolution.
//!
//! The engine resolves accept/reject through two representation-specific
//! paths: the typed-IR model path (`tracked_model.rs`) and the byte/XML path
//! (`normalize.rs`). The two walk different representations and keep their own
//! mechanics — a generic shared walker is deliberately NOT built (RFC-0004
//! §H3: byte XML and typed IR are legitimately different, unifying the walking
//! machinery is rejected). What they must NOT keep separate copies of is the
//! DECISION RULES, which drifted repeatedly when duplicated (wave findings
//! W5-F7, W5-F10b, H3-M1). This module is the single home for those rules.
//!
//! Each rule is a pure function over plain `bool` inputs (or, for the
//! table-emptied fold, an iterator of per-row `bool` marks) — never over XML
//! elements or IR nodes. Each path extracts the marks from its own
//! representation, then consults these rules. The rules here cover the
//! paragraph-mark join family: the per-class survival table, the join
//! decision, the canonical set of zero-width body markers the join steps
//! over, and the "table emptied by resolution" composition it also steps
//! over. Range-marker policy and stacked-origin rules are later phases.

/// Whether a tracked element of the given class SURVIVES a full accept/reject
/// resolution — the four-origin-rule survival table shared by every element
/// class (block, table row, paragraph segment, byte content child).
///
/// `has_ins_mark` = carries an insertion-class mark (`w:ins`/`w:moveTo`, IR
/// `Inserted`); `has_del_mark` = carries a deletion-class mark
/// (`w:del`/`w:moveFrom`, IR `Deleted`). Both true is the STACKED state
/// (`InsertedThenDeleted` — inserted by one pending revision, deleted by
/// another): it drops in BOTH full resolutions (accept applies the deletion,
/// reject un-proposes the insertion and the nested deletion goes with it), so
/// it never survives. `keep_inserted` is the resolution direction: true =
/// accept (keep insertions, drop deletions), false = reject.
///
/// Consumers: model `block_survives_retain` and `row_survives_accept_reject`
/// (tracked_model.rs); byte `table_emptied_by_resolution` and
/// `paragraph_emptied_by_resolution` (normalize.rs).
pub(crate) fn tracked_class_survives(
    has_ins_mark: bool,
    has_del_mark: bool,
    keep_inserted: bool,
) -> bool {
    match (has_ins_mark, has_del_mark) {
        (false, false) => true,          // Normal / untracked — always survives
        (true, false) => keep_inserted,  // insertion-class — survives on accept
        (false, true) => !keep_inserted, // deletion-class — survives on reject
        (true, true) => false,           // stacked — drops in both resolutions
    }
}

/// Whether accepting a deleted cell gives its width to a neighbour.
///
/// Accepting a deleted column gives its width to a neighbour only where the
/// neighbour grew: Word merges a deleted cell that keeps a paragraph mark
/// (its marks not tracked as deleted) into the preceding cell, which takes
/// its width (the following cell when the leading column is deleted); a cell
/// deleted whole, its paragraph marks tracked as deleted too, leaves with its
/// width. Observed in Word 16 (2026-11-09) on simple 4-column grids, auto or
/// fixed layout, auto or `dxa` preferred width: text deleted but mark kept,
/// or content untracked, Accept widens the preceding cell by the deleted
/// width; text and mark deleted, Accept removes the column and its width.
/// The table's preferred width plays no part: Word's save rescales a grid to
/// a `dxa` preferred width with or without a deletion.
///
/// The tracked `delete_column` verb deletes a partition of the neighbour's
/// slot, its text deleted and its mark kept, so Accept gives the slot back.
pub(crate) fn deleted_cell_gives_width(keeps_paragraph_mark: bool) -> bool {
    keeps_paragraph_mark
}

/// Resolve physical slots of a simple tracked column grid. Accepting a
/// deletion marked in `absorb` ([`deleted_cell_gives_width`] in every row)
/// transfers its slot to the preceding surviving column, or to the
/// following survivor when deleting the leading column; any other removed
/// slot leaves with its width. Rejecting an insertion removes its slot
/// without transfer. Stacked origins never transfer a slot.
/// Returned indices refer to the original frame, so both XML and model callers
/// can update cell widths before removing their physical carriers.
pub(crate) fn resolved_column_slots(
    widths: &[u32],
    removed: &[bool],
    absorb: &[bool],
) -> Result<Vec<(usize, u32)>, String> {
    if widths.len() != removed.len() || widths.len() != absorb.len() {
        return Err("column geometry and carrier counts differ".to_owned());
    }
    let mut result: Vec<_> = widths
        .iter()
        .copied()
        .enumerate()
        .filter(|(index, _)| !removed[*index])
        .collect();
    for (index, width) in widths.iter().copied().enumerate() {
        if !removed[index] || !absorb[index] {
            continue;
        }
        let recipient = result
            .iter()
            .rposition(|(slot, _)| *slot < index)
            .or_else(|| (!result.is_empty()).then_some(0));
        if let Some(recipient) = recipient {
            result[recipient].1 = result[recipient]
                .1
                .checked_add(width)
                .ok_or_else(|| format!("column {index} width transfer overflows twips"))?;
        }
    }
    Ok(result)
}

/// Whether removing a paragraph's terminating mark JOINS its content into the
/// following paragraph for the given resolution direction (ECMA-376
/// §17.13.5.15 / §17.13.5.20): accepting a mark DELETION, or rejecting a mark
/// INSERTION, removes the break. A STACKED mark (both markers) joins in BOTH
/// full resolutions. Inputs and `keep_inserted` are as in
/// [`tracked_class_survives`]; a paragraph mark is just another tracked
/// element, so "join needed" is exactly "the mark does not survive this
/// resolution" — expressed directly here to mirror both call sites.
///
/// Consumers: model `para_mark_needs_merge` (tracked_model.rs); byte
/// `join_mark_resolved_paragraphs` (normalize.rs).
pub(crate) fn para_mark_join_needed(
    has_ins_mark: bool,
    has_del_mark: bool,
    keep_inserted: bool,
) -> bool {
    let stacked = has_ins_mark && has_del_mark;
    stacked || (keep_inserted && has_del_mark) || (!keep_inserted && has_ins_mark)
}

/// Local element-names (WordprocessingML `w:` namespace) of the ZERO-WIDTH
/// BODY MARKERS a paragraph-mark join steps over: bookmarks, comment /
/// permission / move / customXml range delimiters, and proof errors. These
/// occupy no space in the resolved flow, so removing a paragraph break joins
/// ACROSS them — but never across a content sibling (a table, an sdt, …).
///
/// This is the single canonical enumeration of that marker kind. Each
/// resolution path keeps its own extraction and consults this table: the byte
/// path (`is_zero_width_body_marker`, normalize.rs) drives its namespace-aware
/// `is_w_tag` off these names over raw XML siblings; the model path
/// (`is_zero_width_marker_block`, tracked_model.rs) strips the prefix off its
/// `OpaqueBlock(Unknown(tag))` and calls [`is_zero_width_body_marker_name`].
/// One list means the two paths cannot diverge on which joins happen.
pub(crate) const ZERO_WIDTH_BODY_MARKER_NAMES: &[&str] = &[
    "bookmarkStart",
    "bookmarkEnd",
    "commentRangeStart",
    "commentRangeEnd",
    "proofErr",
    "permStart",
    "permEnd",
    "moveFromRangeStart",
    "moveFromRangeEnd",
    "moveToRangeStart",
    "moveToRangeEnd",
    "customXmlInsRangeStart",
    "customXmlInsRangeEnd",
    "customXmlDelRangeStart",
    "customXmlDelRangeEnd",
    "customXmlMoveFromRangeStart",
    "customXmlMoveFromRangeEnd",
    "customXmlMoveToRangeStart",
    "customXmlMoveToRangeEnd",
];

/// Whether `local` (a `w:`-namespace element local-name, prefix already
/// stripped by the caller) names a zero-width body marker — membership in
/// [`ZERO_WIDTH_BODY_MARKER_NAMES`]. The model path consults this directly; the
/// byte path iterates the slice through its namespace-aware `is_w_tag` instead
/// (it holds an `Element`, not a bare local-name).
pub(crate) fn is_zero_width_body_marker_name(local: &str) -> bool {
    ZERO_WIDTH_BODY_MARKER_NAMES.contains(&local)
}

/// Whether a full resolution EMPTIES a table completely: it had at least one
/// row and NONE survive this resolution, so the revision pass then removes the
/// rowless shell (§17.4.37 / Word parity). A paragraph-mark join must treat
/// such a table as absent and step ACROSS it (Word rejoins one logical
/// paragraph split around an all-tracked table). A table that was already
/// rowless is untouched by the resolution and does NOT vanish, so it does not
/// satisfy this rule.
///
/// `rows` yields whether each row survives ([`row_survives_resolution`]), so
/// this composition cannot drift from the row retain filter that actually
/// drops the rows. Consumers: byte `table_emptied_by_resolution`
/// (normalize.rs); model `table_emptied_by_accept_reject` (tracked_model.rs).
pub(crate) fn table_emptied_by_resolution(rows: impl Iterator<Item = bool>) -> bool {
    let mut saw_row = false;
    for survives in rows {
        saw_row = true;
        if survives {
            return false;
        }
    }
    saw_row
}

/// Whether a table row survives a full resolution: its own marks survive
/// (`w:trPr/w:ins`, `w:trPr/w:del`) and so does at least one of its cells
/// (`w:tcPr/w:cellIns`, `w:tcPr/w:cellDel`), each through
/// [`tracked_class_survives`]. A row whose every cell is a tracked cell
/// deletion loses all of them on Accept, and a row holds at least one cell
/// (CT_Row, §17.4.79: `tc` one or more), so the row goes with its cells.
/// Consumers: byte accept/reject passes and `table_emptied_by_resolution`
/// (normalize.rs); model `row_survives_accept_reject` (tracked_model.rs).
pub(crate) fn row_survives_resolution(
    row: (bool, bool),
    cells: impl IntoIterator<Item = (bool, bool)>,
    keep_inserted: bool,
) -> bool {
    tracked_class_survives(row.0, row.1, keep_inserted)
        && cells
            .into_iter()
            .any(|(has_ins, has_del)| tracked_class_survives(has_ins, has_del, keep_inserted))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_slots_transfer_only_deleted_geometry() {
        assert_eq!(
            resolved_column_slots(
                &[1000, 1000, 2200],
                &[true, false, false],
                &[true, false, false]
            )
            .unwrap(),
            vec![(1, 2000), (2, 2200)]
        );
        assert_eq!(
            resolved_column_slots(
                &[1800, 1000, 1000, 2400],
                &[false, false, true, false],
                &[false, false, true, false]
            )
            .unwrap(),
            vec![(0, 1800), (1, 2000), (3, 2400)]
        );
        // Rejecting an insertion and dropping a stacked-origin carrier do not
        // donate its width to a pre-existing neighboring cell.
        assert_eq!(
            resolved_column_slots(
                &[1800, 1000, 2400],
                &[false, true, false],
                &[false, false, false]
            )
            .unwrap(),
            vec![(0, 1800), (2, 2400)]
        );
        assert!(resolved_column_slots(&[u32::MAX, 1], &[false, true], &[false, true]).is_err());
        assert!(resolved_column_slots(&[1], &[], &[]).is_err());
    }

    // Post-condition: the four-origin-rule survival table. Justified from the
    // domain (accept keeps insertions/drops deletions; reject the converse;
    // stacked drops in both), not from either path's implementation.
    #[test]
    fn class_survival_truth_table() {
        // (has_ins, has_del, keep_inserted) → survives
        assert!(tracked_class_survives(false, false, true)); // normal, accept
        assert!(tracked_class_survives(false, false, false)); // normal, reject
        assert!(tracked_class_survives(true, false, true)); // insertion kept on accept
        assert!(!tracked_class_survives(true, false, false)); // insertion dropped on reject
        assert!(!tracked_class_survives(false, true, true)); // deletion applied on accept
        assert!(tracked_class_survives(false, true, false)); // deletion undone on reject
        assert!(!tracked_class_survives(true, true, true)); // stacked drops on accept
        assert!(!tracked_class_survives(true, true, false)); // stacked drops on reject
    }

    // Post-condition: a paragraph mark joins exactly when it does not survive.
    #[test]
    fn join_needed_is_mark_not_surviving() {
        for &has_ins in &[false, true] {
            for &has_del in &[false, true] {
                for &keep in &[false, true] {
                    let is_mark = has_ins || has_del;
                    let expected = is_mark && !tracked_class_survives(has_ins, has_del, keep);
                    assert_eq!(
                        para_mark_join_needed(has_ins, has_del, keep),
                        expected,
                        "join_needed({has_ins},{has_del},{keep})"
                    );
                }
            }
        }
    }

    // Post-condition: the canonical zero-width marker set. Justified from the
    // domain (these delimiters occupy no space in the flow), not from either
    // path's list. Membership and non-membership both pinned.
    #[test]
    fn zero_width_marker_names_membership() {
        for name in ZERO_WIDTH_BODY_MARKER_NAMES {
            assert!(is_zero_width_body_marker_name(name), "{name} should match");
        }
        // Content siblings a join must NOT step over.
        for name in ["p", "tbl", "sdt", "r", "ins", "del", "moveFrom", "moveTo"] {
            assert!(
                !is_zero_width_body_marker_name(name),
                "{name} must not be a zero-width marker"
            );
        }
    }

    // Post-condition: a table is emptied iff it had rows and none survive.
    // Justified from the domain (an all-dropped table leaves a rowless shell
    // the revision pass removes), not from either path's fold.
    #[test]
    fn table_emptied_composition() {
        // No rows: never "emptied by resolution" (nothing to drop).
        assert!(!table_emptied_by_resolution(std::iter::empty()));
        // A surviving row keeps the table.
        assert!(!table_emptied_by_resolution([true].into_iter()));
        // Every row drops → emptied.
        assert!(table_emptied_by_resolution([false, false].into_iter()));
        // A mix with one survivor keeps the table.
        assert!(!table_emptied_by_resolution([false, true].into_iter()));
    }

    // Post-condition: a row survives iff its own marks survive and at least
    // one cell survives (CT_Row needs a cell; a deleted cell goes on Accept).
    #[test]
    fn row_survival_needs_a_surviving_cell() {
        let normal = (false, false);
        let deleted = (false, true);
        let inserted = (true, false);
        for &keep in &[false, true] {
            assert!(row_survives_resolution(normal, [normal], keep));
            assert!(row_survives_resolution(normal, [deleted, normal], keep));
            // A deleted row drops on Accept, an inserted one on Reject.
            assert_eq!(row_survives_resolution(deleted, [normal], keep), !keep);
            assert_eq!(row_survives_resolution(inserted, [normal], keep), keep);
        }
        // Every cell deleted: the row goes on Accept, stays on Reject.
        assert!(!row_survives_resolution(normal, [deleted, deleted], true));
        assert!(row_survives_resolution(normal, [deleted, deleted], false));
        // Every cell inserted: the row goes on Reject, stays on Accept.
        assert!(row_survives_resolution(normal, [inserted], true));
        assert!(!row_survives_resolution(normal, [inserted], false));
    }
}
