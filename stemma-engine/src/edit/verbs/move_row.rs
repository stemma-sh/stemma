//! Whole-row target transformation. Native lowering stays engine-owned.
use super::table_ops::TableInsertPosition;
use crate::domain::{TableNode, VerticalMerge};

/// Why the row order `order` (new position -> source row) would break a
/// vertical merge, or `None` when it keeps every merge group. A merge group is
/// a restart cell and the continue cells directly below it in the same grid
/// column; it survives exactly when each continuing row still sits directly
/// below the row it continued, which the check states per grid column.
pub(super) fn merge_groups_broken(table: &TableNode, order: &[usize]) -> Option<String> {
    let mut position = vec![0; order.len()];
    for (new, &source) in order.iter().enumerate() {
        position[source] = new;
    }
    for (index, row) in table.rows.iter().enumerate() {
        let mut column = row.grid_before;
        for cell in &row.cells {
            if cell.v_merge == VerticalMerge::Continue
                && (index == 0 || position[index] != position[index - 1] + 1)
            {
                return Some(format!(
                    "row {index} continues a vertically merged cell (grid column {column}) of \
                     the row above it, which this order would separate; move the whole merge \
                     group together"
                ));
            }
            column += cell.grid_span.max(1);
        }
    }
    None
}
use crate::edit::EditError;

/// Indices have been checked against the original table. A relocation keeps
/// every row/cell/content identity and property, changing only row order.
pub(super) fn reorder(
    table: &mut TableNode,
    row_index: usize,
    ref_row: usize,
    position: TableInsertPosition,
    step_index: usize,
) -> Result<(), EditError> {
    assert!(row_index < table.rows.len() && ref_row < table.rows.len());
    let boundary = ref_row + usize::from(position == TableInsertPosition::After);
    let destination = boundary - usize::from(row_index < boundary);
    if destination == row_index {
        return Ok(());
    }
    // Moving one row must not detach a vertically merged cell from the cell it
    // continues, or silently change the coverage of a table-level range.
    // Horizontal grid spans stay attached to their complete row and are safe.
    let mut order: Vec<usize> = (0..table.rows.len()).collect();
    let row = order.remove(row_index);
    order.insert(destination, row);
    if let Some(reason) = merge_groups_broken(table, &order) {
        return Err(EditError::UnsupportedTableRowMove {
            block_id: table.id.clone(),
            step_index,
            reason,
        });
    }
    if !table.range_markers.is_empty() {
        return Err(EditError::UnsupportedTableRowMove {
            block_id: table.id.clone(),
            step_index,
            reason: "table-level range boundaries require an explicit range-preserving relocation"
                .to_string(),
        });
    }
    let row = table.rows.remove(row_index);
    table.rows.insert(destination, row);
    Ok(())
}
