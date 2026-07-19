//! The public `Document` facade — the durable, session-free API surface.
//!
//! This is the handle the domain model (`docs/domain-model.md` §4, §9) calls
//! the public vocabulary. It wraps an internal [`EditSnapshot`] (the IR plus
//! the package scaffold) and exposes the verbs as pure value transformations:
//! every verb returns a new [`Document`] rather than mutating in place.
//!
//! Unlike [`crate::SimpleRuntime`], a [`Document`] owns no handle store, no
//! TTL, no shared map — it is just a value. Sessions and transport concerns
//! live in the runtime; correctness lives here.

use std::sync::Arc;

use crate::audit::{AuditReport, audit_documents};
use crate::domain::CanonDoc;
pub use crate::domain::{DocProtectEdit, DocumentProtection};
use crate::edit::EditTransaction;
use crate::runtime::{
    EditSnapshot, ExportOptions, Resolution, RuntimeError, ValidationIssue, ValidationIssueCode,
    ValidationReport, serialize_snapshot, snapshot_from_docx_bytes, style_table_from_docx,
    validate_docx_report,
};
use crate::view::build_document_view;

// The designed read projection (`docs/domain-model.md` §4, §9). Re-exported
// here next to `Document` so callers reach it as `stemma::api::DocumentView`,
// matching where the verbs live. The types are defined in `crate::view`,
// independent of the IR.
pub use crate::view::{
    BlockRole, BlockView, DocumentOutline, DocumentView, OpaqueAnchorKind, OutlineEntry,
    RevisionView, SegmentView, TrackStatus, WindowError, build_document_view_from_canon,
};

/// The public document handle: content plus attributed deltas.
///
/// Hold this, transactions, and the DOCX bytes — those are the durable
/// vocabulary. The wrapped [`EditSnapshot`] is engine-version-bound and is not
/// part of the API (see [`Document::snapshot`] for the explicit escape hatch).
///
/// # Example
///
/// Parse a DOCX, author one tracked edit as a typed transaction, and serialize
/// validated bytes. Every verb returns a new `Document` — nothing mutates in
/// place. See `stemma-engine/examples/my_first_edit.rs` for the annotated form.
///
/// ```
/// use stemma::api::{Document, validate};
/// use stemma::edit_v4::parse_transaction;
/// use stemma::ExportOptions;
///
/// let bytes = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/simple-text/before.docx"));
/// let doc = Document::parse(bytes).expect("parse DOCX bytes");
///
/// // Address a block by its stable id, and pin the edit with its staleness
/// // `guard` — both read from the designed projection, never from raw XML.
/// let view = doc.read();
/// let block = &view.blocks[0];
/// let txn_json = format!(
///     r#"{{ "ops": [ {{ "op": "replace", "target": "{}", "guard": "{}",
///            "content": {{ "type": "paragraph",
///                          "content": [ {{ "type": "text", "text": "Rewritten." }} ] }} }} ],
///          "revision": {{ "author": "Docs" }} }}"#,
///     block.id, block.guard
/// );
/// let txn = parse_transaction(&txn_json).unwrap().into_edit_transaction().unwrap();
/// let edited = doc.apply(&txn).expect("apply the tracked edit");
///
/// // One file, three readings: accept-all shows the new text, reject-all the old.
/// assert!(edited.read_accepted().unwrap().to_text().contains("Rewritten."));
/// assert!(!edited.read_rejected().unwrap().to_text().contains("Rewritten."));
///
/// let out = edited.serialize(&ExportOptions::default()).expect("validated DOCX bytes");
/// assert!(validate(&out).ok);
/// ```
pub struct Document {
    snapshot: EditSnapshot,
    /// The open-time canonical tree, retained for [`Document::review`]
    /// (RFC 0001). An `Arc` share of the IR ONLY — the package scaffold is
    /// deliberately NOT retained, so this is a refcount bump at parse, not
    /// a copy of the media payload. Set once by [`Document::parse`];
    /// every verb carries it forward unchanged (re-parsing is the only
    /// baseline reset).
    baseline: Arc<CanonDoc>,
}

/// The render format for a windowed read ([`Document::window`]). A closed set
/// (no catch-all), so a caller picks one of the three slice renderers — and a
/// transport edge (e.g. the MCP server) parses its wire string into this enum
/// with an explicit no-fallback error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowFormat {
    /// Plain text (one U+FFFC per opaque anchor, blocks joined by a blank line).
    Text,
    /// Extended-markdown comprehension surface (id-bearing tagged prose).
    Markdown,
    /// HTML (id/data-id per block, escaped text, ins/del, anchor spans).
    Html,
}

impl Document {
    /// Decode DOCX bytes into the typed model. Fails fast on anything
    /// unrecognized (encrypted package, missing `word/document.xml`, etc.).
    ///
    /// ```
    /// use stemma::api::Document;
    /// let bytes = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/simple-text/before.docx"));
    /// let doc = Document::parse(bytes).expect("valid DOCX package");
    /// assert!(!doc.read().blocks.is_empty());
    /// ```
    pub fn parse(bytes: &[u8]) -> Result<Document, RuntimeError> {
        let snapshot = snapshot_from_docx_bytes(bytes)?;
        let baseline = Arc::clone(&snapshot.canonical);
        Ok(Document { snapshot, baseline })
    }

    /// A verb result: new snapshot, SAME baseline. Verbs never reset the
    /// baseline; only [`Document::parse`] sets it.
    fn derived(&self, snapshot: EditSnapshot) -> Document {
        Document {
            snapshot,
            baseline: Arc::clone(&self.baseline),
        }
    }

    /// Author new deltas by applying a transaction. Precondition-checked and
    /// atomic; returns a new document.
    ///
    /// ```
    /// # use stemma::api::Document;
    /// # use stemma::edit_v4::parse_transaction;
    /// let bytes = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/simple-text/before.docx"));
    /// let doc = Document::parse(bytes).unwrap();
    /// let block = &doc.read().blocks[0];
    /// let json = format!(
    ///     r#"{{ "ops": [ {{ "op": "replace", "target": "{}", "guard": "{}",
    ///            "content": {{ "type": "paragraph",
    ///                          "content": [ {{ "type": "text", "text": "New." }} ] }} }} ],
    ///          "revision": {{ "author": "Docs" }} }}"#,
    ///     block.id, block.guard);
    /// let txn = parse_transaction(&json).unwrap().into_edit_transaction().unwrap();
    /// let edited = doc.apply(&txn).unwrap();
    /// assert!(edited.read_accepted().unwrap().to_text().contains("New."));
    /// ```
    pub fn apply(&self, txn: &EditTransaction) -> Result<Document, RuntimeError> {
        Ok(self.derived(self.snapshot.apply(txn)?))
    }

