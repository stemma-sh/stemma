//! Build one complete-row target permutation before any tracked lowering.
use crate::domain::{
    BlockNode, CanonDoc, RevisionInfo, TableDiffResult, TableNode, TableRowAlignment,
};
use crate::edit::{EditError, MaterializationMode};
use similar::{Algorithm, DiffOp};

/// The caller already supplied exact correspondence. Compare unique source
/// indices, never text signatures: identical labels cannot erase a relocation.
fn row_plan(row_order: &[usize]) -> Vec<TableRowAlignment> {
    let source: Vec<_> = (0..row_order.len()).collect();
    let mut rows = Vec::new();
    for operation in similar::capture_diff_slices(Algorithm::Patience, &source, row_order) {
        match operation {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => {
                rows.extend((0..len).map(|i| TableRowAlignment::Matched {
                    old_row: old_index + i,
                    new_row: new_index + i,
                }));
            }
            DiffOp::Delete {
                old_index, old_len, ..
            } => {
                rows.extend((0..old_len).map(|i| TableRowAlignment::Deleted {
                    old_row: old_index + i,
                }));
            }
            DiffOp::Insert {
                new_index, new_len, ..
            } => {
                rows.extend((0..new_len).map(|i| TableRowAlignment::Inserted {
                    new_row: new_index + i,
                }));
            }
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                rows.extend((0..old_len).map(|i| TableRowAlignment::Deleted {
                    old_row: old_index + i,
                }));
                rows.extend((0..new_len).map(|i| TableRowAlignment::Inserted {
                    new_row: new_index + i,
                }));
            }
        }
    }
    rows
}

/// Lower an explicit permutation through the existing native carrier law.
/// This producer supplies correspondence; it does not modify inference or
/// materialization, and it never changes strategy based on document content.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply(
    doc: &mut CanonDoc,
    block_index: usize,
    base: &TableNode,
    target: &TableNode,
    row_order: &[usize],
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    mode: MaterializationMode,
    step_index: usize,
) -> Result<(), EditError> {
    if row_order.iter().copied().eq(0..base.rows.len()) {
        return Ok(());
    }
    if mode == MaterializationMode::Direct {
        doc.blocks[block_index].block = BlockNode::from(target.clone());
        return Ok(());
    }
    let canonical = |table: &TableNode| {
        crate::table::canonicalize_table(table).map_err(|reason| {
            EditError::UnsupportedParagraphStructure {
                block_id: base.id.clone(),
                step_index,
                reason: format!("explicit row permutation canonicalization failed: {reason}"),
            }
        })
    };
    let plan = TableDiffResult {
        old_table: canonical(base)?,
        new_table: canonical(target)?,
        row_alignment: row_plan(row_order),
        // Matched rows are exact source clones; inserted/deleted whole-row
        // carriers own their complete cell payload. No cell edits are authored.
        cell_diffs: Vec::new(),
    };
    let prepared = super::table_row_resources::prepare(doc, target, &plan, &base.id, step_index)?;
    let first_annotation = *rev_counter;
    crate::tracked_model::apply_table_structure_changed(
        &mut doc.blocks,
        &base.id,
        &prepared,
        &plan,
        revision,
        rev_counter,
        "explicit-row-permutation",
        &super::super::ExplicitEditPlanResolver,
    )
    .map_err(|error| EditError::UnsupportedParagraphStructure {
        block_id: base.id.clone(),
        step_index,
        reason: format!(
            "explicit row permutation lowering failed: {} ({})",
            error.message, error.context
        ),
    })?;

    // Physical row/content envelopes retain their distinct annotation IDs.
    // The producer knows that this entire permutation is one intention; the
    // shared carrier compiler cannot infer that from separate row deltas.
    crate::import::try_mint_identities(doc).map_err(|error| {
        EditError::RevisionIdentityCollision {
            candidate: error.candidate,
            records: error.records,
        }
    })?;
    let annotations = first_annotation..*rev_counter;
    let mut proposal = None;
    crate::import::for_each_rev_carrier_mut(doc, &mut |carrier| {
        if annotations.contains(&carrier.wire_id) {
            assert_eq!(carrier.author, revision.author.clone().unwrap_or_default());
            assert_eq!(carrier.date, revision.date);
            assert_ne!(*carrier.identity, 0, "permutation carrier was not minted");
            let identity = *proposal.get_or_insert(*carrier.identity);
            *carrier.identity = identity;
        }
    });
    assert!(
        proposal.is_some(),
        "nontrivial permutation emitted no carriers"
    );
    Ok(())
}

/// `row_order[target_index]` identifies a row in the unchanged source table.
/// Every source row must occur exactly once: no content reconstruction, loss,
/// duplication, or intervening Accept/Reject operation is part of this intent.
pub(super) fn reorder(
    table: &mut TableNode,
    row_order: &[usize],
    step_index: usize,
) -> Result<(), EditError> {
    let refuse = |reason: String| EditError::UnsupportedTableRowMove {
        block_id: table.id.clone(),
        reason,
        step_index,
    };
    if row_order.len() != table.rows.len() {
        return Err(refuse(format!(
            "row_order must contain each of the {} source rows exactly once; received {} indices",
            table.rows.len(),
            row_order.len()
        )));
    }
    let mut seen = vec![false; table.rows.len()];
    for &index in row_order {
        let Some(present) = seen.get_mut(index) else {
            return Err(refuse(format!(
                "row_order index {index} is outside source row count {}",
                table.rows.len()
            )));
        };
        if *present {
            return Err(refuse(format!(
                "row_order repeats source row index {index}"
            )));
        }
        *present = true;
    }
    if row_order.iter().copied().eq(0..table.rows.len()) {
        return Ok(());
    }
    if let Some(reason) = super::move_row::merge_groups_broken(table, row_order) {
        return Err(refuse(reason));
    }
    if !table.range_markers.is_empty() {
        return Err(refuse(
            "table-level range boundaries require an explicit range-preserving permutation".into(),
        ));
    }
    table.rows = row_order
        .iter()
        .map(|&index| table.rows[index].clone())
        .collect();
    Ok(())
}
