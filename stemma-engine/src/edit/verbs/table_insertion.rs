//! A newly authored table uses the same native row carriers it will reopen as.
use crate::domain::{BlockNode, RevisionInfo, TrackingStatus};

/// One requested table insertion is one review proposal with a carrier on
/// each row (ISO 29500-1 §17.13.5.17). Sharing the revision groups those
/// carriers; inventing an additional block carrier would make the persisted
/// carrier walk disagree with the native document.
pub(crate) fn insertion_status(block: &mut BlockNode, revision: &RevisionInfo) -> TrackingStatus {
    if let BlockNode::Table(table) = block {
        for row in &mut table.rows {
            assert!(
                row.tracking_status.is_none(),
                "new table row already has tracking status"
            );
            row.tracking_status = Some(TrackingStatus::Inserted(revision.clone()));
        }
        TrackingStatus::Normal
    } else {
        TrackingStatus::Inserted(revision.clone())
    }
}