    /// [`Document::apply`], but refusing a write whose `txn.revision.author`
    /// impersonates one of the document's ORIGIN authors — the authors
    /// already present in the redline this `Document` was parsed from (see
    /// [`crate::runtime::EditSnapshot::guard_author`]). Transports that
    /// attribute a write to a caller-supplied author (an HTTP `/apply`
    /// endpoint, an MCP tool call) should call this instead of `apply`;
    /// `allow_existing_author=true` deliberately continues an existing
    /// author's own work.
    pub fn apply_authored(
        &self,
        txn: &EditTransaction,
        allow_existing_author: bool,
    ) -> Result<Document, RuntimeError> {
        Ok(self.derived(self.snapshot.apply_authored(txn, allow_existing_author)?))
    }

    /// Set a package-level **core** document property (`docProps/core.xml`):
    /// `title`, `creator`/`author`, `subject`, `description`, `keywords`,
    /// `lastModifiedBy`, `category`, `created`, `modified`.
    ///
    /// This is an untracked, package-level mutation, NOT an edit transaction —
    /// metadata lives in its own OPC part and carries no tracked-change markup
    /// (it is therefore not replayable via [`Document::apply`]). The body
    /// (`word/document.xml`) is left byte-for-byte unchanged. An unknown field
    /// name is rejected.
    pub fn set_core_property(&self, field: &str, value: &str) -> Result<Document, RuntimeError> {
        Ok(self.derived(self.snapshot.set_core_property(field, value)?))
    }

    /// Set a package-level **custom** document property (`docProps/custom.xml`)
    /// to a string value. Same untracked, non-replayable, package-level
    /// contract as [`Document::set_core_property`].
    pub fn set_custom_property(&self, name: &str, value: &str) -> Result<Document, RuntimeError> {
        Ok(self.derived(self.snapshot.set_custom_property(name, value)?))
    }

    /// Read a package-level core document property, `None` if absent.
    pub fn core_property(&self, field: &str) -> Result<Option<String>, RuntimeError> {
        self.snapshot.core_property(field)
    }

    /// Read a package-level custom document property by name, `None` if absent.
    pub fn custom_property(&self, name: &str) -> Result<Option<String>, RuntimeError> {
        self.snapshot.custom_property(name)
    }

    /// Set the package-level **update-fields-on-open** setting (`w:updateFields`,
    /// §17.15.1.81): when `Some(true)`, Word recomputes every field result
    /// (REF/PAGEREF/TOC/SEQ/…) the next time the document is opened; `Some(false)`
    /// is explicitly off; `None` removes the setting.
    ///
    /// This is an untracked, package-level mutation, NOT an edit transaction —
    /// the setting lives in `word/settings.xml` and carries no tracked-change
    /// markup (so it is not replayable via [`Document::apply`]). It does **not**
    /// recompute field results in-engine; it sets the flag that asks Word to
    /// refresh on open. The body (`word/document.xml`) is left unchanged. If the
    /// package has no settings part yet, a minimal valid one is synthesized.
    pub fn set_update_fields_on_open(
        &self,
        desired: Option<bool>,
    ) -> Result<Document, RuntimeError> {
        Ok(self.derived(self.snapshot.set_update_fields_on_open(desired)?))
    }

    /// Read the package-level `w:updateFields` setting, `None` if the document
    /// never asserted it.
    pub fn update_fields_on_open(&self) -> Result<Option<bool>, RuntimeError> {
        self.snapshot.update_fields_on_open()
    }

    /// The `w:documentProtection` declaration this document was opened with
    /// (ISO/IEC 29500-1 §17.15.1.29), or `None` if the document declares no
    /// protection.
    ///
    /// This is a **reported** fact, read from the open-time baseline: the engine
    /// surfaces what the document declares so a host can decide policy, but it
    /// does NOT enforce the restriction — edits authored via [`Document::apply`]
    /// ignore it. When the opened document declared *enforced* protection, the
    /// import also emits a diagnostic saying so. The declaration is stable across
    /// verbs (no verb edits it); re-parsing is the only reset.
    pub fn document_protection(&self) -> Option<&DocumentProtection> {
        self.baseline.document_protection.as_ref()
    }

    /// Discover the deltas between this document and `other`, materialized as
    /// tracked changes in the returned document.
    ///
    /// ```
    /// # use stemma::api::Document;
    /// let base = Document::parse(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/simple-text/before.docx"))).unwrap();
    /// let target = Document::parse(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/simple-text/after.docx"))).unwrap();
    /// let redline = base.diff(&target).unwrap();
    /// // The redline round-trips: reject-all == base, accept-all == target.
    /// assert_eq!(redline.read_rejected().unwrap().to_text(), base.to_text());
    /// assert_eq!(redline.read_accepted().unwrap().to_text(), target.to_text());
    /// ```
    pub fn diff(&self, other: &Document) -> Result<Document, RuntimeError> {
        Ok(self.derived(self.snapshot.diff(&other.snapshot)?))
    }

    /// Attributed twin of [`Document::diff`]: discover the deltas between this
    /// document and `target` and materialize them as tracked changes, exactly
    /// like [`diff`](Document::diff) — but attribute every produced revision to
    /// `author` rather than leaving it anonymous. Same return type, same
    /// round-trip contract (reject-all == this document, accept-all ==
    /// `target`); attribution is the only difference.
    ///
    /// An empty `author` is refused (no silent fallback to anonymous — that is
    /// what [`diff`](Document::diff) is for).
    ///
    /// ```
    /// # use stemma::api::Document;
    /// let base = Document::parse(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/simple-text/before.docx"))).unwrap();
    /// let target = Document::parse(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/simple-text/after.docx"))).unwrap();
    /// let redline = base.diff_as(&target, "Reviewer").unwrap();
    /// // Same round-trip as `diff`: reject-all == base, accept-all == target.
    /// assert_eq!(redline.read_rejected().unwrap().to_text(), base.to_text());
    /// assert_eq!(redline.read_accepted().unwrap().to_text(), target.to_text());
    /// // An empty author is refused rather than attributed to no one.
    /// assert!(base.diff_as(&target, "").is_err());
    /// ```
    pub fn diff_as(&self, target: &Document, author: &str) -> Result<Document, RuntimeError> {
        Ok(self.derived(self.snapshot.diff_as(&target.snapshot, author)?))
    }

