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
//! Each rule is a pure function over plain `bool` inputs — never over XML
//! elements or IR nodes. Each path extracts `(has_ins_mark, has_del_mark)`
//! from its own representation, then consults these rules. This phase covers
//! ONLY the paragraph-mark join family; retain filters beyond what the join
//! needs, range-marker policy, and stacked-origin rules are later phases.

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
