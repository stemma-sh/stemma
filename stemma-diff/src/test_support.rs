//! Compatibility helpers for integration tests that predate the crate split.
//!
//! This module is feature-gated and is not part of the comparison product
//! surface. It lets existing behavioral tests follow comparison downstream
//! without restoring comparison methods or models to the engine.

use stemma::api::Document;
use stemma::domain::{CanonDoc, RevisionInfo};
use stemma::runtime::{
    ApplyResult, DocxRuntime, ExportMode, ExportOptions, RuntimeError, SimpleRuntime,
};
use stemma::{DocHandle, TransactionMeta};

pub use crate::model::{DiffChange, DocumentDiff};

pub struct FlattenedPendingRevisions {
    pub base: Vec<stemma::tracked_model::PendingRevisionAuthor>,
    pub target: Vec<stemma::tracked_model::PendingRevisionAuthor>,
}

pub struct CompareAndRedlineResult {
    pub redline_bytes: Vec<u8>,
    pub flattened_pending_revisions: FlattenedPendingRevisions,
}

pub trait DocumentComparisonExt {
    fn diff(&self, target: &Document) -> Result<Document, RuntimeError>;
    fn diff_as(&self, target: &Document, author: &str) -> Result<Document, RuntimeError>;
}

impl DocumentComparisonExt for Document {
    fn diff(&self, target: &Document) -> Result<Document, RuntimeError> {
        crate::diff(self, target)
    }

    fn diff_as(&self, target: &Document, author: &str) -> Result<Document, RuntimeError> {
        crate::diff_as(self, target, author)
    }
}

pub trait RuntimeComparisonExt {
    fn diff(
        &self,
        base_handle: &DocHandle,
        target_handle: &DocHandle,
    ) -> Result<DocumentDiff, RuntimeError>;

    fn diff_and_redline(
        &self,
        base_handle: &DocHandle,
        target_handle: &DocHandle,
        meta: TransactionMeta,
    ) -> Result<ApplyResult, RuntimeError>;

    fn compare_and_redline(
        &self,
        base_handle: &DocHandle,
        target_handle: &DocHandle,
        meta: TransactionMeta,
    ) -> Result<CompareAndRedlineResult, RuntimeError>;
}

impl RuntimeComparisonExt for SimpleRuntime {
    fn diff(
        &self,
        base_handle: &DocHandle,
        target_handle: &DocHandle,
    ) -> Result<DocumentDiff, RuntimeError> {
        let base = self.view(base_handle)?;
        let target = self.view(target_handle)?;
        crate::compiler::diff_documents(&base.canonical, &target.canonical)
            .map_err(crate::diff_error)
    }

    fn diff_and_redline(
        &self,
        base_handle: &DocHandle,
        target_handle: &DocHandle,
        meta: TransactionMeta,
    ) -> Result<ApplyResult, RuntimeError> {
        let base_bytes = self.export_docx(base_handle, ExportMode::Redline)?;
        let target_bytes = self.export_docx(target_handle, ExportMode::Redline)?;
        let base = Document::parse(&base_bytes)?;
        let target = Document::parse(&target_bytes)?;
        let comparison = crate::diff_snapshots(
            base.snapshot(),
            target.snapshot(),
            Some(meta.author),
            meta.timestamp_utc,
        )?;
        self.store_derived_snapshot(base_handle, comparison.snapshot)
    }

    fn compare_and_redline(
        &self,
        base_handle: &DocHandle,
        target_handle: &DocHandle,
        meta: TransactionMeta,
    ) -> Result<CompareAndRedlineResult, RuntimeError> {
        let base_bytes = self.export_docx(base_handle, ExportMode::Redline)?;
        let target_bytes = self.export_docx(target_handle, ExportMode::Redline)?;
        let base = Document::parse(&base_bytes)?;
        let target = Document::parse(&target_bytes)?;
        let flattened_pending_revisions = FlattenedPendingRevisions {
            base: stemma::tracked_model::pending_revision_authors(&base.snapshot().canonical),
            target: stemma::tracked_model::pending_revision_authors(&target.snapshot().canonical),
        };
        let comparison = crate::diff_snapshots(
            base.snapshot(),
            target.snapshot(),
            Some(meta.author),
            meta.timestamp_utc,
        )?;
        let redline = base.with_derived_snapshot(comparison.snapshot);
        let redline_bytes = redline.serialize(&ExportOptions::default())?;
        Ok(CompareAndRedlineResult {
            redline_bytes,
            flattened_pending_revisions,
        })
    }
}

pub fn diff_documents(base: &CanonDoc, target: &CanonDoc) -> Result<DocumentDiff, String> {
    crate::compiler::diff_documents(base, target).map_err(|error| error.to_string())
}

pub fn project_tracked_document(
    document: &CanonDoc,
    image_lookup: &std::collections::HashMap<String, String>,
) -> Vec<crate::FullDocBlock> {
    crate::compiler::project_tracked_document(document, image_lookup)
}

pub fn merge_diff(
    base: &CanonDoc,
    target: &CanonDoc,
    diff: &DocumentDiff,
    revision: &RevisionInfo,
) -> Result<crate::materialize::MergeResult, crate::materialize::MergeError> {
    crate::materialize::materialize_change_plan(
        base,
        target,
        diff,
        revision,
        &crate::materialize::ComparisonPlanResolver,
    )
}