    /// Resolve tracked deltas: accept-all, reject-all, or a selective set.
    ///
    /// ```
    /// # use stemma::api::Document;
    /// # use stemma::Resolution;
    /// let bytes = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/simple-text/before.docx"));
    /// let doc = Document::parse(bytes).unwrap();
    /// // A pristine document has no pending deltas, so accept-all projects to
    /// // the same text. (`read_accepted`/`read_rejected` are the shorthands.)
    /// let accepted = doc.project(Resolution::AcceptAll).unwrap();
    /// assert_eq!(accepted.to_text(), doc.to_text());
    /// ```
    pub fn project(&self, resolution: Resolution) -> Result<Document, RuntimeError> {
        Ok(self.derived(self.snapshot.project(resolution)?))
    }

    /// Emit DOCX bytes, running the validator gate in `options` before
    /// returning. `ExportOptions::default()` gates at
    /// [`crate::runtime::ValidatorLevel::Blocking`]: bytes that violate a
    /// structural invariant Word rejects or loses data over are refused, not
    /// returned. Skipping validation requires the explicit
    /// [`ExportOptions::unchecked`] (engine-internal intermediates only).
    ///
    /// ```
    /// # use stemma::api::{Document, validate};
    /// # use stemma::ExportOptions;
    /// let bytes = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/simple-text/before.docx"));
    /// let doc = Document::parse(bytes).unwrap();
    /// let out = doc.serialize(&ExportOptions::default()).expect("validated DOCX bytes");
    /// assert!(validate(&out).ok);
    /// ```
    pub fn serialize(&self, options: &ExportOptions) -> Result<Vec<u8>, RuntimeError> {
        serialize_snapshot(&self.snapshot, options)
    }

    /// `apply`'s dry-run twin: report exactly the preconditions [`Document::apply`]
    /// would reject, mutating nothing observable. Answers "would this still
    /// apply, or is it stale/invalid?" without producing a document.
    ///
    /// This runs the full `apply` on the (pure, immutable) snapshot and discards
    /// the result, so `check` and `apply` share one validation path and cannot
    /// drift: `check` accepts a transaction **iff** `apply` accepts it. That
    /// includes the package-aware ApplyStyle style-existence gate and the
    /// PendingParts validations (media / style / numbering / custom-xml), which
    /// the pure verb core alone does not see. It therefore returns
    /// [`RuntimeError`] — apply's rejection vocabulary — not the narrower
    /// `EditError` of the verb core.
    pub fn check(&self, txn: &EditTransaction) -> Result<(), RuntimeError> {
        self.snapshot.check(txn)
    }

    /// The designed read projection (see [`DocumentView`]): a single-document,
    /// IR-independent surface for targeting and inspection — block ids, role
    /// labels, visible text, tracked status (block / paragraph-mark / inline
    /// segment), and opaque anchors. It deliberately exposes none of the
    /// internal `CanonDoc`/`domain` IR, the change vocabulary, or any diff-only
    /// field; use [`Document::snapshot`] for raw IR access.
    pub fn read(&self) -> DocumentView {
        build_document_view(&self.snapshot)
    }

    /// Discover editable text INSIDE opaque regions (RFC-0002): textbox
    /// paragraphs and inline content-control text regions, addressable by
    /// `opaque_text_edit`/`sdt_text_fill`. Body-level (block) content controls
    /// keep their bytes in the scaffold, not the IR — discover those with
    /// [`Document::block_content_control_targets`].
    pub fn opaque_text_targets(&self) -> Vec<crate::opaque_targets::OpaqueTextTarget> {
        crate::opaque_targets::opaque_text_targets(&self.snapshot.canonical)
    }

    /// Discover fillable body-level (block) content controls, each with the
    /// `body_index` that `sdt_text_fill` addresses (RFC-0002 §Phase-2).
    pub fn block_content_control_targets(&self) -> Vec<crate::opaque_targets::BlockSdtTextTarget> {
        self.snapshot.block_content_control_targets()
    }

    /// The extended-markdown comprehension projection: honest, id-bearing
    /// tagged prose for an LLM to read. A one-way renderer of [`Document::read`]; addressing
    /// and editing still go through the read view and the transaction, never
    /// this string.
    pub fn to_markdown(&self) -> String {
        crate::extended_markdown::to_extended_markdown(&self.read())
    }

    /// The plain-text reading of this document (see [`crate::view::to_plain_text`]).
    ///
    /// A one-way renderer of [`Document::read`]: visible run text, one U+FFFC
    /// per opaque anchor, blocks joined by a blank line. This reads the document
    /// *as it currently stands* — tracked deletions and insertions both surface
    /// (the read view carries both). To read a single resolution, project first:
    /// `doc.read_accepted()?.to_text()` for the accept-all body,
    /// `doc.read_rejected()?.to_text()` for the reject-all body.
    pub fn to_text(&self) -> String {
        crate::view::to_plain_text(&self.read())
    }

    /// The structural index of this document (see [`DocumentOutline`]): one
    /// entry per block, in document order, plus document-level totals. The
    /// navigation tier for a large document — read it to find the block ids
    /// worth windowing into without rendering the whole body. A pure projection
    /// of [`Document::read`]; `entries[i]` is faithful to `read().blocks[i]`.
    pub fn outline(&self) -> DocumentOutline {
        crate::view::build_outline(&self.read())
    }

    /// The HTML reading of this document (see [`crate::html::to_html`]).
    ///
    /// A one-way renderer of [`Document::read`]: every block id surfaces as an
    /// `id`/`data-id`, all text is HTML-escaped, headings map to `<h1>`..`<h6>`,
    /// tracked spans to `<ins>`/`<del>`, and each opaque anchor to exactly one
    /// addressable `<span class="anchor">`. Honest, **not** pixel fidelity:
    /// tables and opaque blocks render as addressable placeholder `<div>`s.
    pub fn to_html(&self) -> String {
        crate::html::to_html(&self.read())
    }

