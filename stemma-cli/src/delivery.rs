//! `tracked-delivery-v0`: the one delivery policy. `apply`/`execute` enforce
//! it on their serialized candidate before committing a DOCX, and `verify`
//! checks it on any before/after pair, so a successful `execute` and a
//! `verify` of its output cannot disagree.
//!
//! The policy, over an engine audit of the exact bytes:
//! - the result validates;
//! - no committed content changed without a covering tracked change;
//! - every block outside the changes is provably untouched;
//! - no pre-existing revision was resolved or changed outside a tracked
//!   edit. A revision edited inside by the new tracked changes (the engine's
//!   `EditedInside`: rejecting exactly the new revisions restores it) is
//!   preserved, not changed: it stays the other author's, still pending.

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

/// `None` when the audited result passes `tracked-delivery-v0`; otherwise the
/// first failed invariant, with its count.
pub(crate) fn tracked_delivery_failure(report: &AuditReport) -> Option<String> {
    if !report.validator.ok {
        return Some("the result failed structural validation".to_string());
    }
    if !report.direct_changes.is_empty() {
        return Some(format!(
            "the result contains {} untracked direct change(s)",
            report.direct_changes.len()
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
