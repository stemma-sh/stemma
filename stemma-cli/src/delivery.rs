//! The delivery policy. `apply`/`execute` enforce `tracked-delivery-v0` on
//! their serialized candidate before committing a DOCX, and `verify` checks
//! a policy on any before/after pair, so a successful `execute` and a
//! `verify` of its output cannot disagree.
//!
//! The policy, over an engine audit of the exact bytes:
//! - the result validates;
//! - no committed content changed without a covering tracked change (a new
//!   comment is not a change to the content: it is counted and allowed; a
//!   changed or removed comment is not);
//! - every block outside the changes is provably untouched;
//! - no pre-existing revision was resolved or changed outside a tracked
//!   edit. A revision edited inside by the new tracked changes (the engine's
//!   `EditedInside`: rejecting exactly the new revisions restores it) is
//!   preserved, not changed: it stays the other author's, still pending.
//!
//! The two versions differ in what "untouched" means, which is decided when
//! the audit is made (`stemma::audit::UntouchedEquality`):
//! - `tracked-delivery-v0`: the same markup, down to revision-save ids and
//!   run boundaries. The reading for a document this engine wrote.
//! - `tracked-delivery-v1` (`verify`'s default): the same text and the same
//!   resolved formatting. The reading for a document another program saved,
//!   which rewrites markup everywhere without changing the document. A
//!   result that passes v0 passes v1.

use stemma::audit::{AuditReport, RevisionDisposition};

/// What the pre-existing revisions became, as the policy counts them.
pub(crate) struct PreexistingOutcome {
    /// Present with identical content.
    pub(crate) untouched: usize,
    /// Edited inside by the new tracked changes only.
    pub(crate) edited_inside: usize,
    /// Resolved, or changed in a way the new revisions do not explain.
    pub(crate) modified_or_resolved: usize,
}

pub(crate) fn preexisting_outcome(report: &AuditReport) -> PreexistingOutcome {
    let mut outcome = PreexistingOutcome {
        untouched: 0,
        edited_inside: 0,
        modified_or_resolved: 0,
    };
    for revision in &report.preexisting_revisions {
        match revision.disposition {
            RevisionDisposition::Untouched => outcome.untouched += 1,
            RevisionDisposition::EditedInside { .. } => outcome.edited_inside += 1,
            RevisionDisposition::Modified { .. } | RevisionDisposition::Resolved => {
                outcome.modified_or_resolved += 1;
            }
        }
    }
    outcome
}

/// The comments the result adds. A new comment is an attributed annotation
/// beside the text, not a change to it: the policy counts it and lets it
/// pass. A comment that was changed or removed is a direct change like any
/// other.
pub(crate) fn added_comments(report: &AuditReport) -> usize {
    report
        .direct_changes
        .iter()
        .filter(|change| {
            change.kind == stemma::audit::DirectChangeKind::StoryInserted
                && matches!(change.story, stemma::StoryScope::Comment { .. })
        })
        .count()
}

/// `None` when the audited result passes the delivery policy; otherwise the
/// first failed invariant, with its count.
pub(crate) fn tracked_delivery_failure(report: &AuditReport) -> Option<String> {
    if !report.validator.ok {
        return Some("the result failed structural validation".to_string());
    }
    let untracked = report.direct_changes.len() - added_comments(report);
    if untracked != 0 {
        return Some(format!(
            "the result contains {untracked} untracked direct change(s)"
        ));
    }
    if !report.untouched.violations.is_empty() {
        return Some(format!(
            "the result contains {} unexplained untouched-scope violation(s)",
            report.untouched.violations.len()
        ));
    }
    let changed = preexisting_outcome(report).modified_or_resolved;
    if changed != 0 {
        return Some(format!(
            "the result resolved or changed {changed} pre-existing revision(s) other than by \
             a tracked edit inside them"
        ));
    }
    None
}