    /// Render an inclusive block-id window (`from_id..=to_id`) of this document
    /// in `format`. A windowed read is, by construction, exactly the slice of
    /// the full read in the same format: it resolves the window with
    /// [`crate::view::block_range`] and renders the resulting slice with the
    /// same slice renderer the full-document projection uses.
    ///
    /// Fails loud (CLAUDE.md — no silent fallbacks): an unknown endpoint id or
    /// an out-of-order pair returns a [`WindowError`] rather than an empty or
    /// best-effort window.
    pub fn window(
        &self,
        from_id: &str,
        to_id: &str,
        format: WindowFormat,
    ) -> Result<String, WindowError> {
        let view = self.read();
        let slice = crate::view::block_range(&view, from_id, to_id)?;
        Ok(match format {
            WindowFormat::Text => crate::view::to_plain_text_blocks(slice),
            WindowFormat::Markdown => crate::extended_markdown::to_extended_markdown_blocks(slice),
            WindowFormat::Html => crate::html::to_html_blocks(slice),
        })
    }

    /// A throwaway accept-all projection: resolve every tracked change as
    /// accepted and return the resulting [`Document`]. Reads never mutate `self`
    /// (this is `self.project(Resolution::AcceptAll)`), so the caller decides
    /// whether to keep the projected document or discard it after reading.
    pub fn read_accepted(&self) -> Result<Document, RuntimeError> {
        self.project(Resolution::AcceptAll)
    }

    /// A throwaway reject-all projection: resolve every tracked change as
    /// rejected and return the resulting [`Document`]. The reject-all body must
    /// equal the document's baseline (the `reject-all == baseline` invariant).
    pub fn read_rejected(&self) -> Result<Document, RuntimeError> {
        self.project(Resolution::RejectAll)
    }

    /// Every pending revision in the document — the engine's ONE canonical
    /// census ([`crate::tracked_model::enumerate_revisions`]): inline and
    /// block insert/delete, paragraph marks, table structure, hyperlink runs,
    /// comment stories, atomic moves, opaque-interior records, and every
    /// `*PrChange` formatting kind. This is the same walk the accept/reject
    /// selectors lower against, so the returned `revision_id`s feed
    /// [`Document::project`] with [`Resolution::Selective`] directly (a
    /// record with `revision_id == 0` is census-only and never selectable).
    ///
    /// Consumers must not re-derive a narrower enumeration from
    /// [`Document::read`]: the segment view carries no formatting-change
    /// records, so any count built from it silently understates the
    /// document's pending state (a formatting-only document reads as
    /// revision-free, and author-scoped resolution misses that author's
    /// formatting changes).
    pub fn revisions(&self) -> Vec<crate::tracked_model::RevisionRecord> {
        crate::tracked_model::enumerate_revisions(&self.snapshot.canonical)
    }

    /// Escape hatch: borrow the internal [`EditSnapshot`].
    ///
    /// The snapshot is engine-version-bound and not semver-stable; this exists
    /// for callers that need state the verbs do not yet cover. Prefer the verbs
    /// and [`Document::read`].
    pub fn snapshot(&self) -> &EditSnapshot {
        &self.snapshot
    }

    /// Session review (RFC 0001): audit everything this document changed
    /// since [`Document::parse`], against the retained baseline — the new
    /// tracked-change census, the fate of every pre-existing revision, the
    /// untracked (direct) delta, the untouched proof, and the package
    /// verdict. See [`crate::audit::AuditReport`].
    ///
    /// The package verdict is computed on this document's would-be save
    /// bytes via an UNGATED serialize followed by honest validation:
    /// review REPORTS validity, it never gates. The save gate
    /// ([`Document::serialize`] at `Blocking`) is unchanged and remains the
    /// only place bytes are refused.
    ///
    /// Pure: mutates nothing, resets nothing. Reviewing after a save shows
    /// the same since-parse delta — re-parsing is the only baseline reset.
    ///
    /// ```
    /// # use stemma::api::Document;
    /// let bytes = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/simple-text/before.docx"));
    /// let doc = Document::parse(bytes).unwrap();
    /// // Freshly parsed: nothing has changed since the baseline, and every
    /// // block is provably untouched.
    /// let report = doc.review().expect("review");
    /// assert!(report.new_revisions.is_empty());
    /// assert!(report.direct_changes.is_empty());
    /// assert!(report.untouched.violations.is_empty());
    /// ```
    pub fn review(&self) -> Result<AuditReport, RuntimeError> {
        let bytes = serialize_snapshot(&self.snapshot, &ExportOptions::unchecked())?;
        // `before` (the retained baseline) resolves against the snapshot's own
        // style table (the scaffold retains the import-time styles.xml); `after`
        // against the serialized current bytes' styles. Both feed the
        // committed-baseline reject so a rejected paragraph-style change
        // re-resolves style-inherited run marks (see `audit_documents`).
        let before_styles = self.snapshot.scaffold_style_table()?;
        let after_styles = style_table_from_docx(&bytes)?;
        audit_documents(
            &self.baseline,
            &self.snapshot.canonical,
            before_styles.as_ref(),
            after_styles.as_ref(),
            validate(&bytes),
        )
    }
}

/// Stateless certification (RFC 0001): audit ANY pair of documents — the
/// edits need not have been made by stemma. `before` is the baseline,
/// `after` the document to certify; the report is the same
/// [`crate::audit::AuditReport`] the session form produces, with the
/// package verdict computed on `after`'s actual bytes.
pub fn audit(before_bytes: &[u8], after_bytes: &[u8]) -> Result<AuditReport, RuntimeError> {
    let before = snapshot_from_docx_bytes(before_bytes)?;
    let after = snapshot_from_docx_bytes(after_bytes)?;
    // Each document's own style table, so the committed-baseline reject
    // re-resolves style-inherited run marks on a rejected paragraph-style
    // change (see `audit_documents`).
    let before_styles = style_table_from_docx(before_bytes)?;
    let after_styles = style_table_from_docx(after_bytes)?;
    audit_documents(
        &before.canonical,
        &after.canonical,
        before_styles.as_ref(),
        after_styles.as_ref(),
        validate(after_bytes),
    )
}

/// Validate DOCX bytes as a property of the bytes, without a [`Document`].
///
/// Delegates to the same package-level checks the runtime's
/// `validate_docx_bytes` uses.
pub fn validate(bytes: &[u8]) -> ValidationReport {
    match validate_docx_report(bytes) {
        Ok(report) => report,
        // `validate_docx_report` only errs on internal failures; surface that
        // as an invalid report rather than panicking.
        Err(err) => ValidationReport {
            ok: false,
            issues: vec![ValidationIssue {
                code: ValidationIssueCode::PackageInvariant,
                message: err.message,
                context: err.details.context,
            }],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{NodeId, RevisionInfo};
    use crate::edit::{
        ContentFragment, EditStep, EditTransaction, MaterializationMode, ParagraphContent,
        StyleDefinition, StyleParaProps, StyleRunProps, StyleType,
    };
    use crate::runtime::ErrorCode;
    use crate::tracked_model::ResolveSelectionAction;

    /// Build a minimal valid DOCX byte stream for testing the facade.
    fn make_test_docx(paragraphs: &[&str]) -> Vec<u8> {
        let mut document_xml = String::from(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#,
        );
        for para in paragraphs {
            document_xml.push_str(&format!(r#"<w:p><w:r><w:t>{para}</w:t></w:r></w:p>"#));
        }
        document_xml.push_str("<w:sectPr/></w:body></w:document>");

        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
        let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
        let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;

        use std::io::Write;
        use zip::write::FileOptions;
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts: FileOptions = FileOptions::default();
            zip.start_file("[Content_Types].xml", opts).unwrap();
            zip.write_all(content_types.as_bytes()).unwrap();
            zip.start_file("_rels/.rels", opts).unwrap();
            zip.write_all(rels.as_bytes()).unwrap();
            zip.start_file("word/_rels/document.xml.rels", opts)
                .unwrap();
            zip.write_all(doc_rels.as_bytes()).unwrap();
            zip.start_file("word/document.xml", opts).unwrap();
            zip.write_all(document_xml.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        buf
    }

    /// The id of the first block in the read projection.
    fn first_block_id(doc: &Document) -> String {
        doc.read()
            .blocks
            .first()
            .expect("at least one block")
            .id
            .to_string()
    }

    /// The accept-all visible text the document serializes to. We read it back
    /// out of the emitted `word/document.xml` rather than depending on the
    /// (engine-bound) shape of the read view's inline-change segments. Tracked
    /// deletions emit `w:delText`, which this excludes — so the result is the
    /// post-accept text.
    fn all_text(doc: &Document) -> String {
        let bytes = doc.serialize(&ExportOptions::default()).expect("serialize");
        let archive = crate::docx::DocxArchive::read(&bytes).expect("read archive");
        let xml = String::from_utf8(
            archive
                .get("word/document.xml")
                .expect("document.xml present")
                .to_vec(),
        )
        .expect("utf8");
        extract_w_t_text(&xml)
    }

    /// Pull the concatenated content of every `<w:t ...>...</w:t>` element
    /// (insertions and unchanged runs) out of a document.xml string. Ignores
    /// `<w:delText>` so the result reflects the accept-all text.
    fn extract_w_t_text(xml: &str) -> String {
        let mut out = String::new();
        let bytes = xml.as_bytes();
        let mut i = 0;
        while let Some(rel) = xml[i..].find("<w:t") {
            let tag_start = i + rel;
            // Require the char after "<w:t" to end the tag name (space or '>'),
            // so we skip <w:tbl, <w:tc, <w:tr, etc.
            let after = tag_start + 4;
            if after >= bytes.len() || (bytes[after] != b' ' && bytes[after] != b'>') {
                i = after;
                continue;
            }
            let Some(gt) = xml[tag_start..].find('>') else {
                break;
            };
            let content_start = tag_start + gt + 1;
            let Some(close_rel) = xml[content_start..].find("</w:t>") else {
                break;
            };
            out.push_str(&xml[content_start..content_start + close_rel]);
            i = content_start + close_rel + "</w:t>".len();
        }
        out
    }

    fn replace_paragraph_txn(block_id: &str, expect: &str, replacement: &str) -> EditTransaction {
        EditTransaction {
            steps: vec![EditStep::ReplaceParagraphText {
                block_id: NodeId::from(block_id),
                rationale: None,
                replacement_role: None,
                expect: expect.to_string(),
                semantic_hash: None,
                content: ParagraphContent {
                    fragments: vec![ContentFragment::Text(replacement.to_string())],
                },
            }],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: RevisionInfo {
                revision_id: 1,
                identity: 0,
                author: Some("Test".to_string()),
                date: Some("2026-05-31T00:00:00Z".to_string()),
                apply_op_id: None,
            },
        }
    }

    #[test]
    fn parse_then_serialize_is_valid_docx() {
        let docx = make_test_docx(&["Hello world", "Second paragraph"]);
        let doc = Document::parse(&docx).expect("parse");
        let bytes = doc.serialize(&ExportOptions::default()).expect("serialize");
        // Post-condition: serialized bytes are themselves a valid DOCX package.
        assert!(validate(&bytes).ok, "serialized output must be valid DOCX");
        // And re-parseable into a Document.
        Document::parse(&bytes).expect("re-parse serialized output");
    }

    #[test]
    fn apply_authors_tracked_change_and_is_pure() {
        let docx = make_test_docx(&["Hello world"]);
        let doc = Document::parse(&docx).expect("parse");
        let id = first_block_id(&doc);
        let txn = replace_paragraph_txn(&id, "Hello world", "Goodbye world");
        let edited = doc.apply(&txn).expect("apply");

        // `apply` is pure: accept-all on the ORIGINAL still yields the original
        // text — the original document was not mutated.
        let orig_accepted = doc.project(Resolution::AcceptAll).expect("project orig");
        assert!(
            all_text(&orig_accepted).contains("Hello world"),
            "original document must be untouched by apply"
        );

        // The edited document accepts to the new text and rejects to the old.
        let edited_accepted = edited.project(Resolution::AcceptAll).expect("accept edit");
        assert!(
            all_text(&edited_accepted).contains("Goodbye world"),
            "accept-all of the edit must yield the new text"
        );
        let edited_rejected = edited.project(Resolution::RejectAll).expect("reject edit");
        assert!(
            all_text(&edited_rejected).contains("Hello world"),
            "reject-all of the edit must restore the original text"
        );
    }

    fn replace_paragraph_txn_by(
        block_id: &str,
        expect: &str,
        replacement: &str,
        author: &str,
    ) -> EditTransaction {
        EditTransaction {
            steps: vec![EditStep::ReplaceParagraphText {
                block_id: NodeId::from(block_id),
                rationale: None,
                replacement_role: None,
                expect: expect.to_string(),
                semantic_hash: None,
                content: ParagraphContent {
                    fragments: vec![ContentFragment::Text(replacement.to_string())],
                },
            }],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: RevisionInfo {
                revision_id: 1,
                identity: 0,
                author: Some(author.to_string()),
                date: Some("2026-05-31T00:00:00Z".to_string()),
                apply_op_id: None,
            },
        }
    }

    /// A document already carrying a pending tracked change authored by
    /// `origin_author` — built by authoring, serializing, and RE-PARSING (the
    /// real path a document with an existing redline arrives through), not by
    /// poking the origin-authors field directly. `origin_author`'s revision is
    /// therefore genuinely present in the redline at parse time, exactly the
    /// domain condition `apply_authored`'s guard exists to check.
    fn docx_with_existing_author(origin_author: &str) -> Vec<u8> {
        let plain = make_test_docx(&["Hello world"]);
        let doc = Document::parse(&plain).expect("parse plain doc");
        let id = first_block_id(&doc);
        let seeded = doc
            .apply(&replace_paragraph_txn_by(
                &id,
                "Hello world",
                "Seeded change",
                origin_author,
            ))
            .expect("seed the origin author's revision");
        seeded
            .serialize(&ExportOptions::default())
            .expect("serialize the seeded redline")
    }

    /// THE CONTRACT: `apply_authored` refuses a write whose `revision.author`
    /// already authors a pending revision in the document's redline at parse
    /// time — editing under that identity would make the new write
    /// indistinguishable from the existing reviewer's and defeat layered
    /// review. `allow_existing_author=true` is the deliberate override; a
    /// distinct author is never refused; plain `apply` (no author check) is
    /// unaffected.
    #[test]
    fn apply_authored_refuses_to_impersonate_the_documents_origin_author() {
        let docx = docx_with_existing_author("AuthorA");
        let doc = Document::parse(&docx).expect("parse redlined doc");
        let id = first_block_id(&doc);

        let impersonating = replace_paragraph_txn_by(&id, "Seeded change", "x", "AuthorA");
        let err = match doc.apply_authored(&impersonating, false) {
            Ok(_) => panic!("impersonating the origin author must be refused"),
            Err(e) => e,
        };
        assert_eq!(err.code, ErrorCode::AuthorImpersonation);
        assert!(
            err.message.contains("AuthorA"),
            "the error names the impersonated author: {}",
            err.message
        );

        // The override deliberately continues that author's own work.
        doc.apply_authored(&impersonating, true)
            .expect("allow_existing_author=true bypasses the refusal");

        // A distinct author is never impersonation.
        let distinct = replace_paragraph_txn_by(&id, "Seeded change", "y", "Reviewer");
        doc.apply_authored(&distinct, false)
            .expect("a distinct author is accepted");

        // Plain `apply` enforces no author policy at all — it is the
        // guard-free primitive `apply_authored` wraps.
        doc.apply(&impersonating)
            .expect("bare apply is guard-free by design");
    }

    #[test]
    fn check_is_dry_run_and_agrees_with_apply() {
        let docx = make_test_docx(&["Hello world"]);
        let doc = Document::parse(&docx).expect("parse");
        let id = first_block_id(&doc);

        // A valid transaction passes the dry run and apply.
        let good = replace_paragraph_txn(&id, "Hello world", "Goodbye world");
        doc.check(&good).expect("check should pass for a valid txn");
        doc.apply(&good)
            .expect("apply should succeed for a valid txn");

        // A stale `expect` is rejected by both check and apply, identically.
        let stale = replace_paragraph_txn(&id, "NOT THE ACTUAL TEXT", "x");
        assert!(doc.check(&stale).is_err(), "stale expect must fail check");
        assert!(doc.apply(&stale).is_err(), "stale expect must fail apply");
        // check mutated nothing observable.
        assert!(all_text(&doc).contains("Hello world"));
    }

    /// A minimal valid DOCX that carries a `word/styles.xml` defining a single
    /// style with the given id (and the part's content-type + relationship), so
    /// the package-aware style gate and the `CreateStyle`-already-exists check
    /// have a real style table to consult.
    fn make_styled_docx(defined_style_id: &str) -> Vec<u8> {
        let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Hello world</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;
        let styles_xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style w:type="paragraph" w:styleId="{defined_style_id}"><w:name w:val="{defined_style_id}"/></w:style></w:styles>"#
        );
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#;
        let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
        let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;

        use std::io::Write;
        use zip::write::FileOptions;
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts: FileOptions = FileOptions::default();
            zip.start_file("[Content_Types].xml", opts).unwrap();
            zip.write_all(content_types.as_bytes()).unwrap();
            zip.start_file("_rels/.rels", opts).unwrap();
            zip.write_all(rels.as_bytes()).unwrap();
            zip.start_file("word/_rels/document.xml.rels", opts)
                .unwrap();
            zip.write_all(doc_rels.as_bytes()).unwrap();
            zip.start_file("word/document.xml", opts).unwrap();
            zip.write_all(document_xml.as_bytes()).unwrap();
            zip.start_file("word/styles.xml", opts).unwrap();
            zip.write_all(styles_xml.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        buf
    }

    fn style_def(style_id: &str, name: &str) -> StyleDefinition {
        StyleDefinition {
            style_id: style_id.to_string(),
            style_type: StyleType::Para,
            based_on: None,
            name: name.to_string(),
            run_props: StyleRunProps::default(),
            para_props: StyleParaProps::default(),
        }
    }

    fn single_step_txn(step: EditStep) -> EditTransaction {
        EditTransaction {
            steps: vec![step],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: RevisionInfo {
                revision_id: 1,
                identity: 0,
                author: Some("Test".to_string()),
                date: Some("2026-05-31T00:00:00Z".to_string()),
                apply_op_id: None,
            },
        }
    }

    /// `check` must enforce the package-aware ApplyStyle style-existence gate —
    /// the same gate `apply` runs but the pure verb core cannot (it holds no
    /// style table). P1 #31: without this, `check` was a dry-run that lied,
    /// passing an `ApplyStyle` that `apply` rejects.
    #[test]
    fn check_enforces_apply_style_existence_gate() {
        let docx = make_styled_docx("Normal");
        let doc = Document::parse(&docx).expect("parse");
        let id = first_block_id(&doc);

        // (a) An ApplyStyle naming a style absent from styles.xml and not a Word
        // built-in must be rejected by BOTH check and apply, identically.
        let dangling = single_step_txn(EditStep::ApplyStyle {
            block_id: NodeId::from(id.as_str()),
            semantic_hash: None,
            style_id: "DefinitelyNotARealStyle".to_string(),
            rationale: None,
        });
        let check_err = doc
            .check(&dangling)
            .expect_err("check must reject an ApplyStyle of an undefined style");
        assert_eq!(check_err.code, ErrorCode::AnchorNotFound);
        assert!(
            doc.apply(&dangling).is_err(),
            "apply must also reject the dangling ApplyStyle (check agrees with apply)"
        );

        // A CreateStyle + ApplyStyle of the same id in ONE transaction is a
        // valid self-resolving reference: check must ACCEPT it (mirrors apply).
        let create_then_apply = EditTransaction {
            steps: vec![
                EditStep::CreateStyle {
                    def: style_def("AuthoredInTxn", "Authored In Txn"),
                    rationale: None,
                },
                EditStep::ApplyStyle {
                    block_id: NodeId::from(id.as_str()),
                    semantic_hash: None,
                    style_id: "AuthoredInTxn".to_string(),
                    rationale: None,
                },
            ],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: RevisionInfo {
                revision_id: 1,
                identity: 0,
                author: Some("Test".to_string()),
                date: Some("2026-05-31T00:00:00Z".to_string()),
                apply_op_id: None,
            },
        };
        doc.check(&create_then_apply)
            .expect("check must accept a CreateStyle + ApplyStyle of the same id");
        doc.apply(&create_then_apply)
            .expect("apply must accept the same transaction (check agrees with apply)");
    }

    /// `check` must enforce the PendingParts validations `apply` runs at the
    /// serialize edge — here, `CreateStyle` of a style id that already exists in
    /// `word/styles.xml` is refused ("already exists"). The pure verb core only
    /// stages the op, so without routing through apply, `check` passed a
    /// transaction `apply` rejects. P1 #31.
    #[test]
    fn check_enforces_pending_parts_validation() {
        let docx = make_styled_docx("Normal");
        let doc = Document::parse(&docx).expect("parse");

        let dup = single_step_txn(EditStep::CreateStyle {
            def: style_def("Normal", "Dup Normal"),
            rationale: None,
        });
        let check_err = doc
            .check(&dup)
            .expect_err("check must reject CreateStyle of an already-defined style id");
        assert!(
            check_err.message.contains("already exists"),
            "check must surface the PendingParts create-existing failure, got: {}",
            check_err.message
        );
        let apply_err = doc
            .apply(&dup)
            .err()
            .expect("apply must also reject (check agrees with apply)");
        assert!(
            apply_err.message.contains("already exists"),
            "apply must reject identically, got: {}",
            apply_err.message
        );
    }

    #[test]
    fn diff_discovers_change_between_two_documents() {
        let base = Document::parse(&make_test_docx(&["Hello world"])).expect("base");
        let target = Document::parse(&make_test_docx(&["Hello brave world"])).expect("target");
        let redlined = base.diff(&target).expect("diff");
        // Reject-all reconstructs the base; accept-all reconstructs the target.
        let rejected = redlined.project(Resolution::RejectAll).expect("reject");
        let accepted = redlined.project(Resolution::AcceptAll).expect("accept");
        assert!(
            all_text(&rejected).contains("Hello world"),
            "reject-all = base"
        );
        assert!(
            all_text(&accepted).contains("Hello brave world"),
            "accept-all = target"
        );
    }

    /// The distinct authors carried by the tracked segments of a document's
    /// read view — both inserted and deleted spans, across every block. Used to
    /// assert diff attribution survives a serialize→reparse round-trip.
    fn revision_authors(doc: &Document) -> std::collections::BTreeSet<Option<String>> {
        use crate::view::{SegmentView, TrackStatus};
        let mut authors = std::collections::BTreeSet::new();
        let mut note = |st: &TrackStatus| match st {
            TrackStatus::Normal => {}
            TrackStatus::Inserted(rev) | TrackStatus::Deleted(rev) => {
                authors.insert(rev.author.clone());
            }
            TrackStatus::InsertedThenDeleted { inserted, deleted } => {
                authors.insert(inserted.author.clone());
                authors.insert(deleted.author.clone());
            }
        };
        for block in &doc.read().blocks {
            for seg in &block.segments {
                match seg {
                    SegmentView::Text { status, .. } => note(status),
                    SegmentView::Opaque { status, .. } => note(status),
                }
            }
        }
        authors
    }

    #[test]
    fn diff_as_attributes_discovered_revisions_and_round_trips() {
        // Domain rule: `diff_as` is `diff` plus attribution — every revision it
        // materializes carries the supplied author, and that author survives a
        // serialize→reparse round-trip (it is real tracked-change markup, not an
        // in-memory annotation). Plain `diff` leaves the author anonymous; the
        // two differ ONLY in attribution.
        let base = Document::parse(&make_test_docx(&["Hello world"])).expect("base");
        let target = Document::parse(&make_test_docx(&["Hello brave world"])).expect("target");

        let attributed = base.diff_as(&target, "Reviewer").expect("diff_as");
        // Round-trip through DOCX bytes and re-parse: the author must be on the
        // reparsed revisions, not lost at the serialize edge.
        let bytes = attributed
            .serialize(&ExportOptions::default())
            .expect("serialize");
        let reparsed = Document::parse(&bytes).expect("re-parse redline");
        assert_eq!(
            revision_authors(&reparsed),
            std::collections::BTreeSet::from([Some("Reviewer".to_string())]),
            "every discovered revision must be attributed to the supplied author after round-trip"
        );

        // Contrast: anonymous `diff` attributes the same pair to no one. (The
        // serializer emits an empty `w:author=""` for an anonymous revision, so
        // the round-tripped author is the empty string, not `None` — the point
        // here is only that it is NOT the named "Reviewer".)
        let anon = base.diff(&target).expect("diff");
        let anon_bytes = anon
            .serialize(&ExportOptions::default())
            .expect("serialize");
        let anon_reparsed = Document::parse(&anon_bytes).expect("re-parse");
        assert!(
            !revision_authors(&anon_reparsed).contains(&Some("Reviewer".to_string())),
            "plain diff must not attribute revisions to the diff_as author, got {:?}",
            revision_authors(&anon_reparsed)
        );
    }

    #[test]
    fn diff_as_preserves_accept_reject_round_trip() {
        // Attribution must not disturb the diff round-trip: reject-all == base,
        // accept-all == target, exactly as for `diff`.
        let base = Document::parse(&make_test_docx(&["Hello world"])).expect("base");
        let target = Document::parse(&make_test_docx(&["Hello brave world"])).expect("target");
        let redlined = base.diff_as(&target, "Reviewer").expect("diff_as");

        let rejected = redlined.project(Resolution::RejectAll).expect("reject");
        let accepted = redlined.project(Resolution::AcceptAll).expect("accept");
        assert!(
            all_text(&rejected).contains("Hello world") && !all_text(&rejected).contains("brave"),
            "reject-all = base"
        );
        assert!(
            all_text(&accepted).contains("Hello brave world"),
            "accept-all = target"
        );
    }

    #[test]
    fn diff_as_rejects_empty_author() {
        // No silent fallback to anonymous: an empty author is refused, so a
        // caller that means "unattributed" reaches for `diff` deliberately.
        let base = Document::parse(&make_test_docx(&["Hello world"])).expect("base");
        let target = Document::parse(&make_test_docx(&["Hello brave world"])).expect("target");
        match base.diff_as(&target, "") {
            Err(e) => {
                assert_eq!(e.code, ErrorCode::ValidationFailed);
                assert!(
                    e.message.contains("author"),
                    "the error names the missing author: {}",
                    e.message
                );
            }
            Ok(_) => panic!("empty author must be refused"),
        }
    }

    #[test]
    fn selective_resolution_rejects_empty_id_set() {
        let doc = Document::parse(&make_test_docx(&["Hello world"])).expect("parse");
        let err = doc.project(Resolution::Selective {
            ids: std::collections::HashSet::new(),
            action: ResolveSelectionAction::Accept,
        });
        // `Document` deliberately does not implement `Debug`, so match rather
        // than `expect_err`.
        match err {
            Err(e) => assert_eq!(e.code, ErrorCode::InvalidRange),
            Ok(_) => panic!("empty id set must be rejected"),
        }
    }

    #[test]
    fn validate_rejects_non_docx_bytes() {
        let report = validate(b"not a zip file");
        assert!(!report.ok);
        assert!(!report.issues.is_empty());
    }

    /// Collapse runs of ASCII whitespace to single spaces and trim, so a
    /// view-derived string and a serializer-derived string can be compared at
    /// the word layer (run/paragraph segmentation differs between the two).
    fn normalize_ws(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn to_text_of_accepted_matches_serialized_body_oracle() {
        // Domain rule: `to_text(read_accepted())` is the accept-all reading of
        // the body. We check it against an INDEPENDENT oracle — `all_text()`,
        // which extracts `<w:t>` (excluding `<w:delText>`) from the serialized
        // document.xml. The two derivations (clean view vs. raw serializer)
        // must agree at the word layer for a tracked word replacement.
        let docx = make_test_docx(&["Hello world"]);
        let doc = Document::parse(&docx).expect("parse");
        let id = first_block_id(&doc);
        let edited = doc
            .apply(&replace_paragraph_txn(&id, "Hello world", "Goodbye world"))
            .expect("apply");

        let accepted = edited.read_accepted().expect("accept-all");
        assert_eq!(
            normalize_ws(&accepted.to_text()),
            normalize_ws(&all_text(&accepted)),
            "to_text(read_accepted) must equal the serialized accept-all body"
        );
        // The accepted text carries the replacement, not the original word.
        assert!(accepted.to_text().contains("Goodbye"));
        assert!(
            !accepted.to_text().contains("Hello"),
            "accept-all dropped the deleted word: {:?}",
            accepted.to_text()
        );
    }

    #[test]
    fn read_rejected_text_equals_baseline_and_accepted_equals_target() {
        // Domain rule (reject-all == baseline, accept-all == target) observed at
        // the TEXT layer: diff(base, target) is a redline whose reject-all text
        // equals base's text and whose accept-all text equals target's text.
        let base = Document::parse(&make_test_docx(&["Hello world"])).expect("base");
        let target = Document::parse(&make_test_docx(&["Hello brave world"])).expect("target");
        let redlined = base.diff(&target).expect("diff");

        let rejected = redlined.read_rejected().expect("reject-all");
        let accepted = redlined.read_accepted().expect("accept-all");
        assert_eq!(
            normalize_ws(&rejected.to_text()),
            normalize_ws(&base.to_text()),
            "reject-all text == baseline text"
        );
        assert_eq!(
            normalize_ws(&accepted.to_text()),
            normalize_ws(&target.to_text()),
            "accept-all text == target text"
        );
    }

    #[test]
    fn to_text_of_pristine_doc_equals_blocks_joined_by_blank_line() {
        // Independent oracle for the join contract: the per-block read-view text
        // joined by a blank line. (Not to_plain_text itself — that would be
        // circular.)
        let docx = make_test_docx(&["One", "Two", "Three"]);
        let doc = Document::parse(&docx).expect("parse");
        let oracle = doc
            .read()
            .blocks
            .iter()
            .map(|b| b.text.clone())
            .collect::<Vec<_>>()
            .join("\n\n");
        assert_eq!(doc.to_text(), oracle);
        assert_eq!(doc.to_text(), "One\n\nTwo\n\nThree");
    }
}
