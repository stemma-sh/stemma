//! Session-backed entry point for the engine.
//!
//! [`SimpleRuntime`] is a `DashMap<DocHandle, EditSnapshot>` keyed by
//! handle, plus operations that look up a handle, call the engine's pure
//! per-module functions ([`crate::import`], [`crate::diff`],
//! [`crate::edit`], [`crate::serialize`], etc.), and store the new snapshot
//! back. The IR ([`crate::CanonDoc`]) lives in this store while a document
//! is in active use; the source-of-truth artifact is the DOCX bytes, and
//! the durable record of intent is the [`crate::edit::EditTransaction`]
//! history. See the crate root for the entity model.
//!
//! The handle store is opinionated about lifecycle (TTL-based eviction via
//! [`SimpleRuntime::evict_expired`]) but otherwise stateless from a
//! persistence perspective — drop the runtime and you lose nothing
//! durable.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Cursor;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use serde::{Deserialize, Serialize};

use xmltree::{Element, EmitterConfig, XMLNode};

use crate::diff::{
    build_full_document_view, build_tracked_document_view, diff_and_full_document, diff_documents,
};
use crate::docx::{DocxArchive, DocxError};
use crate::docx_package::DocxPackage;
use crate::domain::{
    BlockNode, Border, CanonDoc, DocFingerprint, DocHandle, DocumentDiff, FullDocViewResult,
    HeaderFooterKind, IStr, InlineNode, NodeId, OpaqueKind, PageOrientation, ParagraphNode,
    RevisionInfo, SdtWrapper, SectionProperties, TrackedBlock, TrackingStatus, TransactionMeta,
};
use crate::import::{
    build_canonical_from_root_with_stories, build_image_data_lookup, build_rel_lookup_from_rels,
    build_story_payloads, parse_comments, parse_document_relationships, parse_endnotes,
    parse_footers, parse_footnotes, parse_header_footer_refs, parse_headers,
    resolve_hyperlink_urls, sha256_hex,
};
use crate::serialize::{
    build_people_xml, collect_tracked_change_authors, serialize_comments_part,
    serialize_endnotes_part, serialize_footnotes_part, serialize_tracked_block,
};
use crate::tracked_model::{BlockProvenanceMap, ResolveSelectionAction, merge_diff};
use crate::word_xml::{self, WordXmlError, body_element, body_element_mut, is_w_tag, w_el};
use crate::xml_attrs::{attr_get, attr_set};
use crate::xml_write::{self, XmlWriter};
// Re-export so serialize.rs (and other modules) can import from crate::runtime
pub(crate) use crate::import::local_element_name;
// Re-export the faithful style-table projection types. The `styles` module is
// `pub(crate)`, but `EditSnapshot::style_table` returns a `StyleTableProjection`
// across the public API (the MCP `read_styles` tool reads it), so the projection
// types must be publicly nameable. Glob-re-exported by `lib.rs` (`pub use
// runtime::*`).
pub use crate::styles::{DocDefaultRun, StyleRow, StyleTableProjection};

// =============================================================================
// Relationship types for story parsing
// =============================================================================

pub(crate) const HEADER_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header";
pub(crate) const FOOTER_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer";
pub(crate) const FOOTNOTES_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes";
pub(crate) const ENDNOTES_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/endnotes";
pub(crate) const COMMENTS_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments";
pub(crate) const COMMENTS_EXTENDED_REL_TYPE: &str =
    "http://schemas.microsoft.com/office/2011/relationships/commentsExtended";
pub(crate) const HYPERLINK_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink";
pub(crate) const CUSTOM_XML_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/customXml";
pub(crate) const CUSTOM_PROPERTIES_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/custom-properties";

/// A relationship entry from document.xml.rels.
#[derive(Clone, Debug)]
pub(crate) struct Relationship {
    pub(crate) id: String,
    pub(crate) target: String,
}

/// Parsed document relationships.
#[derive(Clone, Debug, Default)]
pub(crate) struct DocumentRelationships {
    pub(crate) headers: Vec<Relationship>,
    pub(crate) footers: Vec<Relationship>,
    pub(crate) footnotes: Option<Relationship>,
    pub(crate) endnotes: Option<Relationship>,
    pub(crate) comments: Option<Relationship>,
    pub(crate) comments_extended: Option<Relationship>,
    pub(crate) custom_xml: Vec<Relationship>,
    /// External hyperlink relationships: rId -> URL.
    pub(crate) hyperlinks: std::collections::HashMap<String, String>,
}

/// Build a `DocumentRelationships` from the typed `RelationshipSet` in a
/// `DocxPackage`, avoiding re-parsing the XML.
fn document_relationships_from_package_rels(
    rels: &crate::docx_package::RelationshipSet,
) -> DocumentRelationships {
    let mut doc_rels = DocumentRelationships::default();
    for r in &rels.entries {
        let rel = Relationship {
            id: r.id.clone(),
            target: r.target.clone(),
        };
        if r.rel_type == HEADER_REL_TYPE {
            doc_rels.headers.push(rel);
        } else if r.rel_type == FOOTER_REL_TYPE {
            doc_rels.footers.push(rel);
        } else if r.rel_type == FOOTNOTES_REL_TYPE {
            doc_rels.footnotes = Some(rel);
        } else if r.rel_type == ENDNOTES_REL_TYPE {
            doc_rels.endnotes = Some(rel);
        } else if r.rel_type == COMMENTS_REL_TYPE {
            doc_rels.comments = Some(rel);
        } else if r.rel_type == COMMENTS_EXTENDED_REL_TYPE {
            doc_rels.comments_extended = Some(rel);
        } else if r.rel_type == CUSTOM_XML_REL_TYPE {
            doc_rels.custom_xml.push(rel);
        } else if r.rel_type == HYPERLINK_REL_TYPE {
            doc_rels.hyperlinks.insert(rel.id, rel.target);
        }
    }
    doc_rels
}

pub(crate) fn relationship_target_to_part_path(target: &str) -> String {
    if let Some(stripped) = target.strip_prefix('/') {
        stripped.to_string()
    } else {
        format!("word/{target}")
    }
}

pub(crate) fn runtime_timing_logs_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("XML_RUNTIME_TIMING")
            .map(|value| {
                let normalized = value.trim().to_ascii_lowercase();
                normalized != "0" && normalized != "false" && normalized != "off"
            })
            .unwrap_or(true)
    })
}

/// A header/footer reference from section properties.
#[derive(Clone, Debug)]
pub(crate) struct HeaderFooterRef {
    pub(crate) rel_id: String,
    pub(crate) kind: HeaderFooterKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportResult {
    pub doc_handle: DocHandle,
    /// Shared with the runtime's stored snapshot via `Arc` so a read does not
    /// produce a second full-resident copy of the IR (Rung 1). Callers that
    /// need to mutate take an owned copy via `Arc::make_mut` /
    /// `Arc::unwrap_or_clone`.
    pub canonical: Arc<CanonDoc>,
    pub diagnostics: Vec<Diagnostic>,
    pub fingerprint: DocFingerprint,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotImportResult {
    pub import: ImportResult,
    pub document_version: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewResult {
    /// Shared with the stored snapshot via `Arc` on the read path (Rung 1).
    pub canonical: Arc<CanonDoc>,
    pub diagnostics: Vec<Diagnostic>,
    pub fingerprint: DocFingerprint,
    /// Pending revisions `view()` projected to their accepted reading,
    /// grouped by author. `view()` FLATTENS: it returns the accepted
    /// projection, so a document carrying pending revisions loses them (and
    /// their attribution) here. This field is the disclosure of that
    /// flattening; empty when the input carried none (or for `tracked_view`,
    /// which preserves revisions instead).
    pub flattened_pending_revisions: Vec<crate::tracked_model::PendingRevisionAuthor>,
}

/// The pending revisions compare consumed from its INPUTS. Compare diffs the
/// accepted readings of base and target (`view()` runs accept-all before the
/// diff), and the output redline re-attributes every change to the compare's
/// own author — so any negotiation record in the inputs (pending revisions,
/// with their original authors) is projected away. This notice is the
/// structured disclosure of that contract, per input. Empty = the input
/// carried no pending revisions.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct FlattenedPendingRevisions {
    pub base: Vec<crate::tracked_model::PendingRevisionAuthor>,
    pub target: Vec<crate::tracked_model::PendingRevisionAuthor>,
}

/// Result of `diff_and_full_document_view`, including the canonical docs
/// needed for clause tree computation.
pub struct DiffAndFullDocViewResult {
    pub diff: DocumentDiff,
    pub full_doc: FullDocViewResult,
    pub base_canonical: Arc<CanonDoc>,
    pub target_canonical: Arc<CanonDoc>,
    /// Disclosure of the flatten contract — see [`FlattenedPendingRevisions`].
    pub flattened_pending_revisions: FlattenedPendingRevisions,
}

/// Result of computing pair analysis IR and redline bytes from one shared pass.
pub struct CompareAndRedlineResult {
    pub diff: DocumentDiff,
    pub full_doc: FullDocViewResult,
    pub base_canonical: Arc<CanonDoc>,
    pub target_canonical: Arc<CanonDoc>,
    pub merged_canonical: Arc<CanonDoc>,
    pub block_provenance: BlockProvenanceMap,
    pub redline_bytes: Vec<u8>,
    pub redline_fingerprint: DocFingerprint,
    /// Disclosure of the flatten contract — see [`FlattenedPendingRevisions`].
    pub flattened_pending_revisions: FlattenedPendingRevisions,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplyResult {
    /// Shared with the stored snapshot via `Arc` (Rung 1).
    pub canonical: Arc<CanonDoc>,
    pub diagnostics: Vec<Diagnostic>,
    pub fingerprint: DocFingerprint,
    pub applied: bool,
    pub step_results: Vec<StepResult>,
    /// Revision ids resolved AS A CASCADE of a selective resolution
    /// (cascades are enumerated, never silent) — the unselected member of
    /// a stacked pair whose segment's fate was entailed by the selected one
    /// (rejecting an insertion discards the deletion stacked inside it;
    /// accepting a deletion settles the insertion's claim on that range).
    /// Sorted. Empty for non-selective operations.
    pub cascaded_revision_ids: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepResult {
    pub step_index: usize,
    pub applied: bool,
    pub error: Option<RuntimeError>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportMode {
    Redline,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationReport {
    pub ok: bool,
    pub issues: Vec<ValidationIssue>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationIssue {
    pub code: ValidationIssueCode,
    pub message: String,
    pub context: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValidationIssueCode {
    PackageInvariant,
    WordprocessingInvariant,
    SchemaInvariant,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub level: DiagnosticLevel,
    pub message: String,
    pub context: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum DiagnosticLevel {
    Info,
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeError {
    pub code: ErrorCode,
    pub message: String,
    pub details: ErrorDetails,
}

impl std::fmt::Display for RuntimeError {
    /// Renders the structured `code` plus the human `message`, e.g.
    /// `StaleEdit: target block changed since the edit was planned`. The
    /// structured `code` / `details` fields stay available for callers that
    /// switch on them; this impl exists so `RuntimeError` can `?`-propagate
    /// into `Box<dyn std::error::Error>`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

impl std::error::Error for RuntimeError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    StaleEdit,
    UnsupportedEdit,
    AnchorNotFound,
    InvalidRange,
    /// A `replace` step would destroy one or more preserved inline anchors
    /// (opaque tokens / hard breaks). This is a distinct `opaque_preservation`
    /// validation check, kept separate from generic range errors so the LLM
    /// retry loop can fix it with the exact missing ids.
    OpaqueDestroyed,
    /// A content-bearing edit op resolved to no change (same text, marks, and
    /// anchors as the target). Surfaced distinctly so the caller can tell a
    /// no-op apart from a stale-edit precondition failure: a no-op means "this
    /// op was unnecessary", not "the document moved under you".
    NoOpEdit,
    /// A write's replacement content begins with a prefix duplicating the
    /// paragraph's numbering label (the serializer re-emits the label, so
    /// applying it would render the number twice). Distinct code so the agent
    /// learns to omit the label — see `EditError::PrefixDuplicatesLabel`.
    PrefixDuplicatesLabel,
    /// A structural op's destination anchor names a block that is a
    /// tracked-move source (a `w:moveFrom` shadow at its OLD position, not
    /// where the content now lives). Distinct code so the agent learns to
    /// anchor on the moved copy or a stable neighbor instead of guessing —
    /// see `EditError::AmbiguousAnchorAfterMove`.
    AmbiguousAnchorAfterMove,
    InvalidDocx,
    InvalidSnapshot,
    InternalError,
    ValidationFailed,
    /// An authored write's `revision.author` matches an author already
    /// present in the document's redline at open time (`SnapshotMeta::
    /// origin_authors`). See [`EditSnapshot::guard_author`].
    AuthorImpersonation,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ErrorDetails {
    pub block_id: Option<NodeId>,
    pub step_index: Option<usize>,
    pub context: Option<String>,
    /// Structured detail for `ErrorCode::StaleEdit`. Surfaced on the wire so
    /// callers can distinguish `expect` mismatches from semantic-hash drift
    /// without string-parsing the human error message.
    pub stale_edit: Option<Box<StaleEditDetails>>,
    /// Structured detail for `ErrorCode::OpaqueDestroyed`. Surfaced verbatim
    /// on the wire so the Python backend and frontend can switch on the
    /// failing check and render the missing ids without string-parsing the
    /// human message.
    ///
    /// Boxed so `RuntimeError` — which appears as the `Err` variant in
    /// many pipeline results — does not grow for the common success path.
    /// Clippy (`result_large_err`) flags any `Result<_, RuntimeError>`
    /// that exceeds ~128 bytes, which the unboxed version easily hit.
    pub opaque_preservation: Option<Box<OpaquePreservationDetails>>,
    /// Structured detail for `ErrorCode::AmbiguousAnchorAfterMove`. Surfaced
    /// verbatim on the wire so callers can render the anchor id and its
    /// moveTo copy id without string-parsing the human message. Boxed for
    /// the same `result_large_err` reason as `opaque_preservation`.
    pub ambiguous_anchor: Option<Box<AmbiguousAnchorDetails>>,
}

/// Structured details for `stale_edit` validation failures.
///
/// The edit engine collapses both `expect` mismatches and semantic-hash
/// mismatches into `ErrorCode::StaleEdit`. Preserve the exact failing check
/// here so higher layers can surface useful diagnostics and retry guidance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StaleEditDetails {
    ExpectMismatch {
        target_block_id: NodeId,
        expected: String,
        actual_text: String,
    },
    SemanticHashMismatch {
        target_block_id: NodeId,
        expected: String,
        actual: String,
    },
}

/// Structured details for the `opaque_preservation` validation check.
///
/// This is the wire shape of the `opaque_preservation` validation error.
/// Carried on `ErrorDetails` rather than serialized inline so we can add
/// future structured-detail variants without widening the top-level
/// `ErrorDetails` struct.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpaquePreservationDetails {
    /// The paragraph whose replacement would have destroyed the anchors.
    pub target_block_id: NodeId,
    /// Stable ids of the preserved inlines that would be destroyed.
    pub missing_opaque_ids: Vec<String>,
    /// Parallel vector of engine-level kind labels ("opaque", "hard_break").
    pub missing_inline_kinds: Vec<String>,
    /// Short preview of the original paragraph's visible text, with
    /// preserved inlines rendered as `[id]` placeholders.
    pub original_text_preview: String,
}

/// Structured details for the `ambiguous_anchor` validation check
/// (`ErrorCode::AmbiguousAnchorAfterMove`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AmbiguousAnchorDetails {
    /// The destination anchor id the caller supplied.
    pub anchor_id: NodeId,
    /// The step index (within the SAME transaction) whose move turned
    /// `anchor_id` into a moveFrom shadow. `None` when `anchor_id` was
    /// already a moveFrom shadow in the document the transaction started
    /// from (a previously committed move, or one imported from DOCX).
    pub moved_by_step_index: Option<usize>,
    /// The id of the moveTo copy holding the content that used to live at
    /// `anchor_id` — anchor there instead. `None` when the copy cannot be
    /// located (a dirty import can carry an unpaired `w:moveFrom`).
    pub moved_to_block_id: Option<NodeId>,
}

pub trait DocxRuntime {
    fn import_docx(&self, docx_bytes: &[u8]) -> Result<ImportResult, RuntimeError>;

    fn import_snapshot_blob(
        &self,
        snapshot_bytes: &[u8],
    ) -> Result<SnapshotImportResult, RuntimeError>;

    /// Import two documents in parallel (base + target for diffing).
    /// Returns both results in wall-clock time of the slower import.
    fn import_docx_pair(
        &self,
        base_bytes: &[u8],
        target_bytes: &[u8],
    ) -> Result<(ImportResult, ImportResult), RuntimeError> {
        // Default sequential implementation; SimpleRuntime overrides with parallel.
        let base = self.import_docx(base_bytes)?;
        let target = self.import_docx(target_bytes)?;
        Ok((base, target))
    }

    fn view(&self, handle: &DocHandle) -> Result<ViewResult, RuntimeError>;

    fn export_docx(&self, handle: &DocHandle, mode: ExportMode) -> Result<Vec<u8>, RuntimeError>;

    fn export_snapshot_blob(&self, handle: &DocHandle) -> Result<Vec<u8>, RuntimeError>;

    fn validate_docx_bytes(&self, docx_bytes: &[u8]) -> Result<ValidationReport, RuntimeError>;

    fn validate_handle(&self, handle: &DocHandle) -> Result<ValidationReport, RuntimeError>;
}

/// In-memory document runtime for DOCX operations.
///
/// # Thread Safety
///
/// `SimpleRuntime` is internally thread-safe. Documents are stored in a `DashMap`
/// with a per-shard lock, and the handle counter uses `AtomicU64`. All public
/// methods take `&self`, so the server can hold `Arc<SimpleRuntime>` directly
/// without an outer `Mutex`.
///
/// Each method clones document bytes out of the map immediately (microsecond
/// shard lock), then does all heavy computation on owned data with no lock held.
/// Callback that validates exported DOCX bytes before returning them.
/// Return `Ok(())` to accept, `Err(message)` to reject with an error.
pub type ExportValidator = Arc<dyn Fn(&[u8]) -> Result<(), String> + Send + Sync>;

pub struct SimpleRuntime {
    docs: DashMap<String, DocState>,
    next_handle: AtomicU64,
    export_validator: Option<ExportValidator>,
}

/// Export scaffold for the main document body.
///
/// This is cold package/template state owned by the editable snapshot, not a
/// transient cache hanging off DOCX bytes. It gives the serializer the shell
/// needed to rebuild `word/document.xml` without treating original upload bytes
/// as authoritative state.
#[derive(Clone)]
struct BodyTemplate {
    /// The fully-parsed document.xml root element with body children drained.
    /// Serialize can reuse this shell (namespaces, prefixes, document element
    /// structure) without re-parsing the multi-MB document.xml.
    root_shell: Element,
    /// Body children referenced by OpaqueBlock proof anchors, keyed by body index.
    opaque_children: HashMap<usize, XMLNode>,
    /// All w:sectPr children from the body.
    sect_pr_nodes: Vec<XMLNode>,
    /// Original body children count (before draining), needed by serialize.
    body_children_len: usize,
}

#[derive(Clone)]
struct PackageScaffold {
    package: DocxPackage,
    body_template: BodyTemplate,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
struct SnapshotMeta {
    snapshot_schema_version: u32,
    document_version: u64,
    source_fingerprint: DocFingerprint,
    current_docx_fingerprint: DocFingerprint,
    /// Authors already present in this document's redline at
    /// snapshot-construction time (import or blob restore) — BEFORE this
    /// handle's session authored anything. Frozen at construction and
    /// carried forward unchanged by every rebuild (edit, metadata rewrite,
    /// merge); nothing that runs after construction ever adds to this set.
    /// This is the author-impersonation guard's off-limits set — see
    /// [`EditSnapshot::guard_author`].
    origin_authors: BTreeSet<String>,
}

/// A document's full working state inside the engine: the typed IR
/// ([`CanonDoc`]) plus the `PackageScaffold` carrying unmodeled OOXML parts
/// (styles, settings, content types, embedded media, comment XML, etc.)
/// that survive round-trip verbatim.
///
/// This is the compilation-unit type. Engine operations take and return
/// `EditSnapshot` values; sessions hold them in a handle store.
///
/// # Do not persist
///
/// `EditSnapshot` is **ephemeral, engine-version-bound state**. The struct
/// shape — and the shapes of its embedded IR types — change with engine
/// releases. Serializing an `EditSnapshot` to durable storage means every
/// engine bump becomes a migration problem.
///
/// Persist the DOCX bytes (the source artifact) and the
/// [`crate::edit::EditTransaction`] history (small, durable, version-stable).
/// Re-derive the snapshot via [`SimpleRuntime::import_docx`] on cold
/// resume.
#[derive(Clone)]
pub struct EditSnapshot {
    /// The working IR. Stored behind an `Arc` so the read path (`import_docx`,
    /// `view`, getters) can hand out cheap shared clones instead of a second
    /// full-resident deep copy of every block (Rung 1 memory reduction).
    ///
    /// Mutation is unchanged: edit/projection paths call `Arc::make_mut` (or
    /// take an owned copy) before mutating, so a writer always sees an
    /// independent `CanonDoc` and never observes another holder's tree.
    pub canonical: Arc<CanonDoc>,
    scaffold: PackageScaffold,
    meta: SnapshotMeta,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
struct PersistedOpaqueChild {
    body_index: usize,
    xml: Vec<u8>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
struct PersistedBodyTemplate {
    root_shell_xml: Vec<u8>,
    opaque_children: Vec<PersistedOpaqueChild>,
    sect_pr_nodes_xml: Vec<Vec<u8>>,
    body_children_len: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
struct PersistedEditSnapshot {
    blob_schema_version: u32,
    canonical: CanonDoc,
    package_bytes: Vec<u8>,
    body_template: PersistedBodyTemplate,
    meta: SnapshotMeta,
    diagnostics: Vec<Diagnostic>,
}

struct DocState {
    snapshot: EditSnapshot,
    diagnostics: Vec<Diagnostic>,
    last_accessed_epoch_secs: AtomicU64,
    /// When present, `snapshot.canonical` is already the correct `view()`
    /// projection because the imported document had no pre-existing tracked
    /// changes. This avoids re-parsing the DOCX bytes on every `view()` call
    /// without storing a duplicate canonical tree.
    cached_view_fingerprint: Option<DocFingerprint>,
    /// Derived anchored DOCX bytes regenerated from the snapshot scaffold on
    /// demand. This is a cache/recovery artifact, never the authoritative edit
    /// state.
    cached_docx_bytes: Option<Arc<[u8]>>,
    /// The open-time canonical tree — the session-review baseline (RFC 0001).
    /// An `Arc` share of the import-time IR (the scaffold is NOT retained
    /// twice). Set once at insert; deliberately untouched by
    /// `update_snapshot` — edits advance the snapshot, never the baseline;
    /// re-opening is the only reset. Cloning a handle carries the origin's
    /// baseline (the clone continues the same session lineage).
    baseline: Arc<CanonDoc>,
    /// The byte package this handle's baseline corresponds to (the anchored
    /// import bytes), retained so a session review can render its delta as a
    /// redline by importing these as compare's base. One input-file-sized
    /// copy per handle, immutable.
    source_bytes: Arc<[u8]>,
}

fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before UNIX epoch")
        .as_secs()
}

// v2: StyleProps gained `preserved: Vec<PreservedProp>` (rPr disciplined-
// preservation remainder). bincode is positional, so a new serde field in the
// IR is a breaking blob change — this bump is the designed fail-fast gate,
// not a behavior change to the version check itself.
// v3: TableMeasurement gained `pct_literal: Option<String>` (source-form
// preservation for ST_Percentage width literals, §17.18.107).
// v4: TableCellNode's single `content_sdt_wrapper: Option<SdtWrapper>` became
// `content_sdt_wraps: Vec<CellSdtWrap>` (explicit per-range block-level content
// controls, so a cell SDT no longer swallows a following sibling block on
// export). bincode is positional, so this IR shape change is a breaking blob
// change — the bump is the designed fail-fast gate.
// v5: ParagraphNode gained `has_direct_numbering: bool` (gate so inherited
// numbering is not materialized as a direct numPr) and `bidi`/`mirror_indents`
// changed from `bool` to `Option<bool>` (three-state on/off so an explicit OFF
// round-trips); `ParagraphFormattingChange`/`PPrChange` `previous_bidi`/
// `previous_mirror_indents` changed to `Option<bool>` to match. Positional IR
// shape change — breaking blob change, fail-fast gated.
// v6: CanonDoc gained `document_protection: Option<DocumentProtection>` (the
// reported w:documentProtection declaration from settings.xml). Positional IR
// shape change — breaking blob change, fail-fast gated.
const SNAPSHOT_BLOB_SCHEMA_VERSION: u32 = 6;
const EDIT_SNAPSHOT_SCHEMA_VERSION: u32 = 1;
const SNAPSHOT_BLOB_ZSTD_LEVEL: i32 = 3;

impl SimpleRuntime {
    pub fn new() -> Self {
        Self {
            docs: DashMap::new(),
            next_handle: AtomicU64::new(1),
            export_validator: None,
        }
    }

    /// Set a validator that runs on every `export_docx()` output.
    ///
    /// When set, the validator receives the exported DOCX bytes before they
    /// are returned. If it returns `Err`, the export fails with a
    /// `ValidationFailed` error. Use this to wire in an external validator
    /// (e.g. a real-Word open-clean check) so no document is returned without
    /// passing it.
    pub fn set_export_validator<F>(&mut self, validator: F)
    where
        F: Fn(&[u8]) -> Result<(), String> + Send + Sync + 'static,
    {
        self.export_validator = Some(Arc::new(validator));
    }

    pub fn block_text_for(
        &self,
        handle: &DocHandle,
        block_id: &NodeId,
    ) -> Result<String, RuntimeError> {
        let entry = self.docs.get(&handle.0).ok_or_else(|| RuntimeError {
            code: ErrorCode::InvalidDocx,
            message: "doc handle not found".to_string(),
            details: ErrorDetails {
                context: Some(handle.0.clone()),
                ..ErrorDetails::default()
            },
        })?;
        for tracked in &entry.snapshot.canonical.blocks {
            if let BlockNode::Paragraph(p) = &tracked.block
                && p.id == *block_id
            {
                return Ok(crate::import::extract_block_text(&tracked.block));
            }
        }
        Err(RuntimeError {
            code: ErrorCode::AnchorNotFound,
            message: format!("block id not found in canonical: {}", block_id.0),
            details: ErrorDetails {
                block_id: Some(block_id.clone()),
                ..ErrorDetails::default()
            },
        })
    }

    /// Build a canonical view of the current handle while preserving existing
    /// tracked changes instead of normalizing them away.
    ///
    /// This is used by source-side redline transfer flows, where the caller
    /// needs to inspect authored revisions rather than the accepted document.
    pub fn tracked_view(&self, handle: &DocHandle) -> Result<ViewResult, RuntimeError> {
        let (canonical, fingerprint) = self.tracked_view_state_for(handle)?;
        let diagnostics = self.diagnostics_for(handle)?;
        Ok(ViewResult {
            canonical,
            diagnostics,
            fingerprint,
            // tracked_view PRESERVES pending revisions — nothing is flattened.
            flattened_pending_revisions: Vec::new(),
        })
    }

    fn insert_doc(
        &self,
        snapshot: EditSnapshot,
        diagnostics: Vec<Diagnostic>,
        cached_view_fingerprint: Option<DocFingerprint>,
        cached_docx_bytes: Option<Arc<[u8]>>,
        baseline: Arc<CanonDoc>,
        source_bytes: Arc<[u8]>,
    ) -> DocHandle {
        let id = self.next_handle.fetch_add(1, Ordering::Relaxed);
        let handle = DocHandle(format!("doc_{id}"));
        self.docs.insert(
            handle.0.clone(),
            DocState {
                snapshot,
                diagnostics,
                last_accessed_epoch_secs: AtomicU64::new(now_epoch_secs()),
                cached_view_fingerprint,
                cached_docx_bytes,
                baseline,
                source_bytes,
            },
        );
        handle
    }

    fn tracked_view_state_for(
        &self,
        handle: &DocHandle,
    ) -> Result<(Arc<CanonDoc>, DocFingerprint), RuntimeError> {
        let entry = self.docs.get(&handle.0).ok_or_else(|| RuntimeError {
            code: ErrorCode::InvalidDocx,
            message: "doc handle not found".to_string(),
            details: ErrorDetails {
                context: Some(handle.0.clone()),
                ..ErrorDetails::default()
            },
        })?;
        entry
            .last_accessed_epoch_secs
            .store(now_epoch_secs(), Ordering::Relaxed);
        Ok((
            Arc::clone(&entry.snapshot.canonical),
            entry.snapshot.meta.current_docx_fingerprint.clone(),
        ))
    }

    fn canonical_for(&self, handle: &DocHandle) -> Result<Arc<CanonDoc>, RuntimeError> {
        let entry = self.docs.get(&handle.0).ok_or_else(|| RuntimeError {
            code: ErrorCode::InvalidDocx,
            message: "doc handle not found".to_string(),
            details: ErrorDetails {
                context: Some(handle.0.clone()),
                ..ErrorDetails::default()
            },
        })?;
        entry
            .last_accessed_epoch_secs
            .store(now_epoch_secs(), Ordering::Relaxed);
        Ok(Arc::clone(&entry.snapshot.canonical))
    }

    fn edit_context_for(
        &self,
        handle: &DocHandle,
    ) -> Result<(CanonDoc, BodyTemplate, SnapshotMeta), RuntimeError> {
        let entry = self.docs.get(&handle.0).ok_or_else(|| RuntimeError {
            code: ErrorCode::InvalidDocx,
            message: "doc handle not found".to_string(),
            details: ErrorDetails {
                context: Some(handle.0.clone()),
                ..ErrorDetails::default()
            },
        })?;
        entry
            .last_accessed_epoch_secs
            .store(now_epoch_secs(), Ordering::Relaxed);
        Ok((
            // The caller mutates this `CanonDoc` (edit / projection / metadata
            // paths), so it must be an independent owned copy, not a shared
            // `Arc`. The snapshot keeps its `Arc`, so this deep-copies the IR
            // once — the unavoidable cost of a write that branches from shared
            // read state.
            (*entry.snapshot.canonical).clone(),
            entry.snapshot.scaffold.body_template.clone(),
            entry.snapshot.meta.clone(),
        ))
    }

    fn scaffold_update_context_for(
        &self,
        handle: &DocHandle,
    ) -> Result<(BodyTemplate, SnapshotMeta), RuntimeError> {
        let entry = self.docs.get(&handle.0).ok_or_else(|| RuntimeError {
            code: ErrorCode::InvalidDocx,
            message: "doc handle not found".to_string(),
            details: ErrorDetails {
                context: Some(handle.0.clone()),
                ..ErrorDetails::default()
            },
        })?;
        entry
            .last_accessed_epoch_secs
            .store(now_epoch_secs(), Ordering::Relaxed);
        Ok((
            entry.snapshot.scaffold.body_template.clone(),
            entry.snapshot.meta.clone(),
        ))
    }

    fn diagnostics_for(&self, handle: &DocHandle) -> Result<Vec<Diagnostic>, RuntimeError> {
        let entry = self.docs.get(&handle.0).ok_or_else(|| RuntimeError {
            code: ErrorCode::InvalidDocx,
            message: "doc handle not found".to_string(),
            details: ErrorDetails {
                context: Some(handle.0.clone()),
                ..ErrorDetails::default()
            },
        })?;
        entry
            .last_accessed_epoch_secs
            .store(now_epoch_secs(), Ordering::Relaxed);
        Ok(entry.diagnostics.clone())
    }

    fn body_template_for(&self, handle: &DocHandle) -> Result<BodyTemplate, RuntimeError> {
        let entry = self.docs.get(&handle.0).ok_or_else(|| RuntimeError {
            code: ErrorCode::InvalidDocx,
            message: "doc handle not found".to_string(),
            details: ErrorDetails {
                context: Some(handle.0.clone()),
                ..ErrorDetails::default()
            },
        })?;
        entry
            .last_accessed_epoch_secs
            .store(now_epoch_secs(), Ordering::Relaxed);
        Ok(entry.snapshot.scaffold.body_template.clone())
    }

    /// Regenerate anchored DOCX bytes from the snapshot scaffold when the
    /// derived byte cache is cold.
    fn get_doc_bytes(&self, handle: &DocHandle) -> Result<Arc<[u8]>, RuntimeError> {
        if let Some(entry) = self.docs.get(&handle.0)
            && let Some(bytes) = &entry.cached_docx_bytes
        {
            entry
                .last_accessed_epoch_secs
                .store(now_epoch_secs(), Ordering::Relaxed);
            return Ok(bytes.clone());
        }

        let mut entry = self.docs.get_mut(&handle.0).ok_or_else(|| RuntimeError {
            code: ErrorCode::InvalidDocx,
            message: "doc handle not found".to_string(),
            details: ErrorDetails {
                context: Some(handle.0.clone()),
                ..ErrorDetails::default()
            },
        })?;
        let archive = entry
            .snapshot
            .scaffold
            .package
            .clone()
            .into_archive()
            .map_err(map_package_error)?;
        let bytes: Arc<[u8]> = Arc::from(archive.write().map_err(map_docx_error)?);
        entry
            .last_accessed_epoch_secs
            .store(now_epoch_secs(), Ordering::Relaxed);
        entry.cached_docx_bytes = Some(bytes.clone());
        Ok(bytes)
    }

    fn update_snapshot(
        &self,
        handle: &DocHandle,
        snapshot: EditSnapshot,
        diagnostics: Vec<Diagnostic>,
        cached_view: Option<DocFingerprint>,
        cached_docx_bytes: Option<Arc<[u8]>>,
    ) -> Result<(), RuntimeError> {
        let mut entry = self.docs.get_mut(&handle.0).ok_or_else(|| RuntimeError {
            code: ErrorCode::InvalidDocx,
            message: "doc handle not found".to_string(),
            details: ErrorDetails {
                context: Some(handle.0.clone()),
                ..ErrorDetails::default()
            },
        })?;
        entry.snapshot = snapshot;
        entry.diagnostics = diagnostics;
        entry.cached_view_fingerprint = cached_view;
        entry.cached_docx_bytes = cached_docx_bytes;
        entry
            .last_accessed_epoch_secs
            .store(now_epoch_secs(), Ordering::Relaxed);
        Ok(())
    }

    /// Clone a document handle, creating an independent copy of the current
    /// editable snapshot and derived caches.
    ///
    /// Borrow the [`EditSnapshot`] for a handle and run an arbitrary read.
    ///
    /// Escape hatch for callers that need IR access not covered by the
    /// dedicated methods on this runtime ([`Self::view`],
    /// [`Self::tracked_view`], [`Self::block_text_for`], etc.). The closure
    /// receives a borrow of the snapshot; the runtime's shard lock is held
    /// for the duration of the call, so keep the body short and don't call
    /// other runtime methods from inside it (deadlock risk on the same
    /// shard).
    ///
    /// For mutation, use the dedicated apply methods; this is read-only by
    /// design.
    pub fn with<F, R>(&self, handle: &DocHandle, f: F) -> Result<R, RuntimeError>
    where
        F: FnOnce(&EditSnapshot) -> R,
    {
        let entry = self.docs.get(&handle.0).ok_or_else(|| RuntimeError {
            code: ErrorCode::InvalidDocx,
            message: "doc handle not found".to_string(),
            details: ErrorDetails {
                context: Some(handle.0.clone()),
                ..ErrorDetails::default()
            },
        })?;
        entry
            .last_accessed_epoch_secs
            .store(now_epoch_secs(), Ordering::Relaxed);
        Ok(f(&entry.snapshot))
    }

    /// IMPORTANT: `self.docs` is a `DashMap` and `get()` returns a `Ref` that
    /// holds a read guard on the key's shard. `insert_doc` acquires a write
    /// guard on the new handle's shard — if the two handles hash to the same
    /// shard, the write blocks forever waiting for the read guard we're
    /// still holding. We extract the cloned fields inside a scoped block so
    /// the `Ref` is dropped before calling `insert_doc`.
    pub fn clone_handle(&self, handle: &DocHandle) -> Result<DocHandle, RuntimeError> {
        let (snapshot, diagnostics, cached_view_fingerprint, cached_docx_bytes, baseline, source) = {
            let entry = self.docs.get(&handle.0).ok_or_else(|| RuntimeError {
                code: ErrorCode::InvalidDocx,
                message: "doc handle not found".to_string(),
                details: ErrorDetails {
                    context: Some(handle.0.clone()),
                    ..ErrorDetails::default()
                },
            })?;
            entry
                .last_accessed_epoch_secs
                .store(now_epoch_secs(), Ordering::Relaxed);
            (
                entry.snapshot.clone(),
                entry.diagnostics.clone(),
                entry.cached_view_fingerprint.clone(),
                entry.cached_docx_bytes.clone(),
                // The clone continues the same session lineage: same review
                // baseline, same baseline bytes.
                Arc::clone(&entry.baseline),
                Arc::clone(&entry.source_bytes),
            )
        };
        Ok(self.insert_doc(
            snapshot,
            diagnostics,
            cached_view_fingerprint,
            cached_docx_bytes,
            baseline,
            source,
        ))
    }

    /// RFC 0001 session review: audit everything this handle changed since
    /// it was opened, against the retained open-time baseline (see
    /// [`crate::audit::AuditReport`]). The package verdict is computed on
    /// the handle's would-be save bytes — an UNGATED serialize followed by
    /// honest validation; review REPORTS validity, the save gate
    /// (`export_docx`) remains the only place bytes are refused.
    ///
    /// Pure with respect to the session: saving does not reset the
    /// baseline; re-opening is the only reset.
    pub fn review_session(
        &self,
        handle: &DocHandle,
    ) -> Result<crate::audit::AuditReport, RuntimeError> {
        let entry = self.docs.get(&handle.0).ok_or_else(|| RuntimeError {
            code: ErrorCode::InvalidDocx,
            message: "doc handle not found".to_string(),
            details: ErrorDetails {
                context: Some(handle.0.clone()),
                ..ErrorDetails::default()
            },
        })?;
        entry
            .last_accessed_epoch_secs
            .store(now_epoch_secs(), Ordering::Relaxed);
        let bytes = serialize_snapshot(&entry.snapshot, &ExportOptions::unchecked())?;
        // Each side's own style table: the baseline's from the retained source
        // bytes, the current snapshot's from its serialization — so a rejected
        // paragraph-style change re-resolves style-inherited run marks on both
        // committed baselines (see `audit_documents`).
        let before_styles = style_table_from_docx(&entry.source_bytes)?;
        let after_styles = style_table_from_docx(&bytes)?;
        crate::audit::audit_documents(
            &entry.baseline,
            &entry.snapshot.canonical,
            before_styles.as_ref(),
            after_styles.as_ref(),
            crate::api::validate(&bytes),
        )
    }

    /// The byte package this handle's review baseline corresponds to (the
    /// anchored open-time bytes) — compare's base when a session review is
    /// rendered as a redline.
    pub fn session_source_bytes(&self, handle: &DocHandle) -> Result<Arc<[u8]>, RuntimeError> {
        let entry = self.docs.get(&handle.0).ok_or_else(|| RuntimeError {
            code: ErrorCode::InvalidDocx,
            message: "doc handle not found".to_string(),
            details: ErrorDetails {
                context: Some(handle.0.clone()),
                ..ErrorDetails::default()
            },
        })?;
        entry
            .last_accessed_epoch_secs
            .store(now_epoch_secs(), Ordering::Relaxed);
        Ok(Arc::clone(&entry.source_bytes))
    }

    /// Serialize a document's full working state ([`EditSnapshot`] +
    /// diagnostics + the in-memory anchored DOCX bytes) into a compressed
    /// bincode blob.
    ///
    /// # Warning: not for durable storage
    ///
    /// The blob format embeds the engine's IR types verbatim. Any change to
    /// those types in a future engine release invalidates older blobs and
    /// turns this into a migration problem.
    ///
    /// Use this for **hot, in-process handoff** between workers running the
    /// same engine build (for example, passing state across a request
    /// boundary in a Python-Rust integration). Treat blobs as short-TTL
    /// cache entries, never as long-term storage. Persist the source DOCX
    /// bytes and the [`crate::edit::EditTransaction`] history instead; on a
    /// cold session, re-derive via [`Self::import_docx`] and replay the
    /// transactions.
    pub fn export_snapshot_blob(&self, handle: &DocHandle) -> Result<Vec<u8>, RuntimeError> {
        let (canonical, body_template, meta) = self.edit_context_for(handle)?;
        let diagnostics = self.diagnostics_for(handle)?;
        let package_bytes = self.get_doc_bytes(handle)?;
        let body_template =
            persist_body_template(&body_template).map_err(|message| RuntimeError {
                code: ErrorCode::InternalError,
                message,
                details: ErrorDetails {
                    context: Some(handle.0.clone()),
                    ..ErrorDetails::default()
                },
            })?;
        let persisted = PersistedEditSnapshot {
            blob_schema_version: SNAPSHOT_BLOB_SCHEMA_VERSION,
            canonical,
            package_bytes: package_bytes.as_ref().to_vec(),
            body_template,
            meta,
            diagnostics,
        };
        let encoded = bincode::serialize(&persisted).map_err(|source| RuntimeError {
            code: ErrorCode::InternalError,
            message: format!("snapshot blob serialization failed: {source}"),
            details: ErrorDetails {
                context: Some(handle.0.clone()),
                ..ErrorDetails::default()
            },
        })?;
        zstd::stream::encode_all(Cursor::new(encoded), SNAPSHOT_BLOB_ZSTD_LEVEL).map_err(|source| {
            RuntimeError {
                code: ErrorCode::InternalError,
                message: format!("snapshot blob compression failed: {source}"),
                details: ErrorDetails {
                    context: Some(handle.0.clone()),
                    ..ErrorDetails::default()
                },
            }
        })
    }

    /// Re-hydrate a blob produced by [`Self::export_snapshot_blob`] into
    /// a fresh session handle.
    ///
    /// # Warning: not for durable storage
    ///
    /// See the warning on [`Self::export_snapshot_blob`]. Blobs only
    /// round-trip across processes running the **same engine build** — they
    /// reject on schema-version mismatch but cannot reject on silent
    /// internal IR-shape drift across builds.
    pub fn import_snapshot_blob(
        &self,
        snapshot_bytes: &[u8],
    ) -> Result<SnapshotImportResult, RuntimeError> {
        let decoded = zstd::stream::decode_all(Cursor::new(snapshot_bytes)).map_err(|source| {
            invalid_snapshot(&format!("snapshot blob decompression failed: {source}"))
        })?;
        let persisted: PersistedEditSnapshot =
            bincode::deserialize(&decoded).map_err(|source| {
                invalid_snapshot(&format!("snapshot blob decode failed: {source}"))
            })?;
        if persisted.blob_schema_version != SNAPSHOT_BLOB_SCHEMA_VERSION {
            return Err(invalid_snapshot(&format!(
                "unsupported snapshot blob schema version {}, expected {}",
                persisted.blob_schema_version, SNAPSHOT_BLOB_SCHEMA_VERSION
            )));
        }
        if persisted.meta.snapshot_schema_version != EDIT_SNAPSHOT_SCHEMA_VERSION {
            return Err(invalid_snapshot(&format!(
                "unsupported edit snapshot schema version {}, expected {}",
                persisted.meta.snapshot_schema_version, EDIT_SNAPSHOT_SCHEMA_VERSION
            )));
        }
        if persisted.canonical.meta.docx_fingerprint != persisted.meta.current_docx_fingerprint {
            return Err(invalid_snapshot(
                "canonical fingerprint does not match snapshot metadata",
            ));
        }

        let archive = DocxArchive::read(&persisted.package_bytes).map_err(|source| {
            invalid_snapshot(&format!(
                "snapshot package bytes are not a valid DOCX archive: {source:?}"
            ))
        })?;
        let package = DocxPackage::from_archive(&archive).map_err(|source| {
            invalid_snapshot(&format!(
                "snapshot package scaffold decode failed: {source}"
            ))
        })?;
        let package_fingerprint = fingerprint(&persisted.package_bytes);
        if package_fingerprint != persisted.meta.current_docx_fingerprint {
            return Err(invalid_snapshot(&format!(
                "snapshot package fingerprint mismatch: metadata={}, package={}",
                persisted.meta.current_docx_fingerprint.0, package_fingerprint.0
            )));
        }

        let body_template =
            hydrate_body_template(&persisted.body_template).map_err(|message| RuntimeError {
                code: ErrorCode::InvalidSnapshot,
                message,
                details: ErrorDetails::default(),
            })?;
        let canonical = Arc::new(persisted.canonical);
        let diagnostics = persisted.diagnostics;
        let fingerprint = persisted.meta.current_docx_fingerprint.clone();
        let document_version = persisted.meta.document_version;
        let snapshot = EditSnapshot {
            canonical: Arc::clone(&canonical),
            scaffold: PackageScaffold {
                package,
                body_template,
            },
            meta: persisted.meta,
        };
        // A restored blob opens a fresh session: its review baseline is the
        // restored state itself (the pre-blob edit history is not a session).
        let source_bytes: Arc<[u8]> = Arc::from(persisted.package_bytes);
        let handle = self.insert_doc(
            snapshot,
            diagnostics.clone(),
            None,
            Some(Arc::clone(&source_bytes)),
            Arc::clone(&canonical),
            source_bytes,
        );
        Ok(SnapshotImportResult {
            import: ImportResult {
                doc_handle: handle,
                canonical,
                diagnostics,
                fingerprint,
            },
            document_version,
        })
    }

    /// Evict documents that haven't been accessed within `ttl_secs` seconds.
    /// Returns the number of evicted documents.
    pub fn evict_expired(&self, ttl_secs: u64) -> usize {
        let now = now_epoch_secs();
        let before = self.docs.len();
        self.docs.retain(|_, state| {
            let last = state.last_accessed_epoch_secs.load(Ordering::Relaxed);
            now.saturating_sub(last) < ttl_secs
        });
        let evicted = before - self.docs.len();
        if evicted > 0 {
            tracing::info!(
                "evicted {evicted} expired document(s), {} remaining",
                self.docs.len()
            );
        }
        evicted
    }
}

fn persist_body_template(template: &BodyTemplate) -> Result<PersistedBodyTemplate, String> {
    let namespaces = template.root_shell.namespaces.as_ref();
    let mut opaque_children = template
        .opaque_children
        .iter()
        .map(|(body_index, node)| {
            serialize_xml_node_fragment(node, namespaces).map(|xml| PersistedOpaqueChild {
                body_index: *body_index,
                xml,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    opaque_children.sort_by_key(|child| child.body_index);
    let sect_pr_nodes_xml = template
        .sect_pr_nodes
        .iter()
        .map(|node| serialize_xml_node_fragment(node, namespaces))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PersistedBodyTemplate {
        root_shell_xml: serialize_xml_element(&template.root_shell)?,
        opaque_children,
        sect_pr_nodes_xml,
        body_children_len: template.body_children_len,
    })
}

fn hydrate_body_template(persisted: &PersistedBodyTemplate) -> Result<BodyTemplate, String> {
    let root_shell = parse_xml_element(&persisted.root_shell_xml)?;
    let mut opaque_children = HashMap::new();
    for child in &persisted.opaque_children {
        let previous =
            opaque_children.insert(child.body_index, parse_xml_node_fragment(&child.xml)?);
        if previous.is_some() {
            return Err(format!(
                "snapshot body template contains duplicate opaque child for body index {}",
                child.body_index
            ));
        }
    }
    let sect_pr_nodes = persisted
        .sect_pr_nodes_xml
        .iter()
        .map(|xml| parse_xml_node_fragment(xml))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(BodyTemplate {
        root_shell,
        opaque_children,
        sect_pr_nodes,
        body_children_len: persisted.body_children_len,
    })
}

fn serialize_xml_element(element: &Element) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    element
        .write_with_config(
            &mut buf,
            EmitterConfig::new().write_document_declaration(false),
        )
        .map_err(|source| format!("failed to serialize XML element: {source}"))?;
    Ok(buf)
}

fn parse_xml_element(raw_xml: &[u8]) -> Result<Element, String> {
    // Persisted opaque body children carry content: whitespace-only text nodes
    // (e.g. `<w:t xml:space="preserve"> </w:t>`) must survive the re-parse —
    // the default config drops them (same class as parse_raw_fragment).
    let config = xmltree::ParserConfig::new()
        .whitespace_to_characters(true)
        .cdata_to_characters(true);
    Element::parse_with_config(Cursor::new(raw_xml), config)
        .map_err(|source| format!("failed to parse XML element: {source}"))
}

fn serialize_xml_node_fragment(
    node: &XMLNode,
    namespaces: Option<&xmltree::Namespace>,
) -> Result<Vec<u8>, String> {
    let mut wrapper = Element::new("snapshot-fragment");
    if let Some(ns) = namespaces {
        wrapper.namespaces = Some(ns.clone());
    }
    wrapper.children.push(node.clone());
    serialize_xml_element(&wrapper)
}

fn parse_xml_node_fragment(raw_xml: &[u8]) -> Result<XMLNode, String> {
    let mut wrapper = parse_xml_element(raw_xml)?;
    if wrapper.children.len() != 1 {
        return Err(format!(
            "expected snapshot fragment to contain exactly 1 XML node, got {}",
            wrapper.children.len()
        ));
    }
    Ok(wrapper.children.remove(0))
}

/// Write the document-level `w:evenAndOddHeaders` toggle into
/// `word/settings.xml`, honoring the three-state model carried by
/// [`CanonDoc::even_and_odd_headers`] (ISO 29500-1 §17.15.1.35).
///
/// - `None`  — the document never asserted the setting: leave settings.xml
///   exactly as-is (we do NOT default it on or off — no silent fallback).
/// - `Some(_)` — assert the requested state, parsing the existing
///   settings.xml (or synthesizing a minimal one if the part is absent) and
///   running the [`crate::settings::set_even_and_odd_headers`] writer.
fn apply_even_and_odd_headers_to_settings(
    base_pkg: &mut DocxPackage,
    desired: Option<bool>,
) -> Result<(), RuntimeError> {
    // None = the IR makes no assertion about the toggle; do not touch the part.
    let Some(_) = desired else {
        return Ok(());
    };

    let settings_err = |message: String| RuntimeError {
        code: ErrorCode::InvalidDocx,
        message,
        details: ErrorDetails::default(),
    };

    // Parse the existing settings.xml, or synthesize a minimal root so the
    // toggle has a home when the document had no settings part at all.
    let (mut root, part_existed) = match base_pkg.get_part("word/settings.xml") {
        Some(bytes) => {
            let root = Element::parse(Cursor::new(bytes))
                .map_err(|e| settings_err(format!("failed to parse word/settings.xml: {e}")))?;
            (root, true)
        }
        None => {
            let mut root = Element::new("w:settings");
            let mut ns = xmltree::Namespace::empty();
            ns.put(
                "w",
                "http://schemas.openxmlformats.org/wordprocessingml/2006/main",
            );
            root.namespaces = Some(ns);
            (root, false)
        }
    };

    crate::settings::set_even_and_odd_headers(&mut root, desired);

    let mut buf = Vec::new();
    root.write_with_config(
        &mut buf,
        EmitterConfig::new().write_document_declaration(true),
    )
    .map_err(|e| settings_err(format!("failed to serialize word/settings.xml: {e}")))?;
    base_pkg.set_part("word/settings.xml", buf);

    // If we created the part from scratch, register its content-type override and
    // a document relationship so Word recognizes it.
    if !part_existed {
        base_pkg.content_types.add_override(
            "/word/settings.xml",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml",
        );
        base_pkg.document_rels.add(
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings",
            "settings.xml",
        );
    }

    Ok(())
}

impl Default for SimpleRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl SimpleRuntime {
    /// Diff two documents and return the diff structure.
    pub fn diff(
        &self,
        base_handle: &DocHandle,
        target_handle: &DocHandle,
    ) -> Result<DocumentDiff, RuntimeError> {
        let start = Instant::now();
        let base = self.view(base_handle)?;
        let view1_elapsed = start.elapsed();

        let view2_start = Instant::now();
        let target = self.view(target_handle)?;
        let view2_elapsed = view2_start.elapsed();

        let diff_start = Instant::now();
        let diff = diff_documents(&base.canonical, &target.canonical).map_err(map_diff_error)?;
        let diff_elapsed = diff_start.elapsed();

        if runtime_timing_logs_enabled() {
            eprintln!(
                "TIMING diff: view1={:.3}s view2={:.3}s diff_algo={:.3}s total={:.3}s changes={}",
                view1_elapsed.as_secs_f64(),
                view2_elapsed.as_secs_f64(),
                diff_elapsed.as_secs_f64(),
                start.elapsed().as_secs_f64(),
                diff.changes.len(),
            );
        }

        Ok(diff)
    }

    /// Build the full document view with inline diff segments for every block.
    pub fn full_document_view(
        &self,
        base_handle: &DocHandle,
        target_handle: &DocHandle,
    ) -> Result<FullDocViewResult, RuntimeError> {
        let base = self.view(base_handle)?;
        let target = self.view(target_handle)?;
        let base_bytes = self.get_doc_bytes(base_handle)?;
        let target_bytes = self.get_doc_bytes(target_handle)?;
        let base_archive = DocxArchive::read(&base_bytes).map_err(map_docx_error)?;
        let target_archive = DocxArchive::read(&target_bytes).map_err(map_docx_error)?;
        let base_image_lookup = build_image_data_lookup(&base_archive)?;
        let target_image_lookup = build_image_data_lookup(&target_archive)?;
        let blocks = build_full_document_view(
            &base.canonical,
            &target.canonical,
            &base_image_lookup,
            &target_image_lookup,
        )
        .map_err(map_diff_error)?;

        Ok(build_story_payloads(
            &base.canonical,
            &target.canonical,
            blocks,
        ))
    }

    /// Project a single document into the full-document block format.
    ///
    /// Unlike `full_document_view` (which diffs two documents), this projects one
    /// document directly. Every block is unchanged with canonical IDs. This is the
    /// editing-ready projection path for viewing/editing without comparison.
    pub fn single_document_view(
        &self,
        handle: &DocHandle,
    ) -> Result<FullDocViewResult, RuntimeError> {
        let canonical = self.canonical_for(handle)?;
        let bytes = self.get_doc_bytes(handle)?;
        let archive = DocxArchive::read(&bytes).map_err(map_docx_error)?;
        let image_lookup = build_image_data_lookup(&archive)?;
        Ok(build_tracked_document_view(&canonical, &image_lookup))
    }

    /// Combined diff + full document view from a single alignment computation.
    ///
    /// This is the preferred method for the comparison pipeline: it parses each
    /// document once, computes alignment once, and produces both the changes array
    /// (for atom assignment / changelets) and the full document view (for rendering)
    /// with guaranteed-consistent block IDs.
    pub fn diff_and_full_document_view(
        &self,
        base_handle: &DocHandle,
        target_handle: &DocHandle,
    ) -> Result<DiffAndFullDocViewResult, RuntimeError> {
        let start = Instant::now();
        let base = self.view(base_handle)?;
        let view1_elapsed = start.elapsed();

        let view2_start = Instant::now();
        let target = self.view(target_handle)?;
        let view2_elapsed = view2_start.elapsed();

        let archive_start = Instant::now();
        let base_bytes = self.get_doc_bytes(base_handle)?;
        let target_bytes = self.get_doc_bytes(target_handle)?;
        let base_archive = DocxArchive::read(&base_bytes).map_err(map_docx_error)?;
        let target_archive = DocxArchive::read(&target_bytes).map_err(map_docx_error)?;
        let base_image_lookup = build_image_data_lookup(&base_archive)?;
        let target_image_lookup = build_image_data_lookup(&target_archive)?;
        let archive_elapsed = archive_start.elapsed();

        let algo_start = Instant::now();
        let (diff, blocks) = diff_and_full_document(
            &base.canonical,
            &target.canonical,
            &base_image_lookup,
            &target_image_lookup,
        )
        .map_err(map_diff_error)?;
        let algo_elapsed = algo_start.elapsed();

        if runtime_timing_logs_enabled() {
            eprintln!(
                "TIMING diff_and_full_doc: view1={:.3}s view2={:.3}s archives={:.3}s algo={:.3}s total={:.3}s changes={} blocks={}",
                view1_elapsed.as_secs_f64(),
                view2_elapsed.as_secs_f64(),
                archive_elapsed.as_secs_f64(),
                algo_elapsed.as_secs_f64(),
                start.elapsed().as_secs_f64(),
                diff.changes.len(),
                blocks.len(),
            );
        }

        let full_doc = build_story_payloads(&base.canonical, &target.canonical, blocks);

        Ok(DiffAndFullDocViewResult {
            diff,
            full_doc,
            base_canonical: base.canonical,
            target_canonical: target.canonical,
            flattened_pending_revisions: FlattenedPendingRevisions {
                base: base.flattened_pending_revisions,
                target: target.flattened_pending_revisions,
            },
        })
    }

    /// Compute pair analysis IR and render the redline from one shared diff/merge pass.
    ///
    /// This is the efficient pair-comparison entrypoint for callers that need both
    /// the analysis IR inputs and the tracked-change DOCX artifact.
    pub fn compare_and_redline(
        &self,
        base_handle: &DocHandle,
        target_handle: &DocHandle,
        meta: TransactionMeta,
    ) -> Result<CompareAndRedlineResult, RuntimeError> {
        let total_start = Instant::now();

        let view_start = Instant::now();
        let base = self.view(base_handle)?;
        let target = self.view(target_handle)?;
        let view_elapsed = view_start.elapsed();

        refuse_quarantined_compare(&base.canonical, &target.canonical)?;

        let archive_start = Instant::now();
        let base_bytes = self.get_doc_bytes(base_handle)?;
        let target_bytes = self.get_doc_bytes(target_handle)?;
        let base_archive = DocxArchive::read(&base_bytes).map_err(map_docx_error)?;
        let target_archive = DocxArchive::read(&target_bytes).map_err(map_docx_error)?;
        let base_image_lookup = build_image_data_lookup(&base_archive)?;
        let target_image_lookup = build_image_data_lookup(&target_archive)?;
        let archive_elapsed = archive_start.elapsed();

        let diff_start = Instant::now();
        let (diff, blocks) = diff_and_full_document(
            &base.canonical,
            &target.canonical,
            &base_image_lookup,
            &target_image_lookup,
        )
        .map_err(map_diff_error)?;
        let diff_elapsed = diff_start.elapsed();

        let full_doc = build_story_payloads(&base.canonical, &target.canonical, blocks);

        let merge_start = Instant::now();
        let next_revision_id = max_revision_id(&base.canonical) + 1;
        let revision = revision_info_from_transaction_meta(&meta, next_revision_id);
        let merge_result = merge_diff(&base.canonical, &target.canonical, &diff, &revision)
            .map_err(map_merge_error)?;
        let mut merged = merge_result.doc;
        let merge_elapsed = merge_start.elapsed();

        let serialize_start = Instant::now();
        let cached_body = Some(self.body_template_for(base_handle)?);
        let redline_bytes = serialize_canonical_docx(
            &base_bytes,
            &target_bytes,
            &mut merged,
            cached_body,
            &crate::edit::PendingParts::default(),
        )?;
        let serialize_elapsed = serialize_start.elapsed();
        let redline_fingerprint = fingerprint(&redline_bytes);
        merged.meta.docx_fingerprint = redline_fingerprint.clone();

        if runtime_timing_logs_enabled() {
            eprintln!(
                "TIMING compare_and_redline: view={:.3}s archives={:.3}s diff_full_doc={:.3}s merge={:.3}s serialize={:.3}s total={:.3}s changes={}",
                view_elapsed.as_secs_f64(),
                archive_elapsed.as_secs_f64(),
                diff_elapsed.as_secs_f64(),
                merge_elapsed.as_secs_f64(),
                serialize_elapsed.as_secs_f64(),
                total_start.elapsed().as_secs_f64(),
                diff.changes.len(),
            );
        }

        Ok(CompareAndRedlineResult {
            diff,
            full_doc,
            base_canonical: base.canonical,
            target_canonical: target.canonical,
            merged_canonical: Arc::new(merged),
            block_provenance: merge_result.block_provenance,
            redline_bytes,
            redline_fingerprint,
            flattened_pending_revisions: FlattenedPendingRevisions {
                base: base.flattened_pending_revisions,
                target: target.flattened_pending_revisions,
            },
        })
    }

    /// Diff and apply as tracked changes, returning the redlined document.
    /// The base document is modified in place with tracked changes applied.
    ///
    /// Memory note: the pipeline produces several large intermediate structures
    /// (base/target canonical docs, diff, merged doc). We scope them so each
    /// phase's intermediates are dropped before the next phase begins, keeping
    /// peak RSS bounded to roughly one canonical doc at a time.
    pub fn diff_and_redline(
        &self,
        base_handle: &DocHandle,
        target_handle: &DocHandle,
        meta: TransactionMeta,
    ) -> Result<ApplyResult, RuntimeError> {
        let view_start = Instant::now();
        let base_view = self.view(base_handle)?;
        let target_view = self.view(target_handle)?;
        let view_elapsed = view_start.elapsed();

        self.diff_and_redline_inner(
            base_handle,
            target_handle,
            base_view,
            target_view,
            view_elapsed,
            meta,
        )
    }

    fn diff_and_redline_inner(
        &self,
        base_handle: &DocHandle,
        target_handle: &DocHandle,
        base_view: ViewResult,
        target_view: ViewResult,
        view_elapsed: Duration,
        meta: TransactionMeta,
    ) -> Result<ApplyResult, RuntimeError> {
        let total_start = Instant::now();

        refuse_quarantined_compare(&base_view.canonical, &target_view.canonical)?;

        let diff_start = Instant::now();
        let diff =
            diff_documents(&base_view.canonical, &target_view.canonical).map_err(map_diff_error)?;
        let diff_elapsed = diff_start.elapsed();

        // Note: we intentionally do NOT early-return when diff.changes is empty.
        // Even when text content is identical, the target document may have
        // different formatting (font, style, color, etc.). The merge pipeline's
        // sync_target_formatting pass adopts the target's formatting for
        // unchanged blocks, so we must always go through merge + serialize.

        let merge_start = Instant::now();
        let diff_changes_len = diff.changes.len();
        let next_revision_id = max_revision_id(&base_view.canonical) + 1;
        let revision = revision_info_from_transaction_meta(&meta, next_revision_id);
        let merge_result = merge_diff(
            &base_view.canonical,
            &target_view.canonical,
            &diff,
            &revision,
        )
        .map_err(map_merge_error)?;
        let mut merged = merge_result.doc;
        let merge_elapsed = merge_start.elapsed();

        // Drop base_view, target_view, and diff — they are no longer needed.
        drop(base_view);
        drop(target_view);
        drop(diff);
        hint_release_memory();

        // Phase 2: serialize the merged canonical doc to a DOCX byte stream.
        // serialize_canonical_docx emits anchor bookmarks on every paragraph,
        // so the output is already fully anchored — no re-import needed.
        let serialize_start = Instant::now();
        let base_bytes = self.get_doc_bytes(base_handle)?;
        let target_bytes = self.get_doc_bytes(target_handle)?;
        let cached_body = Some(self.body_template_for(base_handle)?);
        let redline_bytes = serialize_canonical_docx(
            &base_bytes,
            &target_bytes,
            &mut merged,
            cached_body,
            &crate::edit::PendingParts::default(),
        )?;
        let serialize_elapsed = serialize_start.elapsed();

        // Drop serialize-phase intermediates.
        drop(base_bytes);
        drop(target_bytes);
        hint_release_memory();

        // Phase 3: reuse the merged canonical directly.
        // The serialized DOCX already contains anchor bookmarks (emitted by
        // serialize_paragraph_node), so we skip the expensive import_and_anchor
        // roundtrip. The merged CanonDoc is semantically equivalent to what a
        // re-import would produce: block IDs match anchor names, formatting and
        // numbering are already resolved, and hyperlink URLs are carried over
        // from the base/target imports.
        let canon_start = Instant::now();
        let fp = fingerprint(&redline_bytes);
        merged.meta.docx_fingerprint = fp.clone();
        // Share the merged IR between the stored snapshot and the returned
        // `ApplyResult` via one `Arc` (Rung 1) instead of an extra deep copy.
        let merged = Arc::new(merged);
        let (body_template, base_meta) = self.scaffold_update_context_for(base_handle)?;
        let archive = DocxArchive::read(&redline_bytes).map_err(map_docx_error)?;
        let package = DocxPackage::from_archive(&archive).map_err(map_package_error)?;
        self.update_snapshot(
            base_handle,
            EditSnapshot {
                canonical: Arc::clone(&merged),
                scaffold: PackageScaffold {
                    package,
                    body_template,
                },
                meta: SnapshotMeta {
                    snapshot_schema_version: base_meta.snapshot_schema_version,
                    document_version: base_meta.document_version + 1,
                    origin_authors: base_meta.origin_authors.clone(),
                    source_fingerprint: base_meta.source_fingerprint,
                    current_docx_fingerprint: fp.clone(),
                },
            },
            Vec::new(),
            None,
            Some(Arc::from(redline_bytes)),
        )?;
        let canon_elapsed = canon_start.elapsed();

        if runtime_timing_logs_enabled() {
            eprintln!(
                "TIMING diff_and_redline_v2: view={:.3}s diff={:.3}s merge={:.3}s serialize={:.3}s canon={:.3}s total={:.3}s changes={}",
                view_elapsed.as_secs_f64(),
                diff_elapsed.as_secs_f64(),
                merge_elapsed.as_secs_f64(),
                serialize_elapsed.as_secs_f64(),
                canon_elapsed.as_secs_f64(),
                total_start.elapsed().as_secs_f64(),
                diff_changes_len,
            );
        }

        // Diagnostics are empty because the merged canonical was built from
        // already-validated base/target imports. The old path ran import_and_anchor
        // on the serialized bytes, which would detect parsing issues — but since we
        // just produced those bytes from a valid CanonDoc, re-import diagnostics
        // were always empty in practice.
        Ok(ApplyResult {
            canonical: merged,
            diagnostics: Vec::new(),
            fingerprint: fp,
            applied: true,
            step_results: Vec::new(),
            cascaded_revision_ids: Vec::new(),
        })
    }

    /// Apply an edit transaction to a document, serialize the result, and store
    /// the updated DOCX bytes.
    ///
    /// This is the editing counterpart to `diff_and_redline`: it takes a single
    /// document handle and an `EditTransaction`, applies the steps to the
    /// document's CanonDoc, serializes the result to DOCX with tracked changes,
    /// and stores the bytes back in the handle.
    ///
    /// The serialized output uses the same `TrackedSegment` model as
    /// `diff_and_redline`, so the serializer, accept/reject, and full-doc-view
    /// all work unchanged.
    pub fn apply_edit(
        &self,
        handle: &DocHandle,
        transaction: &crate::edit::EditTransaction,
    ) -> Result<ApplyResult, RuntimeError> {
        // Snapshot the current state, run the pure verb core, store the result.
        let prev = self.with(handle, |s| s.clone())?;
        let updated_snapshot = prev.apply(transaction)?;
        self.store_applied_snapshot(handle, updated_snapshot)
    }

    /// [`Self::apply_edit`], but enforcing [`EditSnapshot::guard_author`]
    /// first against `transaction.revision.author` — the entry point a
    /// transport uses for a caller-attributed write (see
    /// [`EditSnapshot::apply_authored`] for why bare `apply_edit` stays
    /// guard-free).
    pub fn apply_edit_authored(
        &self,
        handle: &DocHandle,
        transaction: &crate::edit::EditTransaction,
        allow_existing_author: bool,
    ) -> Result<ApplyResult, RuntimeError> {
        let prev = self.with(handle, |s| s.clone())?;
        let updated_snapshot = prev.apply_authored(transaction, allow_existing_author)?;
        self.store_applied_snapshot(handle, updated_snapshot)
    }

    /// Shared tail of `apply_edit` / `apply_edit_authored`: store the new
    /// snapshot back on the handle and build the `ApplyResult`.
    fn store_applied_snapshot(
        &self,
        handle: &DocHandle,
        updated_snapshot: EditSnapshot,
    ) -> Result<ApplyResult, RuntimeError> {
        let fp = updated_snapshot.meta.current_docx_fingerprint.clone();
        // Cheap `Arc` clone shared with the snapshot we store below (Rung 1).
        let canonical = Arc::clone(&updated_snapshot.canonical);
        // The byte cache is a recovery artifact; let `get_doc_bytes` regenerate
        // it lazily from the rebuilt scaffold (identical to what it would
        // produce here) rather than serializing twice.
        self.update_snapshot(handle, updated_snapshot, Vec::new(), None, None)?;

        Ok(ApplyResult {
            canonical,
            diagnostics: Vec::new(),
            fingerprint: fp,
            applied: true,
            step_results: Vec::new(),
            cascaded_revision_ids: Vec::new(),
        })
    }

    /// Convert manual-markup runs (strikethrough = deletion, colored
    /// text = insertion) into proper `w:ins` / `w:del` tracked
    /// segments authored as `author` / `date`. Returns the conversion
    /// counts alongside the standard apply result.
    ///
    /// `paragraph_ids` — optional filter; when supplied, only those
    /// paragraphs are converted. Used by the proposal-accept path to
    /// scope conversion to the single paragraph the user approved.
    /// `apply_op_id` — optional caller-supplied id stamped on every
    /// new RevisionInfo so the resulting changelets can be filtered
    /// back to the originating accept call.
    ///
    /// Atomic: either every detected run is wrapped or the snapshot
    /// is left unchanged. There is no partial conversion.
    pub fn convert_manual_markup(
        &self,
        handle: &DocHandle,
        author: &str,
        date: &str,
        paragraph_ids: Option<&std::collections::HashSet<String>>,
        apply_op_id: Option<String>,
    ) -> Result<(ApplyResult, crate::manual_markup::ConversionReport), RuntimeError> {
        if author.trim().is_empty() {
            return Err(RuntimeError {
                code: ErrorCode::InvalidRange,
                message: "author must be a non-empty string".to_string(),
                details: ErrorDetails::default(),
            });
        }
        let (canonical, body_template, meta) = self.edit_context_for(handle)?;
        let mut converted = canonical;
        let next_rid = max_revision_id(&converted) + 1;
        let report = crate::manual_markup::convert_manual_markup(
            &mut converted,
            author,
            date,
            next_rid,
            apply_op_id,
            paragraph_ids,
        );

        // No detected hits → nothing to write back.
        if report.insertions_converted == 0 && report.deletions_converted == 0 {
            return Ok((
                ApplyResult {
                    canonical: Arc::new(converted),
                    diagnostics: Vec::new(),
                    fingerprint: meta.current_docx_fingerprint,
                    applied: false,
                    step_results: Vec::new(),
                    cascaded_revision_ids: Vec::new(),
                },
                report,
            ));
        }

        let doc_bytes = self.get_doc_bytes(handle)?;
        let cached_body = Some(body_template.clone());
        let redline_bytes = serialize_canonical_docx(
            &doc_bytes,
            &doc_bytes,
            &mut converted,
            cached_body,
            &crate::edit::PendingParts::default(),
        )?;

        let fp = fingerprint(&redline_bytes);
        converted.meta.docx_fingerprint = fp.clone();
        // Share the converted IR between the stored snapshot and the returned
        // `ApplyResult` via one `Arc` (Rung 1).
        let converted = Arc::new(converted);
        let archive = DocxArchive::read(&redline_bytes).map_err(map_docx_error)?;
        let package = DocxPackage::from_archive(&archive).map_err(map_package_error)?;
        let updated_snapshot = EditSnapshot {
            canonical: Arc::clone(&converted),
            scaffold: PackageScaffold {
                package,
                body_template,
            },
            meta: SnapshotMeta {
                snapshot_schema_version: meta.snapshot_schema_version,
                document_version: meta.document_version + 1,
                origin_authors: meta.origin_authors.clone(),
                source_fingerprint: meta.source_fingerprint,
                current_docx_fingerprint: fp.clone(),
            },
        };
        self.update_snapshot(
            handle,
            updated_snapshot,
            Vec::new(),
            None,
            Some(Arc::from(redline_bytes)),
        )?;

        Ok((
            ApplyResult {
                canonical: converted,
                diagnostics: Vec::new(),
                fingerprint: fp,
                applied: true,
                step_results: Vec::new(),
                cascaded_revision_ids: Vec::new(),
            },
            report,
        ))
    }

    pub fn resolve_tracked_revisions(
        &self,
        handle: &DocHandle,
        revision_ids: &HashSet<u32>,
        action: ResolveSelectionAction,
    ) -> Result<ApplyResult, RuntimeError> {
        if revision_ids.is_empty() {
            return Err(RuntimeError {
                code: ErrorCode::InvalidRange,
                message: "resolve_tracked_revisions requires at least one revision id".to_string(),
                details: ErrorDetails::default(),
            });
        }

        let prev = self.with(handle, |s| s.clone())?;
        let cascaded =
            crate::tracked_model::cascaded_resolution_ids(&prev.canonical, revision_ids, action);
        let updated_snapshot = prev.project(Resolution::Selective {
            ids: revision_ids.clone(),
            action,
        })?;
        let fp = updated_snapshot.meta.current_docx_fingerprint.clone();
        // Cheap `Arc` clone shared with the snapshot we store below (Rung 1).
        let canonical = Arc::clone(&updated_snapshot.canonical);
        // The byte cache is a recovery artifact; let `get_doc_bytes` regenerate
        // it lazily from the rebuilt scaffold rather than serializing twice.
        self.update_snapshot(handle, updated_snapshot, Vec::new(), None, None)?;

        let mut cascaded_revision_ids: Vec<u32> = cascaded.into_iter().collect();
        cascaded_revision_ids.sort_unstable();
        Ok(ApplyResult {
            canonical,
            diagnostics: Vec::new(),
            fingerprint: fp,
            applied: true,
            step_results: Vec::new(),
            cascaded_revision_ids,
        })
    }
}

/// Hint the allocator to return freed pages to the OS. On glibc (Linux) this
/// calls `malloc_trim(0)` which releases freed pages back to the kernel,
/// reducing RSS. On other platforms this is a no-op.
///
/// This is important for large documents where the pipeline drops large
/// intermediate structures between phases — without this hint, glibc retains
/// the freed pages, causing RSS to stay high and potentially exceeding cgroup
/// memory limits in constrained environments.
fn hint_release_memory() {
    #[cfg(target_os = "linux")]
    {
        unsafe extern "C" {
            fn malloc_trim(pad: usize) -> std::ffi::c_int;
        }
        unsafe { malloc_trim(0) };
    }
}

pub(crate) fn map_diff_error(err: String) -> RuntimeError {
    RuntimeError {
        code: ErrorCode::InvalidDocx,
        message: err,
        details: ErrorDetails::default(),
    }
}

fn map_merge_error(err: crate::tracked_model::MergeError) -> RuntimeError {
    RuntimeError {
        code: ErrorCode::InternalError,
        message: err.message,
        details: ErrorDetails {
            context: Some(err.context),
            ..ErrorDetails::default()
        },
    }
}

/// Walk a canonical document and find the maximum revision ID across all tracked changes.
/// Returns 0 if there are no tracked changes.
///
/// Walks body blocks, table rows (tracking_status), table cell paragraphs,
/// and all story blocks (headers, footers, footnotes, endnotes, comments).
/// Scan all XML parts in the archive for the maximum `w:id` attribute value.
///
/// The `w:id` namespace is shared by bookmarks, tracked changes, comment ranges,
/// and other annotation elements. New annotation IDs must exceed all existing
/// values to avoid collisions.
/// The maximum `w:id` anywhere in the scaffold's block-opaque children — the
/// revision-id population `max_revision_id` (a `CanonDoc` walk) cannot see,
/// because block opaques keep their bytes here, not on the IR node. Fed into
/// `apply_transaction_with_id_floor` so verb-minted ids clear it.
fn max_wid_in_opaque_children(opaque_children: &HashMap<usize, XMLNode>) -> u32 {
    fn walk(node: &XMLNode, max_id: &mut u32) {
        let XMLNode::Element(el) = node else { return };
        if let Some(id) = crate::xml_attrs::attr_get(el, "w:id").and_then(|v| v.parse::<u32>().ok())
        {
            *max_id = (*max_id).max(id);
        }
        for child in &el.children {
            walk(child, max_id);
        }
    }
    let mut max_id = 0;
    for node in opaque_children.values() {
        walk(node, &mut max_id);
    }
    max_id
}

fn max_wid_in_archive(archive: &DocxArchive) -> u32 {
    let mut max_id: u32 = 0;
    let needle = b"w:id=\"";
    for name in archive.list() {
        if !name.ends_with(".xml") {
            continue;
        }
        let Some(bytes) = archive.get(name) else {
            continue;
        };
        let mut pos = 0;
        while let Some(offset) = bytes[pos..].windows(needle.len()).position(|w| w == needle) {
            let start = pos + offset + needle.len();
            let end = bytes[start..]
                .iter()
                .position(|&b| b == b'"')
                .map(|i| start + i)
                .unwrap_or(start);
            if let Ok(s) = std::str::from_utf8(&bytes[start..end])
                && let Ok(id) = s.parse::<u32>()
            {
                max_id = max_id.max(id);
            }
            pos = end + 1;
        }
    }
    max_id
}

/// Scan all XML parts for the maximum structured-document-tag id — the `w:val`
/// of a `w:sdtPr > w:id` (ECMA-376 §17.5.2.18, a `CT_DecimalNumber` written as
/// `<w:id w:val="N"/>`, NOT the `w:id="N"` attribute form scanned by
/// [`max_wid_in_archive`]). The two are distinct id namespaces, so the SDT ids
/// are invisible to the attribute scan.
///
/// Returned so the annotation-id counter can be seeded above every existing SDT
/// id: when a tracked *replace* of an inline content control re-ids the inserted
/// copy (the deleted copy keeps the source id), the fresh id must not collide
/// with any live SDT id. Only non-negative values are considered — a fresh id is
/// always positive, so a negative source id (a legal signed-32-bit SDT id) can
/// never collide with it and need not raise the seed.
fn max_sdt_id_in_archive(archive: &DocxArchive) -> u32 {
    let mut max_id: u32 = 0;
    let needle = b"<w:id w:val=\"";
    for name in archive.list() {
        if !name.ends_with(".xml") {
            continue;
        }
        let Some(bytes) = archive.get(name) else {
            continue;
        };
        let mut pos = 0;
        while let Some(offset) = bytes[pos..].windows(needle.len()).position(|w| w == needle) {
            let start = pos + offset + needle.len();
            let end = bytes[start..]
                .iter()
                .position(|&b| b == b'"')
                .map(|i| start + i)
                .unwrap_or(start);
            if let Ok(s) = std::str::from_utf8(&bytes[start..end])
                && let Ok(id) = s.parse::<u32>()
            {
                max_id = max_id.max(id);
            }
            pos = end + 1;
        }
    }
    max_id
}

pub fn max_revision_id(doc: &CanonDoc) -> u32 {
    fn revision_id_from_status(status: &TrackingStatus) -> Option<u32> {
        match status {
            TrackingStatus::Normal => None,
            TrackingStatus::Inserted(rev) | TrackingStatus::Deleted(rev) => Some(rev.revision_id),
            // Both pending revisions occupy the id namespace.
            TrackingStatus::InsertedThenDeleted(sr) => {
                Some(sr.inserted.revision_id.max(sr.deleted.revision_id))
            }
        }
    }

    fn max_id_in_paragraph(p: &ParagraphNode, max_id: &mut u32) {
        for seg in &p.segments {
            if let Some(id) = revision_id_from_status(&seg.status) {
                *max_id = (*max_id).max(id);
            }
            // Run-level rPrChange ids occupy the same namespace.
            for inline in &seg.inlines {
                match inline {
                    crate::domain::InlineNode::Text(t) => {
                        if let Some(fc) = &t.formatting_change {
                            *max_id = (*max_id).max(fc.revision_id);
                        }
                    }
                    // Hyperlink runs carry their own ins/del status (the layer
                    // ReplaceHyperlinkText writes to) in the SAME id namespace —
                    // missed here originally, the same latent id-collision
                    // hazard as the cell-structural-status miss below.
                    crate::domain::InlineNode::OpaqueInline(opaque) => {
                        if let crate::domain::OpaqueKind::Hyperlink(data) = &opaque.kind {
                            for run in &data.runs {
                                if let Some(id) = revision_id_from_status(&run.status) {
                                    *max_id = (*max_id).max(id);
                                }
                            }
                        } else if let Some(raw) = &opaque.raw_xml {
                            // Interior tracked-change ids live inside opaque raw_xml
                            // and share the whole-document id namespace (RFC-0002
                            // §Phase-3b). Include them so a freshly minted id is
                            // strictly above every interior id and cannot collide.
                            *max_id = (*max_id).max(crate::tracked_model::max_interior_id(raw));
                        }
                    }
                    _ => {}
                }
            }
        }
        if let Some(ref pms) = p.para_mark_status
            && let Some(id) = revision_id_from_status(pms)
        {
            *max_id = (*max_id).max(id);
        }
        // pPrChange ids occupy the same namespace.
        if let Some(fc) = &p.formatting_change {
            *max_id = (*max_id).max(fc.revision_id);
        }
        // A mid-document section break's own sectPrChange ids occupy the same
        // namespace too — missed here before the story/section enumeration
        // work, the same id-collision hazard as the cell-structural-status
        // miss above: a second edit could allocate an id already used by a
        // pending mid-document sectPrChange.
        if let Some(change) = &p.section_property_change {
            *max_id = (*max_id).max(change.revision.revision_id);
        }
    }

    fn max_id_in_block(block: &BlockNode, max_id: &mut u32) {
        match block {
            BlockNode::Paragraph(p) => max_id_in_paragraph(p, max_id),
            BlockNode::Table(table) => {
                if let Some(fc) = &table.formatting_change {
                    *max_id = (*max_id).max(fc.revision_id);
                }
                for row in &table.rows {
                    if let Some(ref ts) = row.tracking_status
                        && let Some(id) = revision_id_from_status(ts)
                    {
                        *max_id = (*max_id).max(id);
                    }
                    if let Some(fc) = &row.formatting_change {
                        *max_id = (*max_id).max(fc.revision_id);
                    }
                    for cell in &row.cells {
                        // Cell structural status ids were MISSED here before
                        // the enumeration work — a latent id-collision hazard.
                        if let Some(ref ts) = cell.tracking_status
                            && let Some(id) = revision_id_from_status(ts)
                        {
                            *max_id = (*max_id).max(id);
                        }
                        if let Some(fc) = &cell.formatting_change {
                            *max_id = (*max_id).max(fc.revision_id);
                        }
                        for cell_block in &cell.blocks {
                            max_id_in_block(cell_block, max_id);
                        }
                    }
                }
            }
            // A block opaque carries NO bytes on the IR node — its interior
            // (and any interior w:ids) lives in the serialize scaffold. That
            // population is covered by `max_wid_in_opaque_children`, which the
            // runtime feeds into `apply_transaction_with_id_floor`; this walk
            // deliberately cannot see it.
            BlockNode::OpaqueBlock(_) => {}
        }
    }

    fn max_id_in_tracked_blocks(blocks: &[TrackedBlock], max_id: &mut u32) {
        for tb in blocks {
            if let Some(id) = revision_id_from_status(&tb.status) {
                *max_id = (*max_id).max(id);
            }
            max_id_in_block(&tb.block, max_id);
        }
    }

    let mut max_id: u32 = 0;

    // Body blocks
    max_id_in_tracked_blocks(&doc.blocks, &mut max_id);

    // Body-level section-properties change (§17.13.5.32) — a CanonDoc-level
    // field, not a TrackedBlock, so it's outside `max_id_in_tracked_blocks`'s
    // walk. Missed here before the story/section enumeration work: the exact
    // id-collision hazard that surfaced it (a second `apply_edit` transaction
    // reused the id of a still-pending body sectPrChange).
    if let Some(change) = &doc.body_section_property_change {
        max_id = max_id.max(change.revision.revision_id);
    }

    // Header stories
    for header in &doc.headers {
        max_id_in_tracked_blocks(&header.blocks, &mut max_id);
    }

    // Footer stories
    for footer in &doc.footers {
        max_id_in_tracked_blocks(&footer.blocks, &mut max_id);
    }

    // Footnote stories
    for footnote in &doc.footnotes {
        max_id_in_tracked_blocks(&footnote.blocks, &mut max_id);
    }

    // Endnote stories
    for endnote in &doc.endnotes {
        max_id_in_tracked_blocks(&endnote.blocks, &mut max_id);
    }

    // Comment stories — including the whole-comment tracking status
    // (`comment_delete`'s marker), which is outside the per-block walk.
    for comment in &doc.comments {
        if let Some(ref ts) = comment.tracking_status
            && let Some(id) = revision_id_from_status(ts)
        {
            max_id = max_id.max(id);
        }
        max_id_in_tracked_blocks(&comment.blocks, &mut max_id);
    }

    max_id
}

pub(crate) fn next_annotation_id(counter: &mut u32) -> u32 {
    let id = *counter;
    *counter += 1;
    id
}

fn revision_info_from_transaction_meta(meta: &TransactionMeta, revision_id: u32) -> RevisionInfo {
    let date = meta
        .timestamp_utc
        .clone()
        .unwrap_or_else(|| chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string());
    RevisionInfo {
        revision_id,
        author: Some(meta.author.clone()),
        date: Some(date),
        apply_op_id: None,
    }
}

/// Extract image relationship mappings from a typed RelationshipSet.
///
/// Returns a map of rId -> media archive path (e.g. "rId4" -> "word/media/image1.tmp").
fn image_rels_from_package(rels: &crate::docx_package::RelationshipSet) -> HashMap<String, String> {
    const IMAGE_REL_TYPE: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";

    let mut map = HashMap::new();
    for r in &rels.entries {
        if r.rel_type != IMAGE_REL_TYPE {
            continue;
        }
        let archive_path = if let Some(stripped) = r.target.strip_prefix('/') {
            stripped.to_string()
        } else {
            format!("word/{}", r.target)
        };
        map.insert(r.id.clone(), archive_path);
    }
    map
}

/// Copy media files from the target archive for inserted drawing nodes and remap their rIds.
fn copy_target_media_for_inserted_drawings(
    doc: &mut CanonDoc,
    base_pkg: &mut DocxPackage,
    target_archive: &DocxArchive,
    target_document_rels: &crate::docx_package::RelationshipSet,
) -> Result<(), RuntimeError> {
    let target_image_rels = image_rels_from_package(target_document_rels);
    if target_image_rels.is_empty() {
        return Ok(());
    }
    let base_image_rels = image_rels_from_package(&base_pkg.document_rels);

    let mut rid_remap: HashMap<String, String> = HashMap::new();
    let mut copied_media: HashMap<String, String> = HashMap::new();

    // Document order (first-appearance, deduped): the new rIds and copied media
    // part names are allocated sequentially in this loop, so the iteration order
    // is observable in the serialized bytes. Walking in document order makes the
    // rId→media-name assignment deterministic across processes (a HashSet's
    // per-process RandomState would otherwise leak into the wire) — see H1.
    let mut target_rids_needed: Vec<String> = Vec::new();
    collect_inserted_drawing_rids(&doc.blocks, &mut target_rids_needed);

    if target_rids_needed.is_empty() {
        return Ok(());
    }

    for target_rid in &target_rids_needed {
        let Some(target_media_path) = target_image_rels.get(target_rid) else {
            return Err(RuntimeError {
                code: ErrorCode::InvalidDocx,
                message: format!(
                    "inserted drawing references rId '{target_rid}' which has no image relationship in the target document — output would contain an orphaned rId"
                ),
                details: ErrorDetails {
                    context: Some(format!(
                        "copy_target_media_for_inserted_drawings: target_rid={target_rid}"
                    )),
                    ..Default::default()
                },
            });
        };
        let Some(target_media_bytes) = target_archive.get(target_media_path) else {
            return Err(RuntimeError {
                code: ErrorCode::InvalidDocx,
                message: format!(
                    "target media file '{target_media_path}' for rId '{target_rid}' not found in archive — output would contain an orphaned rId"
                ),
                details: ErrorDetails {
                    context: Some(format!(
                        "copy_target_media_for_inserted_drawings: target_rid={target_rid}, target_media_path={target_media_path}"
                    )),
                    ..Default::default()
                },
            });
        };

        if let Some(base_media_path) = base_image_rels.get(target_rid)
            && let Some(base_media_bytes) = base_pkg.get_part(base_media_path)
            && base_media_bytes == target_media_bytes
        {
            continue;
        }

        let next_rid_num = base_pkg.document_rels.max_rid_number() + 1;
        let new_media_path = if let Some(existing) = copied_media.get(target_media_path) {
            existing.clone()
        } else {
            let ext = target_media_path.rsplit('.').next().unwrap_or("bin");
            let new_name = format!("word/media/image_target_{next_rid_num}.{ext}");
            base_pkg.set_part(&new_name, target_media_bytes.to_vec());
            copied_media.insert(target_media_path.clone(), new_name.clone());
            new_name
        };

        let rel_target = new_media_path
            .strip_prefix("word/")
            .unwrap_or(&new_media_path);
        let new_rid = base_pkg.document_rels.add(
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image",
            rel_target,
        );
        rid_remap.insert(target_rid.clone(), new_rid);
    }

    if rid_remap.is_empty() {
        return Ok(());
    }

    rewrite_inserted_drawing_rids(&mut doc.blocks, &rid_remap);
    Ok(())
}

const IMAGE_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";

/// Apply verb-staged OPC parts ([`crate::edit::PendingParts`]) to the live
/// package during serialization.
///
/// This is the save-path twin of the pure verb core: `apply_transaction` stages
/// media binaries and styles.xml ops in a `PendingParts` (it has no
/// `DocxPackage` in scope by design), and this function realizes them against
/// the real package. Generalizes `copy_target_media_for_inserted_drawings`:
/// same shape (write a part, register a rel, rewrite the IR rId), but the parts
/// are supplied by a verb rather than inferred from a second archive.
///
/// Fails loud (no silent fallback, no orphaned part) on:
/// - empty image bytes,
/// - empty content-type or empty extension,
/// - sha256 mismatch (the staged digest does not match the staged bytes),
/// - malformed `word/styles.xml` (cannot parse base or the staged fragment),
/// - `Create` of a styleId that already exists,
/// - `Modify` of a styleId that is absent.
///
/// Order: callers invoke this AFTER `merge_styles_xml_preferring_target`, so an
/// authored `Create`/`Modify` style wins a base/target style-id collision
/// instead of being overwritten by the merge.
fn apply_pending_parts(
    doc: &mut CanonDoc,
    base_pkg: &mut DocxPackage,
    pending: &crate::edit::PendingParts,
) -> Result<(), RuntimeError> {
    apply_pending_media(doc, base_pkg, &pending.media)?;
    apply_pending_style_ops(base_pkg, &pending.style_ops)?;
    apply_pending_numbering_ops(doc, base_pkg, &pending.numbering_ops)?;
    apply_pending_custom_xml(base_pkg, &pending.custom_xml)?;
    Ok(())
}

fn pending_parts_error(message: String, context: String) -> RuntimeError {
    RuntimeError {
        code: ErrorCode::InvalidDocx,
        message,
        details: ErrorDetails {
            context: Some(context),
            ..Default::default()
        },
    }
}

fn apply_pending_media(
    doc: &mut CanonDoc,
    base_pkg: &mut DocxPackage,
    media: &[crate::edit::PendingMedia],
) -> Result<(), RuntimeError> {
    if media.is_empty() {
        return Ok(());
    }

    // Validate every staged media item BEFORE mutating the package, so a bad
    // item cannot leave an orphaned part written by an earlier item in the
    // batch.
    for m in media {
        if m.bytes.is_empty() {
            return Err(pending_parts_error(
                format!(
                    "pending media for logical rId '{}' has empty image bytes — refusing to write an empty media part",
                    m.logical_rid
                ),
                format!("apply_pending_media: logical_rid={}", m.logical_rid),
            ));
        }
        if m.content_type.trim().is_empty() {
            return Err(pending_parts_error(
                format!(
                    "pending media for logical rId '{}' has empty content-type — refusing to register a part with an unknown content type",
                    m.logical_rid
                ),
                format!("apply_pending_media: logical_rid={}", m.logical_rid),
            ));
        }
        if m.ext.trim().is_empty() {
            return Err(pending_parts_error(
                format!(
                    "pending media for logical rId '{}' has empty extension — refusing to name a media part without an extension",
                    m.logical_rid
                ),
                format!("apply_pending_media: logical_rid={}", m.logical_rid),
            ));
        }
        let actual = sha256_hex_bytes(&m.bytes);
        if actual != m.bytes_sha256 {
            return Err(pending_parts_error(
                format!(
                    "pending media for logical rId '{}' has a sha256 mismatch (staged {}, actual {}) — refusing to write a part whose digest does not match its bytes",
                    m.logical_rid, m.bytes_sha256, actual
                ),
                format!("apply_pending_media: logical_rid={}", m.logical_rid),
            ));
        }
    }

    // All items validated — now mutate. Dedup identical binaries by digest so a
    // transaction that stages the same image twice writes one part.
    let mut by_digest: HashMap<String, String> = HashMap::new();
    let mut rid_remap: HashMap<String, String> = HashMap::new();

    for m in media {
        let next_rid_num = base_pkg.document_rels.max_rid_number() + 1;
        let media_path = if let Some(existing) = by_digest.get(&m.bytes_sha256) {
            existing.clone()
        } else {
            let new_name = format!("word/media/image_authored_{next_rid_num}.{}", m.ext);
            base_pkg.set_part(&new_name, m.bytes.clone());
            by_digest.insert(m.bytes_sha256.clone(), new_name.clone());
            new_name
        };

        // Ensure the content type is declared. Prefer a per-extension Default
        // (matches how Word declares image parts); fall back to a per-part
        // Override if a Default for this extension already maps elsewhere.
        if !base_pkg.content_types.has_default(&m.ext) {
            base_pkg
                .content_types
                .defaults
                .push(crate::docx_package::DefaultContentType {
                    extension: m.ext.clone(),
                    content_type: m.content_type.clone(),
                });
        }

        let rel_target = media_path.strip_prefix("word/").unwrap_or(&media_path);
        let new_rid = base_pkg.document_rels.add(IMAGE_REL_TYPE, rel_target);
        rid_remap.insert(m.logical_rid.clone(), new_rid);
    }

    // Rewrite the IR's logical rIds to the real rIds the package assigned.
    //
    // Unlike `copy_target_media_for_inserted_drawings` (which only rewrites
    // INSERTED drawings, because the merge path only copies media for inserted
    // content), a verb-staged media item must rewrite its drawing regardless of
    // segment status: `InsertImage` lands an Inserted segment, but `ReplaceImage`
    // rewrites an existing Normal-segment drawing. A staged `logical_rid` is an
    // explicit contract — wherever the verb wrote it, that is exactly the rId we
    // must rewrite, or the output carries an orphan rId.
    rewrite_staged_drawing_rids(&mut doc.blocks, &rid_remap);
    Ok(())
}

/// Rewrite blip rIds in ALL drawings (any segment status) by `remap`. Used by the
/// verb-staged media path, which must rewrite both inserted (`InsertImage`) and
/// normal (`ReplaceImage`) drawings. Distinct from
/// `rewrite_inserted_drawing_rids`, which is intentionally inserted-only for the
/// merge path.
fn rewrite_staged_drawing_rids(blocks: &mut [TrackedBlock], remap: &HashMap<String, String>) {
    for tb in blocks {
        match &mut tb.block {
            BlockNode::Paragraph(p) => {
                for seg in &mut p.segments {
                    for inline in &mut seg.inlines {
                        if let InlineNode::OpaqueInline(o) = inline
                            && matches!(o.kind, OpaqueKind::Drawing)
                        {
                            rewrite_opaque_drawing_rid(o, remap);
                        }
                    }
                }
            }
            BlockNode::Table(t) => {
                for row in &mut t.rows {
                    for cell in &mut row.cells {
                        rewrite_drawing_rids_in_blocks(&mut cell.blocks, remap);
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }
}

fn apply_pending_style_ops(
    base_pkg: &mut DocxPackage,
    style_ops: &[crate::edit::StyleOp],
) -> Result<(), RuntimeError> {
    use crate::edit::StyleOp;

    if style_ops.is_empty() {
        return Ok(());
    }

    // Parse the existing styles part, or BOOTSTRAP a minimal one when the package
    // has no styles.xml at all. This mirrors the settings.xml synthesis precedent
    // (`apply_even_and_odd_headers_to_settings`): a verb that authors a style into
    // a minimal real document must not fail just because that document never
    // carried a styles part. When we synthesize, we also register the part's
    // content-type Override and the document `styles` relationship so Word
    // recognizes it (idempotent: both `add_override` and `add` dedupe). No silent
    // fallback — a malformed existing part still errors.
    let mut styles_root = match base_pkg.get_part("word/styles.xml") {
        Some(styles_bytes) => Element::parse(Cursor::new(styles_bytes.to_vec())).map_err(|e| {
            pending_parts_error(
                format!("failed to parse word/styles.xml before applying staged style ops: {e}"),
                "apply_pending_style_ops: parse base styles.xml".to_string(),
            )
        })?,
        None => {
            let mut root = Element::new("w:styles");
            let mut ns = xmltree::Namespace::empty();
            ns.put(
                "w",
                "http://schemas.openxmlformats.org/wordprocessingml/2006/main",
            );
            root.namespaces = Some(ns);

            base_pkg.content_types.add_override(
                "/word/styles.xml",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml",
            );
            base_pkg.document_rels.add(
                "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles",
                "styles.xml",
            );

            root
        }
    };

    // ── Merge-preserve for StyleOp::Modify ─────────────────────────────────
    // The authored fragment covers only the StyleDefinition subset (name /
    // basedOn / pPr{spacing,ind,jc} / rPr{marks,sz,color,rFonts}). Replacing
    // the existing <w:style> wholesale silently dropped every field outside
    // that subset (w:tabs, w:next, w:qFormat, w:outlineLvl, …) — the
    // known_bug_modify_style_field_drop class. Modify therefore MERGES:
    // fragment children replace same-named children of the existing style in
    // place (recursing one level into pPr/rPr); everything the fragment does
    // not author survives. Omitting a field means "leave it alone" — removal
    // is not expressible via ModifyStyle (documented contract).

    /// Annex-A child order for CT_Style (ECMA-376 §17.7.4.17).
    fn ct_style_rank(name: &str) -> Option<usize> {
        const ORDER: &[&str] = &[
            "name",
            "aliases",
            "basedOn",
            "next",
            "link",
            "autoRedefine",
            "hidden",
            "uiPriority",
            "semiHidden",
            "unhideWhenUsed",
            "qFormat",
            "locked",
            "personal",
            "personalCompose",
            "personalReply",
            "rsid",
            "pPr",
            "rPr",
            "tblPr",
            "trPr",
            "tcPr",
            "tblStylePr",
        ];
        ORDER.iter().position(|n| *n == name)
    }

    /// Annex-A child order for CT_PPrBase (ECMA-376 §17.3.1.26).
    fn ct_ppr_rank(name: &str) -> Option<usize> {
        const ORDER: &[&str] = &[
            "pStyle",
            "keepNext",
            "keepLines",
            "pageBreakBefore",
            "framePr",
            "widowControl",
            "numPr",
            "suppressLineNumbers",
            "pBdr",
            "shd",
            "tabs",
            "suppressAutoHyphens",
            "kinsoku",
            "wordWrap",
            "overflowPunct",
            "topLinePunct",
            "autoSpaceDE",
            "autoSpaceDN",
            "bidi",
            "adjustRightInd",
            "snapToGrid",
            "spacing",
            "ind",
            "contextualSpacing",
            "mirrorIndents",
            "suppressOverlap",
            "jc",
            "textDirection",
            "textAlignment",
            "textboxTightWrap",
            "outlineLvl",
            "divId",
            "cnfStyle",
        ];
        ORDER.iter().position(|n| *n == name)
    }

    /// Annex-A child order for CT_RPr (ECMA-376 §17.3.2.28).
    fn ct_rpr_rank(name: &str) -> Option<usize> {
        const ORDER: &[&str] = &[
            "rStyle",
            "rFonts",
            "b",
            "bCs",
            "i",
            "iCs",
            "caps",
            "smallCaps",
            "strike",
            "dstrike",
            "outline",
            "shadow",
            "emboss",
            "imprint",
            "noProof",
            "snapToGrid",
            "vanish",
            "webHidden",
            "color",
            "spacing",
            "w",
            "kern",
            "position",
            "sz",
            "szCs",
            "highlight",
            "u",
            "effect",
            "bdr",
            "shd",
            "fitText",
            "vertAlign",
            "rtl",
            "cs",
            "em",
            "lang",
            "eastAsianLayout",
            "specVanish",
            "oMath",
        ];
        ORDER.iter().position(|n| *n == name)
    }

    /// Replace all children of `target` named like `new_child` with `new_child`
    /// (at the first occurrence's position), or insert it schema-ordered when
    /// absent: before the first existing child of KNOWN higher rank (unknown /
    /// extension children are transparent to the scan), else appended.
    fn replace_or_insert_child(
        target: &mut Element,
        new_child: Element,
        rank: fn(&str) -> Option<usize>,
    ) {
        let name = new_child.name.clone();
        let first = target
            .children
            .iter()
            .position(|c| matches!(c, XMLNode::Element(el) if local_w_name(el) == name));
        match first {
            Some(idx) => {
                target
                    .children
                    .retain(|c| !matches!(c, XMLNode::Element(el) if local_w_name(el) == name));
                target.children.insert(idx, XMLNode::Element(new_child));
            }
            None => {
                let Some(new_rank) = rank(&name) else {
                    target.children.push(XMLNode::Element(new_child));
                    return;
                };
                let insert_at = target
                    .children
                    .iter()
                    .position(|c| {
                        matches!(c, XMLNode::Element(el)
                            if rank(local_w_name(el)).is_some_and(|r| r > new_rank))
                    })
                    .unwrap_or(target.children.len());
                target
                    .children
                    .insert(insert_at, XMLNode::Element(new_child));
            }
        }
    }

    fn merge_style_fragment(existing: &Element, fragment: &Element) -> Element {
        let mut merged = existing.clone();
        // Fragment attributes (w:type, w:styleId) override; existing extras
        // (w:default, w:customStyle) survive.
        for (attr, value) in fragment.attributes.iter() {
            let key = xmltree::AttributeName {
                local_name: attr.local_name.clone(),
                namespace: attr.namespace.clone(),
                prefix: attr.prefix.clone(),
            };
            merged.attributes.insert(key, value.clone());
        }
        for child in &fragment.children {
            let XMLNode::Element(frag_child) = child else {
                continue;
            };
            let child_name = local_w_name(frag_child).to_string();
            if child_name == "pPr" || child_name == "rPr" {
                let rank = if child_name == "pPr" {
                    ct_ppr_rank
                } else {
                    ct_rpr_rank
                };
                let existing_props = merged.children.iter_mut().find_map(|c| match c {
                    XMLNode::Element(el) if local_w_name(el) == child_name => Some(el),
                    _ => None,
                });
                match existing_props {
                    Some(props) => {
                        // Second-level merge: fragment grandchildren replace
                        // same-named grandchildren; existing extras survive.
                        for grandchild in &frag_child.children {
                            if let XMLNode::Element(g) = grandchild {
                                replace_or_insert_child(props, g.clone(), rank);
                            }
                        }
                    }
                    None => {
                        replace_or_insert_child(&mut merged, frag_child.clone(), ct_style_rank);
                    }
                }
            } else {
                replace_or_insert_child(&mut merged, frag_child.clone(), ct_style_rank);
            }
        }
        merged
    }

    for op in style_ops {
        let (style_id, style_xml, is_create) = match op {
            StyleOp::Create {
                style_id,
                style_xml,
            } => (style_id, style_xml, true),
            StyleOp::Modify {
                style_id,
                style_xml,
            } => (style_id, style_xml, false),
            // SetDocDefaults has NO styleId — it targets the docDefaults block,
            // not a w:style. Handle it here (before the styleId-based
            // Create/Modify match) and move on to the next op.
            StyleOp::SetDocDefaults {
                font_family,
                font_size_half_points,
            } => {
                apply_set_doc_defaults_to_styles_root(
                    &mut styles_root,
                    font_family.as_deref(),
                    *font_size_half_points,
                );
                continue;
            }
        };

        let fragment = Element::parse(Cursor::new(style_xml.clone())).map_err(|e| {
            pending_parts_error(
                format!("staged style fragment for styleId '{style_id}' is malformed XML: {e}"),
                format!("apply_pending_style_ops: parse fragment styleId={style_id}"),
            )
        })?;

        // The fragment's own w:styleId must match the op's declared styleId —
        // a mismatch is a programmer bug that would splice the wrong style.
        let fragment_style_id = attr_get(&fragment, "w:styleId").cloned();
        if fragment_style_id.as_deref() != Some(style_id.as_str()) {
            return Err(pending_parts_error(
                format!(
                    "staged style fragment declares w:styleId {:?} but the op targets '{style_id}' — refusing to splice a mismatched style",
                    fragment_style_id
                ),
                format!("apply_pending_style_ops: styleId mismatch op={style_id}"),
            ));
        }

        let existing_idx = styles_root.children.iter().position(|child| {
            let XMLNode::Element(el) = child else {
                return false;
            };
            if local_w_name(el) != "style" {
                return false;
            }
            attr_get(el, "w:styleId").map(String::as_str) == Some(style_id.as_str())
        });

        match (is_create, existing_idx) {
            (true, Some(_)) => {
                return Err(pending_parts_error(
                    format!(
                        "StyleOp::Create for styleId '{style_id}' but a style with that id already exists — refusing to silently overwrite (use Modify)"
                    ),
                    format!("apply_pending_style_ops: create-existing styleId={style_id}"),
                ));
            }
            (true, None) => {
                styles_root.children.push(XMLNode::Element(fragment));
            }
            (false, Some(idx)) => {
                let XMLNode::Element(existing) = &styles_root.children[idx] else {
                    unreachable!("existing_idx matched an Element above");
                };
                let merged = merge_style_fragment(existing, &fragment);
                styles_root.children[idx] = XMLNode::Element(merged);
            }
            (false, None) => {
                return Err(pending_parts_error(
                    format!(
                        "StyleOp::Modify for styleId '{style_id}' but no style with that id exists — refusing to silently create (use Create)"
                    ),
                    format!("apply_pending_style_ops: modify-missing styleId={style_id}"),
                ));
            }
        }
    }

    let mut out = Vec::new();
    styles_root.write(&mut out).map_err(|e| {
        pending_parts_error(
            format!("failed to reserialize word/styles.xml after applying staged style ops: {e}"),
            "apply_pending_style_ops: reserialize styles.xml".to_string(),
        )
    })?;
    base_pkg.set_part("word/styles.xml", out);
    Ok(())
}

/// Property-merge the document DEFAULT run properties into the `w:styles` root
/// for a [`crate::edit::StyleOp::SetDocDefaults`] op.
///
/// Find-or-creates the `w:docDefaults/w:rPrDefault/w:rPr` chain (per CT_Styles,
/// `w:docDefaults` is the FIRST child of `w:styles` when created), then merges
/// ONLY the named children:
/// - `font_family` → `w:rFonts` @ascii/@hAnsi/@cs set to the literal typeface,
///   with any `*Theme` attributes CLEARED so the literal wins resolution.
/// - `font_size` → `w:sz` and `w:szCs` @val.
///
/// Every other `rPrDefault/rPr` child (`w:lang`, etc.) is preserved untouched —
/// this is a per-property merge, not a replace.
fn apply_set_doc_defaults_to_styles_root(
    styles_root: &mut Element,
    font_family: Option<&str>,
    font_size_half_points: Option<u32>,
) {
    let doc_defaults = find_or_create_first_w_child(styles_root, "docDefaults", true);
    let rpr_default = find_or_create_first_w_child(doc_defaults, "rPrDefault", false);
    let rpr = find_or_create_first_w_child(rpr_default, "rPr", false);

    if let Some(family) = font_family {
        let rfonts = find_or_create_first_w_child(rpr, "rFonts", false);
        // Clear any theme-font references so the literal typeface wins (a present
        // w:asciiTheme would otherwise override our w:ascii at resolution). Match
        // case-insensitively: ECMA-376 spells the complex-script theme attribute
        // `w:cstheme` (lowercase) while the others are camelCase, and real
        // documents carry both spellings.
        rfonts.attributes.retain(|name, _| {
            let lower = name.local_name.to_ascii_lowercase();
            !matches!(
                lower.as_str(),
                "asciitheme" | "hansitheme" | "cstheme" | "eastasiatheme"
            )
        });
        attr_set(rfonts, "w:ascii", family);
        attr_set(rfonts, "w:hAnsi", family);
        attr_set(rfonts, "w:cs", family);
    }

    if let Some(half_points) = font_size_half_points {
        let value = half_points.to_string();
        let sz = find_or_create_first_w_child(rpr, "sz", false);
        attr_set(sz, "w:val", &value);
        let sz_cs = find_or_create_first_w_child(rpr, "szCs", false);
        attr_set(sz_cs, "w:val", &value);
    }
}

/// Return a mutable reference to `parent`'s first `w:`-namespaced child with the
/// given local name, creating it (as `w:<local>`) if absent. When `prepend` is
/// true a newly-created child is inserted at index 0 (used for `w:docDefaults`,
/// which CT_Styles requires to precede the `w:style` definitions); otherwise it
/// is appended.
fn find_or_create_first_w_child<'a>(
    parent: &'a mut Element,
    local: &str,
    prepend: bool,
) -> &'a mut Element {
    let existing = parent
        .children
        .iter()
        .position(|child| matches!(child, XMLNode::Element(el) if local_w_name(el) == local));
    let idx = match existing {
        Some(idx) => idx,
        None => {
            let el = Element::new(&format!("w:{local}"));
            if prepend {
                parent.children.insert(0, XMLNode::Element(el));
                0
            } else {
                parent.children.push(XMLNode::Element(el));
                parent.children.len() - 1
            }
        }
    };
    match &mut parent.children[idx] {
        XMLNode::Element(el) => el,
        _ => unreachable!("index {idx} was just matched/created as an Element"),
    }
}

/// Apply verb-staged `word/numbering.xml` create-definition ops
/// ([`crate::edit::NumberingOp`]) to the live package at save time.
///
/// This is the save-path half of the list-split verb: the pure verb core
/// (`verbs::numbering::apply_split`) re-pointed the split tail's IR at a sentinel
/// PLACEHOLDER `num_id` but could NOT allocate the real one or author the
/// definition, because `word/numbering.xml` is not on the `CanonDoc` value — and
/// the part routinely defines `numId`s no body paragraph references (orphan
/// definitions, style-linked / story-only lists), so any body-scan guess
/// collides on a large fraction of real documents. Here, with the part in scope,
/// we:
///
/// 1. Parse the base numbering part (BOOTSTRAP a minimal one if absent — see
///    below).
/// 2. Allocate the REAL `num_id` as `max(existing <w:num> numId) + 1`. The part
///    is the authoritative registry of every list — body-referenced,
///    story-referenced (header/footer/footnote), and orphan alike — so this is
///    fresh against ALL of them by construction. This is the allocator the verb
///    could not run.
/// 3. Resolve `cloned_from_num_id` → its `<w:abstractNum>` (following the
///    `w:num` → `w:abstractNumId` link). **Fail loud** if the source num or its
///    abstractNum is missing — a split of a list whose definition we can't find
///    is invalid; we never fabricate empty levels.
/// 4. **Clone** that `<w:abstractNum>` under a fresh `abstractNumId =
///    max(existing) + 1` (every `<w:lvl>` format preserved verbatim — opaque
///    preservation), and append a fresh `<w:num numId=real_num_id>` pointing at
///    the clone.
/// 5. Rewrite the placeholder → the real id in the re-pointed paragraphs' live
///    `w:numPr` on `doc` (mirroring `apply_pending_media`'s `logical_rid`
///    rewrite). The `pPrChange` snapshots carry the SOURCE id, not the
///    placeholder, so only the live `numbering` needs rewriting.
/// 6. Re-sort children so all `<w:abstractNum>` precede all `<w:num>`
///    (ECMA-376 Annex A, CT_Numbering).
///
/// The old body-scan collision check is retained as an INVARIANT assert (step
/// 2's `max + 1` is fresh by construction): a staged placeholder that somehow
/// matches a real `<w:num>` means the reserved sentinel range overlapped a real
/// definition — a programmer bug we refuse loudly rather than let capture a real
/// paragraph.
///
/// ## Bootstrap when absent
///
/// Mirrors `apply_pending_style_ops`: if the package has no numbering part we
/// synthesize a minimal `<w:numbering>` root and register the content-type
/// Override + the document `numbering` relationship. In practice a split always
/// targets a list item, which can only exist if numbering.xml was present at
/// import — so step 2 fails loud anyway when we bootstrap. The bootstrap branch
/// exists for parity/robustness; it never silently produces a list with no
/// levels.
///
/// Sort key ordering a `w:numbering` root's children into the ECMA-376 §17.9
/// CT_Numbering xsd:sequence: `numPicBullet*, abstractNum*, num*,
/// numIdMacAtCleanup?`. `numPicBullet` must sort AHEAD of `abstractNum` — a
/// picture bullet that lands after `num` makes Word repair the file and can drop
/// the bullet. `numIdMacAtCleanup` and any other unknown element fall into the
/// catch-all Element bucket, preserving their trailing position; non-element
/// nodes (comments, whitespace) sort first. Every code path that rebuilds
/// numbering.xml partitions through this one key, so the schema sequence lives
/// in a single place. Pair it with the **stable** `sort_by_key` so document
/// order within each group is preserved.
fn ct_numbering_child_order(node: &XMLNode) -> u8 {
    use crate::word_xml::is_w_tag;
    match node {
        XMLNode::Element(el) if is_w_tag(el, "numPicBullet") => 1,
        XMLNode::Element(el) if is_w_tag(el, "abstractNum") => 2,
        XMLNode::Element(el) if is_w_tag(el, "num") => 3,
        XMLNode::Element(_) => 4,
        _ => 0,
    }
}

fn apply_pending_numbering_ops(
    doc: &mut CanonDoc,
    base_pkg: &mut DocxPackage,
    numbering_ops: &[crate::edit::NumberingOp],
) -> Result<(), RuntimeError> {
    use crate::edit::NumberingOp;

    if numbering_ops.is_empty() {
        return Ok(());
    }

    // placeholder num_id (staged by the verb) -> real num_id (allocated here).
    // Applied to the body IR after every op is materialized, so the split tail's
    // live `w:numPr` points at the real definition we just authored.
    let mut placeholder_to_real: HashMap<u32, u32> = HashMap::new();

    const NUMBERING_CONTENT_TYPE: &str =
        "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml";
    const NUMBERING_REL_TYPE: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering";

    let mut root = match base_pkg.get_part("word/numbering.xml") {
        Some(bytes) => Element::parse(Cursor::new(bytes.to_vec())).map_err(|e| {
            pending_parts_error(
                format!(
                    "failed to parse word/numbering.xml before applying staged numbering ops: {e}"
                ),
                "apply_pending_numbering_ops: parse base numbering.xml".to_string(),
            )
        })?,
        None => {
            let mut r = Element::new("w:numbering");
            let mut ns = xmltree::Namespace::empty();
            ns.put(
                "w",
                "http://schemas.openxmlformats.org/wordprocessingml/2006/main",
            );
            r.namespaces = Some(ns);

            base_pkg
                .content_types
                .add_override("/word/numbering.xml", NUMBERING_CONTENT_TYPE);
            base_pkg
                .document_rels
                .add(NUMBERING_REL_TYPE, "numbering.xml");
            r
        }
    };

    for op in numbering_ops {
        let NumberingOp::CreateDefinition {
            placeholder_num_id,
            cloned_from_num_id,
        } = op;

        // Index the current part: numId set (+ its max, the allocator), the
        // abstractNumId set, and the numId -> abstractNumId link. Re-derived per
        // op so a previous op's insert is visible to the next.
        let mut existing_num_ids: HashSet<u32> = HashSet::new();
        let mut max_num_id: u32 = 0;
        let mut max_abstract_id: u32 = 0;
        let mut num_to_abstract: HashMap<u32, u32> = HashMap::new();
        let mut abstract_by_id: HashMap<u32, usize> = HashMap::new();
        for (i, child) in root.children.iter().enumerate() {
            let XMLNode::Element(el) = child else {
                continue;
            };
            if is_w_tag(el, "num") {
                if let Some(id) = attr_get(el, "numId").and_then(|s| s.parse::<u32>().ok()) {
                    existing_num_ids.insert(id);
                    max_num_id = max_num_id.max(id);
                    let abs = el.children.iter().find_map(|c| {
                        if let XMLNode::Element(inner) = c
                            && is_w_tag(inner, "abstractNumId")
                        {
                            attr_get(inner, "val").and_then(|v| v.parse::<u32>().ok())
                        } else {
                            None
                        }
                    });
                    if let Some(abs_id) = abs {
                        num_to_abstract.insert(id, abs_id);
                    }
                }
            } else if is_w_tag(el, "abstractNum")
                && let Some(id) = attr_get(el, "abstractNumId").and_then(|s| s.parse::<u32>().ok())
            {
                max_abstract_id = max_abstract_id.max(id);
                abstract_by_id.insert(id, i);
            }
        }

        // Invariant (repurposed collision check): the staged id is a sentinel
        // PLACEHOLDER, never a real numId. If it matches a real `<w:num>` the
        // reserved range overlapped a real definition — refuse loudly rather than
        // let the rewrite below capture that definition's paragraphs. With the
        // sentinel scheme this is a genuine should-never-happen, not the routine
        // outcome the old body-scan allocator produced.
        if existing_num_ids.contains(placeholder_num_id) {
            return Err(pending_parts_error(
                format!(
                    "list-split create-definition: staged placeholder numId {placeholder_num_id} collides with a real <w:num> in word/numbering.xml — the split placeholder range overlapped a real definition"
                ),
                format!(
                    "apply_pending_numbering_ops: placeholder collision placeholder_num_id={placeholder_num_id}"
                ),
            ));
        }

        // Allocate the REAL num_id against the part's authoritative population.
        // `max + 1` is fresh against every list the part defines — body-,
        // story-referenced, and orphan alike — which is exactly what the pure
        // verb core (body only) could not compute.
        let real_num_id = max_num_id
            .checked_add(1)
            .expect("numId space exhausted (u32 overflow) — not reachable for real documents");
        debug_assert!(
            !existing_num_ids.contains(&real_num_id),
            "freshly allocated numId {real_num_id} must not already exist"
        );

        // Resolve the source num -> its abstractNum element. Fail loud if
        // either is missing.
        let source_abstract_id = num_to_abstract.get(cloned_from_num_id).copied().ok_or_else(|| {
            pending_parts_error(
                format!(
                    "list-split create-definition: source numId {cloned_from_num_id} has no resolvable <w:num>/<w:abstractNumId> in word/numbering.xml — refusing to author a list with no levels"
                ),
                format!("apply_pending_numbering_ops: source num missing cloned_from={cloned_from_num_id}"),
            )
        })?;
        let source_abstract_idx = *abstract_by_id.get(&source_abstract_id).ok_or_else(|| {
            pending_parts_error(
                format!(
                    "list-split create-definition: source numId {cloned_from_num_id} points at abstractNumId {source_abstract_id}, which is not defined in word/numbering.xml"
                ),
                format!("apply_pending_numbering_ops: source abstractNum missing abs_id={source_abstract_id}"),
            )
        })?;

        // Clone the source abstractNum under a fresh abstractNumId. The clone
        // keeps every <w:lvl> child verbatim, so the new list renders identically.
        let new_abstract_id = max_abstract_id + 1;
        let XMLNode::Element(source_abstract) = &root.children[source_abstract_idx] else {
            unreachable!("abstract_by_id only indexes Element children");
        };
        let mut cloned_abstract = source_abstract.clone();
        attr_set(
            &mut cloned_abstract,
            "w:abstractNumId",
            new_abstract_id.to_string(),
        );
        // A cloned abstractNum must not keep the source's nsid/styleLink/
        // numStyleLink identity — those bind it to the original list. Drop the
        // ones that would alias the two definitions; keep the level formats.
        cloned_abstract.children.retain(|c| {
            !matches!(c, XMLNode::Element(el) if is_w_tag(el, "nsid")
                || is_w_tag(el, "styleLink")
                || is_w_tag(el, "numStyleLink"))
        });

        // Build the fresh <w:num numId=real_num_id><w:abstractNumId val=new_abstract_id/></w:num>.
        let mut num_el = w_el("num");
        attr_set(&mut num_el, "w:numId", real_num_id.to_string());
        let mut abs_ref = w_el("abstractNumId");
        attr_set(&mut abs_ref, "w:val", new_abstract_id.to_string());
        num_el.children.push(XMLNode::Element(abs_ref));

        root.children.push(XMLNode::Element(cloned_abstract));
        root.children.push(XMLNode::Element(num_el));

        placeholder_to_real.insert(*placeholder_num_id, real_num_id);
    }

    // Re-partition into the CT_Numbering xsd:sequence (see
    // ct_numbering_child_order): numPicBullet*, abstractNum*, num*.
    root.children.sort_by_key(ct_numbering_child_order);

    let xml_bytes = word_xml::write_document_xml(&root).map_err(map_word_xml_error)?;
    base_pkg.set_part("word/numbering.xml", xml_bytes);

    // Rewrite the split tail's live `w:numPr` from placeholder → the real id we
    // just authored. `apply_split` re-points BODY paragraphs only, so the
    // placeholder lives nowhere else (the `pPrChange` snapshots carry the SOURCE
    // id, which is not a remap key and is left untouched). Mirrors
    // `apply_pending_media`'s `logical_rid` rewrite of the body IR.
    remap_numids_in_blocks(&mut doc.blocks, &placeholder_to_real);
    Ok(())
}

/// Author (or reuse) custom-XML datastore parts for content-control data
/// bindings ([`crate::edit::CustomXmlPart`]).
///
/// For each staged binding, the control's `w:dataBinding` already carries a
/// `storeItemID`; Word resolves that id to a custom-XML part via the document's
/// `customXml` relationships. This function realizes the part triad Word needs:
/// - `customXml/itemN.xml` — a minimal well-formed data store document (the
///   binding's XPath addresses a node inside it);
/// - `customXml/itemPropsN.xml` — a `ds:datastoreItem` whose `ds:itemID` IS the
///   `storeItemID` (this is what Word matches the binding against);
/// - `customXml/_rels/itemN.xml.rels` — links the item to its itemProps;
/// - a `customXml` relationship on document.xml pointing at `../customXml/itemN.xml`;
/// - content-type Overrides for both parts.
///
/// This is a direct generalization of the styles.xml / numbering.xml
/// part-bootstrap: a verb stages the intent (a `storeItemID` + a root element),
/// and the save path writes the parts + content-types + relationship.
///
/// Reuse / dedup: if a custom-XML part with the same `ds:itemID` already exists
/// (either pre-existing in the package or authored earlier in this batch), the
/// staged entry is a no-op — multiple bindings can share one datastore.
///
/// Fails loud (no silent fallback): an empty `store_item_id` is unresolvable
/// and is refused rather than written.
fn apply_pending_custom_xml(
    base_pkg: &mut DocxPackage,
    custom_xml: &[crate::edit::CustomXmlPart],
) -> Result<(), RuntimeError> {
    if custom_xml.is_empty() {
        return Ok(());
    }

    const CUSTOM_XML_REL_TYPE: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/customXml";
    const CUSTOM_XML_PROPS_REL_TYPE: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/customXmlProps";
    const CUSTOM_XML_PROPS_CONTENT_TYPE: &str =
        "application/vnd.openxmlformats-officedocument.customXmlProperties+xml";
    const DATASTORE_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/customXml";

    // Index the storeItemIDs already present in the package so a binding that
    // reuses an existing datastore (or a duplicate in this batch) is a no-op.
    let mut existing_item_ids: HashSet<String> = existing_custom_xml_item_ids(base_pkg);

    // The highest existing customXml/itemN.xml index, so we never overwrite a
    // pre-existing data part.
    let mut max_item_index: u32 = base_pkg
        .part_names()
        .filter_map(custom_xml_item_index)
        .max()
        .unwrap_or(0);

    for part in custom_xml {
        if part.store_item_id.trim().is_empty() {
            return Err(pending_parts_error(
                "custom-xml data binding has an empty storeItemID — refusing to author an unresolvable datastore part".to_string(),
                "apply_pending_custom_xml: empty store_item_id".to_string(),
            ));
        }

        // Reuse: a datastore with this itemID already exists. Nothing to author;
        // the binding's storeItemID already resolves.
        if existing_item_ids.contains(&part.store_item_id) {
            continue;
        }

        max_item_index += 1;
        let n = max_item_index;
        let item_path = format!("customXml/item{n}.xml");
        let item_props_path = format!("customXml/itemProps{n}.xml");
        let item_rels_path = format!("customXml/_rels/item{n}.xml.rels");

        // 1. The data store document — a minimal well-formed root the binding's
        //    XPath addresses. Word writes the bound value into it on edit.
        let root_local = sanitize_xml_name(&part.root_element);
        let data_xml = match &part.namespace {
            Some(ns) => format!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<{root_local} xmlns="{}"/>"#,
                xml_escape_attr(ns)
            ),
            None => format!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<{root_local}/>"#
            ),
        };
        base_pkg.set_part(&item_path, data_xml.into_bytes());

        // 2. The itemProps — its ds:itemID is what Word matches the binding's
        //    storeItemID against.
        let props_xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<ds:datastoreItem ds:itemID="{}" xmlns:ds="{DATASTORE_NS}"><ds:schemaRefs/></ds:datastoreItem>"#,
            xml_escape_attr(&part.store_item_id)
        );
        base_pkg.set_part(&item_props_path, props_xml.into_bytes());

        // 3. The item's own relationship part links item -> itemProps.
        let mut item_rels = crate::docx_package::RelationshipSet::empty();
        item_rels.add(CUSTOM_XML_PROPS_REL_TYPE, &format!("itemProps{n}.xml"));
        base_pkg.set_part(
            &item_rels_path,
            item_rels.serialize(&item_rels_path).map_err(|e| {
                pending_parts_error(
                    format!("failed to serialize {item_rels_path}: {e}"),
                    "apply_pending_custom_xml: serialize item rels".to_string(),
                )
            })?,
        );

        // 4. Content-type Overrides. The item.xml itself is plain XML (covered by
        //    the `xml` Default if present; add an explicit Override otherwise so a
        //    package without an xml Default still declares it). itemProps needs its
        //    own Override.
        if !base_pkg.content_types.has_default("xml")
            && !base_pkg
                .content_types
                .has_override(&format!("/{item_path}"))
        {
            base_pkg
                .content_types
                .add_override(&format!("/{item_path}"), "application/xml");
        }
        base_pkg.content_types.add_override(
            &format!("/{item_props_path}"),
            CUSTOM_XML_PROPS_CONTENT_TYPE,
        );

        // 5. The document -> customXml relationship.
        base_pkg
            .document_rels
            .add(CUSTOM_XML_REL_TYPE, &format!("../customXml/item{n}.xml"));

        // This itemID is now present for subsequent staged bindings in the batch.
        existing_item_ids.insert(part.store_item_id.clone());
    }

    Ok(())
}

/// Strip `prefix` from the start of `s`, comparing ASCII case-insensitively.
/// OPC part names are equivalent up to ASCII case (ECMA-376 Part 2 §9.1), so a
/// `customXML/…` (uppercase directory) part is the same slot as `customXml/…`.
fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

/// Strip `suffix` from the end of `s`, comparing ASCII case-insensitively
/// (ECMA-376 Part 2 §9.1).
fn strip_suffix_ci<'a>(s: &'a str, suffix: &str) -> Option<&'a str> {
    let start = s.len().checked_sub(suffix.len())?;
    let tail = s.get(start..)?;
    tail.eq_ignore_ascii_case(suffix).then(|| &s[..start])
}

/// The numeric index `N` of a `customXml/itemN.xml` data part path (NOT the
/// itemProps part). Returns `None` for any other path. The prefix and extension
/// are matched case-insensitively so a wild `customXML/item1.xml` occupies index
/// 1 for the allocator (ECMA-376 Part 2 §9.1); otherwise the next-index scan
/// would miss it and reuse index 1, colliding with (and clobbering) that part.
fn custom_xml_item_index(path: &str) -> Option<u32> {
    let rest = strip_prefix_ci(path, "customXml/item")?;
    let num = strip_suffix_ci(rest, ".xml")?;
    // Exclude `itemPropsN.xml` (which starts with "Props…" after the strip).
    num.parse::<u32>().ok()
}

/// Scan the package's existing `customXml/itemProps*.xml` parts and collect the
/// `ds:itemID` of each, so a staged binding that reuses an existing datastore is
/// recognized and not re-authored.
fn existing_custom_xml_item_ids(base_pkg: &DocxPackage) -> HashSet<String> {
    let mut ids = HashSet::new();
    let props_paths: Vec<String> = base_pkg
        .part_names()
        // OPC part names are ASCII case-insensitive (ECMA-376 Part 2 §9.1), so an
        // existing datastore under an uppercase `customXML/` directory is still
        // recognized and deduped rather than silently re-authored.
        .filter(|p| {
            strip_prefix_ci(p, "customXml/itemProps").is_some()
                && strip_suffix_ci(p, ".xml").is_some()
        })
        .map(|p| p.to_string())
        .collect();
    for path in props_paths {
        if let Some(bytes) = base_pkg.get_part(&path)
            && let Ok(root) = Element::parse(Cursor::new(bytes.to_vec()))
        {
            // ds:itemID attribute on the <ds:datastoreItem> root.
            if let Some(id) = attr_get(&root, "itemID") {
                ids.insert(id.clone());
            }
        }
    }
    ids
}

/// Replace any character not valid in an XML element name with `_`, and ensure
/// the name starts with a letter or underscore. Used to author the datastore
/// root element from a verb-supplied local name (already sanitized at the verb
/// edge; this is a defensive last line so the save path never emits malformed
/// XML).
fn sanitize_xml_name(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty()
        || !out
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
    {
        out.insert(0, '_');
    }
    out
}

/// Escape the five XML predefined entities for use in attribute content.
fn xml_escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

/// Local name of a `w:`-namespaced element, stripping any namespace prefix.
fn local_w_name(el: &Element) -> &str {
    el.name.rsplit(':').next().unwrap_or(&el.name)
}

/// Lowercase hex SHA-256 of a byte slice (used to verify staged media digests).
fn sha256_hex_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut s = String::with_capacity(digest.len() * 2);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Collect the rIds of inserted drawings in first-appearance document order,
/// deduped. Order matters: the caller allocates new rIds/media part names in
/// iteration order, so it must not depend on hash iteration (see H1).
fn collect_inserted_drawing_rids(blocks: &[TrackedBlock], out: &mut Vec<String>) {
    for tb in blocks {
        match &tb.block {
            BlockNode::Paragraph(p) => {
                for seg in &p.segments {
                    if !matches!(seg.status, TrackingStatus::Inserted(_)) {
                        continue;
                    }
                    for inline in &seg.inlines {
                        if let InlineNode::OpaqueInline(o) = inline
                            && matches!(o.kind, OpaqueKind::Drawing)
                            && let Some(ref raw) = o.raw_xml
                            && let Ok(s) = std::str::from_utf8(raw)
                            && let Some(rid) = crate::diff::find_blip_rid(s)
                            // Skip verb-staged image rIds (reserved "rIdimg" prefix,
                            // see image_insert.rs::logical_rid): their media is
                            // registered by apply_pending_media, not copied from the
                            // target package, so they're absent from target_image_rels
                            // and must not be looked up there.
                            && !rid.starts_with("rIdimg")
                        {
                            push_unique_rid(out, rid);
                        }
                    }
                }
            }
            BlockNode::Table(t) => {
                for row in &t.rows {
                    for cell in &row.cells {
                        if matches!(tb.status, TrackingStatus::Inserted(_)) {
                            collect_drawing_rids_from_blocks(&cell.blocks, out);
                        }
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }
}

/// Append `rid` to `out` if not already present (ordered-set semantics; the
/// count of distinct inserted-drawing rIds is small, so the linear scan is
/// cheaper than a parallel index set and keeps first-appearance order).
fn push_unique_rid(out: &mut Vec<String>, rid: String) {
    if !out.contains(&rid) {
        out.push(rid);
    }
}

fn collect_drawing_rids_from_blocks(blocks: &[BlockNode], out: &mut Vec<String>) {
    for block in blocks {
        match block {
            BlockNode::Paragraph(p) => {
                for seg in &p.segments {
                    for inline in &seg.inlines {
                        if let InlineNode::OpaqueInline(o) = inline
                            && matches!(o.kind, OpaqueKind::Drawing)
                            && let Some(ref raw) = o.raw_xml
                            && let Ok(s) = std::str::from_utf8(raw)
                            && let Some(rid) = crate::diff::find_blip_rid(s)
                            // Skip verb-staged image rIds (see the sibling collector).
                            && !rid.starts_with("rIdimg")
                        {
                            push_unique_rid(out, rid);
                        }
                    }
                }
            }
            BlockNode::Table(t) => {
                for row in &t.rows {
                    for cell in &row.cells {
                        collect_drawing_rids_from_blocks(&cell.blocks, out);
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }
}

fn rewrite_inserted_drawing_rids(blocks: &mut [TrackedBlock], remap: &HashMap<String, String>) {
    for tb in blocks {
        match &mut tb.block {
            BlockNode::Paragraph(p) => {
                for seg in &mut p.segments {
                    if !matches!(seg.status, TrackingStatus::Inserted(_)) {
                        continue;
                    }
                    for inline in &mut seg.inlines {
                        if let InlineNode::OpaqueInline(o) = inline
                            && matches!(o.kind, OpaqueKind::Drawing)
                        {
                            rewrite_opaque_drawing_rid(o, remap);
                        }
                    }
                }
            }
            BlockNode::Table(t) => {
                for row in &mut t.rows {
                    if matches!(tb.status, TrackingStatus::Inserted(_)) {
                        for cell in &mut row.cells {
                            rewrite_drawing_rids_in_blocks(&mut cell.blocks, remap);
                        }
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }
}

fn rewrite_drawing_rids_in_blocks(blocks: &mut [BlockNode], remap: &HashMap<String, String>) {
    for block in blocks {
        match block {
            BlockNode::Paragraph(p) => {
                for seg in &mut p.segments {
                    for inline in &mut seg.inlines {
                        if let InlineNode::OpaqueInline(o) = inline
                            && matches!(o.kind, OpaqueKind::Drawing)
                        {
                            rewrite_opaque_drawing_rid(o, remap);
                        }
                    }
                }
            }
            BlockNode::Table(t) => {
                for row in &mut t.rows {
                    for cell in &mut row.cells {
                        rewrite_drawing_rids_in_blocks(&mut cell.blocks, remap);
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }
}

fn rewrite_opaque_drawing_rid(
    o: &mut crate::domain::OpaqueInlineNode,
    remap: &HashMap<String, String>,
) {
    let Some(ref raw) = o.raw_xml else { return };
    let Ok(s) = std::str::from_utf8(raw) else {
        return;
    };
    let Some(old_rid) = crate::diff::find_blip_rid(s) else {
        return;
    };
    let Some(new_rid) = remap.get(&old_rid) else {
        return;
    };
    let updated = s.replace(&old_rid, new_rid);
    o.raw_xml = Some(updated.into_bytes());
}

/// Merge numbering definitions from the target DOCX into the base archive.
///
/// Collects `numId` values referenced by inserted paragraphs in the merged doc,
/// then copies the corresponding `w:num` and `w:abstractNum` elements from the
/// target's `word/numbering.xml` into the base's. Remaps conflicting IDs so
/// inserted paragraphs can keep their `w:numPr` references.
///
/// After merging, clears `literal_prefix` and restores `numbering` on paragraphs
/// whose definitions are now available.
///
/// # Fails loud, does not silently drop a merge
///
/// A block carrying `NumberingInfo` asserts that its rendered label depends on
/// a `<w:num>` definition existing in the *output* package. If a needed numId
/// cannot be sourced from base or target, continuing anyway would export a
/// document whose `w:numPr` points at nothing — Word silently renders the
/// paragraph as unnumbered plain text, with no error anywhere in the pipeline.
/// So this refuses (`Err`) rather than returning early, in every case except
/// one documented exception (see the per-numId lookup below): a numId that
/// will be authored later by `apply_pending_numbering_ops` (the list-split
/// verb) is legitimately absent from both archives at this point.
fn merge_target_numbering(
    doc: &mut CanonDoc,
    base_pkg: &mut DocxPackage,
    target_archive: &DocxArchive,
) -> Result<(), RuntimeError> {
    use xmltree::{Element, XMLNode};

    // Collect numIds from all paragraphs (not just inserted ones) that reference
    // numbering definitions. Modified paragraphs may have had their numId updated
    // to a target value by apply_block_modified, so we need to ensure those
    // definitions are also present in the base archive.
    let mut needed_num_ids: HashSet<u32> = HashSet::new();

    /// Collect numIds from bare BlockNode slices (e.g. table cells). Recurses
    /// into nested tables.
    fn collect_num_ids_from_cell_blocks(blocks: &[BlockNode], out: &mut HashSet<u32>) {
        for block in blocks {
            match block {
                BlockNode::Paragraph(p) => {
                    if let Some(n) = &p.numbering {
                        out.insert(n.num_id);
                    }
                }
                BlockNode::Table(t) => {
                    for row in &t.rows {
                        for cell in &row.cells {
                            collect_num_ids_from_cell_blocks(&cell.blocks, out);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn collect_num_ids_from_blocks(blocks: &[TrackedBlock], out: &mut HashSet<u32>) {
        for tb in blocks {
            match &tb.block {
                BlockNode::Paragraph(p) => {
                    if let Some(n) = &p.numbering {
                        out.insert(n.num_id);
                    }
                }
                BlockNode::Table(t) => {
                    for row in &t.rows {
                        for cell in &row.cells {
                            collect_num_ids_from_cell_blocks(&cell.blocks, out);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // Collect from body and all story parts.
    collect_num_ids_from_blocks(&doc.blocks, &mut needed_num_ids);
    for header in &doc.headers {
        collect_num_ids_from_blocks(&header.blocks, &mut needed_num_ids);
    }
    for footer in &doc.footers {
        collect_num_ids_from_blocks(&footer.blocks, &mut needed_num_ids);
    }
    for footnote in &doc.footnotes {
        collect_num_ids_from_blocks(&footnote.blocks, &mut needed_num_ids);
    }
    for endnote in &doc.endnotes {
        collect_num_ids_from_blocks(&endnote.blocks, &mut needed_num_ids);
    }
    for comment in &doc.comments {
        collect_num_ids_from_blocks(&comment.blocks, &mut needed_num_ids);
    }
    if needed_num_ids.is_empty() {
        return Ok(());
    }

    // Bounded, deterministic sample of the needed numIds for error messages —
    // large documents can reference dozens of lists; the full set is noise,
    // but the caller needs concrete ids to go debug the source document with.
    let needed_ids_sample = || {
        let mut ids: Vec<u32> = needed_num_ids.iter().copied().collect();
        ids.sort_unstable();
        ids.truncate(10);
        ids.iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    };

    // Parse target numbering.xml. Blocks in the merged doc need at least one
    // numId that is not already in base, so the target archive MUST supply a
    // parseable word/numbering.xml — a document can only have populated
    // `NumberingInfo` on a paragraph by resolving numPr against a real
    // numbering.xml at import time (see import::build_canonical_from_archive),
    // so reaching "part missing" here with a non-empty needed set means the
    // CanonDoc and the archive it was built from have gone out of sync: a
    // real invariant violation, not a legitimate flow.
    let target_numbering_bytes =
        target_archive
            .get("word/numbering.xml")
            .ok_or_else(|| RuntimeError {
                code: ErrorCode::InvalidDocx,
                message: format!(
                    "cannot merge numbering: target archive has no word/numbering.xml, \
                 but the merged document references numId(s) [{}] that are not already \
                 in the base archive — exporting would leave dangling w:numPr references",
                    needed_ids_sample()
                ),
                details: ErrorDetails::default(),
            })?;
    let target_root =
        Element::parse(std::io::Cursor::new(target_numbering_bytes)).map_err(|err| {
            RuntimeError {
                code: ErrorCode::InvalidDocx,
                message: "failed to parse target word/numbering.xml during numbering merge"
                    .to_string(),
                details: ErrorDetails {
                    context: Some(format!(
                        "needed numId(s): [{}]; parse error: {err}",
                        needed_ids_sample()
                    )),
                    ..ErrorDetails::default()
                },
            }
        })?;

    // Parse base numbering.xml (may not exist — create empty root if so). If
    // it exists but fails to parse, that is the runtime's own doc going
    // corrupt, not an absent-part case — fail loud rather than silently
    // dropping the merge and leaving base's numbering.xml stale.
    let (mut base_root, base_existed) = match base_pkg.get_part("word/numbering.xml") {
        Some(b) => {
            let r = Element::parse(std::io::Cursor::new(b)).map_err(|err| RuntimeError {
                code: ErrorCode::InvalidDocx,
                message: "failed to parse base word/numbering.xml during numbering merge"
                    .to_string(),
                details: ErrorDetails {
                    context: Some(format!("parse error: {err}")),
                    ..ErrorDetails::default()
                },
            })?;
            (r, true)
        }
        None => {
            let mut r = Element::new("numbering");
            r.prefix = Some("w".to_string());
            r.namespaces = target_root.namespaces.clone();
            (r, false)
        }
    };

    // Collect existing base numId and abstractNumId values to detect conflicts.
    let mut base_num_ids: HashSet<u32> = HashSet::new();
    let mut base_abstract_ids: HashSet<u32> = HashSet::new();
    for child in &base_root.children {
        if let XMLNode::Element(el) = child {
            if is_w_tag(el, "num") {
                if let Some(id_str) = attr_get(el, "numId")
                    && let Ok(id) = id_str.parse::<u32>()
                {
                    base_num_ids.insert(id);
                }
            } else if is_w_tag(el, "abstractNum")
                && let Some(id_str) = attr_get(el, "abstractNumId")
                && let Ok(id) = id_str.parse::<u32>()
            {
                base_abstract_ids.insert(id);
            }
        }
    }

    // Build lookup tables from target: numId -> w:num element, abstractNumId -> w:abstractNum element.
    let mut target_nums: HashMap<u32, Element> = HashMap::new();
    let mut target_abstracts: HashMap<u32, Element> = HashMap::new();
    for child in &target_root.children {
        if let XMLNode::Element(el) = child {
            if is_w_tag(el, "num") {
                if let Some(id_str) = attr_get(el, "numId")
                    && let Ok(id) = id_str.parse::<u32>()
                {
                    target_nums.insert(id, el.clone());
                }
            } else if is_w_tag(el, "abstractNum")
                && let Some(id_str) = attr_get(el, "abstractNumId")
                && let Ok(id) = id_str.parse::<u32>()
            {
                target_abstracts.insert(id, el.clone());
            }
        }
    }

    // For each needed numId, copy from target to base (remapping if needed).
    let mut num_id_remap: HashMap<u32, u32> = HashMap::new();
    let mut next_num_id = base_num_ids.iter().copied().max().unwrap_or(0) + 1;
    let mut next_abstract_id = base_abstract_ids.iter().copied().max().unwrap_or(0) + 1;

    for &num_id in &needed_num_ids {
        if base_num_ids.contains(&num_id) {
            // Already exists in base — no copy needed, no remap.
            continue;
        }
        // Documented non-error exception: a numId absent from BOTH base and
        // target is not necessarily dangling. The list-split verb
        // (edit/verbs/numbering.rs `apply_split`) re-points a run of
        // paragraphs at a freshly allocated numId and stages a
        // `NumberingOp::CreateDefinition` for it in `PendingParts`, which
        // `apply_pending_numbering_ops` materializes into base's
        // word/numbering.xml AFTER this function returns (see the ordering
        // comment on the `merge_target_numbering` call site). At this point
        // in the pipeline we cannot see `PendingParts` to confirm coverage,
        // so we leave the id unresolved here rather than erroring — the verb
        // requires the split's SOURCE numId to already be a real, resolvable
        // list, so a numId only reaches here unclaimed when it is the new
        // instance a pending numbering op is about to author.
        let Some(target_num) = target_nums.get(&num_id) else {
            continue;
        };

        // Find the abstract_num_id this num references.
        let abstract_num_id: Option<u32> = target_num.children.iter().find_map(|c| {
            if let XMLNode::Element(el) = c
                && is_w_tag(el, "abstractNumId")
            {
                return attr_get(el, "val").and_then(|v| v.parse::<u32>().ok());
            }
            None
        });

        // Copy the abstractNum if not already in base.
        if let Some(abs_id) = abstract_num_id
            && !base_abstract_ids.contains(&abs_id)
            && let Some(abstract_el) = target_abstracts.get(&abs_id)
        {
            let mut copied = abstract_el.clone();
            if base_abstract_ids.contains(&abs_id) {
                // Remap abstractNumId
                let new_abs_id = next_abstract_id;
                next_abstract_id += 1;
                attr_set(&mut copied, "w:abstractNumId", new_abs_id.to_string());
                base_abstract_ids.insert(new_abs_id);
            } else {
                base_abstract_ids.insert(abs_id);
            }
            base_root.children.push(XMLNode::Element(copied));
        }

        // Copy the w:num element.
        let mut num_el = target_num.clone();
        let final_num_id = if base_num_ids.contains(&num_id) {
            // Need to remap
            let new_id = next_num_id;
            next_num_id += 1;
            attr_set(&mut num_el, "w:numId", new_id.to_string());
            num_id_remap.insert(num_id, new_id);
            base_num_ids.insert(new_id);
            new_id
        } else {
            base_num_ids.insert(num_id);
            num_id
        };
        let _ = final_num_id;
        base_root.children.push(XMLNode::Element(num_el));
    }

    // Re-sort into the CT_Numbering xsd:sequence (see ct_numbering_child_order):
    // numPicBullet*, abstractNum*, num*, numIdMacAtCleanup?. Base numbering parts
    // carry their numPicBullet elements first; the copies above only ever append
    // abstractNum/num, which can interleave the groups.
    base_root.children.sort_by_key(ct_numbering_child_order);

    // Write the updated numbering.xml back. A serialization failure here must
    // propagate: silently skipping the write leaves base_pkg's numbering.xml
    // stale (missing the copies made above) while num_id_remap below still
    // rewrites the doc's w:numPr to point at ids that were never written.
    let xml_bytes = word_xml::write_document_xml(&base_root).map_err(map_word_xml_error)?;
    base_pkg.set_part("word/numbering.xml", xml_bytes);

    // Ensure the numbering relationship exists
    if !base_existed {
        const NUMBERING_REL_TYPE: &str =
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering";
        base_pkg
            .document_rels
            .add(NUMBERING_REL_TYPE, "numbering.xml");
    }

    // Apply numId remaps to the doc if any were needed.
    if !num_id_remap.is_empty() {
        remap_numids_in_blocks(&mut doc.blocks, &num_id_remap);
        for header in &mut doc.headers {
            remap_numids_in_blocks(&mut header.blocks, &num_id_remap);
        }
        for footer in &mut doc.footers {
            remap_numids_in_blocks(&mut footer.blocks, &num_id_remap);
        }
        for footnote in &mut doc.footnotes {
            remap_numids_in_blocks(&mut footnote.blocks, &num_id_remap);
        }
        for endnote in &mut doc.endnotes {
            remap_numids_in_blocks(&mut endnote.blocks, &num_id_remap);
        }
        for comment in &mut doc.comments {
            remap_numids_in_blocks(&mut comment.blocks, &num_id_remap);
        }
    }

    Ok(())
}

/// Materialize pending numbering-restart requests into `word/numbering.xml`.
///
/// For each paragraph with `numbering.restart_numbering == true`, allocates
/// a fresh `w:num` instance that references the same `abstractNumId` as the
/// paragraph's current `numId` but carries a `w:lvlOverride/w:startOverride`
/// that resets the counter to 1 at the paragraph's ilvl (ECMA-376 §17.9.8 +
/// §17.9.26). The paragraph's `num_id` is then remapped to the new instance,
/// and the restart flag is cleared so subsequent serializations are a no-op.
///
/// Implements the `restart_numbering` field of the LLM edit schema's
/// insert op kind.
///
/// ### Sibling propagation
///
/// OOXML's `w:startOverride` applies only on the *first encounter* of a
/// numId (§17.9.26). That means a multi-item new list can't use one
/// override paragraph plus naive sibling paragraphs still pointing at the
/// original numId — the siblings would either render under the old list's
/// counter (if left alone) or each start a fresh counter of their own (if
/// each got its own override). Neither matches "1. foo / 2. bar".
///
/// The fix: when we process a paragraph with `restart_numbering: true`,
/// we walk forward through the sibling sequence and remap every
/// subsequent list paragraph that shares the ORIGINAL `(num_id, ilvl)` AND
/// the same `apply_op_id` (i.e. it was inserted by the same edit
/// transaction) to the same freshly allocated override numId. The walk
/// skips non-matching paragraphs (body text, different lists) rather than
/// stopping at them, so `list / body / list` inside a single insert batch
/// still groups correctly. It stops when it exits the `apply_op_id`
/// batch, when it hits another paragraph with `restart_numbering: true`
/// (that's the start of a new list run), or when the sibling slice ends.
///
/// Invariants:
/// - A paragraph requesting restart must reference a concrete `w:num` that
///   exists in `word/numbering.xml`. If the part is missing, or the numId
///   is unknown, or its `abstractNumId` is missing, we fail — we do not
///   silently fall back (see CLAUDE.md "no silent fallbacks").
/// - Within a single insert batch (same `apply_op_id`), a run of list
///   paragraphs at the same `(num_id, ilvl)` that starts with
///   `restart_numbering: true` shares ONE fresh numId override. Every
///   sibling in the run renders in the new counter, so `1. foo / 2. bar`
///   works end-to-end.
fn apply_numbering_restart_overrides(
    doc: &mut CanonDoc,
    base_pkg: &mut DocxPackage,
) -> Result<(), RuntimeError> {
    use xmltree::{Element, XMLNode};

    fn has_restart_request(blocks: &[TrackedBlock]) -> bool {
        fn any_in_block(block: &BlockNode) -> bool {
            match block {
                BlockNode::Paragraph(p) => {
                    p.numbering.as_ref().is_some_and(|n| n.restart_numbering)
                }
                BlockNode::Table(t) => t.rows.iter().any(|row| {
                    row.cells
                        .iter()
                        .any(|cell| cell.blocks.iter().any(any_in_block))
                }),
                BlockNode::OpaqueBlock(_) => false,
            }
        }
        blocks.iter().any(|tb| any_in_block(&tb.block))
    }

    let needs_work = has_restart_request(&doc.blocks)
        || doc.headers.iter().any(|h| has_restart_request(&h.blocks))
        || doc.footers.iter().any(|f| has_restart_request(&f.blocks))
        || doc
            .footnotes
            .iter()
            .any(|fn_| has_restart_request(&fn_.blocks))
        || doc
            .endnotes
            .iter()
            .any(|en| has_restart_request(&en.blocks))
        || doc.comments.iter().any(|c| has_restart_request(&c.blocks));
    if !needs_work {
        return Ok(());
    }

    // Parse base numbering.xml. Absence is fatal here — a paragraph can
    // only request restart if it already has a valid NumberingInfo, which
    // means the document must have had a numbering part at import time.
    let numbering_bytes = base_pkg
        .get_part("word/numbering.xml")
        .ok_or_else(|| RuntimeError {
            code: ErrorCode::UnsupportedEdit,
            message:
                "cannot materialize restart_numbering: base document has no word/numbering.xml"
                    .to_string(),
            details: ErrorDetails::default(),
        })?;
    let mut root =
        Element::parse(std::io::Cursor::new(numbering_bytes)).map_err(|err| RuntimeError {
            code: ErrorCode::InvalidDocx,
            message: "failed to parse word/numbering.xml for restart override".to_string(),
            details: ErrorDetails {
                context: Some(format!("{err}")),
                ..ErrorDetails::default()
            },
        })?;

    // Index: numId -> abstractNumId from the parsed root.
    let mut num_to_abstract: HashMap<u32, u32> = HashMap::new();
    let mut max_num_id: u32 = 0;
    for child in &root.children {
        let XMLNode::Element(el) = child else {
            continue;
        };
        if !is_w_tag(el, "num") {
            continue;
        }
        let Some(num_id_str) = attr_get(el, "numId") else {
            continue;
        };
        let Ok(num_id) = num_id_str.parse::<u32>() else {
            continue;
        };
        max_num_id = max_num_id.max(num_id);
        let abstract_num_id: Option<u32> = el.children.iter().find_map(|c| {
            if let XMLNode::Element(inner) = c
                && is_w_tag(inner, "abstractNumId")
            {
                attr_get(inner, "val").and_then(|v| v.parse::<u32>().ok())
            } else {
                None
            }
        });
        if let Some(abs_id) = abstract_num_id {
            num_to_abstract.insert(num_id, abs_id);
        }
    }

    let mut new_overrides: Vec<Element> = Vec::new();

    /// Build a `<w:num>` override element that clones `abstract_num_id`
    /// and restarts `ilvl` at 1. Returns the element + the freshly
    /// allocated numId (the caller advances `next_num_id`).
    fn build_override_num_el(new_num_id: u32, abstract_num_id: u32, ilvl: u32) -> Element {
        let mut num_el = w_el("num");
        attr_set(&mut num_el, "w:numId", new_num_id.to_string());

        let mut abs_el = w_el("abstractNumId");
        attr_set(&mut abs_el, "w:val", abstract_num_id.to_string());
        num_el.children.push(XMLNode::Element(abs_el));

        let mut lvl_override = w_el("lvlOverride");
        attr_set(&mut lvl_override, "w:ilvl", ilvl.to_string());

        let mut start_override = w_el("startOverride");
        attr_set(&mut start_override, "w:val", "1");
        lvl_override.children.push(XMLNode::Element(start_override));

        num_el.children.push(XMLNode::Element(lvl_override));
        num_el
    }

    /// Remap `p.numbering.num_id` from `old_num_id` to `new_num_id`
    /// (and the shadow `materialized_numbering` if it matches). When
    /// `clear_restart` is true, also clear the `restart_numbering` flag.
    fn remap_paragraph(
        p: &mut crate::domain::ParagraphNode,
        old_num_id: u32,
        ilvl: u32,
        new_num_id: u32,
        clear_restart: bool,
    ) {
        if let Some(n) = p.numbering.as_mut()
            && n.num_id == old_num_id
            && n.ilvl == ilvl
        {
            n.num_id = new_num_id;
            if clear_restart {
                n.restart_numbering = false;
            }
        }
        if let Some(mat) = p.materialized_numbering.as_mut()
            && mat.num_id == old_num_id
            && mat.ilvl == ilvl
        {
            mat.num_id = new_num_id;
        }
    }

    /// Read the restart-pending descriptor of the paragraph at position
    /// `i` if it has one. Returns `(original_num_id, ilvl)`.
    fn restart_intent_of(p: &crate::domain::ParagraphNode) -> Option<(u32, u32)> {
        let n = p.numbering.as_ref()?;
        if n.restart_numbering {
            Some((n.num_id, n.ilvl))
        } else {
            None
        }
    }

    /// Allocate a fresh `num_id` override referencing the same
    /// `abstractNumId` as `old_num_id`, push the `<w:num>` element into
    /// `new_overrides`, and return the allocated numId. The caller is
    /// responsible for remapping the triggering paragraph and any
    /// propagated siblings.
    fn allocate_override(
        old_num_id: u32,
        ilvl: u32,
        para_id: &NodeId,
        num_to_abstract: &HashMap<u32, u32>,
        next_num_id: &mut u32,
        new_overrides: &mut Vec<Element>,
    ) -> Result<u32, RuntimeError> {
        let abstract_num_id =
            num_to_abstract
                .get(&old_num_id)
                .copied()
                .ok_or_else(|| RuntimeError {
                    code: ErrorCode::UnsupportedEdit,
                    message: format!(
                        "cannot materialize restart_numbering for paragraph '{para_id}': \
                     numId {old_num_id} not found in word/numbering.xml"
                    ),
                    details: ErrorDetails {
                        block_id: Some(para_id.clone()),
                        ..ErrorDetails::default()
                    },
                })?;
        let new_num_id = *next_num_id;
        *next_num_id += 1;
        new_overrides.push(build_override_num_el(new_num_id, abstract_num_id, ilvl));
        Ok(new_num_id)
    }

    /// Inspect `TrackedBlock.status` and return the `apply_op_id` of the
    /// revision if the block is Inserted, else `None`. Two restart-run
    /// paragraphs must share the same apply_op_id to be grouped — that
    /// is, they must have been inserted by the same `apply_edit` call.
    fn tracked_apply_op_id(tb: &TrackedBlock) -> Option<&str> {
        match &tb.status {
            TrackingStatus::Inserted(rev) => rev.apply_op_id.as_deref(),
            _ => None,
        }
    }

    /// Process a top-level tracked-block sequence with forward sibling
    /// propagation for `restart_numbering` runs. Recurses into table
    /// cells using the simpler (non-propagating) paragraph walker,
    /// because cell paragraphs don't carry their own tracking status —
    /// cell-level multi-item propagation is a known v1 limitation.
    fn process_tracked_sequence(
        blocks: &mut [TrackedBlock],
        num_to_abstract: &HashMap<u32, u32>,
        next_num_id: &mut u32,
        new_overrides: &mut Vec<Element>,
    ) -> Result<(), RuntimeError> {
        let n = blocks.len();
        for i in 0..n {
            // Extract the restart intent + apply_op_id + paragraph id
            // without holding a mutable borrow through the forward walk.
            let restart_trigger = match &blocks[i].block {
                BlockNode::Paragraph(p) => {
                    restart_intent_of(p).map(|(nid, ilvl)| (nid, ilvl, p.id.clone()))
                }
                _ => None,
            };
            if let Some((old_num_id, ilvl, para_id)) = restart_trigger {
                let apply_op_id = tracked_apply_op_id(&blocks[i]).map(str::to_string);
                let new_num_id = allocate_override(
                    old_num_id,
                    ilvl,
                    &para_id,
                    num_to_abstract,
                    next_num_id,
                    new_overrides,
                )?;
                // Remap the trigger paragraph (clears restart flag).
                if let BlockNode::Paragraph(p) = &mut blocks[i].block {
                    remap_paragraph(
                        p, old_num_id, ilvl, new_num_id, /*clear_restart=*/ true,
                    );
                }
                // Forward propagate to sibling list items in the same batch.
                for tb_j in blocks.iter_mut().skip(i + 1) {
                    // Stop if the sibling left the current apply_op_id
                    // batch. Non-Inserted siblings and differently-stamped
                    // inserts are outside the run.
                    let j_apply = tracked_apply_op_id(tb_j).map(str::to_string);
                    if j_apply.as_deref() != apply_op_id.as_deref() {
                        break;
                    }
                    match &mut tb_j.block {
                        BlockNode::Paragraph(p) => {
                            // Another restart trigger starts its own run.
                            if restart_intent_of(p).is_some() {
                                break;
                            }
                            // Matching sibling — remap but don't touch
                            // the restart flag (it was already false).
                            remap_paragraph(
                                p, old_num_id, ilvl, new_num_id, /*clear_restart=*/ false,
                            );
                            // Non-matching paragraphs fall through the
                            // remap (remap_paragraph is a no-op when the
                            // numbering doesn't match) and the walk
                            // continues — so `list / body / list` inside
                            // one batch still groups correctly.
                        }
                        BlockNode::Table(_) | BlockNode::OpaqueBlock(_) => {
                            // Intervening blocks don't break the run;
                            // they just don't receive the override.
                        }
                    }
                }
            }

            // Recurse into tables regardless: cells may contain their
            // own restart-pending paragraphs that need processing.
            if let BlockNode::Table(t) = &mut blocks[i].block {
                for row in &mut t.rows {
                    for cell in &mut row.cells {
                        process_cell_blocks(
                            &mut cell.blocks,
                            num_to_abstract,
                            next_num_id,
                            new_overrides,
                        )?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Cell-scope walker: processes paragraphs inside a table cell one
    /// at a time. Each `restart_numbering: true` gets its own fresh
    /// numId — no sibling propagation at the cell level (v1 limit).
    /// Recurses into nested tables.
    fn process_cell_blocks(
        blocks: &mut [BlockNode],
        num_to_abstract: &HashMap<u32, u32>,
        next_num_id: &mut u32,
        new_overrides: &mut Vec<Element>,
    ) -> Result<(), RuntimeError> {
        for block in blocks {
            match block {
                BlockNode::Paragraph(p) => {
                    let Some((old_num_id, ilvl)) = restart_intent_of(p) else {
                        continue;
                    };
                    let para_id = p.id.clone();
                    let new_num_id = allocate_override(
                        old_num_id,
                        ilvl,
                        &para_id,
                        num_to_abstract,
                        next_num_id,
                        new_overrides,
                    )?;
                    remap_paragraph(
                        p, old_num_id, ilvl, new_num_id, /*clear_restart=*/ true,
                    );
                }
                BlockNode::Table(t) => {
                    for row in &mut t.rows {
                        for cell in &mut row.cells {
                            process_cell_blocks(
                                &mut cell.blocks,
                                num_to_abstract,
                                next_num_id,
                                new_overrides,
                            )?;
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    // Convenience alias so the outer per-story loop reads cleanly.
    let process_tracked_blocks = process_tracked_sequence;

    let mut next_num_id = max_num_id + 1;
    process_tracked_blocks(
        &mut doc.blocks,
        &num_to_abstract,
        &mut next_num_id,
        &mut new_overrides,
    )?;
    for h in &mut doc.headers {
        process_tracked_blocks(
            &mut h.blocks,
            &num_to_abstract,
            &mut next_num_id,
            &mut new_overrides,
        )?;
    }
    for f in &mut doc.footers {
        process_tracked_blocks(
            &mut f.blocks,
            &num_to_abstract,
            &mut next_num_id,
            &mut new_overrides,
        )?;
    }
    for fn_ in &mut doc.footnotes {
        process_tracked_blocks(
            &mut fn_.blocks,
            &num_to_abstract,
            &mut next_num_id,
            &mut new_overrides,
        )?;
    }
    for en in &mut doc.endnotes {
        process_tracked_blocks(
            &mut en.blocks,
            &num_to_abstract,
            &mut next_num_id,
            &mut new_overrides,
        )?;
    }
    for c in &mut doc.comments {
        process_tracked_blocks(
            &mut c.blocks,
            &num_to_abstract,
            &mut next_num_id,
            &mut new_overrides,
        )?;
    }

    if new_overrides.is_empty() {
        return Ok(());
    }

    // Append new `w:num` elements, then resort into the CT_Numbering xsd:sequence
    // (see ct_numbering_child_order): numPicBullet*, abstractNum*, num*,
    // numIdMacAtCleanup?.
    for el in new_overrides {
        root.children.push(XMLNode::Element(el));
    }
    root.children.sort_by_key(ct_numbering_child_order);

    let xml_bytes = word_xml::write_document_xml(&root).map_err(map_word_xml_error)?;
    base_pkg.set_part("word/numbering.xml", xml_bytes);

    Ok(())
}

/// Remap numId references in bare BlockNode slices (table cells). Recurses
/// into nested tables.
fn remap_numids_in_cell_blocks(blocks: &mut [BlockNode], remap: &HashMap<u32, u32>) {
    for block in blocks {
        match block {
            BlockNode::Paragraph(p) => {
                remap_paragraph_numids(p, remap);
            }
            BlockNode::Table(t) => {
                for row in &mut t.rows {
                    for cell in &mut row.cells {
                        remap_numids_in_cell_blocks(&mut cell.blocks, remap);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Remap numId references in TrackedBlock slices (body blocks & story parts).
/// Recursively walks into table cells to find nested paragraphs.
fn remap_numids_in_blocks(blocks: &mut [TrackedBlock], remap: &HashMap<u32, u32>) {
    for tb in blocks {
        match &mut tb.block {
            BlockNode::Paragraph(p) => {
                remap_paragraph_numids(p, remap);
            }
            BlockNode::Table(t) => {
                for row in &mut t.rows {
                    for cell in &mut row.cells {
                        remap_numids_in_cell_blocks(&mut cell.blocks, remap);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Remap all numId references on a single paragraph: `numbering`,
/// `materialized_numbering`, and `formatting_change.previous_numbering`.
fn remap_paragraph_numids(p: &mut crate::domain::ParagraphNode, remap: &HashMap<u32, u32>) {
    if let Some(n) = &mut p.numbering
        && let Some(&new_id) = remap.get(&n.num_id)
    {
        n.num_id = new_id;
    }
    if let Some(n) = &mut p.materialized_numbering
        && let Some(&new_id) = remap.get(&n.num_id)
    {
        n.num_id = new_id;
    }
    if let Some(fc) = &mut p.formatting_change
        && let Some(n) = &mut fc.previous_numbering
        && let Some(&new_id) = remap.get(&n.num_id)
    {
        n.num_id = new_id;
    }
}

/// Detect style ID collisions between base and target documents.
///
/// Collects all style IDs referenced by paragraphs and tables in the merged
/// document, then compares their definitions across both archives. Emits a
/// `tracing::warn!` for each style ID that has diverging XML in base vs target.
fn detect_and_warn_style_collisions(
    doc: &CanonDoc,
    base_pkg: &DocxPackage,
    target_archive: &DocxArchive,
) {
    let base_styles_xml = match base_pkg.get_part("word/styles.xml") {
        Some(b) => b,
        None => return,
    };
    let target_styles_xml = match target_archive.get("word/styles.xml") {
        Some(b) => b,
        None => return,
    };

    let referenced = collect_referenced_style_ids(doc);
    if referenced.is_empty() {
        return;
    }

    let collisions =
        crate::styles::detect_style_collisions(base_styles_xml, target_styles_xml, &referenced);

    for collision in &collisions {
        tracing::warn!(
            style_id = %collision.style_id,
            style_type = %collision.style_type,
            style_name = collision.style_name.as_deref().unwrap_or("(unnamed)"),
            kept = "base",
            "style ID collision — base and target define style with different formatting; base definition will be used"
        );
    }
}

/// Collect all style IDs referenced by paragraphs and tables in a CanonDoc.
///
/// Walks the main body blocks plus all story blocks (headers, footers,
/// footnotes, endnotes, comments) to find every style ID actually in use.
fn collect_referenced_style_ids(doc: &CanonDoc) -> HashSet<IStr> {
    let mut ids = HashSet::new();

    fn collect_from_blocks(blocks: &[TrackedBlock], ids: &mut HashSet<IStr>) {
        for tb in blocks {
            collect_from_block_node(&tb.block, ids);
        }
    }

    fn collect_from_block_node(block: &BlockNode, ids: &mut HashSet<IStr>) {
        match block {
            BlockNode::Paragraph(p) => {
                if let Some(ref style_id) = p.style_id {
                    ids.insert(style_id.clone());
                }
                // Also collect character style IDs from text runs.
                for seg in &p.segments {
                    for inline in &seg.inlines {
                        if let InlineNode::Text(t) = inline
                            && let Some(ref char_style) = t.style_props.char_style_id
                        {
                            ids.insert(char_style.clone());
                        }
                    }
                }
            }
            BlockNode::Table(t) => {
                if let Some(ref style_id) = t.formatting.style_id {
                    ids.insert(style_id.clone());
                }
                for row in &t.rows {
                    for cell in &row.cells {
                        for block in &cell.blocks {
                            collect_from_block_node(block, ids);
                        }
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }

    collect_from_blocks(&doc.blocks, &mut ids);
    for h in &doc.headers {
        collect_from_blocks(&h.blocks, &mut ids);
    }
    for f in &doc.footers {
        collect_from_blocks(&f.blocks, &mut ids);
    }
    for fn_ in &doc.footnotes {
        collect_from_blocks(&fn_.blocks, &mut ids);
    }
    for en in &doc.endnotes {
        collect_from_blocks(&en.blocks, &mut ids);
    }
    for c in &doc.comments {
        collect_from_blocks(&c.blocks, &mut ids);
    }

    ids
}

/// Structural rules whose violation means Word will reject the file or lose
/// data. These are the only findings the serialize-time gate refuses on; they
/// check invariants the serializer maintains by construction and are covered by
/// daily tests. Shared by the debug-assertion gate inside
/// [`serialize_canonical_docx`] and the [`ValidatorLevel`] gate in
/// [`serialize_snapshot`] so both paths refuse on exactly the same set.
pub(crate) const BLOCKING_RULES: &[&str] = &[
    "I-TC-001", // tracked change content model (no hyperlink/fldSimple inside del/ins)
    "I-TC-002", // tracked change missing w:id
    "I-TC-003", // tracked-change nesting: del-in-ins allowed, same-type nesting never
    //             (promoted to blocking with stacked revisions —
    //             cheap insurance against a same-type-nesting emission bug)
    "I-DOC-001", // root must be w:document
    "I-DOC-002", // exactly one w:body
    "I-PKG-000", // package unreadable (ZIP open/read failure)
    "I-PKG-001", // _rels/.rels must exist
    "I-PKG-002", // word/document.xml must exist
    "I-CT-002", // WML parts must carry their canonical content type (§15.2; Word drops the part otherwise)
    "I-XML-001", // a part that is not well-formed XML — nothing downstream of it is checkable
    "I-REL-001", // dangling r:id/r:embed/r:link reference (Word repairs and drops the content)
    "I-REL-002", // duplicate relationship Id (resolution is ambiguous)
    "I-REL-003", // relationship target missing from the package (repair/data-loss class)
    "I-NS-002", // undeclared namespace prefix — the part is not even well-formed XML at the use site
];

/// The stable node id of a tracked body block, for error context.
fn block_id_of_tracked(tracked: &TrackedBlock) -> &NodeId {
    match &tracked.block {
        BlockNode::Paragraph(p) => &p.id,
        BlockNode::Table(t) => &t.id,
        BlockNode::OpaqueBlock(o) => &o.id,
    }
}

/// Apply verb-staged body-level content-control fills to the scaffold nodes
/// (RFC-0002 §Phase-2). Each `OpaqueChildTextSet` names a block `w:sdt` by its
/// frozen `body_index`; we set its first text paragraph's value with the
/// pre-minted tracked-change ids. Fails loud (no silent fallback) when the index
/// is absent, its node is not an element, it has no `w:sdtContent`, or that
/// content has no text paragraph to fill.
fn apply_opaque_child_text_sets(
    opaque_children: &mut HashMap<usize, XMLNode>,
    sets: &[crate::edit::pending_parts::OpaqueChildTextSet],
) -> Result<(), RuntimeError> {
    let fail = |message: &str, body_index: usize| RuntimeError {
        code: ErrorCode::InvalidDocx,
        message: format!("sdt_text_fill: {message}"),
        details: ErrorDetails {
            context: Some(format!("body_index={body_index}")),
            ..ErrorDetails::default()
        },
    };
    for set in sets {
        let node = opaque_children.get_mut(&set.body_index).ok_or_else(|| {
            fail(
                "body-level content control not found at save time",
                set.body_index,
            )
        })?;
        let XMLNode::Element(el) = node else {
            return Err(fail(
                "body-level opaque child is not an element",
                set.body_index,
            ));
        };
        let content =
            crate::opaque_splice::first_descendant_mut(el, "sdtContent").ok_or_else(|| {
                fail(
                    "body-level content control has no w:sdtContent",
                    set.body_index,
                )
            })?;
        let para = crate::opaque_splice::first_text_paragraph_mut(content).ok_or_else(|| {
            fail(
                "content control has no text paragraph to fill",
                set.body_index,
            )
        })?;
        crate::opaque_splice::set_region_text_with_ids(
            para,
            &set.value,
            &set.author,
            set.date.as_deref(),
            set.revision_ids,
            set.tracked,
        )
        .map_err(|e| fail(&format!("fill failed: {e:?}"), set.body_index))?;
    }
    Ok(())
}

fn serialize_canonical_docx(
    base_bytes: &[u8],
    target_bytes: &[u8],
    doc: &mut CanonDoc,
    cached_body: Option<BodyTemplate>,
    pending: &crate::edit::PendingParts,
) -> Result<Vec<u8>, RuntimeError> {
    // Normalize the base archive when it contains pre-existing revision markup.
    // The merged CanonDoc was built from normalized views (view() calls
    // normalize_if_needed before parsing), so OpaqueBlock body_index anchors
    // reference the *normalized* body structure. The raw XML of opaque body
    // children must also be normalized so tracked-change elements (w:ins, w:del)
    // from the original document don't leak into the serialized output.
    let raw_base_archive = DocxArchive::read(base_bytes).map_err(map_docx_error)?;
    // Scan the RAW archive for max w:id BEFORE normalization strips tracked changes.
    let max_raw_wid = max_wid_in_archive(&raw_base_archive);
    let base_archive = {
        crate::normalize::normalize_if_needed(&raw_base_archive).map_err(|e| RuntimeError {
            code: ErrorCode::InvalidDocx,
            message: format!("failed to normalize base archive for serialization: {e:?}"),
            details: ErrorDetails::default(),
        })?
    };
    let target_archive = DocxArchive::read(target_bytes).map_err(map_docx_error)?;

    // Parse both archives into typed package models.
    let mut base_pkg = DocxPackage::from_archive(&base_archive).map_err(map_package_error)?;
    let target_pkg = DocxPackage::from_archive(&target_archive).map_err(map_package_error)?;

    // The main document part is located by the OPC officeDocument relationship
    // (ECMA-376 Part 2 §9.3); its name is not fixed. Every main-part read/write
    // below derives from the resolved name. (Sibling story parts continue to
    // resolve relative to `word/` — the main part's directory for every
    // Word-authored package, including non-standard main-part names.)
    let main_part_name = base_pkg.main_document_part_name().to_string();

    // Build DocumentRelationships views (bridge for existing code that uses the old type).
    let base_rels = document_relationships_from_package_rels(&base_pkg.document_rels);
    let target_rels = document_relationships_from_package_rels(&target_pkg.document_rels);

    // The annotation ID counter must exceed ALL existing w:id values in the
    // output — not just tracked changes in the CanonDoc, but also bookmarks,
    // comment ranges, and any other elements that share the w:id namespace.
    // Uses the raw (pre-normalization) base archive to capture IDs from
    // pre-existing tracked changes that normalization strips.
    // Also scans the target archive because story parts copied from it carry
    // their own w:id values that would otherwise collide.
    //
    // The SDT-id namespace (`<w:id w:val="N"/>`) is disjoint from the annotation
    // `w:id` attribute namespace and thus invisible to `max_wid_in_archive`. Fold
    // it in too: re-iding the inserted copy of a replaced inline content control
    // (see `serialize::emit_tracked_chunks`) draws a fresh id from THIS counter,
    // which must therefore also exceed every live SDT id in either document.
    let max_target_wid = max_wid_in_archive(&target_archive);
    let max_sdt_id =
        max_sdt_id_in_archive(&raw_base_archive).max(max_sdt_id_in_archive(&target_archive));
    let mut annotation_id = max_revision_id(doc)
        .max(max_raw_wid)
        .max(max_target_wid)
        .max(max_sdt_id)
        + 1;

    // Copy media files from target archive for inserted drawings and remap their rIds.
    // Must happen before body serialization so the rewritten raw_xml is used.
    copy_target_media_for_inserted_drawings(
        doc,
        &mut base_pkg,
        &target_archive,
        &target_pkg.document_rels,
    )?;

    // Merge target's numbering definitions into the base package so inserted
    // paragraphs can keep their w:numPr references instead of materializing
    // numbering as literal text.
    merge_target_numbering(doc, &mut base_pkg, &target_archive)?;

    // Materialize any pending numbering-restart requests produced by edit
    // steps with `restart_numbering: true`. Must run after
    // `merge_target_numbering` so every referenced numId is present and
    // remapped in the base numbering.xml before we allocate overrides.
    apply_numbering_restart_overrides(doc, &mut base_pkg)?;

    // Detect style ID collisions between base and target.
    // When the same style ID has different definitions in each document, the
    // merged output silently uses the base definition — warn so callers know.
    detect_and_warn_style_collisions(doc, &base_pkg, &target_archive);

    if let (Some(base_styles_xml), Some(target_styles_xml)) = (
        base_pkg.get_part("word/styles.xml"),
        target_archive.get("word/styles.xml"),
    ) && let Some(merged_styles_xml) =
        crate::styles::merge_styles_xml_preferring_target(base_styles_xml, target_styles_xml)
    {
        base_pkg.set_part("word/styles.xml", merged_styles_xml);
    }

    // Apply verb-staged OPC parts (media binaries + styles.xml ops). Runs AFTER
    // copy_target_media_for_inserted_drawings (so logical rIds rewrite cleanly)
    // and AFTER merge_styles_xml_preferring_target (so an authored Create/Modify
    // style wins a base/target style-id collision instead of being overwritten
    // by the merge). Empty `pending` => no-op for all current verbs.
    apply_pending_parts(doc, &mut base_pkg, pending)?;

    // Use cached body nodes from import when available, avoiding a full
    // xmltree re-parse of document.xml (saves ~2.8s for large documents).
    let (root, mut opaque_body_children, sect_pr_nodes, _body_children_len) = if let Some(cached) =
        cached_body
    {
        (
            cached.root_shell,
            cached.opaque_children,
            cached.sect_pr_nodes,
            cached.body_children_len,
        )
    } else {
        let document_xml = base_pkg
            .get_part(&main_part_name)
            .ok_or_else(|| invalid_docx(&format!("missing main document part {main_part_name}")))?;
        let parsed_root = word_xml::parse_document_xml(document_xml).map_err(map_word_xml_error)?;
        let body = body_element(&parsed_root).map_err(map_word_xml_error)?;
        let children_len = body.children.len();

        // Collect the set of indices referenced by OpaqueBlocks.
        let mut opaque_indices: HashSet<usize> = HashSet::new();
        for tracked in &doc.blocks {
            if let BlockNode::OpaqueBlock(opaque) = &tracked.block
                && let Some(index_str) = opaque.proof_ref.docx_anchor.strip_prefix("body_index:")
                && let Ok(idx) = index_str.parse::<usize>()
                && idx < children_len
            {
                opaque_indices.insert(idx);
            }
        }

        // Single pass: selectively clone only the body children referenced by
        // OpaqueBlocks, and collect sectPr nodes — avoids deep-cloning the
        // entire body (~9MB / 11k nodes for large docs).
        let mut opaque_children: HashMap<usize, XMLNode> =
            HashMap::with_capacity(opaque_indices.len());
        let mut sect_pr: Vec<XMLNode> = Vec::new();
        for (idx, child) in body.children.iter().enumerate() {
            if opaque_indices.contains(&idx) {
                opaque_children.insert(idx, child.clone());
                if let XMLNode::Element(el) = child
                    && is_w_tag(el, "sectPr")
                {
                    sect_pr.push(child.clone());
                }
            } else if let XMLNode::Element(el) = child
                && is_w_tag(el, "sectPr")
            {
                sect_pr.push(child.clone());
            }
        }

        (parsed_root, opaque_children, sect_pr, children_len)
    };

    // Apply verb-staged body-level content-control fills into the scaffold nodes
    // BEFORE they are streamed (RFC-0002 §Phase-2 block-SDT plumbing). A block
    // `w:sdt`'s bytes live here, not on the IR, so the pure edit core validated
    // the fill and minted its tracked-change ids; the save path performs the
    // whole-value set — the same PendingParts seam custom-XML/media/styles use.
    apply_opaque_child_text_sets(&mut opaque_body_children, &pending.opaque_child_text_sets)?;

    let mut sect_pr_nodes = sect_pr_nodes;
    let opaque_body_children = opaque_body_children;

    // Bookmark/move-range id policy for word/document.xml (ids pair per part,
    // ECMA-376 §17.13.2/§17.13.6): pre-scan everything the body emission will
    // produce — model blocks (rebuilt paragraphs/tables, including the
    // decorations carried inside Inserted target blocks) AND the raw-preserved
    // body children (body-level bookmarkStart/bookmarkEnd markers, quarantined
    // blocks), which are base content emitted verbatim. Base markers keep
    // their original ids; only target-origin pairs are remapped/dropped, both
    // halves consistently. This is what keeps a pair intact when its two
    // halves take DIFFERENT emission paths (the RP023 `_GoBack` shape: inline
    // start, body-level end).
    let body_bookmark_policy = {
        let mut scan = crate::serialize::BookmarkScan::default();
        scan.scan_tracked_blocks(&doc.blocks);
        for node in opaque_body_children.values() {
            scan.scan_raw_node(node);
        }
        scan.into_policy(&mut annotation_id)
    };

    // --- Streaming XML output for document.xml ---
    // Instead of building the entire body tree in memory and then serializing,
    // we stream each block element-by-element using the XmlWriter.
    let mut w = XmlWriter::new();
    xml_write::write_ooxml_root_start(&mut w, "w:document", &root).map_err(map_xml_write_error)?;
    // `<w:background>` (CT_Document, ISO 29500-1 §17.2.1) precedes `<w:body>`.
    if let Some(bg) = &doc.document_background {
        w.write_background(bg).map_err(map_xml_write_error)?;
    }
    w.start_tag("w:body").map_err(map_xml_write_error)?;

    // Pre-allocate bookmark IDs for move range markers (same logic as serialize_body_blocks).
    let mut move_bookmark_ids: HashMap<String, (u32, u32)> = HashMap::new();
    for tracked in &doc.blocks {
        if let Some(mid) = &tracked.move_id
            && !move_bookmark_ids.contains_key(mid)
        {
            let from_bm = next_annotation_id(&mut annotation_id);
            let to_bm = next_annotation_id(&mut annotation_id);
            move_bookmark_ids.insert(mid.clone(), (from_bm, to_bm));
        }
    }

    // Stream each block.
    //
    // Block-level content-control wrapping (`WrapBlocksInContentControl`):
    // a `block_sdt_wrap` marker on the FIRST block of a range opens a
    // `<w:sdt><w:sdtPr>…</w:sdtPr><w:sdtContent>` envelope that encloses the
    // marker block plus `span - 1` following blocks. The wrap is UNTRACKED
    // structure (OOXML has no `w:sdtChange`), so it is emitted verbatim and
    // survives accept/reject. We track `sdt_remaining` = how many already-opened
    // wrap blocks are still due to be emitted; it is decremented for the
    // previously-emitted block at the top of the next iteration (the per-block
    // emission paths below use `continue`, so close-after-emit can't run at the
    // loop's foot) and flushed once more after the loop.
    let mut sdt_remaining: usize = 0;
    for tracked in &doc.blocks {
        // Close a wrap that completed on the previous iteration: account for the
        // block emitted last time round, and emit `</w:sdtContent></w:sdt>` when
        // the range is exhausted.
        if sdt_remaining > 0 {
            sdt_remaining -= 1;
            if sdt_remaining == 0 {
                w.end_tag("w:sdtContent").map_err(map_xml_write_error)?;
                w.end_tag("w:sdt").map_err(map_xml_write_error)?;
            }
        }
        // Open a new block-level SDT if this block starts a wrap range. A wrap
        // must never start while another is still open (the verb forbids nesting
        // an authored body wrap inside an authored one); fail loud rather than
        // emit an unbalanced `w:sdt`.
        if let Some(wrap) = &tracked.block_sdt_wrap {
            if sdt_remaining > 0 {
                return Err(RuntimeError {
                    code: ErrorCode::UnsupportedEdit,
                    message: "overlapping block-level content-control wraps".to_string(),
                    details: ErrorDetails {
                        block_id: Some(block_id_of_tracked(tracked).clone()),
                        context: Some(format!("span={}", wrap.span)),
                        ..ErrorDetails::default()
                    },
                });
            }
            w.start_tag("w:sdt").map_err(map_xml_write_error)?;
            let sdt_pr = crate::word_xml::parse_raw_fragment(&wrap.wrapper.sdt_pr_xml).map_err(
                |source| RuntimeError {
                    code: ErrorCode::InvalidDocx,
                    message: "failed to parse block content-control properties".to_string(),
                    details: ErrorDetails {
                        block_id: Some(block_id_of_tracked(tracked).clone()),
                        context: Some(format!("err={source}")),
                        ..ErrorDetails::default()
                    },
                },
            )?;
            w.write_element(&sdt_pr).map_err(map_xml_write_error)?;
            if let Some(ref end_pr_xml) = wrap.wrapper.sdt_end_pr_xml {
                let sdt_end_pr =
                    crate::word_xml::parse_raw_fragment(end_pr_xml).map_err(|source| {
                        RuntimeError {
                            code: ErrorCode::InvalidDocx,
                            message: "failed to parse block content-control end properties"
                                .to_string(),
                            details: ErrorDetails {
                                block_id: Some(block_id_of_tracked(tracked).clone()),
                                context: Some(format!("err={source}")),
                                ..ErrorDetails::default()
                            },
                        }
                    })?;
                w.write_element(&sdt_end_pr).map_err(map_xml_write_error)?;
            }
            w.start_tag("w:sdtContent").map_err(map_xml_write_error)?;
            sdt_remaining = wrap.span;
        }
        // Handle opaque blocks: look up the pre-cloned body child and stream it.
        // Base-origin blocks use "body_index:N", target-origin blocks use "target_body_index:N".
        // Target-origin OpaqueBlocks (Inserted status) are skipped — they can't be wrapped in
        // <w:ins> tracked-change markup (OOXML doesn't support block-level insertion tracking
        // for SDTs/custom XML), so they'd appear as untracked content breaking accept/reject.
        // The canonical model retains them for fixpoint correctness.
        if let BlockNode::OpaqueBlock(opaque) = &tracked.block {
            if opaque
                .proof_ref
                .docx_anchor
                .starts_with("target_body_index:")
            {
                continue;
            }
            let (index_str, source_children) = if let Some(idx) =
                opaque.proof_ref.docx_anchor.strip_prefix("body_index:")
            {
                (idx, &opaque_body_children)
            } else {
                return Err(RuntimeError {
                    code: ErrorCode::UnsupportedEdit,
                    message: "cannot serialize body opaque block without body_index proof anchor"
                        .to_string(),
                    details: ErrorDetails {
                        block_id: Some(opaque.id.clone()),
                        context: Some(opaque.proof_ref.docx_anchor.clone()),
                        ..ErrorDetails::default()
                    },
                });
            };
            let index = index_str.parse::<usize>().map_err(|source| RuntimeError {
                code: ErrorCode::InvalidDocx,
                message: "invalid body_index proof anchor on opaque block".to_string(),
                details: ErrorDetails {
                    block_id: Some(opaque.id.clone()),
                    context: Some(format!(
                        "anchor={} err={source}",
                        opaque.proof_ref.docx_anchor
                    )),
                    ..ErrorDetails::default()
                },
            })?;
            let child = source_children.get(&index).ok_or_else(|| RuntimeError {
                code: ErrorCode::InvalidDocx,
                message: "body_index proof anchor out of bounds during serialization".to_string(),
                details: ErrorDetails {
                    block_id: Some(opaque.id.clone()),
                    context: Some(format!("anchor={}", opaque.proof_ref.docx_anchor,)),
                    ..ErrorDetails::default()
                },
            })?;
            w.write_xml_node(child).map_err(map_xml_write_error)?;
            continue;
        }

        // Emit move range bookmarks around moved blocks.
        if let Some(mid) = &tracked.move_id
            && let Some(&(from_bm, to_bm)) = move_bookmark_ids.get(mid)
        {
            let (author, date) = match &tracked.status {
                TrackingStatus::Deleted(rev) | TrackingStatus::Inserted(rev) => (
                    rev.author.as_deref().unwrap_or(""),
                    rev.date.as_deref().unwrap_or(""),
                ),
                // Block-level stacked status is never constructed; total via
                // the deleting revision (the outermost pending act).
                TrackingStatus::InsertedThenDeleted(sr) => (
                    sr.deleted.author.as_deref().unwrap_or(""),
                    sr.deleted.date.as_deref().unwrap_or(""),
                ),
                TrackingStatus::Normal => ("", ""),
            };
            match &tracked.status {
                TrackingStatus::Deleted(_) => {
                    w.write_element(&word_xml::w_move_from_range_start(
                        from_bm, mid, author, date,
                    ))
                    .map_err(map_xml_write_error)?;
                    let mut rid_resolver = |part_path: &str, rel_type: &str| -> String {
                        resolve_story_part_to_rid(part_path, rel_type, &mut base_pkg, &target_pkg)
                    };
                    let el = serialize_tracked_block(
                        tracked,
                        &mut annotation_id,
                        &body_bookmark_policy,
                        Some(&mut rid_resolver),
                    )?;
                    w.write_element(&el).map_err(map_xml_write_error)?;
                    w.write_element(&word_xml::w_move_from_range_end(from_bm))
                        .map_err(map_xml_write_error)?;
                    continue;
                }
                TrackingStatus::Inserted(_) => {
                    w.write_element(&word_xml::w_move_to_range_start(to_bm, mid, author, date))
                        .map_err(map_xml_write_error)?;
                    let mut rid_resolver = |part_path: &str, rel_type: &str| -> String {
                        resolve_story_part_to_rid(part_path, rel_type, &mut base_pkg, &target_pkg)
                    };
                    let el = serialize_tracked_block(
                        tracked,
                        &mut annotation_id,
                        &body_bookmark_policy,
                        Some(&mut rid_resolver),
                    )?;
                    w.write_element(&el).map_err(map_xml_write_error)?;
                    w.write_element(&word_xml::w_move_to_range_end(to_bm))
                        .map_err(map_xml_write_error)?;
                    continue;
                }
                _ => {}
            }
        }

        // Regular block: serialize and stream immediately.
        let mut rid_resolver = |part_path: &str, rel_type: &str| -> String {
            resolve_story_part_to_rid(part_path, rel_type, &mut base_pkg, &target_pkg)
        };
        let el = serialize_tracked_block(
            tracked,
            &mut annotation_id,
            &body_bookmark_policy,
            Some(&mut rid_resolver),
        )?;
        w.write_element(&el).map_err(map_xml_write_error)?;
    }

    // Flush a block-level SDT wrap whose last block was the final body block:
    // the loop-top decrement accounts for every block except the one emitted in
    // the final iteration, so close the still-open envelope here.
    if sdt_remaining > 0 {
        sdt_remaining -= 1;
        debug_assert_eq!(
            sdt_remaining, 0,
            "block_sdt_wrap span ran past the end of the body"
        );
        w.end_tag("w:sdtContent").map_err(map_xml_write_error)?;
        w.end_tag("w:sdt").map_err(map_xml_write_error)?;
    }

    // Stream sectPr.
    // When section properties changed, rebuild sectPr from the target values
    // and append a sectPrChange child recording the previous (base) state.
    if let Some(ref change) = doc.body_section_property_change {
        if let Some(ref target_sp) = doc.body_section_properties {
            // Build the sectPrChange element.
            let mut sect_pr_change = w_el("sectPrChange");
            attr_set(
                &mut sect_pr_change,
                "w:id",
                next_annotation_id(&mut annotation_id).to_string(),
            );
            if let Some(ref author) = change.revision.author {
                attr_set(&mut sect_pr_change, "w:author", author.clone());
            }
            if let Some(ref date) = change.revision.date {
                attr_set(&mut sect_pr_change, "w:date", date.clone());
            }
            if let Ok(mut prev_el) = word_xml::parse_raw_fragment(&change.previous_properties_raw) {
                materialize_empty_sect_pr_snapshot(&mut prev_el);
                sect_pr_change.children.push(XMLNode::Element(prev_el));
            }

            // Extract the base sectPr element (if any) for merging.
            let base_el = sect_pr_nodes.first().and_then(|n| match n {
                XMLNode::Element(el) => Some(el),
                _ => None,
            });

            // section_properties_to_element owns the full assembly:
            // modeled children + base merge + sectPrChange + ordering.
            // Resolve part_path → rId for header/footer references.
            let mut rid_resolver = |part_path: &str, rel_type: &str| -> String {
                resolve_story_part_to_rid(part_path, rel_type, &mut base_pkg, &target_pkg)
            };
            // The previous sectPr inside sectPrChange carries placeholder
            // (part_path) r:id values; resolve them through the same package
            // resolver so they become real, registered relationship rIds.
            resolve_sect_pr_change_story_refs(&mut sect_pr_change, &mut rid_resolver);
            let new_sect_pr = section_properties_to_element(
                target_sp,
                base_el,
                Some(sect_pr_change),
                Some(&mut rid_resolver),
            );

            w.write_element(&new_sect_pr).map_err(map_xml_write_error)?;
        } else {
            // Change recorded but no target properties — fall back to base sectPr.
            for node in &mut sect_pr_nodes {
                if let XMLNode::Element(el) = node
                    && is_w_tag(el, "sectPr")
                {
                    remap_sect_pr_story_refs(el, &mut base_pkg, &target_pkg)?;
                }
                w.write_xml_node(node).map_err(map_xml_write_error)?;
            }
        }
    } else {
        // No section property change — preserve the base sectPr opaquely.
        // Still validate header/footer rIds in case the base sectPr carries
        // references that were remapped or removed during package assembly.
        for node in &mut sect_pr_nodes {
            if let XMLNode::Element(el) = node
                && is_w_tag(el, "sectPr")
            {
                remap_sect_pr_story_refs(el, &mut base_pkg, &target_pkg)?;
            }
            w.write_xml_node(node).map_err(map_xml_write_error)?;
        }
    }

    w.end_tag("w:body").map_err(map_xml_write_error)?;
    w.end_tag("w:document").map_err(map_xml_write_error)?;
    let updated_document_xml = w.into_inner();
    base_pkg.set_part(&main_part_name, updated_document_xml);

    // Body-level stories (headers/footers) — streamed element-by-element.
    // §17.10.5 blank-synthesized stories are render-time models only: writing
    // them would create orphan blank parts (+ injected references).
    for header in doc.headers.iter().filter(|h| !h.synthesized) {
        let part_path = relationship_target_to_part_path(&header.part_name);
        let root = load_story_template_root(&base_pkg, &target_archive, &part_path)?;
        let root_tag = story_root_tag(&root);
        let mut hw = XmlWriter::new();
        xml_write::write_ooxml_root_start(&mut hw, &root_tag, &root)
            .map_err(map_xml_write_error)?;
        // Bookmark ids pair per part: each header gets its own policy.
        let story_bookmark_policy = {
            let mut scan = crate::serialize::BookmarkScan::default();
            scan.scan_tracked_blocks(&header.blocks);
            scan.into_policy(&mut annotation_id)
        };
        write_story_blocks_with_sdt_envelopes(
            &mut hw,
            &header.blocks,
            &mut annotation_id,
            &story_bookmark_policy,
        )?;
        hw.end_tag(&root_tag).map_err(map_xml_write_error)?;
        let xml = hw.into_inner();
        base_pkg.set_part(&part_path, xml);
        ensure_story_part_rels(&mut base_pkg, &target_pkg, &part_path);
        let ct_path = format!("/{part_path}");
        base_pkg
            .content_types
            .add_override(&ct_path, content_type_for_story_rel(HEADER_REL_TYPE)?);
        if base_rels
            .headers
            .iter()
            .all(|rel| relationship_target_to_part_path(&rel.target) != part_path)
            && let Some(target_rel) = target_rels
                .headers
                .iter()
                .find(|rel| relationship_target_to_part_path(&rel.target) == part_path)
        {
            base_pkg.document_rels.add_with_preferred_id(
                HEADER_REL_TYPE,
                &target_rel.target,
                &target_rel.id,
            );
        }
    }

    for footer in doc.footers.iter().filter(|f| !f.synthesized) {
        let part_path = relationship_target_to_part_path(&footer.part_name);
        let root = load_story_template_root(&base_pkg, &target_archive, &part_path)?;
        let root_tag = story_root_tag(&root);
        let mut fw = XmlWriter::new();
        xml_write::write_ooxml_root_start(&mut fw, &root_tag, &root)
            .map_err(map_xml_write_error)?;
        // Bookmark ids pair per part: each footer gets its own policy.
        let story_bookmark_policy = {
            let mut scan = crate::serialize::BookmarkScan::default();
            scan.scan_tracked_blocks(&footer.blocks);
            scan.into_policy(&mut annotation_id)
        };
        write_story_blocks_with_sdt_envelopes(
            &mut fw,
            &footer.blocks,
            &mut annotation_id,
            &story_bookmark_policy,
        )?;
        fw.end_tag(&root_tag).map_err(map_xml_write_error)?;
        let xml = fw.into_inner();
        base_pkg.set_part(&part_path, xml);
        ensure_story_part_rels(&mut base_pkg, &target_pkg, &part_path);
        let ct_path = format!("/{part_path}");
        base_pkg
            .content_types
            .add_override(&ct_path, content_type_for_story_rel(FOOTER_REL_TYPE)?);
        if base_rels
            .footers
            .iter()
            .all(|rel| relationship_target_to_part_path(&rel.target) != part_path)
            && let Some(target_rel) = target_rels
                .footers
                .iter()
                .find(|rel| relationship_target_to_part_path(&rel.target) == part_path)
        {
            base_pkg.document_rels.add_with_preferred_id(
                FOOTER_REL_TYPE,
                &target_rel.target,
                &target_rel.id,
            );
        }
    }

    serialize_footnotes_part(
        &mut base_pkg,
        &target_archive,
        &base_rels,
        &target_rels,
        &doc.footnotes,
        &mut annotation_id,
    )?;
    serialize_endnotes_part(
        &mut base_pkg,
        &target_archive,
        &base_rels,
        &target_rels,
        &doc.endnotes,
        &mut annotation_id,
    )?;
    serialize_comments_part(
        &mut base_pkg,
        &target_archive,
        &base_rels,
        &target_rels,
        &doc.comments,
        &mut annotation_id,
    )?;
    // commentsExtended.xml (reply threading + resolved state) is a typed model:
    // re-emit it from `comments_extended` when present. A document we never
    // authored into still round-trips equivalently (same paraId/parent/done
    // set). Beside the people-part synthesis below in spirit, but emitted here
    // next to comments since it is the comments sidecar.
    crate::serialize::serialize_comments_extended_part(&mut base_pkg, &doc.comments_extended)?;
    // commentsIds.xml (w16cid durable-id sidecar, MS-DOCX §2.5.3.1) is opaque
    // passthrough; reconcile it against the current comment set so a
    // newly-authored comment gets its durable-id entry (Word distrusts a comment
    // absent from this part). Only maintained when the package already carries
    // the part — never created where absent.
    crate::serialize::serialize_comments_ids_part(&mut base_pkg, &doc.comments)?;
    sync_document_custom_xml_parts(&mut base_pkg, &target_pkg, &target_rels);
    sync_custom_properties_part(&mut base_pkg, &target_pkg);

    // Copy story parts from the target archive that aren't already in the output.
    // The model-based serialization above handles stories present in the CanonDoc,
    // but the target (or base) may reference additional header/footer/endnotes files
    // (e.g. endnotes.xml when there are only separator notes, or extra headers
    // referenced by sectPr sections). Copy them through so the output is complete.
    copy_missing_story_parts(&mut base_pkg, &target_pkg, &base_rels, &target_rels)?;

    // Copy standard parts from target when base lacks them.
    for part in &["word/styles.xml", "word/settings.xml", "word/fontTable.xml"] {
        if !base_pkg.has_part(part)
            && let Some(data) = target_archive.get(part)
        {
            base_pkg.set_part(part, data.to_vec());
        }
    }

    // Apply the document-level evenAndOddHeaders toggle (ISO 29500-1
    // §17.15.1.35) to word/settings.xml, honoring the three-state model
    // (None = absent, Some(true) = on, Some(false) = explicit off). Only writes
    // when the IR carries a state to assert; a fully-absent setting on a
    // document we never toggled is left untouched (no silent fallback).
    apply_even_and_odd_headers_to_settings(&mut base_pkg, doc.even_and_odd_headers)?;

    // Generate word/people.xml with all tracked change authors.
    let authors = collect_tracked_change_authors(doc);
    if !authors.is_empty() {
        let people_xml = build_people_xml(&authors);
        base_pkg.set_part("word/people.xml", people_xml.into_bytes());
        base_pkg.document_rels.add(
            "http://schemas.microsoft.com/office/2011/relationships/people",
            "people.xml",
        );
        base_pkg.content_types.add_override(
            "/word/people.xml",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.people+xml",
        );
    }

    // Post-serialization bookmark-pairing guard (read-only). Imbalance the
    // INPUT already had passes through byte-faithfully; imbalance the
    // serialization INTRODUCED is an engine bug and fails loudly. This
    // replaces the old silent "repair" pass that synthesized zero-span ends
    // and deleted orphan ends — masking torn pairs by collapsing the
    // bookmark's range (fix-at-symptom; banned).
    enforce_story_bookmark_integrity(&base_pkg, &base_archive, &target_archive)?;

    // Post-serialization field-character integrity guard (read-only). The merge
    // pipeline could in principle split a field sequence (begin/separate/end)
    // across tracked-change boundaries, leaving e.g. a fldChar begin inside
    // <w:del> without its matching end — Word treats that as corruption. This
    // replaces the old silent "repair" pass that stripped the offending <w:del>
    // runs: that masked the upstream merge defect (fix-at-symptom; banned). An
    // imbalance the INPUT already carried passes through; one the serialization
    // INTRODUCED is an engine bug and fails loudly. (Measured 0 firings across
    // the daily suite and the full corpus sweep — the merge defect it used to
    // compensate no longer reproduces, so the guard is silent in practice and
    // surfaces a regression instead of hiding it.)
    enforce_story_field_char_integrity(&base_pkg, &base_archive, &target_archive)?;

    // Post-serialization deleted-text-form guard (read-only). `w:delText`
    // (§17.4.20) and `w:delInstrText` (§17.16.13) are the deleted forms of run
    // content and are legal ONLY inside `w:del` ancestry; a bare one in a plain
    // run is schema-invalid and makes Word repair the file on open. The restore
    // path (`serialize::coerce_opaque_run_text` with `deleted = false`) converts
    // them back on reject, so the engine never emits one — this guard is the
    // ratchet that keeps that true forever. Inherited-vs-introduced mirrors the
    // sibling guards: a malformed input part passes through byte-faithfully; one
    // the serialization INTRODUCED is an engine bug and fails loudly.
    enforce_story_deleted_text_integrity(&base_pkg, &base_archive, &target_archive)?;

    // Guarantee canonical content-type Overrides for every WML part in the
    // merged package (OPC §10.1.2 / ECMA-376 Part 1 §15.2). The merge path
    // copies the base package's content types verbatim, so a base that shipped
    // a WML part without its Override (e.g. `word/comments.xml`) would emit the
    // same defect — Word can no longer locate the part by content type and
    // drops it on repair. Mirrors the scaffold-build guarantee.
    base_pkg.ensure_canonical_wml_content_types();

    // Serialize the typed package back to a DocxArchive, then to ZIP bytes.
    let final_archive = base_pkg.into_archive().map_err(map_package_error)?;
    let docx_bytes = final_archive.write().map_err(map_docx_error)?;

    // Early validator pass in debug/test builds: catch a blocking-rule
    // violation at the point the merge produced it, with the producing path
    // still identifiable. Release builds rely on the outer gate instead —
    // these bytes are re-parsed into a snapshot whose eventual emission goes
    // through `serialize_snapshot`, which gates at Blocking by default.
    #[cfg(debug_assertions)]
    {
        let validation = crate::docx_validate::validate_docx(&docx_bytes);

        let blocking_errors: Vec<String> = validation
            .errors()
            .filter(|f| BLOCKING_RULES.contains(&f.rule_id))
            .map(|f| format!("[{}] {}: {}", f.rule_id, f.location, f.message))
            .collect();

        if !blocking_errors.is_empty() {
            return Err(RuntimeError {
                code: ErrorCode::ValidationFailed,
                message: format!(
                    "DOCX validation failed with {} error(s):\n{}",
                    blocking_errors.len(),
                    blocking_errors.join("\n")
                ),
                details: ErrorDetails::default(),
            });
        }

        // The remaining findings are advisory on this path: the serialize gate
        // refuses only on BLOCKING_RULES, so everything else is a condition the
        // merge deliberately lets through — Word opens the file and loses no
        // data (e.g. I-ANN-001 duplicate annotation ids, non-conformant per
        // ECMA-376 but Word-tolerated and inherited byte-faithfully from the
        // input, not introduced here). Surface them so the producing path stays
        // observable, but honestly: a finding's Display renders "ERROR", which
        // is a false alarm on a path that does not block on it, and one line
        // per occurrence buries the signal. `advisory_summary` collapses them
        // to one labelled, counted line per rule.
        for line in validation.advisory_summary(BLOCKING_RULES) {
            eprintln!("DOCX validation (advisory, non-blocking): {line}");
        }
    }

    Ok(docx_bytes)
}

// =============================================================================
// Post-serialization bookmark-pairing guard
// =============================================================================
//
// Invariant I1 (no torn pairs): every `bookmarkStart` the engine emits has
// exactly one `bookmarkEnd` with the same id in the same part (ECMA-376
// §17.13.2/§17.13.6 — the id is the part-local pairing key). The upstream id
// policy (`crate::serialize::BookmarkIdPolicy`) preserves this by
// construction; the guard here is the fail-loud backstop:
//
// - Imbalance the INPUT already had (base or target part carried the same
//   orphan ids) is the document's own state — it passes through
//   byte-faithfully. Opaque fidelity forbids "repairing" input content.
// - Imbalance the serialization INTRODUCED is an engine bug — refuse with
//   part path + orphan ids rather than launder it (the old repair pass
//   synthesized zero-span ends / deleted orphan ends, silently collapsing
//   bookmark ranges).

/// Returns true if the part path is a story part that can contain bookmarks.
/// `main_part` is the resolved main document part (its name is not fixed at
/// word/document.xml), so it is always a story part regardless of naming.
fn is_story_part(path: &str, main_part: &str) -> bool {
    path.eq_ignore_ascii_case(main_part)
        || path.starts_with("word/header") && path.ends_with(".xml")
        || path.starts_with("word/footer") && path.ends_with(".xml")
        || path == "word/footnotes.xml"
        || path == "word/endnotes.xml"
}

/// Orphaned bookmark ids in a parsed part: (starts without ends, ends
/// without starts).
fn bookmark_orphans(root: &Element) -> (HashSet<String>, HashSet<String>) {
    let mut start_ids: HashSet<String> = HashSet::new();
    let mut end_ids: HashSet<String> = HashSet::new();
    collect_bookmark_ids(root, &mut start_ids, &mut end_ids);
    let orphan_starts = start_ids.difference(&end_ids).cloned().collect();
    let orphan_ends = end_ids.difference(&start_ids).cloned().collect();
    (orphan_starts, orphan_ends)
}

/// Orphan sets of the same part in an input archive (empty when the part is
/// absent or unparseable — an unparseable INPUT part contributes nothing we
/// can inherit from).
fn inherited_bookmark_orphans(
    archive: &DocxArchive,
    path: &str,
) -> (HashSet<String>, HashSet<String>) {
    let Some(bytes) = archive.get(path) else {
        return (HashSet::new(), HashSet::new());
    };
    let Ok(root) = word_xml::parse_document_xml(bytes) else {
        return (HashSet::new(), HashSet::new());
    };
    bookmark_orphans(&root)
}

/// Fail-loud check of one part's orphans against the inherited sets.
/// Read-only: inherited orphans pass through byte-faithfully; new orphans
/// are an engine bug.
fn check_part_bookmark_integrity(
    part_path: &str,
    orphan_starts: &HashSet<String>,
    orphan_ends: &HashSet<String>,
    inherited_starts: &HashSet<String>,
    inherited_ends: &HashSet<String>,
) -> Result<(), RuntimeError> {
    let mut new_starts: Vec<&String> = orphan_starts.difference(inherited_starts).collect();
    let mut new_ends: Vec<&String> = orphan_ends.difference(inherited_ends).collect();
    if new_starts.is_empty() && new_ends.is_empty() {
        if runtime_timing_logs_enabled() && (!orphan_starts.is_empty() || !orphan_ends.is_empty()) {
            eprintln!(
                "[bookmark guard] {part_path}: inherited unpaired bookmarks passed through \
                 (orphan starts {orphan_starts:?}, orphan ends {orphan_ends:?})"
            );
        }
        return Ok(());
    }
    new_starts.sort();
    new_ends.sort();
    Err(RuntimeError {
        code: ErrorCode::ValidationFailed,
        message: format!(
            "serialization introduced unpaired bookmarks in {part_path}: \
             {} orphaned bookmarkStart id(s) {:?}, {} orphaned bookmarkEnd id(s) {:?} \
             — engine bug (a bookmark pair was torn across emission paths); \
             refusing to emit (ECMA-376 §17.13.6 requires start/end pairing per id)",
            new_starts.len(),
            new_starts,
            new_ends.len(),
            new_ends,
        ),
        details: ErrorDetails {
            context: Some(format!("part={part_path}")),
            ..ErrorDetails::default()
        },
    })
}

/// Check all story parts of the serialized package. `base_archive` is the
/// (normalized) base the serialization consumed; `target_archive` provides
/// parts copied through verbatim from the target. Orphans present in either
/// input part of the same name are inherited.
fn enforce_story_bookmark_integrity(
    pkg: &DocxPackage,
    base_archive: &DocxArchive,
    target_archive: &DocxArchive,
) -> Result<(), RuntimeError> {
    let story_paths: Vec<String> = pkg
        .part_names()
        .filter(|p| is_story_part(p, pkg.main_document_part_name()))
        .map(|p| p.to_string())
        .collect();

    for path in story_paths {
        let Some(xml_bytes) = pkg.get_part(&path) else {
            continue;
        };
        // An unparseable OUTPUT part is caught by the Blocking validator gate
        // (I-XML-001) — bookmark pairing is not checkable inside it.
        let Ok(root) = word_xml::parse_document_xml(xml_bytes) else {
            continue;
        };
        let (orphan_starts, orphan_ends) = bookmark_orphans(&root);
        if orphan_starts.is_empty() && orphan_ends.is_empty() {
            continue;
        }
        let (base_starts, base_ends) = inherited_bookmark_orphans(base_archive, &path);
        let (target_starts, target_ends) = inherited_bookmark_orphans(target_archive, &path);
        let inherited_starts: HashSet<String> =
            base_starts.union(&target_starts).cloned().collect();
        let inherited_ends: HashSet<String> = base_ends.union(&target_ends).cloned().collect();
        check_part_bookmark_integrity(
            &path,
            &orphan_starts,
            &orphan_ends,
            &inherited_starts,
            &inherited_ends,
        )?;
    }
    Ok(())
}

/// Recursively collect all bookmarkStart and bookmarkEnd w:id values.
fn collect_bookmark_ids(
    el: &Element,
    start_ids: &mut HashSet<String>,
    end_ids: &mut HashSet<String>,
) {
    if is_w_tag(el, "bookmarkStart") {
        if let Some(id) = attr_get(el, "w:id") {
            start_ids.insert(id.clone());
        }
    } else if is_w_tag(el, "bookmarkEnd")
        && let Some(id) = attr_get(el, "w:id")
    {
        end_ids.insert(id.clone());
    }
    for child in &el.children {
        if let XMLNode::Element(child_el) = child {
            collect_bookmark_ids(child_el, start_ids, end_ids);
        }
    }
}

/// Read-only field-character integrity guard over the serialized story parts.
///
/// A complete OOXML field is `fldChar(begin) → instrText → fldChar(separate) →
/// result → fldChar(end)`. If the merge pipeline deletes only part of that
/// sequence — e.g. the `begin` lands inside `<w:del>` while its matching `end`
/// does not — Word treats the paragraph as corrupt. This guard detects such an
/// imbalance in the OUTPUT and refuses, naming the part, UNLESS the same part in
/// an input (base or target) already carried a deleted-fldChar imbalance: an
/// imbalance the input shipped is the user's, passes through byte-faithfully,
/// and is not ours to launder. Only an imbalance the serialization INTRODUCED is
/// an engine bug.
///
/// This replaces the old mutating `repair_story_field_chars`, which silently
/// stripped the offending `<w:del>` runs — masking the upstream merge defect
/// (fix-at-symptom; banned by CLAUDE.md).
fn enforce_story_field_char_integrity(
    pkg: &DocxPackage,
    base_archive: &DocxArchive,
    target_archive: &DocxArchive,
) -> Result<(), RuntimeError> {
    let story_paths: Vec<String> = pkg
        .part_names()
        .filter(|p| is_story_part(p, pkg.main_document_part_name()))
        .map(|p| p.to_string())
        .collect();

    for path in story_paths {
        let Some(xml_bytes) = pkg.get_part(&path) else {
            continue;
        };
        // An unparseable OUTPUT part is caught by the Blocking validator gate
        // (I-XML-001); field balance is not checkable inside it.
        let Ok(root) = word_xml::parse_document_xml(xml_bytes) else {
            continue;
        };
        if !part_has_del_field_char_imbalance(&root) {
            continue;
        }
        // Output is imbalanced — inherited only if an input part of the same
        // name was already imbalanced.
        let inherited = input_part_has_del_field_char_imbalance(base_archive, &path)
            || input_part_has_del_field_char_imbalance(target_archive, &path);
        if inherited {
            if runtime_timing_logs_enabled() {
                eprintln!(
                    "[field-char guard] {path}: inherited deleted-fldChar imbalance passed through"
                );
            }
            continue;
        }
        return Err(RuntimeError {
            code: ErrorCode::ValidationFailed,
            message: format!(
                "serialization introduced an unbalanced deleted field character sequence in \
                 {path}: a fldChar begin/end inside <w:del> has no matching counterpart also \
                 deleted — engine bug (the merge pipeline tore a field across a tracked-change \
                 boundary); refusing to emit (Word treats this as document corruption)"
            ),
            details: ErrorDetails {
                context: Some(format!("part={path}")),
                ..ErrorDetails::default()
            },
        });
    }
    Ok(())
}

/// Same imbalance check over a part in an input archive (empty/false when the
/// part is absent or unparseable — an unparseable INPUT contributes nothing we
/// can inherit from).
fn input_part_has_del_field_char_imbalance(archive: &DocxArchive, path: &str) -> bool {
    let Some(bytes) = archive.get(path) else {
        return false;
    };
    let Ok(root) = word_xml::parse_document_xml(bytes) else {
        return false;
    };
    part_has_del_field_char_imbalance(&root)
}

/// True if any paragraph in the tree (including nested table paragraphs) has an
/// unbalanced count of `fldChar` begins vs ends inside its `<w:del>` blocks.
fn part_has_del_field_char_imbalance(root: &Element) -> bool {
    fn walk(el: &Element) -> bool {
        if is_w_tag(el, "p") && paragraph_del_field_chars_imbalanced(el) {
            return true;
        }
        el.children.iter().any(|child| {
            if let XMLNode::Element(child_el) = child {
                walk(child_el)
            } else {
                false
            }
        })
    }
    walk(root)
}

/// Count `fldChar` begins vs ends that appear inside `<w:del>` blocks among a
/// paragraph's direct children; `true` when they don't balance. Only `fldChar`
/// (begin/separate/end) affects field balance — standalone deleted `instrText`
/// is harmless and is intentionally not counted.
fn paragraph_del_field_chars_imbalanced(para: &Element) -> bool {
    let mut del_begins: u32 = 0;
    let mut del_ends: u32 = 0;

    for child in &para.children {
        let XMLNode::Element(child_el) = child else {
            continue;
        };
        if !is_w_tag(child_el, "del") {
            continue;
        }
        for del_child in &child_el.children {
            let XMLNode::Element(run) = del_child else {
                continue;
            };
            for run_child in &run.children {
                if let XMLNode::Element(rc) = run_child
                    && is_w_tag(rc, "fldChar")
                    && let Some(ftype) = attr_get(rc, "w:fldCharType")
                {
                    match ftype.as_str() {
                        "begin" => del_begins += 1,
                        "end" => del_ends += 1,
                        _ => {}
                    }
                }
            }
        }
    }

    del_begins != del_ends
}

/// Read-only guard: `w:delText`/`w:delInstrText` may appear only inside `w:del`
/// ancestry.
///
/// These are the deleted forms of run content (ECMA-376 §17.4.20 / §17.16.13);
/// outside a `w:del` they are schema-invalid and make Word repair the file on
/// open. The reject/restore path converts them back to `w:t`/`w:instrText`
/// (`serialize::coerce_opaque_run_text`), so a well-formed emission never carries
/// one outside a deletion — this guard is the ratchet that keeps the class gated.
/// UNLESS the same part in an input already carried the defect (inherited,
/// passes through byte-faithfully, mirroring the sibling guards), an introduced
/// occurrence is an engine bug and fails loudly.
fn enforce_story_deleted_text_integrity(
    pkg: &DocxPackage,
    base_archive: &DocxArchive,
    target_archive: &DocxArchive,
) -> Result<(), RuntimeError> {
    let story_paths: Vec<String> = pkg
        .part_names()
        .filter(|p| is_story_part(p, pkg.main_document_part_name()))
        .map(|p| p.to_string())
        .collect();

    for path in story_paths {
        let Some(xml_bytes) = pkg.get_part(&path) else {
            continue;
        };
        // An unparseable OUTPUT part is caught by the Blocking validator gate.
        let Ok(root) = word_xml::parse_document_xml(xml_bytes) else {
            continue;
        };
        if !part_has_deleted_text_outside_del(&root) {
            continue;
        }
        let inherited = input_part_has_deleted_text_outside_del(base_archive, &path)
            || input_part_has_deleted_text_outside_del(target_archive, &path);
        if inherited {
            if runtime_timing_logs_enabled() {
                eprintln!(
                    "[deleted-text guard] {path}: inherited delText/delInstrText outside w:del passed through"
                );
            }
            continue;
        }
        return Err(RuntimeError {
            code: ErrorCode::ValidationFailed,
            message: format!(
                "serialization introduced a w:delText/w:delInstrText outside w:del ancestry in \
                 {path}: the deleted form of run content is legal only inside a deletion \
                 (ECMA-376 §17.4.20 / §17.16.13) — engine bug (a reject/restore left deleted-form \
                 run content in a plain run); refusing to emit (Word repairs the file on open)"
            ),
            details: ErrorDetails {
                context: Some(format!("part={path}")),
                ..ErrorDetails::default()
            },
        });
    }
    Ok(())
}

/// Same check over a part in an input archive (false when the part is absent or
/// unparseable — an unparseable INPUT contributes nothing we can inherit from).
fn input_part_has_deleted_text_outside_del(archive: &DocxArchive, path: &str) -> bool {
    let Some(bytes) = archive.get(path) else {
        return false;
    };
    let Ok(root) = word_xml::parse_document_xml(bytes) else {
        return false;
    };
    part_has_deleted_text_outside_del(&root)
}

/// True if any `w:delText`/`w:delInstrText` in the tree has no `w:del` ancestor.
fn part_has_deleted_text_outside_del(root: &Element) -> bool {
    fn walk(el: &Element, inside_del: bool) -> bool {
        let local = local_element_name(el);
        if (local == "delText" || local == "delInstrText") && !inside_del {
            return true;
        }
        let child_inside_del = inside_del || local == "del";
        el.children.iter().any(|child| {
            if let XMLNode::Element(child_el) = child {
                walk(child_el, child_inside_del)
            } else {
                false
            }
        })
    }
    walk(root, false)
}

/// Ensure the `.rels` file for a story part exists in the output package.
///
/// Story parts like `word/footer3.xml` may reference external targets (e.g.
/// hyperlinks) via relationship IDs (rId1, rId2, etc.). Those IDs are resolved
/// from the part's own `.rels` file at `word/_rels/footer3.xml.rels`. If we
/// serialize the story part content from the target document but don't carry
/// over the `.rels` file, the references become dangling (I-REL-001).
fn ensure_story_part_rels(base_pkg: &mut DocxPackage, target_pkg: &DocxPackage, part_path: &str) {
    let rels_path = story_part_rels_path(part_path);
    // Already have it (either typed or as raw part).
    if base_pkg.story_rels.contains_key(&rels_path) {
        return;
    }
    if let Some(target_rels) = target_pkg.story_rels.get(&rels_path) {
        base_pkg.story_rels.insert(rels_path, target_rels.clone());
    } else {
        tracing::warn!(
            part_path,
            rels_path,
            "story part .rels file missing from both base and target packages"
        );
    }
}

/// Convert a story part path to its `.rels` file path.
/// e.g. `word/footer3.xml` → `word/_rels/footer3.xml.rels`
fn story_part_rels_path(part_path: &str) -> String {
    if let Some(slash_pos) = part_path.rfind('/') {
        let dir = &part_path[..slash_pos];
        let filename = &part_path[slash_pos + 1..];
        format!("{dir}/_rels/{filename}.rels")
    } else {
        format!("_rels/{part_path}.rels")
    }
}

/// Copy header/footer/footnotes/endnotes files from the target archive that
/// are missing from the output archive. The CanonDoc-based serialization
/// handles stories that were diffed/merged, but the target (or base) may
/// reference additional parts (e.g. endnotes.xml with only separator notes,
/// or extra headerN.xml files referenced by sectPr). Copying them ensures
/// the redline DOCX is complete.
fn copy_missing_story_parts(
    base_pkg: &mut DocxPackage,
    target_pkg: &DocxPackage,
    base_rels: &DocumentRelationships,
    target_rels: &DocumentRelationships,
) -> Result<(), RuntimeError> {
    // Collect (rel_type, relationship) tuples from target relationships.
    let mut parts_to_check: Vec<(&str, &Relationship)> = Vec::new();
    for rel in &target_rels.headers {
        parts_to_check.push((HEADER_REL_TYPE, rel));
    }
    for rel in &target_rels.footers {
        parts_to_check.push((FOOTER_REL_TYPE, rel));
    }
    if let Some(rel) = &target_rels.footnotes {
        parts_to_check.push((FOOTNOTES_REL_TYPE, rel));
    }
    if let Some(rel) = &target_rels.endnotes {
        parts_to_check.push((ENDNOTES_REL_TYPE, rel));
    }

    for (rel_type, rel) in parts_to_check {
        let part_path = relationship_target_to_part_path(&rel.target);

        // Already in the output package (either from base or set during serialization).
        if base_pkg.has_part(&part_path) {
            // File exists — just make sure the relationship entry is present.
            if !rels_contain_target(base_rels, rel_type, &rel.target) {
                base_pkg
                    .document_rels
                    .add_with_preferred_id(rel_type, &rel.target, &rel.id);
            }
            continue;
        }

        // Not in the output package — copy from target if available.
        let Some(data) = target_pkg.get_part(&part_path) else {
            continue;
        };
        base_pkg.set_part(&part_path, data.to_vec());

        // Copy the part's .rels file too (e.g. word/_rels/footer3.xml.rels).
        ensure_story_part_rels(base_pkg, target_pkg, &part_path);

        // Ensure the relationship is present.
        base_pkg
            .document_rels
            .add_with_preferred_id(rel_type, &rel.target, &rel.id);

        // Ensure the content type override is present.
        let content_type = content_type_for_story_rel(rel_type)?;
        let ct_part_name = if part_path.starts_with('/') {
            part_path.clone()
        } else {
            format!("/{part_path}")
        };
        base_pkg
            .content_types
            .add_override(&ct_part_name, content_type);
    }

    Ok(())
}

fn sync_document_custom_xml_parts(
    base_pkg: &mut DocxPackage,
    target_pkg: &DocxPackage,
    target_rels: &DocumentRelationships,
) {
    if target_rels.custom_xml.is_empty() {
        return;
    }

    for rel in &target_rels.custom_xml {
        base_pkg
            .document_rels
            .add_with_preferred_id(CUSTOM_XML_REL_TYPE, &rel.target, &rel.id);
    }

    for part_path in target_pkg.part_names() {
        if !part_path.starts_with("customXml/") {
            continue;
        }
        if let Some(data) = target_pkg.get_part(part_path) {
            base_pkg.set_part(part_path, data.to_vec());
        }
    }

    for override_entry in &target_pkg.content_types.overrides {
        if override_entry.part_name.starts_with("/customXml/") {
            base_pkg
                .content_types
                .add_override(&override_entry.part_name, &override_entry.content_type);
        }
    }
}

fn sync_custom_properties_part(base_pkg: &mut DocxPackage, target_pkg: &DocxPackage) {
    let Some(rel) = target_pkg
        .root_rels
        .entries
        .iter()
        .find(|rel| rel.rel_type == CUSTOM_PROPERTIES_REL_TYPE)
    else {
        return;
    };

    let part_path = if let Some(stripped) = rel.target.strip_prefix('/') {
        stripped.to_string()
    } else {
        rel.target.clone()
    };

    let Some(data) = target_pkg.get_part(&part_path) else {
        return;
    };

    base_pkg.set_part(&part_path, data.to_vec());
    base_pkg
        .root_rels
        .add_with_preferred_id(CUSTOM_PROPERTIES_REL_TYPE, &rel.target, &rel.id);

    let override_part = if part_path.starts_with('/') {
        part_path.clone()
    } else {
        format!("/{part_path}")
    };
    if let Some(entry) = target_pkg
        .content_types
        .overrides
        .iter()
        .find(|entry| entry.part_name == override_part)
    {
        base_pkg
            .content_types
            .add_override(&entry.part_name, &entry.content_type);
    }
}

/// Check whether the base relationships already contain a relationship of the
/// given type pointing at the given target.
fn rels_contain_target(rels: &DocumentRelationships, rel_type: &str, target: &str) -> bool {
    if rel_type == HEADER_REL_TYPE {
        rels.headers
            .iter()
            .any(|r| relationship_targets_match(&r.target, target))
    } else if rel_type == FOOTER_REL_TYPE {
        rels.footers
            .iter()
            .any(|r| relationship_targets_match(&r.target, target))
    } else if rel_type == FOOTNOTES_REL_TYPE {
        rels.footnotes
            .as_ref()
            .is_some_and(|r| relationship_targets_match(&r.target, target))
    } else if rel_type == ENDNOTES_REL_TYPE {
        rels.endnotes
            .as_ref()
            .is_some_and(|r| relationship_targets_match(&r.target, target))
    } else {
        false
    }
}

/// Return the OOXML content type for a story relationship type.
fn content_type_for_story_rel(rel_type: &str) -> Result<&'static str, RuntimeError> {
    if rel_type == HEADER_REL_TYPE {
        Ok("application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml")
    } else if rel_type == FOOTER_REL_TYPE {
        Ok("application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml")
    } else if rel_type == FOOTNOTES_REL_TYPE {
        Ok("application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml")
    } else if rel_type == ENDNOTES_REL_TYPE {
        Ok("application/vnd.openxmlformats-officedocument.wordprocessingml.endnotes+xml")
    } else if rel_type == COMMENTS_REL_TYPE {
        Ok("application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml")
    } else {
        Err(RuntimeError {
            code: ErrorCode::InternalError,
            message: format!("unexpected story relationship type: {rel_type}"),
            details: ErrorDetails::default(),
        })
    }
}

fn story_root_tag(root: &Element) -> String {
    match &root.prefix {
        Some(prefix) => format!("{prefix}:{}", root.name),
        None => root.name.clone(),
    }
}

/// Stream story blocks (header/footer parts), honoring `block_sdt_wrap`
/// envelope markers (§17.5.2) — the story twin of the body streaming loop's
/// SDT-envelope logic. Story parts are rebuilt wholesale on serialize, so
/// without this the envelope of an imported story content control (Word's
/// page-number gallery `docPartObj`, repeating-section bindings, …) would be
/// silently unwrapped on the first edit of anything in the document.
///
/// Same invariants as the body loop: a wrap must never open while another is
/// still open (import records only the outermost story envelope), and a span
/// running past the last block is a programmer bug.
fn write_story_blocks_with_sdt_envelopes(
    w: &mut XmlWriter,
    blocks: &[crate::domain::TrackedBlock],
    annotation_id: &mut u32,
    bookmark_policy: &crate::serialize::BookmarkIdPolicy,
) -> Result<(), RuntimeError> {
    let mut sdt_remaining: usize = 0;
    for tracked in blocks {
        // Close a wrap that completed on the previous iteration (mirrors the
        // body loop: decrement for the previously-emitted block, then flush).
        if sdt_remaining > 0 {
            sdt_remaining -= 1;
            if sdt_remaining == 0 {
                w.end_tag("w:sdtContent").map_err(map_xml_write_error)?;
                w.end_tag("w:sdt").map_err(map_xml_write_error)?;
            }
        }
        if let Some(wrap) = &tracked.block_sdt_wrap {
            if sdt_remaining > 0 {
                return Err(RuntimeError {
                    code: ErrorCode::UnsupportedEdit,
                    message: "overlapping block-level content-control wraps in story part"
                        .to_string(),
                    details: ErrorDetails {
                        block_id: Some(block_id_of_tracked(tracked).clone()),
                        context: Some(format!("span={}", wrap.span)),
                        ..ErrorDetails::default()
                    },
                });
            }
            w.start_tag("w:sdt").map_err(map_xml_write_error)?;
            let sdt_pr = crate::word_xml::parse_raw_fragment(&wrap.wrapper.sdt_pr_xml).map_err(
                |source| RuntimeError {
                    code: ErrorCode::InvalidDocx,
                    message: "failed to parse story content-control properties".to_string(),
                    details: ErrorDetails {
                        block_id: Some(block_id_of_tracked(tracked).clone()),
                        context: Some(format!("err={source}")),
                        ..ErrorDetails::default()
                    },
                },
            )?;
            w.write_element(&sdt_pr).map_err(map_xml_write_error)?;
            if let Some(ref end_pr_xml) = wrap.wrapper.sdt_end_pr_xml {
                let sdt_end_pr =
                    crate::word_xml::parse_raw_fragment(end_pr_xml).map_err(|source| {
                        RuntimeError {
                            code: ErrorCode::InvalidDocx,
                            message: "failed to parse story content-control end properties"
                                .to_string(),
                            details: ErrorDetails {
                                block_id: Some(block_id_of_tracked(tracked).clone()),
                                context: Some(format!("err={source}")),
                                ..ErrorDetails::default()
                            },
                        }
                    })?;
                w.write_element(&sdt_end_pr).map_err(map_xml_write_error)?;
            }
            w.start_tag("w:sdtContent").map_err(map_xml_write_error)?;
            sdt_remaining = wrap.span;
        }
        let el = serialize_tracked_block(tracked, annotation_id, bookmark_policy, None)?;
        w.write_element(&el).map_err(map_xml_write_error)?;
    }
    // Flush a wrap that ends on the final block.
    if sdt_remaining > 0 {
        sdt_remaining -= 1;
        assert_eq!(
            sdt_remaining, 0,
            "block_sdt_wrap span ran past the end of the story part"
        );
        w.end_tag("w:sdtContent").map_err(map_xml_write_error)?;
        w.end_tag("w:sdt").map_err(map_xml_write_error)?;
    }
    Ok(())
}

pub(crate) fn load_story_template_root(
    base_pkg: &DocxPackage,
    target_archive: &DocxArchive,
    part_path: &str,
) -> Result<Element, RuntimeError> {
    // An EXISTING but empty (0-byte / whitespace-only) part carries no template:
    // Word emits such a part for an empty running head. Treat it exactly like an
    // absent part and synthesize a fresh root below, so a new story can be
    // written into the slot. A part that has content but no root is malformed and
    // still fails loud in `parse_document_xml` (`NoRootElement`).
    if let Some(xml) = base_pkg.get_part(part_path)
        && !word_xml::is_empty_or_whitespace_xml(xml)
    {
        return word_xml::parse_document_xml(xml).map_err(map_word_xml_error);
    }
    if let Some(xml) = target_archive.get(part_path)
        && !word_xml::is_empty_or_whitespace_xml(xml)
    {
        return word_xml::parse_document_xml(xml).map_err(map_word_xml_error);
    }

    // Synthesized stories (e.g., blank headers per §17.10.2), and existing but
    // empty running-head parts, have no backing XML template. Create a minimal
    // root element so the serializer can write content into it. The element name
    // is inferred from the part path.
    let root_name = if part_path.contains("header") {
        "hdr"
    } else if part_path.contains("footer") {
        "ftr"
    } else {
        return Err(invalid_docx(&format!(
            "missing story template part in base and target: {part_path}"
        )));
    };
    let mut root = Element::new(root_name);
    root.prefix = Some("w".to_string());
    let mut ns = std::collections::BTreeMap::new();
    ns.insert(
        "w".to_string(),
        "http://schemas.openxmlformats.org/wordprocessingml/2006/main".to_string(),
    );
    root.namespaces = Some(xmltree::Namespace(ns));
    Ok(root)
}

impl DocxRuntime for SimpleRuntime {
    fn import_docx(&self, docx_bytes: &[u8]) -> Result<ImportResult, RuntimeError> {
        let start = Instant::now();
        let (snapshot, diagnostics, has_revisions, docx_bytes) =
            build_snapshot_from_bytes(docx_bytes)?;
        let size_kb = docx_bytes.len() / 1024;
        let fingerprint = snapshot.meta.source_fingerprint.clone();
        // Cheap `Arc` clone: the returned `ImportResult` shares the snapshot's
        // IR instead of producing a second full-resident deep copy (Rung 1).
        let canonical = Arc::clone(&snapshot.canonical);

        // Cache only the fingerprint when the imported document has no
        // pre-existing revision markup. In that case `snapshot.canonical` is
        // already the correct `view()` projection, so we can avoid re-parsing
        // without storing a second copy of the canonical tree.
        let cached_view_fingerprint = if has_revisions {
            None
        } else {
            Some(fingerprint.clone())
        };
        let source_bytes: Arc<[u8]> = Arc::from(docx_bytes);
        let handle = self.insert_doc(
            snapshot,
            diagnostics.clone(),
            cached_view_fingerprint,
            Some(Arc::clone(&source_bytes)),
            Arc::clone(&canonical),
            source_bytes,
        );
        if runtime_timing_logs_enabled() {
            eprintln!(
                "TIMING import_docx: {:.3}s size={}KB cached_view={}",
                start.elapsed().as_secs_f64(),
                size_kb,
                !has_revisions,
            );
        }
        Ok(ImportResult {
            doc_handle: handle,
            canonical,
            diagnostics,
            fingerprint,
        })
    }

    fn import_snapshot_blob(
        &self,
        snapshot_bytes: &[u8],
    ) -> Result<SnapshotImportResult, RuntimeError> {
        SimpleRuntime::import_snapshot_blob(self, snapshot_bytes)
    }

    fn import_docx_pair(
        &self,
        base_bytes: &[u8],
        target_bytes: &[u8],
    ) -> Result<(ImportResult, ImportResult), RuntimeError> {
        std::thread::scope(|s| {
            let base_thread = s.spawn(|| self.import_docx(base_bytes));
            let target_thread = s.spawn(|| self.import_docx(target_bytes));
            let base = base_thread.join().expect("base import thread panicked")?;
            let target = target_thread
                .join()
                .expect("target import thread panicked")?;
            Ok((base, target))
        })
    }

    fn view(&self, handle: &DocHandle) -> Result<ViewResult, RuntimeError> {
        let start = Instant::now();

        let entry = self.docs.get(&handle.0).ok_or_else(|| RuntimeError {
            code: ErrorCode::InvalidDocx,
            message: "doc handle not found".to_string(),
            details: ErrorDetails {
                context: Some(handle.0.clone()),
                ..ErrorDetails::default()
            },
        })?;
        entry
            .last_accessed_epoch_secs
            .store(now_epoch_secs(), Ordering::Relaxed);

        // `view()` returns the canonical with all tracked changes accepted.
        // When the imported document had no pre-existing revision markup the
        // snapshot canonical is already that projection; otherwise we accept
        // the tracked segments in-place on a clone of the IR. Either way,
        // block_ids are preserved across the operation — the IR is the
        // source of truth, not a re-parse of serialized bytes.
        let fingerprint = entry.snapshot.meta.current_docx_fingerprint.clone();
        let (canonical, flattened_pending_revisions) = if entry.cached_view_fingerprint.is_some() {
            // No pre-existing revisions: the snapshot canonical already is the
            // accepted projection. Hand out a cheap shared `Arc` clone (Rung 1).
            (Arc::clone(&entry.snapshot.canonical), Vec::new())
        } else {
            // Pre-existing revisions: accept-all must mutate, so take an owned
            // copy of the IR, project it, and re-wrap for the result. Summarize
            // what the projection consumes FIRST (from the un-projected
            // snapshot) — the flatten contract's disclosure.
            let flattened =
                crate::tracked_model::pending_revision_authors(&entry.snapshot.canonical);
            let mut canonical = (*entry.snapshot.canonical).clone();
            crate::tracked_model::accept_all(&mut canonical);
            (Arc::new(canonical), flattened)
        };
        let diagnostics = entry.diagnostics.clone();
        if runtime_timing_logs_enabled() {
            eprintln!("TIMING view: {:.3}s", start.elapsed().as_secs_f64());
        }
        Ok(ViewResult {
            canonical,
            diagnostics,
            fingerprint,
            flattened_pending_revisions,
        })
    }

    fn export_docx(&self, handle: &DocHandle, _mode: ExportMode) -> Result<Vec<u8>, RuntimeError> {
        let bytes = self.get_doc_bytes(handle)?;

        if let Some(validator) = &self.export_validator {
            validator(&bytes).map_err(|msg| RuntimeError {
                code: ErrorCode::ValidationFailed,
                message: format!("export validator rejected output: {msg}"),
                details: ErrorDetails::default(),
            })?;
        }

        Ok(bytes.as_ref().to_vec())
    }

    fn export_snapshot_blob(&self, handle: &DocHandle) -> Result<Vec<u8>, RuntimeError> {
        SimpleRuntime::export_snapshot_blob(self, handle)
    }

    fn validate_docx_bytes(&self, docx_bytes: &[u8]) -> Result<ValidationReport, RuntimeError> {
        validate_docx_report(docx_bytes)
    }

    fn validate_handle(&self, handle: &DocHandle) -> Result<ValidationReport, RuntimeError> {
        let bytes = self.get_doc_bytes(handle)?;
        self.validate_docx_bytes(&bytes)
    }
}

// =============================================================================
// Public facade: pure verb cores over `EditSnapshot`.
//
// These are the session-free, DashMap-free building blocks the `Document`
// handle in `crate::api` composes. Each is a pure function over an
// `EditSnapshot` (or a free function over bytes) that mirrors exactly the
// transformation the corresponding `SimpleRuntime` method performs, minus the
// handle-store bookkeeping. The runtime methods are refactored to call these
// so there is one implementation of each transformation.
// =============================================================================

/// How aggressively the built-in OOXML linker
/// ([`crate::docx_validate::validate_docx`]) gates serialized bytes.
///
/// The default is [`ValidatorLevel::Blocking`]: bytes do not leave the engine
/// unchecked. The linker parses every part exactly once and runs all checks
/// over the shared trees; measured cost (release) is ~6s on an 800KB
/// heavily-tracked contract and milliseconds on typical documents (was ~28s
/// before the parse-once restructure; the remaining cost is the DOM parse
/// itself). Skipping it is an explicit caller decision — see
/// [`ExportOptions::unchecked`] for the one sanctioned reason
/// (engine-internal intermediates).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidatorLevel {
    /// No serialize-time validation. ONLY for bytes that never leave the
    /// engine (intermediate serializations inside a multi-step pipeline) or
    /// bulk pipelines that gate elsewhere. Never the default.
    Off,
    /// Run the linker, refuse on [`BLOCKING_RULES`] findings (the structural
    /// invariants Word rejects the file or loses data over). The default.
    Blocking,
    /// Run the linker and refuse on ANY error-severity finding (strictest;
    /// includes Annex A ordering — for release-gating callers).
    Full,
}

/// Options controlling how a snapshot is lowered to DOCX bytes.
pub struct ExportOptions {
    pub mode: ExportMode,
    /// Built-in OOXML linker gate run on the serialized bytes. See
    /// [`ValidatorLevel`]. Defaults to [`ValidatorLevel::Blocking`].
    pub validator_level: ValidatorLevel,
    /// Optional caller-supplied gate run on the serialized bytes before they
    /// are returned, AFTER the built-in linker gate. `Ok(())` accepts,
    /// `Err(message)` rejects with `ErrorCode::ValidationFailed`.
    pub validator: Option<ExportValidator>,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            mode: ExportMode::Redline,
            // Blocking: by default, bytes leave the engine validated. This is
            // the contract docs/summary.md promises; opting out is explicit.
            validator_level: ValidatorLevel::Blocking,
            validator: None,
        }
    }
}

impl ExportOptions {
    /// Options for engine-internal serializations whose bytes never leave the
    /// engine — e.g. re-zipping an unmodified input scaffold as the base/target
    /// input of a merge, or recovering media parts for a read projection.
    ///
    /// Validating those would re-check the *input* document (already gated at
    /// import or at its own export), so the skip is sound. Any path that
    /// returns bytes to a caller must NOT use this.
    pub fn unchecked() -> Self {
        Self {
            mode: ExportMode::Redline,
            validator_level: ValidatorLevel::Off,
            validator: None,
        }
    }
}

/// Lower an [`EditSnapshot`] to DOCX bytes, running the optional validator gate.
///
/// Pure on `&EditSnapshot`: re-zips the snapshot's package scaffold (the same
/// path `SimpleRuntime::get_doc_bytes` uses for a cold byte cache) and, if a
/// validator is supplied, refuses to return bytes the validator rejects.
pub fn serialize_snapshot(
    snapshot: &EditSnapshot,
    options: &ExportOptions,
) -> Result<Vec<u8>, RuntimeError> {
    // `mode` is currently single-variant (`Redline`); destructure so adding a
    // variant forces a decision here rather than silently ignoring it.
    let ExportMode::Redline = options.mode;
    let bytes = snapshot
        .scaffold
        .package
        .clone()
        .into_archive()
        .map_err(map_package_error)?
        .write()
        .map_err(map_docx_error)?;

    // Built-in OOXML linker gate. Runs in any build (not gated on
    // `debug_assertions`) so release callers that opt in get the structural
    // guarantee. `Off` skips it entirely to keep the hot path fast.
    gate_serialized_bytes(&bytes, options.validator_level)?;

    if let Some(validator) = &options.validator {
        validator(&bytes).map_err(|msg| RuntimeError {
            code: ErrorCode::ValidationFailed,
            message: format!("export validator rejected output: {msg}"),
            details: ErrorDetails::default(),
        })?;
    }
    Ok(bytes)
}

/// Run the built-in OOXML linker over serialized bytes and refuse on findings
/// per [`ValidatorLevel`]. The ONE implementation shared by
/// [`serialize_snapshot`] and the MCP to-disk save path so both gate on exactly
/// the same rules with the same error mapping.
///
/// - [`ValidatorLevel::Off`]: no-op, returns `Ok`.
/// - [`ValidatorLevel::Blocking`]: refuses on [`BLOCKING_RULES`] error findings.
/// - [`ValidatorLevel::Full`]: refuses on ANY error-severity finding.
pub fn gate_serialized_bytes(bytes: &[u8], level: ValidatorLevel) -> Result<(), RuntimeError> {
    match level {
        ValidatorLevel::Off => Ok(()),
        ValidatorLevel::Blocking | ValidatorLevel::Full => {
            let validation = crate::docx_validate::validate_docx(bytes);
            let findings: Vec<String> = validation
                .errors()
                .filter(|f| level == ValidatorLevel::Full || BLOCKING_RULES.contains(&f.rule_id))
                .map(|f| format!("[{}] {}: {}", f.rule_id, f.location, f.message))
                .collect();
            if findings.is_empty() {
                Ok(())
            } else {
                Err(RuntimeError {
                    code: ErrorCode::ValidationFailed,
                    message: format!(
                        "DOCX validation failed with {} error(s):\n{}",
                        findings.len(),
                        findings.join("\n")
                    ),
                    details: ErrorDetails {
                        context: Some(format!("validator_level={level:?}")),
                        ..ErrorDetails::default()
                    },
                })
            }
        }
    }
}

/// The first quarantined block (import-time nested-tracked-changes
/// quarantine) in a document's body, if any. Quarantine is body-level by
/// construction — stories containing the shape refuse at import — so scanning
/// `doc.blocks` is complete. Used by the operations whose OUTPUT would
/// misrepresent the quarantine's content (compare, audit); resolution does
/// NOT refuse on it — the placeholder is carried through un-resolved and
/// stays census-visible.
pub(crate) fn first_quarantined_block(doc: &CanonDoc) -> Option<&crate::domain::NodeId> {
    doc.blocks.iter().find_map(|tb| match &tb.block {
        BlockNode::OpaqueBlock(o)
            if matches!(o.kind, crate::domain::OpaqueKind::QuarantinedNestedTracking) =>
        {
            Some(&o.id)
        }
        _ => None,
    })
}

/// Scan every opaque inline (body + all stories) for a `raw_xml` fragment that
/// the accept/reject descent cannot parse YET carries a revision marker. Such a
/// fragment would have its revisions silently left unresolved by the projection
/// (the exact silent-fallback M0.1 exists to kill), so `project` refuses on it —
/// mirroring the quarantine guard's "revisions invisible to this operation"
/// precedent. Returns the offending opaque's id.
pub(crate) fn first_unparseable_opaque_with_revisions(
    doc: &CanonDoc,
) -> Option<crate::domain::NodeId> {
    fn scan_inline(inline: &InlineNode) -> Option<crate::domain::NodeId> {
        let InlineNode::OpaqueInline(o) = inline else {
            return None;
        };
        // Hyperlink has a typed projection path (no raw_xml descent); the
        // quarantine kind is handled by its own guard. Every other kind goes
        // through the fragment resolver.
        if matches!(o.kind, crate::domain::OpaqueKind::Hyperlink(_)) {
            return None;
        }
        let raw = o.raw_xml.as_deref()?;
        // keep_inserted is irrelevant to the parse outcome; pick one.
        match crate::normalize::resolve_opaque_fragment_revisions(raw, true) {
            crate::normalize::FragmentResolution::UnparseableWithRevisions => Some(o.id.clone()),
            _ => None,
        }
    }
    fn scan_block(block: &BlockNode) -> Option<crate::domain::NodeId> {
        match block {
            BlockNode::Paragraph(p) => p
                .segments
                .iter()
                .flat_map(|s| s.inlines.iter())
                .find_map(scan_inline),
            BlockNode::Table(t) => t
                .rows
                .iter()
                .flat_map(|r| r.cells.iter())
                .flat_map(|c| c.blocks.iter())
                .find_map(scan_block),
            BlockNode::OpaqueBlock(_) => None,
        }
    }
    fn scan_story_blocks(blocks: &[crate::domain::TrackedBlock]) -> Option<crate::domain::NodeId> {
        blocks.iter().find_map(|tb| scan_block(&tb.block))
    }
    if let Some(id) = doc.blocks.iter().find_map(|tb| scan_block(&tb.block)) {
        return Some(id);
    }
    for s in &doc.headers {
        if let Some(id) = scan_story_blocks(&s.blocks) {
            return Some(id);
        }
    }
    for s in &doc.footers {
        if let Some(id) = scan_story_blocks(&s.blocks) {
            return Some(id);
        }
    }
    for s in &doc.footnotes {
        if let Some(id) = scan_story_blocks(&s.blocks) {
            return Some(id);
        }
    }
    for s in &doc.endnotes {
        if let Some(id) = scan_story_blocks(&s.blocks) {
            return Some(id);
        }
    }
    for s in &doc.comments {
        if let Some(id) = scan_story_blocks(&s.blocks) {
            return Some(id);
        }
    }
    None
}

/// THE COMPARE CONTRACT (flatten): compare diffs the ACCEPTED READINGS of
/// its inputs. `view()` runs accept-all before the diff, so pending
/// revisions in base or target — plain Inserted/Deleted and the stacked
/// state alike — are projected to their accepted image (the stacked state's
/// is "dropped", origin rule 3), and the output redline re-attributes every
/// change to the compare's own author. This matches Word's own Compare,
/// which compares as-if-accepted when inputs carry revisions. The flattening
/// is DISCLOSED, not silent: the compare results carry
/// [`FlattenedPendingRevisions`] naming what was consumed, per input, by
/// author. Carrying pending revisions through with their original
/// attribution is a different operation (a rebase of negotiation state onto
/// a new base), not a variant of compare.
///
/// The one refusal: quarantined blocks — nested tracked changes in an
/// unsupported shape, preserved byte-faithfully as opaque placeholders with
/// no readable content. Accept-all cannot reach inside them, so their
/// placeholders would identity-compare and the diff would silently miss
/// whatever the quarantine holds. That is not honestly comparable; refuse.
fn refuse_quarantined_compare(base: &CanonDoc, target: &CanonDoc) -> Result<(), RuntimeError> {
    for (label, doc) in [("base", base), ("target", target)] {
        if let Some(block_id) = first_quarantined_block(doc) {
            return Err(RuntimeError {
                code: ErrorCode::UnsupportedEdit,
                message: format!(
                    "compare refused: {label} document block '{block_id}' is quarantined \
                     (nested tracked changes in an unsupported shape), so its content \
                     cannot be honestly compared"
                ),
                details: ErrorDetails::default(),
            });
        }
    }
    Ok(())
}

/// How to resolve the tracked deltas in a document.
pub enum Resolution {
    /// Accept every tracked change (the accept-all projection).
    AcceptAll,
    /// Reject every tracked change (the reject-all projection).
    RejectAll,
    /// Accept or reject a specific set of revision ids, leaving the rest.
    Selective {
        ids: std::collections::HashSet<u32>,
        action: ResolveSelectionAction,
    },
}

/// Map an [`crate::edit::EditError`] to a [`RuntimeError`], surfacing the
/// structured `stale_edit` / `opaque_preservation` details so callers can act
/// on validation failures without string-parsing the human message.
///
/// This is the single mapping shared by `EditSnapshot::apply`,
/// `SimpleRuntime::apply_edit`, and any other consumer of `apply_transaction`.
pub fn map_edit_error(e: crate::edit::EditError) -> RuntimeError {
    use crate::edit::EditError;
    let code = match &e {
        EditError::BlockNotFound { .. }
        | EditError::StoryNotFound { .. }
        | EditError::StoryBlockNotFound { .. }
        | EditError::SectionPropertiesNotFound { .. }
        | EditError::HeaderFooterRefNotResolvable { .. }
        | EditError::BookmarkNotFound { .. } => ErrorCode::AnchorNotFound,
        EditError::StyleNotFound { .. } => ErrorCode::AnchorNotFound,
        EditError::DrawingNotFound { .. } => ErrorCode::AnchorNotFound,
        EditError::ContentControlNotFound { .. } => ErrorCode::AnchorNotFound,
        EditError::OpaqueTextTargetNotFound { .. } => ErrorCode::AnchorNotFound,
        EditError::OpaqueTextRegionNotFound { .. } => ErrorCode::AnchorNotFound,
        EditError::SdtFillBlockNotFound { .. } => ErrorCode::AnchorNotFound,
        EditError::FormFieldNotFound { .. } => ErrorCode::AnchorNotFound,
        EditError::BlockRangeInvalid { .. } => ErrorCode::AnchorNotFound,
        EditError::CommentTargetNotFound { .. }
        | EditError::CommentAnchorNotFound { .. }
        | EditError::CommentAnchorOverlapsDeleted { .. } => ErrorCode::AnchorNotFound,
        EditError::CommentRangeOrphaned { .. }
        | EditError::CommentParentUnanchored { .. }
        | EditError::CommentEmptyBody { .. }
        | EditError::CommentOnTrackedBlock { .. } => ErrorCode::UnsupportedEdit,
        EditError::NoteNotFound { .. } | EditError::NoteReferenceMissing { .. } => {
            ErrorCode::AnchorNotFound
        }
        EditError::NoteAnchorNotAParagraph { .. }
        | EditError::NoteEmptyBody { .. }
        | EditError::NoteIdNotNumeric { .. }
        | EditError::NoteBodyMultiParagraph { .. } => ErrorCode::UnsupportedEdit,
        EditError::ExpectMismatch { .. }
        | EditError::BlockSemanticHashMismatch { .. }
        | EditError::SpanHandleStale { .. }
        | EditError::SpanTextMismatch { .. } => ErrorCode::StaleEdit,
        EditError::AnchorNotFound { .. } => ErrorCode::AnchorNotFound,
        EditError::SpanCrossesTrackedSegment { .. }
        | EditError::SpanSplitsBracketPair { .. }
        | EditError::SpanStyledContentUnsupported { .. } => ErrorCode::UnsupportedEdit,
        EditError::BlockHasTrackedStatus { .. }
        | EditError::ParagraphContainsTrackedSegments { .. }
        | EditError::InvalidColorValue { .. }
        | EditError::InvalidFontSize { .. }
        | EditError::NoParagraphFormattingRequested { .. }
        | EditError::NoCellFormattingRequested { .. }
        | EditError::NoRowFormattingRequested { .. }
        | EditError::TableRowNotEditable { .. }
        | EditError::NoTableFormattingRequested { .. }
        | EditError::TableAlreadyHasFormattingChange { .. }
        | EditError::NoPageSetupRequested { .. }
        | EditError::SectionAlreadyHasTrackedChange { .. }
        | EditError::NoHeaderFooterModeRequested { .. }
        | EditError::HeaderFooterAlreadyExists { .. }
        | EditError::CrossRefEmptyBookmark { .. }
        | EditError::NoNumberingChangeRequested { .. }
        | EditError::NumberingLevelOnUnnumbered { .. }
        | EditError::NumberingLevelOutOfBounds { .. }
        | EditError::NumberingManualPrefixUnsupported { .. }
        | EditError::NumberingSplitOnUnnumbered { .. }
        | EditError::NoStyleChangeRequested { .. }
        | EditError::RaggedTableGrid { .. }
        | EditError::OrphanVMergeContinue { .. }
        | EditError::TableRowIndexOutOfRange { .. }
        | EditError::TableColumnIndexOutOfRange { .. }
        | EditError::TableColumnOpOnMergedGrid { .. }
        | EditError::MergeRegionNotRectangular { .. }
        | EditError::TableWouldBeEmpty { .. }
        | EditError::TableInsertRowCellCountExceedsColumns { .. }
        | EditError::TableCellNotEditable { .. }
        | EditError::TableMidRedline { .. }
        | EditError::TableSpecFormattingRequiresDirect { .. }
        | EditError::FindReplaceBarrierStraddle { .. } => ErrorCode::UnsupportedEdit,
        EditError::BookmarkDuplicateName { .. }
        | EditError::BookmarkOrphanEnd { .. }
        | EditError::BookmarkEmptyName { .. }
        | EditError::BookmarkRawXmlUnparsable => ErrorCode::UnsupportedEdit,
        EditError::NotADrawing { .. }
        | EditError::DrawingMissingRawXml { .. }
        | EditError::DrawingRawXmlParse { .. }
        | EditError::ImageAttributeTargetAbsent { .. }
        | EditError::NoImageAttributeRequested { .. }
        | EditError::ImageLayoutRequiresAnchor { .. }
        | EditError::ImageLayoutTargetAbsent { .. }
        | EditError::NoImageLayoutRequested { .. }
        | EditError::EquationXmlInvalid { .. }
        | EditError::EquationNotMath { .. }
        | EditError::EmptyContentControlSpec { .. }
        | EditError::MalformedDataBinding { .. }
        | EditError::ContentControlBlockUnsupported { .. }
        | EditError::BlockAlreadyWrapped { .. }
        | EditError::NotAContentControl { .. }
        | EditError::ContentControlMissingRawXml { .. }
        | EditError::ContentControlRawXmlParse { .. }
        | EditError::ContentControlTypeMismatch { .. }
        | EditError::ImageBytesEmpty { .. }
        | EditError::UnsupportedImageFormat { .. }
        | EditError::ImageAspectMismatch { .. }
        | EditError::ImageHeaderUndecodable { .. }
        | EditError::NotAFormField { .. }
        | EditError::FormFieldMissingRawXml { .. }
        | EditError::FormFieldRawXmlParse { .. }
        | EditError::FormFieldTypeMismatch { .. }
        | EditError::FormFieldValueNotInList { .. }
        | EditError::MalformedFfData { .. }
        | EditError::FormFieldResultHasTrackedChanges { .. }
        | EditError::TrackedContentControlSetUnsupported { .. }
        | EditError::TextboxHasTrackedChanges { .. }
        | EditError::MultipleDistinctTextboxes { .. }
        | EditError::OpaqueTextMissingRawXml { .. }
        | EditError::OpaqueTextRawXmlParse { .. }
        | EditError::OpaqueTextNotFound { .. }
        | EditError::OpaqueTextRegionHasTrackedChanges { .. }
        | EditError::OpaqueTextUnsupportedShape { .. }
        | EditError::SdtFillAmbiguousTarget { .. }
        | EditError::SdtFillEmpty { .. }
        | EditError::SdtFillComplexContent { .. }
        | EditError::SdtFillBlockHashUnsupported { .. }
        | EditError::SdtFillDuplicateBlockTarget { .. }
        | EditError::OpaqueTextMirrorDivergence { .. }
        | EditError::StyleDefEmptyId { .. }
        | EditError::StyleDefEmptyName { .. }
        | EditError::StyleDefIdMismatch { .. }
        | EditError::DocDefaultsEmpty { .. } => ErrorCode::UnsupportedEdit,
        EditError::OpaqueDestroyed { .. } => ErrorCode::OpaqueDestroyed,
        EditError::NoOpEdit { .. } => ErrorCode::NoOpEdit,
        EditError::PrefixDuplicatesLabel { .. } => ErrorCode::PrefixDuplicatesLabel,
        EditError::AmbiguousAnchorAfterMove { .. } => ErrorCode::AmbiguousAnchorAfterMove,

        // ── Remaining "the addressed thing does not exist" variants ──────────
        // Same not-found class as the *NotFound family above.
        EditError::HyperlinkNotFound { .. } | EditError::ParagraphRoleNotFound { .. } => {
            ErrorCode::AnchorNotFound
        }

        // ── Precondition (expect-*) mismatches against current state ─────────
        // Same stale-edit class as ExpectMismatch / BlockSemanticHashMismatch:
        // the caller's asserted current value no longer matches the document.
        EditError::HyperlinkAttrMismatch { .. } => ErrorCode::StaleEdit,

        // ── Structural / shape / unsupported-construct refusals ──────────────
        // The edit addressed real content but the requested shape cannot be
        // materialized honestly (wrong block kind, malformed replacement
        // content, no-op, out-of-scope construct). Closest existing public
        // class is UnsupportedEdit.
        EditError::NotAParagraph { .. }
        | EditError::PreservedInlineNotFound { .. }
        | EditError::DuplicatePreservedInlineRef { .. }
        | EditError::PreservedInlineOrderChanged { .. }
        | EditError::NotAPreservedInline { .. }
        | EditError::UnsupportedParagraphStructure { .. }
        | EditError::UnsupportedParagraphRole { .. }
        | EditError::UnsupportedInlineMarkup { .. }
        | EditError::UnsupportedNumberingRestart { .. }
        | EditError::MoveDestinationInsideSource { .. }
        | EditError::NotAHyperlink { .. }
        | EditError::HyperlinkContainsTrackedChanges { .. }
        | EditError::HyperlinkSetAttrNoOp { .. }
        | EditError::NotATable { .. }
        | EditError::EmptyTableStructure { .. }
        | EditError::EmptyRowContent { .. }
        | EditError::EmptyCellContent { .. }
        | EditError::TableHasFormattingNotInSpec { .. }
        | EditError::NoFormattingRequested { .. }
        | EditError::InsertListNumIdUnknown { .. }
        | EditError::BlocksToTableNonParagraph { .. }
        | EditError::BlocksToTableOpaqueInline { .. }
        | EditError::BlocksToTableSplitMismatch { .. }
        | EditError::BlocksToTableEmptySpec { .. } => ErrorCode::UnsupportedEdit,
    };
    let details = match &e {
        EditError::ExpectMismatch {
            block_id,
            expected,
            actual_text,
            step_index,
        } => ErrorDetails {
            block_id: Some(block_id.clone()),
            step_index: Some(*step_index),
            context: None,
            stale_edit: Some(Box::new(StaleEditDetails::ExpectMismatch {
                target_block_id: block_id.clone(),
                expected: expected.clone(),
                actual_text: actual_text.clone(),
            })),
            opaque_preservation: None,
            ambiguous_anchor: None,
        },
        EditError::BlockSemanticHashMismatch {
            block_id,
            expected,
            actual,
            step_index,
        } => ErrorDetails {
            block_id: Some(block_id.clone()),
            step_index: Some(*step_index),
            context: None,
            stale_edit: Some(Box::new(StaleEditDetails::SemanticHashMismatch {
                target_block_id: block_id.clone(),
                expected: expected.clone(),
                actual: actual.clone(),
            })),
            opaque_preservation: None,
            ambiguous_anchor: None,
        },
        EditError::OpaqueDestroyed {
            step_index,
            target_block_id,
            missing_opaque_ids,
            missing_inline_kinds,
            original_text_preview,
        } => ErrorDetails {
            block_id: Some(target_block_id.clone()),
            step_index: Some(*step_index),
            context: None,
            stale_edit: None,
            opaque_preservation: Some(Box::new(OpaquePreservationDetails {
                target_block_id: target_block_id.clone(),
                missing_opaque_ids: missing_opaque_ids.clone(),
                missing_inline_kinds: missing_inline_kinds
                    .iter()
                    .map(|k| (*k).to_string())
                    .collect(),
                original_text_preview: original_text_preview.clone(),
            })),
            ambiguous_anchor: None,
        },
        EditError::NoOpEdit {
            block_id,
            step_index,
            reason,
        } => ErrorDetails {
            block_id: Some(block_id.clone()),
            step_index: Some(*step_index),
            context: Some((*reason).to_string()),
            stale_edit: None,
            opaque_preservation: None,
            ambiguous_anchor: None,
        },
        EditError::PrefixDuplicatesLabel {
            block_id,
            step_index,
            label,
            paragraph_label,
            current_text,
        } => ErrorDetails {
            block_id: Some(block_id.clone()),
            step_index: Some(*step_index),
            context: Some(format!(
                "content label {label:?} vs paragraph label {paragraph_label:?}; \
                 paragraph already reads {current_text:?}"
            )),
            stale_edit: None,
            opaque_preservation: None,
            ambiguous_anchor: None,
        },
        EditError::AmbiguousAnchorAfterMove {
            anchor_id,
            moved_by_step_index,
            moved_to_block_id,
            step_index,
        } => ErrorDetails {
            block_id: Some(anchor_id.clone()),
            step_index: Some(*step_index),
            context: None,
            stale_edit: None,
            opaque_preservation: None,
            ambiguous_anchor: Some(Box::new(AmbiguousAnchorDetails {
                anchor_id: anchor_id.clone(),
                moved_by_step_index: *moved_by_step_index,
                moved_to_block_id: moved_to_block_id.clone(),
            })),
        },
        _ => ErrorDetails::default(),
    };
    RuntimeError {
        code,
        message: format!("{e}"),
        details,
    }
}

/// Build the new [`EditSnapshot`] that results from a single-document
/// transformation: the new canonical IR plus serialized bytes, with the
/// scaffold and meta carried over from `prev` per the rebuild pattern.
///
/// `prev` supplies the body template (re-used for serialization), the schema
/// version, and the source fingerprint. `document_version` is bumped and
/// `current_docx_fingerprint` is set to the fingerprint of `serialized_bytes`.
/// The package scaffold is rebuilt by parsing `serialized_bytes` back into a
/// `DocxPackage` so the snapshot's scaffold reflects the new state.
/// Map a [`crate::docprops::DocPropsError`] to a [`RuntimeError`]. An unknown
/// field name is a caller error (`InvalidRange`); a malformed part is a
/// document-integrity error (`InvalidDocx`).
fn map_update_fields_error(
    e: crate::edit::verbs::update_fields::UpdateFieldsError,
) -> RuntimeError {
    // Both variants are a malformed/unserializable settings part — an invalid
    // package, fail loud with the part context the error already carries.
    RuntimeError {
        code: ErrorCode::InvalidDocx,
        message: e.to_string(),
        details: ErrorDetails::default(),
    }
}

fn map_docprops_error(e: crate::docprops::DocPropsError) -> RuntimeError {
    use crate::docprops::DocPropsError;
    let code = match &e {
        DocPropsError::UnknownCoreField { .. } => ErrorCode::InvalidRange,
        DocPropsError::MalformedXml { .. }
        | DocPropsError::WriteFailed { .. }
        | DocPropsError::MalformedCustomProperty { .. } => ErrorCode::InvalidDocx,
    };
    RuntimeError {
        code,
        message: e.to_string(),
        details: ErrorDetails::default(),
    }
}

/// Rebuild a snapshot after a package-level metadata mutation. Unlike
/// [`rebuild_snapshot`], the canonical IR and body template are unchanged — only
/// a `docProps/*` part was rewritten. We re-serialize the mutated package and
/// reparse it so the scaffold is internally consistent and the fingerprint
/// reflects the new bytes.
fn rebuild_metadata_snapshot(
    prev: &EditSnapshot,
    mutated: EditSnapshot,
) -> Result<EditSnapshot, RuntimeError> {
    // Internal intermediate: these bytes are re-parsed into the new scaffold,
    // not returned to a caller; emission is gated at `serialize`/`save`.
    let bytes = serialize_snapshot(&mutated, &ExportOptions::unchecked())?;
    rebuild_snapshot(prev, Arc::unwrap_or_clone(mutated.canonical), &bytes)
}

fn rebuild_snapshot(
    prev: &EditSnapshot,
    mut new_canonical: CanonDoc,
    serialized_bytes: &[u8],
) -> Result<EditSnapshot, RuntimeError> {
    let fp = fingerprint(serialized_bytes);
    new_canonical.meta.docx_fingerprint = fp.clone();
    let archive = DocxArchive::read(serialized_bytes).map_err(map_docx_error)?;
    let package = DocxPackage::from_archive(&archive).map_err(map_package_error)?;
    Ok(EditSnapshot {
        canonical: Arc::new(new_canonical),
        scaffold: PackageScaffold {
            package,
            body_template: prev.scaffold.body_template.clone(),
        },
        meta: SnapshotMeta {
            snapshot_schema_version: prev.meta.snapshot_schema_version,
            document_version: prev.meta.document_version + 1,
            source_fingerprint: prev.meta.source_fingerprint.clone(),
            current_docx_fingerprint: fp,
            origin_authors: prev.meta.origin_authors.clone(),
        },
    })
}

/// Word's built-in (latent) paragraph / character / table / numbering style
/// IDs. These are valid `w:pStyle` / `w:rStyle` / `w:tblStyle` targets even when
/// the package carries no explicit `w:style` definition for them: Word ships
/// them as latent styles and materializes a definition on demand. A
/// `word/styles.xml`-free package (e.g. a freshly synthesized minimal doc) can
/// still legitimately reference these.
///
/// Source: the built-in style table enumerated in the ECMA-376 / MS-OI29500
/// style documentation and Word's default `styles.xml` (§17.7.4 latent styles).
/// This is intentionally an explicit allowlist (not a "best-effort accept
/// anything that looks built-in"): an id NOT in this set and NOT defined in the
/// package's style table is a dangling reference and is refused.
const BUILTIN_STYLE_IDS: &[&str] = &[
    // Default / structural styles.
    "Normal",
    "DefaultParagraphFont",
    "NoList",
    "TableNormal",
    "NoSpacing",
    // Headings 1–9 (§17.7.4 built-in heading styles).
    "Heading1",
    "Heading2",
    "Heading3",
    "Heading4",
    "Heading5",
    "Heading6",
    "Heading7",
    "Heading8",
    "Heading9",
    // Title block.
    "Title",
    "Subtitle",
    // Body / quote styles.
    "Quote",
    "IntenseQuote",
    "ListParagraph",
    "Caption",
    "BodyText",
    "BodyText2",
    "BodyText3",
    "BodyTextIndent",
    "BodyTextIndent2",
    "BodyTextIndent3",
    "PlainText",
    // TOC / index / reference styles.
    "TOC1",
    "TOC2",
    "TOC3",
    "TOC4",
    "TOC5",
    "TOC6",
    "TOC7",
    "TOC8",
    "TOC9",
    "TOCHeading",
    "Index1",
    "Index2",
    "Index3",
    "IndexHeading",
    "TableofFigures",
    "TableofAuthorities",
    "Bibliography",
    // Header / footer / page-furniture styles.
    "Header",
    "Footer",
    "PageNumber",
    "FootnoteText",
    "FootnoteReference",
    "EndnoteText",
    "EndnoteReference",
    "CommentText",
    "CommentReference",
    "CommentSubject",
    "Hyperlink",
    "FollowedHyperlink",
    "BalloonText",
    // List styles.
    "ListBullet",
    "ListBullet2",
    "ListBullet3",
    "ListNumber",
    "ListNumber2",
    "ListNumber3",
    "List",
    "List2",
    "List3",
    "ListContinue",
    "ListContinue2",
    "ListContinue3",
    // Emphasis character styles.
    "Strong",
    "Emphasis",
    "IntenseEmphasis",
    "SubtleEmphasis",
    "SubtleReference",
    "IntenseReference",
    "BookTitle",
    // Built-in table styles.
    "TableGrid",
    "TableGridLight",
];

/// Collect every `w:styleId` defined in the package's `word/styles.xml`, if any.
///
/// Mirrors the cross-reference validator's notion of "defined style ID"
/// (`docx_validate_xref::collect_defined_style_ids`): a style is defined iff a
/// `<w:style w:styleId="…">` element carries it. An absent or unparsable
/// `word/styles.xml` yields the empty set (no styles defined) rather than a
/// silent pass — the built-in allowlist is the only remaining source of valid
/// ids in that case.
/// Parse `word/styles.xml` (plus theme fonts) from an already-decoded package
/// into a resolvable [`crate::styles::StyleDefinitions`], or `None` when there is
/// no styles part. The single style-table loader shared by the runtime
/// projection ([`EditSnapshot::style_definitions`]) and the public
/// [`style_table_from_docx`], so both resolve against the same table import used.
pub(crate) fn style_definitions_from_package(
    package: &DocxPackage,
) -> Result<Option<crate::styles::StyleDefinitions>, RuntimeError> {
    let invalid = |context: &str, message: String| RuntimeError {
        code: ErrorCode::InvalidDocx,
        message,
        details: ErrorDetails {
            context: Some(context.to_string()),
            ..Default::default()
        },
    };
    let Some(bytes) = package.get_part("word/styles.xml") else {
        return Ok(None);
    };
    let mut style_defs = crate::styles::StyleDefinitions::parse(bytes)
        .map_err(|m| invalid("style_definitions_from_package styles.xml", m))?;
    if let Some(theme_bytes) = package.get_part("word/theme/theme1.xml") {
        let theme_fonts = crate::styles::ThemeFonts::parse(theme_bytes)
            .map_err(|m| invalid("style_definitions_from_package theme1.xml", m))?;
        style_defs.set_theme_fonts(theme_fonts);
    }
    Ok(Some(style_defs))
}

/// Parse a DOCX's style table into an opaque [`crate::styles::StyleTable`], or
/// `None` when the document has no `word/styles.xml`.
///
/// This is the seam for re-resolving style-inherited run marks OUTSIDE the
/// runtime projection: a caller holding a bare [`CanonDoc`] (e.g. from
/// [`crate::build_canonical_from_docx_preserving_tracked`] or
/// `SimpleRuntime::import_docx`) parses the same bytes here and passes the result
/// to [`crate::reject_all_with_styles`] /
/// [`crate::resolve_selected_revisions_with_styles`] so a rejected
/// paragraph-style change re-resolves correctly (see those functions and the
/// note on the style-table-free [`crate::reject_all`]).
pub fn style_table_from_docx(
    bytes: &[u8],
) -> Result<Option<crate::styles::StyleTable>, RuntimeError> {
    let archive = DocxArchive::read(bytes).map_err(|source| RuntimeError {
        code: ErrorCode::InvalidDocx,
        message: format!("style_table_from_docx: not a valid DOCX archive: {source:?}"),
        details: ErrorDetails::default(),
    })?;
    let package = DocxPackage::from_archive(&archive).map_err(|source| RuntimeError {
        code: ErrorCode::InvalidDocx,
        message: format!("style_table_from_docx: package decode failed: {source}"),
        details: ErrorDetails::default(),
    })?;
    Ok(style_definitions_from_package(&package)?.map(crate::styles::StyleTable))
}

fn defined_style_ids(package: &DocxPackage) -> HashSet<String> {
    let Some(bytes) = package.get_part("word/styles.xml") else {
        return HashSet::new();
    };
    let Ok(root) = crate::word_xml::parse_document_xml(bytes) else {
        return HashSet::new();
    };
    let mut ids = HashSet::new();
    for child in &root.children {
        let XMLNode::Element(el) = child else {
            continue;
        };
        // Top-level `<w:style w:styleId="…">` elements. `parse_document_xml`
        // keeps the local name in `el.name` with the prefix in `el.prefix`.
        let is_style = el.name == "style" || el.name == "w:style";
        if is_style && let Some(id) = crate::xml_attrs::attr_get(el, "w:styleId") {
            ids.insert(id.clone());
        }
    }
    ids
}

/// Resolve whether a paragraph-style id is a valid `w:pStyle` target for this
/// package: either explicitly defined in `word/styles.xml` or a recognized Word
/// built-in (latent) style. An id that is neither is a dangling reference.
fn style_id_is_known(style_id: &str, defined: &HashSet<String>) -> bool {
    defined.contains(style_id) || BUILTIN_STYLE_IDS.contains(&style_id)
}

impl EditSnapshot {
    /// Discover fillable body-level (block) content controls from the serialize
    /// scaffold — their bytes live there, not on the IR node, so they are
    /// invisible to the `CanonDoc`-level `opaque_targets::opaque_text_targets`.
    /// Each carries the `body_index` `sdt_text_fill` addresses (RFC-0002
    /// §Phase-2). Sorted by `body_index` for a stable order.
    pub fn block_content_control_targets(&self) -> Vec<crate::opaque_targets::BlockSdtTextTarget> {
        let mut out: Vec<_> = self
            .scaffold
            .body_template
            .opaque_children
            .iter()
            .filter_map(|(idx, node)| crate::opaque_targets::block_sdt_target(*idx, node))
            .collect();
        out.sort_by_key(|t| t.body_index);
        out
    }

    /// Author new tracked deltas by applying an [`crate::edit::EditTransaction`].
    ///
    /// Pure twin of [`SimpleRuntime::apply_edit`]: precondition-checked and
    /// atomic. Returns a new snapshot; mutates nothing.
    pub fn apply(&self, txn: &crate::edit::EditTransaction) -> Result<EditSnapshot, RuntimeError> {
        // Package-aware style-existence check (verbs/styles.rs §"style existence"
        // + EditError::StyleNotFound). `apply_transaction` operates on a
        // `&CanonDoc`, which carries no style table, so it CANNOT verify a
        // pStyle target exists. This caller holds the `DocxPackage` (incl.
        // `word/styles.xml`), so it is the layer responsible for refusing a
        // dangling style — authoring a pStyle into a style table that does not
        // define it (and is not a Word built-in) is exactly the "no silent
        // fallback" the contract forbids.
        let has_apply_style = txn
            .steps
            .iter()
            .any(|s| matches!(s, crate::edit::EditStep::ApplyStyle { .. }));
        if has_apply_style {
            // The set of ids that resolve to a real style for THIS transaction:
            // styles defined in the package's `word/styles.xml`, Word built-ins,
            // PLUS any style this transaction authors (`CreateStyle` /
            // `ModifyStyle` stage a `w:style` into styles.xml in the same apply).
            // A `CreateStyle` + `ApplyStyle` pair in one transaction is a valid,
            // self-resolving reference (see tests/style_defs.rs); the pre-existing
            // styles.xml does not yet contain the authored id, so it must be
            // counted here too.
            let mut defined = defined_style_ids(&self.scaffold.package);
            for step in &txn.steps {
                match step {
                    crate::edit::EditStep::CreateStyle { def, .. }
                    | crate::edit::EditStep::ModifyStyle { def, .. } => {
                        defined.insert(def.style_id.clone());
                    }
                    _ => {}
                }
            }
            for (step_index, step) in txn.steps.iter().enumerate() {
                if let crate::edit::EditStep::ApplyStyle {
                    block_id, style_id, ..
                } = step
                    && !style_id_is_known(style_id, &defined)
                {
                    return Err(map_edit_error(crate::edit::EditError::StyleNotFound {
                        block_id: block_id.clone(),
                        style_id: style_id.clone(),
                        step_index,
                    }));
                }
            }
        }

        // A transaction that inserts a `toc` block produces a field with no
        // cached result (`resolve_toc_spec` synthesizes it fresh — see
        // `edit/mod.rs`); Word only computes TOC entries on open, via the
        // `w:updateFields` package setting. Detected before `apply_transaction`
        // so the flag can be forced on as part of THIS SAME commit once the
        // transaction is known to have succeeded — a ToC insert without it is
        // silently blank until the user manually updates fields in Word.
        let inserts_toc = txn.steps.iter().any(|s| {
            matches!(
                s,
                crate::edit::EditStep::InsertParagraphs { blocks, .. }
                    | crate::edit::EditStep::ReplaceBlockRange { blocks, .. }
                    if blocks.iter().any(|b| matches!(b, crate::edit::BlockSpec::Toc(_)))
            )
        });

        let (mut edited, pending) = crate::edit::apply_transaction_with_id_floor(
            &self.canonical,
            txn,
            // Block-opaque interiors keep their bytes in the serialize
            // scaffold, invisible to the pure core's id scan — pass their
            // max id so a minted id (e.g. a staged block fill's w:del/w:ins
            // pair) can never collide with a pre-existing interior id.
            max_wid_in_opaque_children(&self.scaffold.body_template.opaque_children),
        )
        .map_err(map_edit_error)?;
        // Single-document edit: base and target are the same archive. This
        // re-zips the UNMODIFIED input scaffold as merge input — an internal
        // intermediate, not output (output is gated at `serialize`/`save`).
        let bytes = serialize_snapshot(self, &ExportOptions::unchecked())?;
        let redline_bytes = serialize_canonical_docx(
            &bytes,
            &bytes,
            &mut edited,
            Some(self.scaffold.body_template.clone()),
            &pending,
        )?;
        let mut next = rebuild_snapshot(self, edited, &redline_bytes)?;

        // Staged block-SDT fills mutated the serialize-path CLONE of the body
        // template (their bytes live in the scaffold, not the IR), so the
        // serialized package above carries them — but `rebuild_snapshot`
        // carries the PRE-FILL template forward. Re-apply the same fills to
        // the next snapshot's template so cache == package: without this, the
        // next apply re-streams the stale pre-fill node (silently reverting
        // the fill) and `block_content_control_targets` reads pre-fill text.
        // Deterministic re-application: same source node, same pre-minted ids.
        if !pending.opaque_child_text_sets.is_empty() {
            apply_opaque_child_text_sets(
                &mut next.scaffold.body_template.opaque_children,
                &pending.opaque_child_text_sets,
            )?;
        }

        if inserts_toc {
            // Documented product default (see `edit/verbs/update_fields.rs`):
            // force `w:updateFields` ON as part of this same commit, via the
            // same mutate-then-refingerprint helper `set_update_fields_on_open`
            // already uses (`rebuild_metadata_snapshot`) — NOT a second,
            // separate `apply` — so this stays one logical transaction (one
            // `document_version` bump) with a package whose fingerprint
            // matches its actual bytes.
            let mut with_settings = next;
            crate::edit::verbs::update_fields::set_update_fields_on_open(
                &mut with_settings.scaffold.package,
                Some(true),
            )
            .map_err(map_update_fields_error)?;
            return rebuild_metadata_snapshot(self, with_settings);
        }

        Ok(next)
    }

    /// Refuse an authored write whose `author` impersonates one of this
    /// document's ORIGIN authors (`SnapshotMeta::origin_authors` — the
    /// authors already present in the redline this snapshot was built from,
    /// frozen before this handle's own session authored anything). Editing
    /// under an existing author's identity makes an agent's changes
    /// indistinguishable from theirs and silently defeats layered review —
    /// a security property in a negotiation, not a nicety (an adversarial
    /// agent could impersonate a counterparty reviewer to HIDE its edits
    /// inside their redline). This is a refusal, not a documented default: a
    /// default an agent can drift off is the invisible-ink pattern again; a
    /// refusal cannot be drifted off.
    ///
    /// `allow_existing_author=true` deliberately continues an existing
    /// author's own work. `author = None` (an anonymized write) is never
    /// impersonation, since it adopts no identity.
    pub fn guard_author(
        &self,
        author: Option<&str>,
        allow_existing_author: bool,
    ) -> Result<(), RuntimeError> {
        if allow_existing_author {
            return Ok(());
        }
        let Some(author) = author else {
            return Ok(());
        };
        if self.meta.origin_authors.contains(author) {
            let existing = self
                .meta
                .origin_authors
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            return Err(RuntimeError {
                code: ErrorCode::AuthorImpersonation,
                message: format!(
                    "author {author:?} already authors revisions in this document (the \
                     existing redline's authors: {existing}). Editing under an existing \
                     author's identity makes your changes indistinguishable from theirs and \
                     defeats layered review. Choose a distinct author name, or pass \
                     allow_existing_author=true to deliberately continue that author's work."
                ),
                details: ErrorDetails {
                    context: Some(author.to_string()),
                    ..ErrorDetails::default()
                },
            });
        }
        Ok(())
    }

    /// [`EditSnapshot::apply`], but enforcing [`EditSnapshot::guard_author`]
    /// first against `txn.revision.author`. This is the entry point a
    /// transport should use for a write it attributes to a caller-supplied
    /// author (MCP's `apply_edit`/`replace_text`/…, the HTTP `/apply`
    /// endpoint). Bare `apply` stays guard-free: it is also the pure verb
    /// entry point hundreds of engine-internal tests drive directly against
    /// synthetic transactions that carry no document-origin concept to check
    /// against.
    pub fn apply_authored(
        &self,
        txn: &crate::edit::EditTransaction,
        allow_existing_author: bool,
    ) -> Result<EditSnapshot, RuntimeError> {
        self.guard_author(txn.revision.author.as_deref(), allow_existing_author)?;
        self.apply(txn)
    }

    /// `apply`'s dry-run twin: report exactly the preconditions `apply` would
    /// reject, mutating nothing observable.
    ///
    /// This runs the **full** [`EditSnapshot::apply`] on `self` (which is pure —
    /// it borrows the IR and produces a fresh snapshot, mutating nothing) and
    /// discards the produced snapshot. Running the real `apply` is deliberate:
    /// `apply` enforces more than the pure verb core
    /// ([`crate::edit::apply_transaction`]) does — it also enforces the
    /// package-aware ApplyStyle style-existence gate and the PendingParts
    /// validations (media digest / empty bytes, style / numbering / custom-xml
    /// ops). Those PendingParts checks are interleaved with mutation in the
    /// serialize path, so there is no separable pure "validate phase" to call in
    /// isolation. Reusing `apply` wholesale is therefore the only way `check`
    /// answers the real question — "would this apply?" — without re-implementing
    /// (and drifting from) apply's validation. The produced snapshot is
    /// discarded, so it stays a dry run.
    pub fn check(&self, txn: &crate::edit::EditTransaction) -> Result<(), RuntimeError> {
        self.apply(txn).map(|_| ())
    }

    /// Set a package-level **core** document property (`docProps/core.xml`,
    /// §15.2.12.1) — e.g. `title`, `creator`/`author`, `subject`, `keywords`,
    /// `lastModifiedBy`.
    ///
    /// This is an **untracked, package-level** mutation, NOT an edit
    /// transaction: core properties live in their own OPC part, carry no
    /// `w:ins`/`w:del` tracked-change markup, and are not replayable as an
    /// [`crate::edit::EditTransaction`] (that grammar operates on `CanonDoc`
    /// body content, which has no `DocxPackage` handle). It returns a new
    /// snapshot with the part rewritten; the body (`word/document.xml`) is
    /// untouched. An unknown field name is rejected (no silent default).
    pub fn set_core_property(
        &self,
        field: &str,
        value: &str,
    ) -> Result<EditSnapshot, RuntimeError> {
        let mut next = self.clone();
        crate::edit::verbs::metadata::set_core_property(&mut next.scaffold.package, field, value)
            .map_err(map_docprops_error)?;
        rebuild_metadata_snapshot(self, next)
    }

    /// Set a package-level **custom** document property (`docProps/custom.xml`,
    /// §15.2.12.2) to a string value. Same untracked, non-replayable,
    /// package-level contract as [`EditSnapshot::set_core_property`].
    pub fn set_custom_property(
        &self,
        name: &str,
        value: &str,
    ) -> Result<EditSnapshot, RuntimeError> {
        let mut next = self.clone();
        crate::edit::verbs::metadata::set_custom_property(&mut next.scaffold.package, name, value)
            .map_err(map_docprops_error)?;
        rebuild_metadata_snapshot(self, next)
    }

    /// Read a package-level core document property, `None` if absent. An
    /// unknown field name is rejected.
    pub fn core_property(&self, field: &str) -> Result<Option<String>, RuntimeError> {
        crate::edit::verbs::metadata::get_core_property(&self.scaffold.package, field)
            .map_err(map_docprops_error)
    }

    /// Read a package-level custom document property by name, `None` if absent.
    pub fn custom_property(&self, name: &str) -> Result<Option<String>, RuntimeError> {
        crate::edit::verbs::metadata::get_custom_property(&self.scaffold.package, name)
            .map_err(map_docprops_error)
    }

    /// Set the package-level **update-fields-on-open** setting (`w:updateFields`
    /// in `word/settings.xml`, §17.15.1.81): when `Some(true)`, Microsoft Word
    /// recomputes every field result (REF/PAGEREF/TOC/SEQ/…) the next time the
    /// document is opened.
    ///
    /// Like [`EditSnapshot::set_core_property`], this is an **untracked,
    /// package-level** mutation, NOT an edit transaction: it lives in its own
    /// settings part, carries no `w:ins`/`w:del` markup, and is not replayable
    /// as an [`crate::edit::EditTransaction`]. It does **not** recompute field
    /// results in-engine (that needs a layout pass the IR cannot perform — a
    /// guessed result would be a silent fallback); it sets the flag that asks
    /// Word to refresh. The body (`word/document.xml`) is untouched. If the
    /// package has no settings part yet, a minimal valid one is synthesized.
    pub fn set_update_fields_on_open(
        &self,
        desired: Option<bool>,
    ) -> Result<EditSnapshot, RuntimeError> {
        let mut next = self.clone();
        crate::edit::verbs::update_fields::set_update_fields_on_open(
            &mut next.scaffold.package,
            desired,
        )
        .map_err(map_update_fields_error)?;
        rebuild_metadata_snapshot(self, next)
    }

    /// Read the package-level `w:updateFields` "refresh on open" setting,
    /// `None` if the document never asserted it. Same untracked, package-level
    /// contract as [`EditSnapshot::set_update_fields_on_open`].
    pub fn update_fields_on_open(&self) -> Result<Option<bool>, RuntimeError> {
        crate::edit::verbs::update_fields::get_update_fields_on_open(&self.scaffold.package)
            .map_err(map_update_fields_error)
    }

    /// Read a faithful, UN-resolved projection of `word/styles.xml`: the
    /// document-default run properties plus every `w:style` exactly as authored
    /// (no basedOn-chain resolution, no doc-default fold). This is the read half
    /// of the global re-skin workflow — an agent inspects which style literally
    /// sets a font (vs inherits it) before editing the style table.
    ///
    /// An ABSENT styles part is the empty table (`StyleTableProjection::default()`),
    /// not an error — a minimal document legitimately carries no styles.xml. A
    /// PRESENT-but-malformed part, or a `w:style` with no `w:styleId`, fails loud
    /// (CLAUDE.md "no silent fallbacks").
    pub fn style_table(&self) -> Result<StyleTableProjection, RuntimeError> {
        match self.scaffold.package.get_part("word/styles.xml") {
            None => Ok(StyleTableProjection::default()),
            Some(bytes) => {
                crate::styles::style_table_projection(bytes).map_err(|message| RuntimeError {
                    code: ErrorCode::InvalidDocx,
                    message,
                    details: ErrorDetails {
                        context: Some("EditSnapshot::style_table".to_string()),
                        ..Default::default()
                    },
                })
            }
        }
    }

    /// Parse `word/styles.xml` (plus theme fonts) into resolvable
    /// [`crate::styles::StyleDefinitions`], or `None` when the package has no
    /// styles part. Mirrors the import-time load (`import_docx`) so accept/reject
    /// re-resolution of style-inherited run marks uses the SAME style table the
    /// canonical was originally resolved against.
    fn style_definitions(&self) -> Result<Option<crate::styles::StyleDefinitions>, RuntimeError> {
        style_definitions_from_package(&self.scaffold.package)
    }

    /// This snapshot's own style table (from the scaffold's `word/styles.xml`),
    /// as the opaque [`StyleTable`] handle the audit / accept-reject re-resolution
    /// takes. `None` when the package has no styles part. This is the baseline
    /// document's styles for a freshly-parsed snapshot (the scaffold retains the
    /// import-time non-body parts) — the table a rejected paragraph-style change
    /// must re-resolve against.
    pub(crate) fn scaffold_style_table(
        &self,
    ) -> Result<Option<crate::styles::StyleTable>, RuntimeError> {
        Ok(self.style_definitions()?.map(crate::styles::StyleTable))
    }

    /// Resolve tracked deltas: accept-all, reject-all, or a selective set.
    ///
    /// Pure twin of [`SimpleRuntime::resolve_tracked_revisions`] (and the
    /// accept-all / reject-all projections). Returns a new snapshot.
    pub fn project(&self, resolution: Resolution) -> Result<EditSnapshot, RuntimeError> {
        // Projection mutates the IR (accept/reject), so it needs an owned copy.
        let mut resolved = (*self.canonical).clone();
        // Whether an imported body-level `w:sectPrChange` (§17.13.5.32) must be
        // resolved in the verbatim `sectPr` cache the serializer emits.
        // `accept_all`/`reject_all` clear `body_section_property_change` and
        // update the typed `body_section_properties`, but the raw `sect_pr_nodes`
        // cache still carries the imported `sectPrChange` and the un-resolved
        // props. The serializer takes its verbatim branch once the change is
        // `None`, so we must mirror the byte-path resolution onto that raw XML.
        // `Some(keep_new)`: accept (true) drops the record + keeps current props;
        // reject (false) restores the recorded previous props + drops the record.
        // `None`: no body-section change to resolve, or (Selective only) one
        // exists but its id was not among the ones selected this call — left
        // pending, exactly like an unselected pPrChange.
        let mut body_section_resolution: Option<bool> = None;
        // Preflight (M0.1 fail-loud): the accept/reject descent resolves
        // revisions inside opaque `raw_xml` by reparsing the fragment. If a
        // fragment cannot be parsed yet its bytes carry a revision marker, the
        // descent would silently leave those revisions unresolved — the exact
        // silent-fallback this projection exists to kill. Refuse before mutating
        // anything, mirroring the quarantine guard's "revisions invisible to this
        // operation" precedent. Only the full-resolution paths descend into
        // opaques; Selective does not touch inner-opaque revisions, so it keeps
        // its own (quarantine) guard below and is not affected here.
        if matches!(resolution, Resolution::AcceptAll | Resolution::RejectAll)
            && let Some(opaque_id) = first_unparseable_opaque_with_revisions(&resolved)
        {
            return Err(RuntimeError {
                code: ErrorCode::UnsupportedEdit,
                message: format!(
                    "accept/reject refused: opaque '{opaque_id}' carries tracked changes inside \
                     raw_xml that could not be parsed to resolve them; resolving would silently \
                     leave pending revisions in the document"
                ),
                details: ErrorDetails::default(),
            });
        }
        match resolution {
            Resolution::AcceptAll => {
                crate::tracked_model::accept_all(&mut resolved);
                if self.canonical.body_section_property_change.is_some() {
                    body_section_resolution = Some(true);
                }
            }
            Resolution::RejectAll => {
                // Rejecting a tracked paragraph-style change restores the prior
                // style; the runs' style-inherited marks must be re-resolved
                // against it, which needs the document's style table.
                let style_defs = self.style_definitions()?;
                crate::tracked_model::reject_all_with_style_defs(
                    &mut resolved,
                    style_defs.as_ref(),
                );
                if self.canonical.body_section_property_change.is_some() {
                    body_section_resolution = Some(false);
                }
            }
            Resolution::Selective { ids, action } => {
                if ids.is_empty() {
                    return Err(RuntimeError {
                        code: ErrorCode::InvalidRange,
                        message: "selective resolution requires at least one revision id"
                            .to_string(),
                        details: ErrorDetails::default(),
                    });
                }
                // A quarantined block is a DISCIPLINED ISOLATION BOUNDARY,
                // not a reason to refuse unrelated work. Selective
                // resolution is provably disjoint from the quarantine:
                //   1. selectable ids come only from the enumerate census,
                //      which reports a quarantined interior as census-only
                //      id 0 ("not individually resolvable") — never
                //      selectable by construction;
                //   2. the completeness check below refuses any id that
                //      does not match a visible carrier, so an id aimed at
                //      quarantined content refuses loud (InvalidRange), not
                //      silently no-ops;
                //   3. the resolver has no descent into
                //      `QuarantinedNestedTracking`, and block-opaque bytes
                //      live in the serialize scaffold, not the model — the
                //      projection cannot physically touch them.
                // The placeholder is carried through UN-resolved — the same
                // disclosed line the all-or-nothing projections already
                // take — and the census keeps reporting it, so the
                // document's remaining contested state stays observable.
                // Completeness (the domain rule this selector exists to
                // enforce): every id the caller selected must match a
                // carrier `resolve_selected_revisions` actually resolves —
                // a stale, mistyped, or unhandled-carrier id is a refusal,
                // not a silent no-op success. On success the call reports
                // whether the body-level sectPrChange was among the resolved
                // ids, which the verbatim-`sectPr`-cache resolution below
                // must mirror.
                // A selectively rejected paragraph-style change needs the style
                // table to re-resolve style-inherited run marks, same as RejectAll.
                let style_defs = self.style_definitions()?;
                match crate::tracked_model::resolve_selected_revisions_with_style_defs(
                    &mut resolved,
                    &ids,
                    action,
                    style_defs.as_ref(),
                ) {
                    Ok(result) => body_section_resolution = result,
                    Err(unresolved) => {
                        return Err(RuntimeError {
                            code: ErrorCode::InvalidRange,
                            message: format!(
                                "selective resolution refused: revision id(s) {unresolved:?} do \
                                 not match any tracked change in the document (stale, mistyped, \
                                 or an unsupported carrier)"
                            ),
                            details: ErrorDetails {
                                context: Some(
                                    unresolved
                                        .iter()
                                        .map(u32::to_string)
                                        .collect::<Vec<_>>()
                                        .join(","),
                                ),
                                ..ErrorDetails::default()
                            },
                        });
                    }
                }
            }
        }
        // H2: the resolution-scoped body-state validator after the projection
        // producer, covering ALL Resolution variants (AcceptAll / RejectAll /
        // Selective). The resolution projections are exactly where the recurring
        // "another producer violates the invariant" meta-bug kept surfacing, and
        // they are the ONLY producer held to the final-mark rule (they must never
        // strand a tracked final pilcrow — see assert_resolution_body_invariants).
        crate::tracked_model::debug_assert_resolution_body_invariants(&resolved, "project");

        // Clone the body template the serializer consumes, then resolve an
        // imported body `sectPrChange` in its verbatim `sectPr` cache so the
        // serialized body section reflects the accept/reject decision — using
        // the SAME byte-path `*PrChange` logic (`normalize::resolve_pr_change_on_element`)
        // that `reject_all_docx` / `normalize_docx` apply. Lossless: it edits the
        // raw element tree, so unmodeled section props (cols, docGrid,
        // header/footer refs, …) survive untouched.
        let mut body_template = self.scaffold.body_template.clone();
        if let Some(keep_new) = body_section_resolution {
            for node in &mut body_template.sect_pr_nodes {
                if let XMLNode::Element(el) = node
                    && is_w_tag(el, "sectPr")
                {
                    crate::normalize::resolve_pr_change_on_element(el, keep_new);
                }
            }
        }

        // Re-zip of the unmodified input scaffold as merge input — internal
        // intermediate, not output (output is gated at `serialize`/`save`).
        let bytes = serialize_snapshot(self, &ExportOptions::unchecked())?;
        let redline_bytes = serialize_canonical_docx(
            &bytes,
            &bytes,
            &mut resolved,
            Some(body_template),
            &crate::edit::PendingParts::default(),
        )?;
        rebuild_snapshot(self, resolved, &redline_bytes)
    }

    /// Discover the deltas between this snapshot and `other` and materialize
    /// them as tracked changes.
    ///
    /// Pure twin of [`SimpleRuntime::diff_and_redline`]: diff -> merge ->
    /// serialize -> rebuild. The revision id namespace is advanced past this
    /// snapshot's existing tracked changes; author/date are left to the
    /// engine default (the discovery side does not carry a transaction meta).
    pub fn diff(&self, other: &EditSnapshot) -> Result<EditSnapshot, RuntimeError> {
        self.diff_with_author(other, None)
    }

    /// Attributed twin of [`Self::diff`]: identical discovery, but every
    /// produced revision is stamped with `author`.
    ///
    /// An empty author is refused (`ErrorCode::ValidationFailed`) rather than
    /// silently attributing the revisions to no one — anonymous discovery is
    /// exactly what [`Self::diff`] is for.
    pub fn diff_as(
        &self,
        other: &EditSnapshot,
        author: &str,
    ) -> Result<EditSnapshot, RuntimeError> {
        if author.is_empty() {
            return Err(RuntimeError {
                code: ErrorCode::ValidationFailed,
                message: "diff_as requires a non-empty author; use diff for anonymous discovery"
                    .to_string(),
                details: ErrorDetails::default(),
            });
        }
        self.diff_with_author(other, Some(author.to_string()))
    }

    /// Shared implementation behind [`Self::diff`] (anonymous) and
    /// [`Self::diff_as`] (attributed): diff -> merge -> serialize -> rebuild,
    /// stamping every produced revision with `author` (`None` = anonymous).
    fn diff_with_author(
        &self,
        other: &EditSnapshot,
        author: Option<String>,
    ) -> Result<EditSnapshot, RuntimeError> {
        let diff = diff_documents(&self.canonical, &other.canonical).map_err(map_diff_error)?;
        let next_revision_id = max_revision_id(&self.canonical) + 1;
        // Discovery does not carry an authoring transaction. Anonymous `diff`
        // leaves `author` None (the engine's own attribution); `diff_as`
        // threads a caller-supplied author through. `date` is left None in both
        // cases — the runtime's `diff_and_redline` (which takes a full
        // `TransactionMeta`) is the path that stamps a timestamp.
        let revision = RevisionInfo {
            revision_id: next_revision_id,
            author,
            date: None,
            apply_op_id: None,
        };
        let merge_result = merge_diff(&self.canonical, &other.canonical, &diff, &revision)
            .map_err(map_merge_error)?;
        let mut merged = merge_result.doc;
        // Re-zips of the two unmodified input scaffolds as merge inputs —
        // internal intermediates, not output (output is gated at `serialize`).
        let self_bytes = serialize_snapshot(self, &ExportOptions::unchecked())?;
        let other_bytes = serialize_snapshot(other, &ExportOptions::unchecked())?;
        let redline_bytes = serialize_canonical_docx(
            &self_bytes,
            &other_bytes,
            &mut merged,
            Some(self.scaffold.body_template.clone()),
            &crate::edit::PendingParts::default(),
        )?;
        rebuild_snapshot(self, merged, &redline_bytes)
    }
}

/// Build an [`EditSnapshot`] from DOCX bytes without touching any handle store.
///
/// This is the snapshot-construction portion factored out of
/// `SimpleRuntime::import_docx`. It returns the snapshot plus the
/// `(diagnostics, has_revisions, anchored_bytes)` the runtime needs for its
/// caches; `crate::api::Document::parse` ignores those and keeps the snapshot.
fn build_snapshot_from_bytes(
    docx_bytes: &[u8],
) -> Result<(EditSnapshot, Vec<Diagnostic>, bool, Vec<u8>), RuntimeError> {
    let (anchored_bytes, canonical, diagnostics, has_revisions, body_template) =
        import_and_anchor(docx_bytes)?;
    let fp = fingerprint(&anchored_bytes);
    let mut package =
        DocxPackage::from_archive(&DocxArchive::read(&anchored_bytes).map_err(map_docx_error)?)
            .map_err(map_package_error)?;
    // Guarantee every WordprocessingML part the scaffold carries has its
    // canonical content-type Override (OPC §10.1.2 / ECMA-376 Part 1 §15.2).
    // `import_and_anchor` already applied this same correction to the anchored
    // bytes (so the cached export path and the canonical fingerprint agree);
    // re-applying it on the scaffold package is idempotent and keeps the
    // scaffold honest if the anchored-bytes shape ever diverges.
    package.ensure_canonical_wml_content_types();
    // Capture the ORIGIN authors now, before any edit exists: the authors
    // already present in the redline this document carried on arrival. Use
    // `pending_revision_authors` (the flatten-contract summary — segments,
    // paragraph marks, rows/cells, hyperlink runs, every story, PLUS
    // formatting-change records) rather than a narrower carrier walk, so the
    // guard's off-limits set matches what an accept-all would actually
    // attribute. Anonymized revisions (author == None) are not an identity
    // anyone can impersonate, so they are excluded.
    let origin_authors = crate::tracked_model::pending_revision_authors(&canonical)
        .into_iter()
        .filter_map(|a| a.author)
        .collect();
    let snapshot = EditSnapshot {
        canonical: Arc::new(canonical),
        scaffold: PackageScaffold {
            package,
            body_template,
        },
        meta: SnapshotMeta {
            snapshot_schema_version: EDIT_SNAPSHOT_SCHEMA_VERSION,
            document_version: 1,
            source_fingerprint: fp.clone(),
            current_docx_fingerprint: fp,
            origin_authors,
        },
    };
    Ok((snapshot, diagnostics, has_revisions, anchored_bytes))
}

/// Build an [`EditSnapshot`] from DOCX bytes, discarding the runtime-only
/// caches. This is the session-free `parse` core used by
/// [`crate::api::Document::parse`].
pub fn snapshot_from_docx_bytes(docx_bytes: &[u8]) -> Result<EditSnapshot, RuntimeError> {
    let (snapshot, _diagnostics, _has_revisions, _anchored_bytes) =
        build_snapshot_from_bytes(docx_bytes)?;
    Ok(snapshot)
}

/// Build the v1 read projection for a single snapshot.
///
/// Mirrors [`SimpleRuntime::single_document_view`] but operates on an owned
/// [`EditSnapshot`] with no handle store: re-zip the scaffold, read the
/// archive's media, and project the canonical IR into the tracked
/// single-document view.
pub fn build_tracked_document_view_from_snapshot(snapshot: &EditSnapshot) -> FullDocViewResult {
    // Re-zip the scaffold to recover the media parts the view needs. If the
    // package cannot be re-zipped or the image lookup fails, fall back to an
    // empty image lookup: the view is a read projection, not an authoritative
    // artifact, and the IR is the source of structure.
    let image_lookup = serialize_snapshot(snapshot, &ExportOptions::unchecked())
        .ok()
        .and_then(|bytes| DocxArchive::read(&bytes).ok())
        .and_then(|archive| build_image_data_lookup(&archive).ok())
        .unwrap_or_default();
    build_tracked_document_view(&snapshot.canonical, &image_lookup)
}

/// Validate DOCX bytes as a property of the bytes (no handle / session).
///
/// Shares the package-level checks used by
/// [`SimpleRuntime::validate_docx_bytes`]; exposed free so
/// [`crate::api::validate`] can call it without a runtime.
pub fn validate_docx_report(docx_bytes: &[u8]) -> Result<ValidationReport, RuntimeError> {
    let mut issues = Vec::new();
    let archive = match DocxArchive::read(docx_bytes) {
        Ok(a) => a,
        Err(err) => {
            issues.push(ValidationIssue {
                code: ValidationIssueCode::PackageInvariant,
                message: map_docx_error(err).message,
                context: None,
            });
            return Ok(ValidationReport { ok: false, issues });
        }
    };
    if is_encrypted_docx(&archive) {
        issues.push(ValidationIssue {
            code: ValidationIssueCode::PackageInvariant,
            message: "password-protected DOCX files are not supported".to_string(),
            context: None,
        });
    }
    // Locate the main document part via the OPC officeDocument relationship
    // (ECMA-376 Part 2 §9.3): its name is not fixed at word/document.xml. A
    // package with no discoverable main part is itself a package invariant
    // failure (reported here with the specific reason), and the structural
    // checks below are skipped since there is no main story to inspect.
    let main_part = match crate::docx_package::resolve_main_document_part(&archive) {
        Ok(name) => Some(name),
        Err(err) => {
            issues.push(ValidationIssue {
                code: ValidationIssueCode::PackageInvariant,
                message: err.to_string(),
                context: None,
            });
            None
        }
    };
    // Curated structural checks that mirror genuine Word rejections. The rich
    // content-model validator is not on the production path, so run this targeted
    // subset on the main document story here:
    //   I-MATH-001/002 — m:oMath nested in oMath (Word repairs) / oMath outside a
    //                     paragraph (Word cannot open);
    //   I-PERM-001     — non-integer permStart/permEnd w:id;
    //   I-RANGE-001    — lone colFirst/colLast on bookmarkStart/permStart.
    if let Some(main_part) = &main_part
        && let Some(doc_xml) = archive.get(main_part)
    {
        match xmltree::Element::parse(doc_xml) {
            Ok(root) => {
                use crate::docx_validate_annotations::{
                    check_colfirst_collast_pairing, check_custom_xml_range_pairing,
                    check_footnote_endnote_id_range, check_omath_placement, check_perm_id_validity,
                };
                let story = [(main_part.clone(), &root)];
                let mut structural = check_omath_placement(&story);
                structural.extend(check_perm_id_validity(&story));
                structural.extend(check_colfirst_collast_pairing(&story));
                // I-ANN-009 — torn customXml*Range pairs (start without end, or
                // vice versa; §17.13.5.4-.11). The transparent-wrapper model
                // (task #6) carries these as paired Decoration markers, so an
                // edit could tear a pair; Word treats a torn range as
                // non-conformant.
                //
                // NOTE: this `story` is the BODY part only, so I-ANN-009 catches
                // body-local torn pairs; a pair that legitimately spans
                // body↔header/footer would not be checkable here. No corpus doc
                // exercises a cross-story customXml range (4079/0).
                //
                // FOLLOW-UP (pre-existing, out of task #6 scope): the bookmark
                // (I-ANN-003 check_bookmark_pairing) and comment (I-ANN-005
                // check_comment_marker_pairing) pairing checks are likewise OFF
                // this production path — torn bookmark/comment pairs are NOT
                // flagged via api::validate today. Wiring them in mirrors the
                // call below, BUT first needs the cross-story question resolved
                // (a body↔header bookmark must not false-positive on a body-only
                // check), so it is intentionally NOT a drop-in two-liner. Filed.
                structural.extend(check_custom_xml_range_pairing(&story));
                // I-ANN-006 — footnote/endnote REFERENCE ids past Word's 32767 ceiling
                // (MS-OI29500 §2.1.300-302). References live in the body story; the note
                // definitions in footnotes.xml/endnotes.xml are covered by the rich
                // validator only (off the production path).
                structural.extend(check_footnote_endnote_id_range(&story));
                for finding in structural {
                    issues.push(ValidationIssue {
                        code: ValidationIssueCode::WordprocessingInvariant,
                        message: finding.message,
                        context: Some(finding.location),
                    });
                }
                // I-REL-001 — dangling r:id/r:embed/r:link references on the
                // main story (a:blip r:embed/r:link, w:hyperlink r:id,
                // headerReference r:id, a:hlinkClick/OLE r:id, …). The rich
                // PackageState validator runs this, but it is off the production
                // path; share its exact logic via `check_story_rel_references`
                // so api::validate flags explicit references that resolve to no
                // Relationship Id in word/_rels/document.xml.rels. Word repairs
                // such a file and drops the referenced content.
                // ECMA-376 Part 2 OPC §6.5.3; ISO 29500-1 §9.2.
                let rels_path = crate::docx_package::rels_part_path(main_part);
                let rel_findings = crate::docx_validate::check_story_rel_references(
                    main_part,
                    &root,
                    archive.get(&rels_path),
                );
                for finding in rel_findings {
                    issues.push(ValidationIssue {
                        code: ValidationIssueCode::WordprocessingInvariant,
                        message: finding.message,
                        context: Some(finding.location),
                    });
                }
                // I-REL-004 — a w:headerReference/w:footerReference (CT_HdrFtrRef
                // extends CT_Rel) whose required r:id is ABSENT or EMPTY is
                // non-conformant: the header/footer relationship cannot be
                // resolved. Confirmed against real Word:
                // absent r:id → cannot open; empty r:id="" → opens with repair.
                // Shared with the rich validator. ISO 29500-1 §17.10.5;
                // ECMA-376 Annex A CT_HdrFtrRef/CT_Rel.
                for finding in crate::docx_validate::check_story_hdrftr_ref_rid(main_part, &root) {
                    issues.push(ValidationIssue {
                        code: ValidationIssueCode::WordprocessingInvariant,
                        message: finding.message,
                        context: Some(finding.location),
                    });
                }
            }
            Err(e) => {
                // A main story that is not well-formed XML is itself a hard
                // failure — never "ok" because the checks could not run.
                issues.push(ValidationIssue {
                    code: ValidationIssueCode::PackageInvariant,
                    message: format!("main document part {main_part} is not well-formed XML: {e}"),
                    context: Some(main_part.clone()),
                });
            }
        }
    }
    // Model-level body-state invariants (hardening H2). Build the CanonDoc the
    // producers operate on and run the unified validator, surfacing each
    // violation as a WordprocessingInvariant issue. This is the RELEASE-available
    // explicit form of the debug-assert that wraps the transforming producers,
    // and it is the coverage for the import boundary: a violation here may be the
    // INPUT's own non-conformance (a wild document's tracked final pilcrow, a
    // cell-less row), which is REPORTED honestly rather than panicked. A build
    // failure is already reported by the structural checks above, so an
    // un-buildable package simply skips this section.
    if let Ok((canonical, _diagnostics)) =
        crate::import::build_canonical_from_docx_preserving_tracked(
            docx_bytes,
            fingerprint(docx_bytes),
        )
        && let Err(violations) = crate::tracked_model::assert_body_invariants(&canonical)
    {
        for violation in violations {
            issues.push(ValidationIssue {
                code: ValidationIssueCode::WordprocessingInvariant,
                message: format!(
                    "body-state invariant [{}]: {}",
                    violation.invariant.name(),
                    violation.detail
                ),
                context: violation.block_id,
            });
        }
    }
    Ok(ValidationReport {
        ok: issues.is_empty(),
        issues,
    })
}

/// Import and anchor a DOCX, returning `(bytes, canonical, diagnostics, has_revisions, cached_body)`.
/// `has_revisions` is true when the archive contains pre-existing revision markup,
/// meaning the canonical was built without normalization and differs from what
/// `build_canonical_from_docx` (the `view()` path) would produce.
/// `cached_body` contains pre-extracted body nodes that can be consumed by
/// `serialize_canonical_docx` to avoid a redundant xmltree re-parse.
#[allow(clippy::type_complexity)]
fn import_and_anchor(
    docx_bytes: &[u8],
) -> Result<(Vec<u8>, CanonDoc, Vec<Diagnostic>, bool, BodyTemplate), RuntimeError> {
    let mut archive = DocxArchive::read(docx_bytes).map_err(map_docx_error)?;
    ensure_docx_not_encrypted(&archive)?;
    // Locate the main document part via the OPC officeDocument relationship
    // (ECMA-376 Part 2 §9.3): its name is not fixed at word/document.xml.
    let main_part = crate::docx_package::resolve_main_document_part(&archive)
        .map_err(crate::import::map_package_error)?;
    let main_dir = crate::docx_package::part_dir(&main_part).to_string();
    let document_xml = archive
        .get(&main_part)
        .ok_or_else(|| invalid_docx(&format!("main document part {main_part} is missing")))?;
    let mut root = word_xml::parse_document_xml(document_xml).map_err(map_word_xml_error)?;

    // Guarantee every recognized WordprocessingML part carries its canonical
    // content-type Override before we anchor (OPC §10.1.2 / ECMA-376 Part 1
    // §15.2). Some inbound packages content-type a WML part — including
    // `word/document.xml` itself (e.g. OpenXmlPowerTools HtmlConverter Test-08)
    // — only via the generic `Default Extension="xml"`; Word locates parts by
    // content type and drops such a part on repair. The anchored bytes are the
    // source of truth for the cold export/validate path AND the basis for the
    // canonical fingerprint, so the correction must land here, before the
    // re-zip, to keep the bytes, the fingerprint, and the scaffold package all
    // consistent. `add_override` never rewrites an existing author choice.
    if let Some(ct_bytes) = archive.get(crate::docx_package::CONTENT_TYPES_PATH) {
        let mut content_types =
            crate::docx_package::ContentTypes::parse(ct_bytes).map_err(map_package_error)?;
        let part_paths: Vec<String> = archive.list().map(str::to_string).collect();
        let before = content_types.overrides.len();
        content_types.ensure_canonical_wml_for_parts(part_paths.iter().map(String::as_str));
        // The main document part's content type is fixed (ECMA-376 Part 1 §15.2)
        // regardless of its non-fixed name; a non-conventional name (e.g.
        // word/document2.xml) is skipped by the filename-keyed sweep above, so
        // ensure its canonical main-part Override here for the RESOLVED name.
        let main_override = format!("/{main_part}");
        if !content_types.has_override(&main_override) {
            content_types.add_override(
                &main_override,
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml",
            );
        }
        if content_types.overrides.len() != before {
            let serialized = content_types.serialize().map_err(map_package_error)?;
            archive
                .set(crate::docx_package::CONTENT_TYPES_PATH, serialized)
                .map_err(map_docx_error)?;
        }
    }

    let updated_bytes = archive.write().map_err(map_docx_error)?;
    let fingerprint = fingerprint(&updated_bytes);

    // Check for pre-existing revision markup BEFORE building the canonical.
    // When revision markup is present, `build_canonical_from_docx` (the view
    // path) normalises it away first, so the import-time canonical would not
    // match. We surface this flag so the caller can decide whether to cache.
    let has_revisions = crate::normalize::has_revision_markup_fast(&archive);

    // Load numbering definitions (optional - may not exist in all docx files)
    let numbering_defs = crate::import::parse_optional_docx_part(
        &archive,
        "word/numbering.xml",
        crate::numbering::NumberingDefinitions::parse,
    )?;

    // Load style definitions (optional - may not exist in all docx files)
    let mut style_defs = crate::import::parse_optional_docx_part(
        &archive,
        "word/styles.xml",
        crate::styles::StyleDefinitions::parse,
    )?;

    // Load theme font definitions (optional) and attach to style definitions
    let theme_fonts = crate::import::parse_optional_docx_part(
        &archive,
        "word/theme/theme1.xml",
        crate::styles::ThemeFonts::parse,
    )?;
    if let (Some(theme_fonts), Some(ref mut sd)) = (theme_fonts, style_defs.as_mut()) {
        sd.set_theme_fonts(theme_fonts);
    }

    // Load default tab stop interval from settings.xml (default: 720 twips = 0.5 inch)
    let default_tab_stop = crate::settings::parse_default_tab_stop(&archive)
        .map_err(crate::import::invalid_docx_message)?
        .unwrap_or(720);

    // Parse compatibility settings from settings.xml (MS-DOCX §2.3)
    let compat_settings = crate::settings::parse_compat_settings(&archive)
        .map_err(crate::import::invalid_docx_message)?;

    // Parse document relationships and stories
    let rels = parse_document_relationships(&archive, &main_part)?;
    let (header_refs, footer_refs) = parse_header_footer_refs(&root)?;

    // Preserve every header/footer part explicitly referenced by sectPr.
    // `evenAndOddHeaders` affects display, not whether the story part exists.
    let mut story_diagnostics = Vec::new();
    let headers = parse_headers(
        &archive,
        &rels,
        &header_refs,
        numbering_defs.as_ref(),
        style_defs.as_ref(),
        default_tab_stop,
        &main_dir,
        &mut story_diagnostics,
    )?;
    let footers = parse_footers(
        &archive,
        &rels,
        &footer_refs,
        numbering_defs.as_ref(),
        style_defs.as_ref(),
        default_tab_stop,
        &main_dir,
        &mut story_diagnostics,
    )?;
    let footnotes = parse_footnotes(
        &archive,
        &rels,
        numbering_defs.as_ref(),
        style_defs.as_ref(),
        default_tab_stop,
        &main_dir,
    )?;
    let endnotes = parse_endnotes(
        &archive,
        &rels,
        numbering_defs.as_ref(),
        style_defs.as_ref(),
        default_tab_stop,
        &main_dir,
    )?;
    let comments = parse_comments(
        &archive,
        &rels,
        numbering_defs.as_ref(),
        style_defs.as_ref(),
        default_tab_stop,
        &main_dir,
    )?;
    // commentsExtended.xml (MS-DOCX §2.5.1) carries the reply-threading
    // (w15:paraIdParent) and resolved (w15:done) state, keyed by each comment's
    // first-body-paragraph w14:paraId. This is the parse path the public API
    // (`Document::parse`) actually uses; omitting it silently dropped the
    // resolved flag and the thread structure on every import — failing loud is
    // impossible here, so the fix is to parse it (no fallback).
    let comments_extended = crate::import::parse_comments_extended(&archive, &rels, &main_dir)?;

    // Build rId → target lookup for resolving header/footer references in sectPr.
    let rel_lookup = build_rel_lookup_from_rels(&rels);

    let (mut canonical, mut diagnostics) = build_canonical_from_root_with_stories(
        &root,
        fingerprint,
        numbering_defs.as_ref(),
        style_defs.as_ref(),
        default_tab_stop,
        &compat_settings,
        &rel_lookup,
        headers,
        footers,
        footnotes,
        endnotes,
        comments,
    )?;
    // Empty-running-head tolerances were recorded while parsing the story parts
    // above, before the diagnostics sink existed; fold them in.
    diagnostics.extend(story_diagnostics);

    canonical.compat_settings = compat_settings;
    canonical.comments_extended = comments_extended;

    // Parse the three-state w:evenAndOddHeaders toggle (§17.15.1.35): None =
    // absent, Some(true) = on, Some(false) = explicitly off. Carried honestly so
    // the settings.xml writer round-trips the absent-vs-off distinction.
    canonical.even_and_odd_headers = crate::settings::parse_even_and_odd_headers_state(&archive)
        .map_err(crate::import::invalid_docx_message)?;

    // Record the w:documentProtection declaration (ISO/IEC 29500-1 §17.15.1.29)
    // and emit an import diagnostic when it is enforced. Reported, not enforced —
    // this is the public parse path, so the flag and diagnostic must land here.
    crate::import::apply_document_protection(&archive, &mut canonical, &mut diagnostics)
        .map_err(crate::import::invalid_docx_message)?;

    // Resolve external hyperlink URLs from document relationships
    resolve_hyperlink_urls(&mut canonical, &rels.hyperlinks);

    // Extract cached body nodes for later use by serialize_canonical_docx,
    // avoiding a redundant full xmltree re-parse of document.xml.
    let cached = {
        let body = body_element_mut(&mut root).map_err(map_word_xml_error)?;
        let body_children_len = body.children.len();

        // Extract opaque body children referenced by the canonical model.
        let mut opaque_children: HashMap<usize, XMLNode> = HashMap::new();
        for tracked in &canonical.blocks {
            if let BlockNode::OpaqueBlock(opaque) = &tracked.block
                && let Some(index_str) = opaque.proof_ref.docx_anchor.strip_prefix("body_index:")
                && let Ok(idx) = index_str.parse::<usize>()
                && let Some(child) = body.children.get(idx)
            {
                opaque_children.entry(idx).or_insert_with(|| child.clone());
            }
        }

        // Extract sectPr nodes from the body.
        let mut sect_pr_nodes: Vec<XMLNode> = Vec::new();
        for child in &body.children {
            if let XMLNode::Element(el) = child
                && is_w_tag(el, "sectPr")
            {
                sect_pr_nodes.push(child.clone());
            }
        }

        // Drain body children to keep the root shell lightweight.
        body.children.clear();

        BodyTemplate {
            root_shell: root,
            opaque_children,
            sect_pr_nodes,
            body_children_len,
        }
    };

    Ok((updated_bytes, canonical, diagnostics, has_revisions, cached))
}

/// Build a `w:sdt` element wrapping the given content children,
/// using the preserved SDT properties from an `SdtWrapper`.
pub(crate) fn build_sdt_wrapper(
    wrapper: &SdtWrapper,
    content_children: Vec<XMLNode>,
) -> Result<Element, RuntimeError> {
    let mut sdt = w_el("sdt");

    // Re-parse and add w:sdtPr. The bytes are a self-contained fragment written
    // by `serialize_raw_fragment`, so parse them with the matching
    // `parse_raw_fragment` (which understands the fragment's own namespace
    // declarations) rather than the strict whole-document parser.
    if !wrapper.sdt_pr_xml.is_empty() {
        let sdt_pr =
            crate::word_xml::parse_raw_fragment(&wrapper.sdt_pr_xml).map_err(|source| {
                RuntimeError {
                    code: ErrorCode::InvalidDocx,
                    message: "failed to parse preserved SDT properties".to_string(),
                    details: ErrorDetails {
                        context: Some(format!("err={source}")),
                        ..ErrorDetails::default()
                    },
                }
            })?;
        sdt.children.push(XMLNode::Element(sdt_pr));
    }

    // Re-parse and add w:sdtEndPr if present
    if let Some(ref end_pr_xml) = wrapper.sdt_end_pr_xml {
        let sdt_end_pr =
            crate::word_xml::parse_raw_fragment(end_pr_xml).map_err(|source| RuntimeError {
                code: ErrorCode::InvalidDocx,
                message: "failed to parse preserved SDT end properties".to_string(),
                details: ErrorDetails {
                    context: Some(format!("err={source}")),
                    ..ErrorDetails::default()
                },
            })?;
        sdt.children.push(XMLNode::Element(sdt_end_pr));
    }

    // Add w:sdtContent with the provided children
    let mut sdt_content = w_el("sdtContent");
    sdt_content.children = content_children;
    sdt.children.push(XMLNode::Element(sdt_content));

    Ok(sdt)
}

/// Insert a paragraph block in a story root (`w:hdr`, `w:ftr`, `w:footnotes`, etc.).
fn relationship_targets_match(lhs: &str, rhs: &str) -> bool {
    relationship_target_to_part_path(lhs) == relationship_target_to_part_path(rhs)
}

/// Resolve a story part_path (e.g. "header3.xml") to an rId in the output
/// package. If the relationship already exists, returns its rId. Otherwise,
/// copies the part from the target package if needed and creates a new
/// relationship entry.
fn resolve_story_part_to_rid(
    part_path: &str,
    rel_type: &str,
    base_pkg: &mut DocxPackage,
    target_pkg: &DocxPackage,
) -> String {
    if rel_type == HYPERLINK_REL_TYPE {
        return base_pkg.document_rels.add_external(rel_type, part_path);
    }

    // Already in the output rels?
    if let Some(existing) = base_pkg
        .document_rels
        .find_by_type_and_target(rel_type, part_path)
    {
        return existing.id.clone();
    }

    // Copy from target if the part isn't already in the output package.
    let part_full = relationship_target_to_part_path(part_path);
    if !base_pkg.has_part(&part_full) {
        if let Some(data) = target_pkg.get_part(&part_full) {
            base_pkg.set_part(&part_full, data.to_vec());
            ensure_story_part_rels(base_pkg, target_pkg, &part_full);
            if let Ok(ct) = content_type_for_story_rel(rel_type) {
                base_pkg
                    .content_types
                    .add_override(&format!("/{part_full}"), ct);
            }
        } else {
            // Part not in either package. This is expected for synthesized
            // blank headers/footers — their content is written later during
            // story serialization. Log for visibility.
            tracing::debug!(
                part_path,
                "story part not yet in base or target — expected for synthesized parts"
            );
        }
    }

    base_pkg.document_rels.add(rel_type, part_path)
}

/// Walk headerReference / footerReference children inside a sectPr element and
/// remap any `r:id` values that don't exist in the output package's document
/// relationships.
///
/// When the target document's sectPr is used (either rebuilt or raw-copied),
/// its header/footer rIds come from the target's `.rels` file and may not match
/// the output package. This function:
///   1. Checks each header/footerReference r:id against `base_pkg.document_rels`.
///   2. If the rId is missing, looks up the target relationship to find the part
///      target, copies the part if needed, adds the relationship to the output,
///      and rewrites the r:id attribute in-place.
fn remap_sect_pr_story_refs(
    sect_pr: &mut Element,
    base_pkg: &mut DocxPackage,
    target_pkg: &DocxPackage,
) -> Result<(), RuntimeError> {
    for child in &mut sect_pr.children {
        let XMLNode::Element(el) = child else {
            continue;
        };
        let is_header_ref = is_w_tag(el, "headerReference");
        let is_footer_ref = is_w_tag(el, "footerReference");
        if !is_header_ref && !is_footer_ref {
            continue;
        }
        let rel_type = if is_header_ref {
            HEADER_REL_TYPE
        } else {
            FOOTER_REL_TYPE
        };

        let Some(rid) = attr_get(el, "r:id").cloned() else {
            continue;
        };

        // If the rId already exists in the output rels, nothing to fix.
        if base_pkg.document_rels.find_by_id(&rid).is_some() {
            continue;
        }

        // The rId is dangling — look it up in the target package's rels.
        let target_rel = target_pkg
            .document_rels
            .find_by_id(&rid)
            .ok_or_else(|| RuntimeError {
                code: ErrorCode::InvalidDocx,
                message: format!(
                    "sectPr header/footerReference r:id={rid} not found in target document rels"
                ),
                details: ErrorDetails::default(),
            })?;
        let target_target = target_rel.target.clone();
        let part_path = relationship_target_to_part_path(&target_target);

        // Copy the part from target if not already in the output package.
        if !base_pkg.has_part(&part_path)
            && let Some(data) = target_pkg.get_part(&part_path)
        {
            base_pkg.set_part(&part_path, data.to_vec());
            ensure_story_part_rels(base_pkg, target_pkg, &part_path);
            let ct_path = format!("/{part_path}");
            base_pkg
                .content_types
                .add_override(&ct_path, content_type_for_story_rel(rel_type)?);
        }

        // Add the relationship to the output, preferring the original rId.
        let actual_rid =
            base_pkg
                .document_rels
                .add_with_preferred_id(rel_type, &target_target, &rid);

        // Rewrite the r:id attribute if the assigned rId differs from the original.
        if actual_rid != rid {
            attr_set(el, "r:id", &actual_rid);
        }
    }

    Ok(())
}

/// Resolve placeholder header/footer `r:id` values inside a `w:sectPrChange`'s
/// previous `w:sectPr` fragment.
///
/// `previous_properties_raw` is serialized by `section_properties_to_element`
/// with `resolve_rid = None`, which emits each header/footerReference `r:id`
/// as the bare story `part_path` (the "raw XML context" placeholder, see the
/// `resolve` fallback in `append_modeled_children`). That placeholder is NOT a
/// valid relationship reference — if it reaches the output verbatim, Word hits
/// the "needs repair" dialog (I-REL-001 dangling reference).
///
/// The previous sectPr references the same story parts as the live sectPr, so
/// we resolve each placeholder through the same package-aware resolver, which
/// registers the relationship (and copies/synthesizes the part) exactly as for
/// the live refs. After this runs, every `r:id` in the fragment is a real rId
/// backed by an entry in `word/_rels/document.xml.rels`.
pub(crate) fn resolve_sect_pr_change_story_refs(
    sect_pr_change: &mut Element,
    resolve: &mut dyn FnMut(&str, &str) -> String,
) {
    for change_child in &mut sect_pr_change.children {
        let XMLNode::Element(prev_sect_pr) = change_child else {
            continue;
        };
        if !is_w_tag(prev_sect_pr, "sectPr") {
            continue;
        }
        for child in &mut prev_sect_pr.children {
            let XMLNode::Element(el) = child else {
                continue;
            };
            let rel_type = if is_w_tag(el, "headerReference") {
                HEADER_REL_TYPE
            } else if is_w_tag(el, "footerReference") {
                FOOTER_REL_TYPE
            } else {
                continue;
            };
            let Some(part_path) = attr_get(el, "r:id").cloned() else {
                continue;
            };
            let rid = resolve(&part_path, rel_type);
            attr_set(el, "r:id", &rid);
        }
    }
}

/// WORD RULE (bisected against real Word): a
/// `w:sectPrChange` whose previous snapshot is an EMPTY `<w:sectPr/>` registers
/// NO revision in Word — the tracked layout change is invisible in the review
/// pane and unrejectable (reject silently keeps the new layout). Any non-empty
/// snapshot registers. So at the WRITE EDGE, an empty snapshot materializes
/// Word's default page geometry, exactly as Word's own writer does. The stored
/// model keeps the faithful (possibly empty) authored state — stemma's own
/// reject restores that verbatim; only the serialized wire form is widened.
pub(crate) fn materialize_empty_sect_pr_snapshot(prev: &mut Element) {
    use crate::edit::verbs::page_setup::{
        WORD_DEFAULT_HEADER_FOOTER_DISTANCE, WORD_DEFAULT_MARGIN, WORD_DEFAULT_PAGE_HEIGHT,
        WORD_DEFAULT_PAGE_WIDTH,
    };
    if !prev.children.is_empty() {
        return;
    }
    let mut pg_sz = w_el("pgSz");
    attr_set(&mut pg_sz, "w:w", WORD_DEFAULT_PAGE_WIDTH.to_string());
    attr_set(&mut pg_sz, "w:h", WORD_DEFAULT_PAGE_HEIGHT.to_string());
    prev.children.push(XMLNode::Element(pg_sz));
    let mut pg_mar = w_el("pgMar");
    for edge in ["w:top", "w:right", "w:bottom", "w:left"] {
        attr_set(&mut pg_mar, edge, WORD_DEFAULT_MARGIN.to_string());
    }
    attr_set(
        &mut pg_mar,
        "w:header",
        WORD_DEFAULT_HEADER_FOOTER_DISTANCE.to_string(),
    );
    attr_set(
        &mut pg_mar,
        "w:footer",
        WORD_DEFAULT_HEADER_FOOTER_DISTANCE.to_string(),
    );
    attr_set(&mut pg_mar, "w:gutter", "0");
    prev.children.push(XMLNode::Element(pg_mar));
}

/// Build a w:sectPr Element from parsed SectionProperties.
///
/// This is the inverse of `word_ir::parse_section_properties`. When `raw`
/// The element is built from scratch using the modeled fields. The `base_sect_pr`
/// argument supplies non-dominated extension children (used by the sectPrChange
/// overlay path); unknown children are merged in only from that base.
#[allow(clippy::type_complexity)]
pub(crate) fn section_properties_to_element(
    sp: &SectionProperties,
    base_sect_pr: Option<&Element>,
    sect_pr_change: Option<Element>,
    resolve_rid: Option<&mut dyn FnMut(&str, &str) -> String>,
) -> Element {
    let mut sect_pr = w_el("sectPr");

    // Append rebuilt known children from the parsed struct fields.
    append_modeled_children(&mut sect_pr, sp, resolve_rid);

    // Merge non-dominated children and attributes from the base sectPr so that
    // unknown extension elements roundtrip correctly through sectPrChange.
    if let Some(base) = base_sect_pr {
        for child in &base.children {
            if let XMLNode::Element(el) = child
                && !is_dominated_sect_pr_child(el)
                && !is_w_tag(el, "sectPrChange")
            {
                sect_pr.children.push(child.clone());
            }
        }
        for (key, value) in &base.attributes {
            if !sect_pr.attributes.contains_key(key) {
                sect_pr.attributes.insert(key.clone(), value.clone());
            }
        }
    }

    // sectPrChange must be the last child per CT_SectPr sequence.
    if let Some(change_el) = sect_pr_change {
        sect_pr.children.push(XMLNode::Element(change_el));
    }

    // Sort children to ECMA-376 CT_SectPr order.
    sort_sect_pr_children(&mut sect_pr);

    sect_pr
}

/// Spec position for a sectPr child element (CT_SectPr sequence).
/// EG_HdrFtrReferences (0..6) come first, then EG_SectPrContents, then sectPrChange.
fn sect_pr_child_order(el: &Element) -> u32 {
    let name = local_element_name(el);
    match name {
        "headerReference" | "footerReference" => 0,
        "footnotePr" => 1,
        "endnotePr" => 2,
        "type" => 3,
        "pgSz" => 4,
        "pgMar" => 5,
        "paperSrc" => 6,
        "pgBorders" => 7,
        "lnNumType" => 8,
        "pgNumType" => 9,
        "cols" => 10,
        "formProt" => 11,
        "vAlign" => 12,
        "noEndnote" => 13,
        "titlePg" => 14,
        "textDirection" => 15,
        "bidi" => 16,
        "rtlGutter" => 17,
        "docGrid" => 18,
        "printerSettings" => 19,
        "sectPrChange" => 20,
        _ => 21, // unknown extension elements go at the end
    }
}

fn sort_sect_pr_children(sect_pr: &mut Element) {
    sect_pr.children.sort_by_key(|child| match child {
        XMLNode::Element(el) => sect_pr_child_order(el),
        _ => u32::MAX,
    });
}

/// Returns `true` for child element tags of `w:sectPr` that are modeled in
/// `SectionProperties` and will be rebuilt from the parsed struct fields.
/// Matches the dominated set used in the sectPrChange overlay path.
fn is_dominated_sect_pr_child(el: &Element) -> bool {
    is_w_tag(el, "headerReference")
        || is_w_tag(el, "footerReference")
        || is_w_tag(el, "pgSz")
        || is_w_tag(el, "pgMar")
        || is_w_tag(el, "paperSrc")
        || is_w_tag(el, "cols")
        || is_w_tag(el, "type")
        || is_w_tag(el, "pgBorders")
        || is_w_tag(el, "lnNumType")
        || is_w_tag(el, "vAlign")
        || is_w_tag(el, "textDirection")
        || is_w_tag(el, "pgNumType")
        || is_w_tag(el, "rtlGutter")
        || is_w_tag(el, "docGrid")
        || is_w_tag(el, "titlePg")
        || is_w_tag(el, "bidi")
        || is_w_tag(el, "formProt")
        || is_w_tag(el, "noEndnote")
        || is_w_tag(el, "footnotePr")
        || is_w_tag(el, "endnotePr")
        || is_w_tag(el, "printerSettings")
}

/// Append the child elements that `SectionProperties` models to the given
/// `sect_pr` element, built from the parsed struct fields.
///
/// `resolve_rid` maps a (part_path, rel_type) pair to an rId string for the
/// output package. When `None`, the part_path is used directly as the r:id
/// value (suitable for raw XML roundtrip contexts like sectPrChange).
#[allow(clippy::type_complexity)]
fn append_modeled_children(
    sect_pr: &mut Element,
    sp: &SectionProperties,
    resolve_rid: Option<&mut dyn FnMut(&str, &str) -> String>,
) {
    // EG_HdrFtrReferences — headerReference/footerReference come first in CT_SectPr.
    let hf_kind_to_xml = |kind: &HeaderFooterKind| -> &'static str {
        match kind {
            HeaderFooterKind::Default => "default",
            HeaderFooterKind::First => "first",
            HeaderFooterKind::Even => "even",
        }
    };

    // When no resolver is provided, use part_path directly (raw XML context).
    let resolve: &mut dyn FnMut(&str, &str) -> String = match resolve_rid {
        Some(f) => f,
        None => &mut |part_path: &str, _rel_type: &str| part_path.to_string(),
    };

    // Refs inherited via §17.10.2 resolution (or blank-synthesized per
    // §17.10.5) are render-time semantics, not authored markup — emitting
    // them would materialize inheritance onto every mid-document sectPr.
    for href in sp.header_refs.iter().filter(|r| !r.synthesized) {
        let mut el = w_el("headerReference");
        attr_set(&mut el, "w:type", hf_kind_to_xml(&href.kind));
        let rid = resolve(&href.part_path, HEADER_REL_TYPE);
        attr_set(&mut el, "r:id", &rid);
        sect_pr.children.push(XMLNode::Element(el));
    }
    for fref in sp.footer_refs.iter().filter(|r| !r.synthesized) {
        let mut el = w_el("footerReference");
        attr_set(&mut el, "w:type", hf_kind_to_xml(&fref.kind));
        let rid = resolve(&fref.part_path, FOOTER_REL_TYPE);
        attr_set(&mut el, "r:id", &rid);
        sect_pr.children.push(XMLNode::Element(el));
    }

    // Helper to serialize a NoteProperties element.
    let serialize_note_pr = |name: &str, np: &crate::domain::NoteProperties| -> Element {
        let mut el = w_el(name);
        if let Some(ref pos) = np.position {
            let mut pos_el = w_el("pos");
            attr_set(&mut pos_el, "w:val", pos.to_xml_str());
            el.children.push(XMLNode::Element(pos_el));
        }
        if let Some(ref fmt) = np.num_fmt {
            let mut fmt_el = w_el("numFmt");
            attr_set(&mut fmt_el, "w:val", fmt.to_xml_str());
            el.children.push(XMLNode::Element(fmt_el));
        }
        if let Some(start) = np.num_start {
            let mut start_el = w_el("numStart");
            attr_set(&mut start_el, "w:val", start.to_string());
            el.children.push(XMLNode::Element(start_el));
        }
        if let Some(ref restart) = np.num_restart {
            let mut restart_el = w_el("numRestart");
            attr_set(&mut restart_el, "w:val", restart.to_xml_str());
            el.children.push(XMLNode::Element(restart_el));
        }
        el
    };

    // w:footnotePr (§17.11.3) — before pgSz in CT_SectPr sequence
    if let Some(ref fp) = sp.footnote_pr {
        sect_pr
            .children
            .push(XMLNode::Element(serialize_note_pr("footnotePr", fp)));
    }

    // w:endnotePr (§17.11.2)
    if let Some(ref ep) = sp.endnote_pr {
        sect_pr
            .children
            .push(XMLNode::Element(serialize_note_pr("endnotePr", ep)));
    }

    // EG_SectPrContents sequence (ECMA-376 Annex A):
    // footnotePr, endnotePr, type, pgSz, pgMar, paperSrc, pgBorders,
    // lnNumType, pgNumType, cols, formProt, vAlign, noEndnote, titlePg,
    // textDirection, bidi, rtlGutter, docGrid, printerSettings

    // Helper for boolean on/off flags used by several elements below.
    let serialize_bool_flag = |sect: &mut Element, name: &str, val: Option<bool>| {
        if let Some(v) = val {
            let mut el = w_el(name);
            if !v {
                attr_set(&mut el, "w:val", "0");
            }
            sect.children.push(XMLNode::Element(el));
        }
    };

    // w:type (§17.6.17)
    if let Some(ref st) = sp.section_type {
        let mut type_el = w_el("type");
        attr_set(&mut type_el, "w:val", st.to_xml_str());
        sect_pr.children.push(XMLNode::Element(type_el));
    }

    // w:pgSz (§17.6.14)
    if sp.page_width.is_some()
        || sp.page_height.is_some()
        || sp.orientation.is_some()
        || sp.paper_size_code.is_some()
    {
        let mut pg_sz = w_el("pgSz");
        if let Some(w) = sp.page_width {
            attr_set(&mut pg_sz, "w:w", w.to_string());
        }
        if let Some(h) = sp.page_height {
            attr_set(&mut pg_sz, "w:h", h.to_string());
        }
        if let Some(ref orient) = sp.orientation {
            let val = match orient {
                PageOrientation::Portrait => "portrait",
                PageOrientation::Landscape => "landscape",
            };
            attr_set(&mut pg_sz, "w:orient", val);
        }
        if let Some(code) = sp.paper_size_code {
            attr_set(&mut pg_sz, "w:code", code.to_string());
        }
        sect_pr.children.push(XMLNode::Element(pg_sz));
    }

    // w:pgMar (§17.6.11)
    if sp.margin_top.is_some()
        || sp.margin_bottom.is_some()
        || sp.margin_left.is_some()
        || sp.margin_right.is_some()
        || sp.header_distance.is_some()
        || sp.footer_distance.is_some()
        || sp.gutter.is_some()
    {
        let mut pg_mar = w_el("pgMar");
        if let Some(v) = sp.margin_top {
            attr_set(&mut pg_mar, "w:top", v.to_string());
        }
        if let Some(v) = sp.margin_bottom {
            attr_set(&mut pg_mar, "w:bottom", v.to_string());
        }
        if let Some(v) = sp.margin_left {
            attr_set(&mut pg_mar, "w:left", v.to_string());
        }
        if let Some(v) = sp.margin_right {
            attr_set(&mut pg_mar, "w:right", v.to_string());
        }
        if let Some(v) = sp.header_distance {
            attr_set(&mut pg_mar, "w:header", v.to_string());
        }
        if let Some(v) = sp.footer_distance {
            attr_set(&mut pg_mar, "w:footer", v.to_string());
        }
        if let Some(v) = sp.gutter {
            attr_set(&mut pg_mar, "w:gutter", v.to_string());
        }
        sect_pr.children.push(XMLNode::Element(pg_mar));
    }

    // w:paperSrc (§17.6.9)
    if let Some(ref ps) = sp.paper_source {
        let mut ps_el = w_el("paperSrc");
        if let Some(first) = ps.first {
            attr_set(&mut ps_el, "w:first", first.to_string());
        }
        if let Some(other) = ps.other {
            attr_set(&mut ps_el, "w:other", other.to_string());
        }
        sect_pr.children.push(XMLNode::Element(ps_el));
    }

    // w:pgBorders (§17.6.7)
    if let Some(ref pb) = sp.page_borders {
        let mut pg_borders = w_el("pgBorders");
        attr_set(&mut pg_borders, "w:zOrder", pb.z_order.clone());
        attr_set(&mut pg_borders, "w:offsetFrom", pb.offset_from.clone());
        let serialize_border = |name: &str, border: &Option<Border>| -> Option<Element> {
            let b = border.as_ref()?;
            let mut el = w_el(name);
            attr_set(&mut el, "w:val", b.style.to_xml_str());
            if let Some(ref color) = b.color {
                attr_set(&mut el, "w:color", color.clone());
            }
            if let Some(sz) = b.size {
                attr_set(&mut el, "w:sz", sz.to_string());
            }
            if let Some(space) = b.space {
                attr_set(&mut el, "w:space", space.to_string());
            }
            Some(el)
        };
        if let Some(el) = serialize_border("top", &pb.top) {
            pg_borders.children.push(XMLNode::Element(el));
        }
        if let Some(el) = serialize_border("left", &pb.left) {
            pg_borders.children.push(XMLNode::Element(el));
        }
        if let Some(el) = serialize_border("bottom", &pb.bottom) {
            pg_borders.children.push(XMLNode::Element(el));
        }
        if let Some(el) = serialize_border("right", &pb.right) {
            pg_borders.children.push(XMLNode::Element(el));
        }
        sect_pr.children.push(XMLNode::Element(pg_borders));
    }

    // w:lnNumType (§17.6.8)
    if let Some(ref ln) = sp.line_numbering {
        let mut ln_el = w_el("lnNumType");
        if let Some(count_by) = ln.count_by {
            attr_set(&mut ln_el, "w:countBy", count_by.to_string());
        }
        if let Some(start) = ln.start {
            attr_set(&mut ln_el, "w:start", start.to_string());
        }
        if let Some(ref restart) = ln.restart {
            attr_set(&mut ln_el, "w:restart", restart.clone());
        }
        if let Some(distance) = ln.distance {
            attr_set(&mut ln_el, "w:distance", distance.to_string());
        }
        sect_pr.children.push(XMLNode::Element(ln_el));
    }

    // w:pgNumType (§17.6.12)
    if let Some(ref pn) = sp.page_number_type {
        let mut pn_el = w_el("pgNumType");
        if let Some(ref fmt) = pn.fmt {
            attr_set(&mut pn_el, "w:fmt", fmt.clone());
        }
        if let Some(start) = pn.start {
            attr_set(&mut pn_el, "w:start", start.to_string());
        }
        if let Some(chap_style) = pn.chap_style {
            attr_set(&mut pn_el, "w:chapStyle", chap_style.to_string());
        }
        if let Some(ref chap_sep) = pn.chap_sep {
            attr_set(&mut pn_el, "w:chapSep", chap_sep.clone());
        }
        sect_pr.children.push(XMLNode::Element(pn_el));
    }

    // w:cols (§17.6.4)
    if sp.columns.is_some()
        || sp.column_space.is_some()
        || !sp.column_defs.is_empty()
        || sp.column_separator.is_some()
        || sp.equal_width.is_some()
    {
        let mut cols = w_el("cols");
        if let Some(num) = sp.columns {
            attr_set(&mut cols, "w:num", num.to_string());
        }
        if let Some(space) = sp.column_space {
            attr_set(&mut cols, "w:space", space.to_string());
        }
        // §17.6.4 equalWidth: re-emit faithfully. Critical for "0" — Word defaults
        // to true, so dropping it flips unequal columns to equal (MS-OI §2.1.213).
        if let Some(eq) = sp.equal_width {
            attr_set(&mut cols, "w:equalWidth", if eq { "1" } else { "0" });
        }
        if let Some(true) = sp.column_separator {
            attr_set(&mut cols, "w:sep", "1");
        }
        for col_def in &sp.column_defs {
            let mut col = w_el("col");
            attr_set(&mut col, "w:w", col_def.width.to_string());
            attr_set(&mut col, "w:space", col_def.space.to_string());
            cols.children.push(XMLNode::Element(col));
        }
        sect_pr.children.push(XMLNode::Element(cols));
    }

    // w:formProt (§17.6.6)
    serialize_bool_flag(sect_pr, "formProt", sp.form_prot);

    // w:vAlign (§17.6.20)
    if let Some(ref va) = sp.v_align {
        let mut va_el = w_el("vAlign");
        attr_set(&mut va_el, "w:val", va.to_xml_str());
        sect_pr.children.push(XMLNode::Element(va_el));
    }

    // w:noEndnote (§17.6.9)
    serialize_bool_flag(sect_pr, "noEndnote", sp.no_endnote);

    // w:titlePg (§17.6.18)
    serialize_bool_flag(sect_pr, "titlePg", sp.title_page);

    // w:textDirection (§17.6.19)
    if let Some(ref td) = sp.text_direction {
        let mut td_el = w_el("textDirection");
        attr_set(&mut td_el, "w:val", td.to_xml_str());
        sect_pr.children.push(XMLNode::Element(td_el));
    }

    // w:bidi (§17.6.1)
    serialize_bool_flag(sect_pr, "bidi", sp.bidi);

    // w:rtlGutter (§17.6.15)
    if let Some(rtl) = sp.rtl_gutter {
        let mut rtl_el = w_el("rtlGutter");
        if !rtl {
            attr_set(&mut rtl_el, "w:val", "0");
        }
        sect_pr.children.push(XMLNode::Element(rtl_el));
    }

    // w:docGrid (§17.6.5)
    if sp.doc_grid_type.is_some()
        || sp.doc_grid_line_pitch.is_some()
        || sp.doc_grid_char_space.is_some()
    {
        let mut dg = w_el("docGrid");
        if let Some(ref t) = sp.doc_grid_type {
            attr_set(&mut dg, "w:type", t.to_xml_str());
        }
        if let Some(lp) = sp.doc_grid_line_pitch {
            attr_set(&mut dg, "w:linePitch", lp.to_string());
        }
        if let Some(cs) = sp.doc_grid_char_space {
            attr_set(&mut dg, "w:charSpace", cs.to_string());
        }
        sect_pr.children.push(XMLNode::Element(dg));
    }

    // w:printerSettings (§17.6.14)
    if let Some(ref rid) = sp.printer_settings_rid {
        let mut ps_el = w_el("printerSettings");
        attr_set(&mut ps_el, "r:id", rid.clone());
        sect_pr.children.push(XMLNode::Element(ps_el));
    }
}

fn fingerprint(bytes: &[u8]) -> DocFingerprint {
    DocFingerprint(sha256_hex(bytes))
}

const DOCX_ENCRYPTION_MARKERS: [&str; 2] = ["EncryptedPackage", "EncryptionInfo"];

fn is_encrypted_docx(archive: &DocxArchive) -> bool {
    DOCX_ENCRYPTION_MARKERS
        .iter()
        .any(|name| archive.get(name).is_some())
}

fn ensure_docx_not_encrypted(archive: &DocxArchive) -> Result<(), RuntimeError> {
    if is_encrypted_docx(archive) {
        return Err(invalid_docx(
            "password-protected DOCX files are not supported",
        ));
    }
    Ok(())
}

pub(crate) fn invalid_docx(message: &str) -> RuntimeError {
    RuntimeError {
        code: ErrorCode::InvalidDocx,
        message: message.to_string(),
        details: ErrorDetails::default(),
    }
}

pub(crate) fn invalid_snapshot(message: &str) -> RuntimeError {
    RuntimeError {
        code: ErrorCode::InvalidSnapshot,
        message: message.to_string(),
        details: ErrorDetails::default(),
    }
}

fn map_docx_error(err: DocxError) -> RuntimeError {
    let message = match err {
        DocxError::ZipRead(source) => format!("docx read failed: {source}"),
        DocxError::ZipWrite(source) => format!("docx write failed: {source}"),
        DocxError::Io(source) => format!("docx io error: {source}"),
        DocxError::MissingFile(name) => format!("docx missing file: {name}"),
        DocxError::ZipBomb(detail) => format!("docx rejected: {detail}"),
        DocxError::DuplicatePartName { name, existing } => format!(
            "docx rejected: duplicate ZIP part name {name:?} (case-equivalent to {existing:?}); \
             part names must be unique (OPC §6.2, §7.3) — Word reports such packages as corrupt"
        ),
    };
    RuntimeError {
        code: ErrorCode::InvalidDocx,
        message,
        details: ErrorDetails::default(),
    }
}

fn map_package_error(err: crate::docx_package::PackageError) -> RuntimeError {
    RuntimeError {
        code: ErrorCode::InvalidDocx,
        message: format!("package error: {err}"),
        details: ErrorDetails::default(),
    }
}

fn map_xml_write_error(err: std::io::Error) -> RuntimeError {
    RuntimeError {
        code: ErrorCode::InvalidDocx,
        message: format!("XML streaming write error: {err}"),
        details: ErrorDetails::default(),
    }
}

pub(crate) fn map_word_xml_error(err: WordXmlError) -> RuntimeError {
    let message = match err {
        WordXmlError::XmlParse(source) => format!("wordprocessingml parse error: {source}"),
        WordXmlError::XmlDepthExceeded { limit, depth } => {
            format!("wordprocessingml nesting depth {depth} exceeds supported limit {limit}")
        }
        WordXmlError::XmlWrite(source) => format!("wordprocessingml write error: {source}"),
        WordXmlError::MissingBody => "wordprocessingml missing body element".to_string(),
        WordXmlError::MultipleBody(n) => {
            format!("wordprocessingml has {n} body elements, expected exactly 1")
        }
        WordXmlError::MissingDocument => "wordprocessingml missing document element".to_string(),
        WordXmlError::QuickXml { position, reason } => {
            format!("wordprocessingml parse error at byte {position}: {reason}")
        }
        WordXmlError::DoctypeRejected => {
            "wordprocessingml contains a DOCTYPE/DTD, which is not allowed".to_string()
        }
        WordXmlError::NoRootElement => {
            "wordprocessingml stream ended without a root element".to_string()
        }
    };
    RuntimeError {
        code: ErrorCode::InvalidDocx,
        message,
        details: ErrorDetails::default(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use super::*;
    use crate::domain::{
        BorderStyle, CellFormatting, FormattingChange, HighlightColor, InlineNode, MarkValue,
        NodeId, NumberingInfo, StyleProps, TableCellNode, TableFormatting, TableNode, TableRowNode,
        TextNode, TrackedSegment, VerticalMerge, normal_tracked_block,
    };
    use crate::import::{extract_inline_text_simple, strip_literal_prefix};
    use crate::serialize::{build_paragraph_properties, build_text_run, serialize_paragraph_node};

    /// Parse a `<w:p>` fragment for the field-char integrity detector tests.
    fn parse_para(xml: &str) -> Element {
        let wrapped = format!(
            r#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{xml}</w:p>"#
        );
        Element::parse(Cursor::new(wrapped.into_bytes())).expect("test paragraph XML must parse")
    }

    #[test]
    fn del_field_chars_balanced_begin_and_end_is_not_an_imbalance() {
        // A fully-deleted field: both begin and end live inside <w:del>.
        let para = parse_para(
            r#"<w:del><w:r><w:fldChar w:fldCharType="begin"/></w:r>
               <w:r><w:fldChar w:fldCharType="end"/></w:r></w:del>"#,
        );
        assert!(!paragraph_del_field_chars_imbalanced(&para));
        assert!(!part_has_del_field_char_imbalance(&para));
    }

    #[test]
    fn del_field_char_begin_without_deleted_end_is_an_imbalance() {
        // The begin is deleted but the matching end is live (outside <w:del>) —
        // the torn-field shape the merge pipeline must never produce.
        let para = parse_para(
            r#"<w:del><w:r><w:fldChar w:fldCharType="begin"/></w:r></w:del>
               <w:r><w:fldChar w:fldCharType="end"/></w:r>"#,
        );
        assert!(paragraph_del_field_chars_imbalanced(&para));
        assert!(part_has_del_field_char_imbalance(&para));
    }

    #[test]
    fn standalone_deleted_instr_text_is_not_a_field_char_imbalance() {
        // instrText inside <w:del> does not affect field balance and must not
        // trip the guard (it is preserved for the reject view's opaque markers).
        let para = parse_para(r#"<w:del><w:r><w:instrText> PAGE </w:instrText></w:r></w:del>"#);
        assert!(!paragraph_del_field_chars_imbalanced(&para));
    }
    use zip::ZipWriter;
    use zip::write::FileOptions;

    /// Helper: make a Vec<InlineNode> from a single text string.
    fn text_inlines(text: &str) -> Vec<InlineNode> {
        vec![InlineNode::from(TextNode {
            id: NodeId::from("t1"),
            text_role: None,
            text: text.to_string(),
            marks: vec![],
            style_props: StyleProps::default(),
            rpr_authored: crate::domain::RunRprAuthored::default(),
            formatting_change: None,
        })]
    }

    /// Helper: make a Vec<InlineNode> from multiple text strings (separate runs).
    fn multi_run_inlines(texts: &[&str]) -> Vec<InlineNode> {
        texts
            .iter()
            .enumerate()
            .map(|(i, t)| {
                InlineNode::from(TextNode {
                    id: NodeId::from(format!("t{}", i)),
                    text_role: None,
                    text: t.to_string(),
                    marks: vec![],
                    style_props: StyleProps::default(),
                    rpr_authored: crate::domain::RunRprAuthored::default(),
                    formatting_change: None,
                })
            })
            .collect()
    }

    fn prefix_tuple(
        prefix: Option<crate::import::StrippedLiteralPrefix>,
    ) -> Option<(String, bool, bool)> {
        prefix.map(|p| (p.label, p.has_leading_tab, p.has_trailing_tab))
    }

    const CONTENT_TYPES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

    const ROOT_RELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

    const DOCUMENT_RELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;

    fn wrap_body(inner: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    {inner}
    <w:sectPr/>
  </w:body>
</w:document>"#
        )
    }

    fn build_docx(document_xml: &str) -> Vec<u8> {
        let cursor = Cursor::new(Vec::new());
        let mut zip = ZipWriter::new(cursor);
        let options = FileOptions::default();

        zip.start_file("[Content_Types].xml", options).unwrap();
        zip.write_all(CONTENT_TYPES_XML.as_bytes()).unwrap();

        zip.start_file("_rels/.rels", options).unwrap();
        zip.write_all(ROOT_RELS_XML.as_bytes()).unwrap();

        zip.start_file("word/_rels/document.xml.rels", options)
            .unwrap();
        zip.write_all(DOCUMENT_RELS_XML.as_bytes()).unwrap();

        zip.start_file("word/document.xml", options).unwrap();
        zip.write_all(document_xml.as_bytes()).unwrap();

        zip.finish().unwrap().into_inner()
    }

    fn decode_snapshot_blob(blob: &[u8]) -> PersistedEditSnapshot {
        let decoded = zstd::stream::decode_all(Cursor::new(blob)).expect("decode snapshot blob");
        bincode::deserialize(&decoded).expect("deserialize snapshot blob")
    }

    #[test]
    fn snapshot_blob_roundtrip_preserves_tracked_state_and_document_version() {
        let document_xml = wrap_body(
            r#"
    <w:p>
      <w:r><w:t>The liability cap is one million dollars.</w:t></w:r>
    </w:p>"#,
        );
        let bytes = build_docx(&document_xml);
        let runtime = SimpleRuntime::new();
        let imported = runtime.import_docx(&bytes).expect("import clean docx");

        let tx = crate::edit::EditTransaction {
            steps: vec![crate::edit::EditStep::ReplaceParagraphText {
                block_id: NodeId::from("p_1"),
                rationale: Some("lower the liability cap".to_string()),
                replacement_role: None,
                expect: "one million dollars".to_string(),
                semantic_hash: None,
                content: crate::edit::ParagraphContent {
                    fragments: vec![crate::edit::ContentFragment::Text(
                        "The liability cap is five hundred thousand dollars.".to_string(),
                    )],
                },
            }],
            summary: Some("Lower liability cap".to_string()),
            materialization_mode: crate::edit::MaterializationMode::TrackedChange,
            revision: RevisionInfo {
                revision_id: 1,
                author: Some("Stemma".to_string()),
                date: Some("2026-04-09T00:00:00Z".to_string()),
                apply_op_id: None,
            },
        };

        runtime
            .apply_edit(&imported.doc_handle, &tx)
            .expect("apply_edit should succeed");
        let before = runtime
            .tracked_view(&imported.doc_handle)
            .expect("tracked_view before snapshot export");

        let snapshot_blob = runtime
            .export_snapshot_blob(&imported.doc_handle)
            .expect("export snapshot blob");
        let decoded = decode_snapshot_blob(&snapshot_blob);
        assert_eq!(decoded.blob_schema_version, SNAPSHOT_BLOB_SCHEMA_VERSION);
        assert_eq!(decoded.meta.document_version, 2);

        let restored = runtime
            .import_snapshot_blob(&snapshot_blob)
            .expect("import snapshot blob");
        assert_eq!(restored.document_version, 2);
        let after = runtime
            .tracked_view(&restored.import.doc_handle)
            .expect("tracked_view after snapshot import");

        assert_eq!(after, before);
        assert_eq!(restored.import.canonical, before.canonical);
        assert_eq!(restored.import.fingerprint, before.fingerprint);
    }

    #[test]
    fn snapshot_blob_reload_can_continue_editing() {
        let document_xml = wrap_body(
            r#"
    <w:p>
      <w:r><w:t>The liability cap is one million dollars.</w:t></w:r>
    </w:p>"#,
        );
        let bytes = build_docx(&document_xml);
        let runtime = SimpleRuntime::new();
        let imported = runtime.import_docx(&bytes).expect("import clean docx");

        let snapshot_blob = runtime
            .export_snapshot_blob(&imported.doc_handle)
            .expect("export snapshot blob");
        let restored = runtime
            .import_snapshot_blob(&snapshot_blob)
            .expect("import snapshot blob");
        assert_eq!(restored.document_version, 1);

        let tx = crate::edit::EditTransaction {
            steps: vec![crate::edit::EditStep::ReplaceParagraphText {
                block_id: NodeId::from("p_1"),
                rationale: Some("lower the liability cap".to_string()),
                replacement_role: None,
                expect: "one million dollars".to_string(),
                semantic_hash: None,
                content: crate::edit::ParagraphContent {
                    fragments: vec![crate::edit::ContentFragment::Text(
                        "The liability cap is two hundred fifty thousand dollars.".to_string(),
                    )],
                },
            }],
            summary: Some("Lower again".to_string()),
            materialization_mode: crate::edit::MaterializationMode::TrackedChange,
            revision: RevisionInfo {
                revision_id: 2,
                author: Some("Stemma".to_string()),
                date: Some("2026-04-09T01:00:00Z".to_string()),
                apply_op_id: None,
            },
        };
        runtime
            .apply_edit(&restored.import.doc_handle, &tx)
            .expect("apply_edit after snapshot import should succeed");

        let updated_blob = runtime
            .export_snapshot_blob(&restored.import.doc_handle)
            .expect("export updated snapshot blob");
        let decoded = decode_snapshot_blob(&updated_blob);
        assert_eq!(decoded.meta.document_version, 2);

        let current = runtime
            .tracked_view(&restored.import.doc_handle)
            .expect("tracked_view after snapshot-backed edit");
        let BlockNode::Paragraph(para) = &current.canonical.blocks[0].block else {
            panic!("expected paragraph");
        };
        let paragraph_text = para
            .segments
            .iter()
            .flat_map(|seg| seg.inlines.iter())
            .filter_map(|inline| match inline {
                InlineNode::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert!(
            paragraph_text.contains("two hundred fifty"),
            "snapshot-backed editing should preserve the latest replacement text"
        );
    }

    #[test]
    fn snapshot_blob_import_rejects_invalid_bytes() {
        let runtime = SimpleRuntime::new();
        let err = runtime
            .import_snapshot_blob(b"not a valid snapshot")
            .expect_err("invalid snapshot should fail");
        assert_eq!(err.code, ErrorCode::InvalidSnapshot);
        assert!(
            err.message.contains("snapshot blob"),
            "unexpected error message: {}",
            err.message
        );
    }

    #[test]
    fn snapshot_blob_roundtrip_handles_safe_singapore_fixture() {
        let before_bytes =
            std::fs::read("testdata/safe-us-vs-singapore/before.docx").expect("read before");
        let after_bytes =
            std::fs::read("testdata/safe-us-vs-singapore/after.docx").expect("read after");
        let runtime = SimpleRuntime::new();

        for (label, bytes) in [("before", before_bytes), ("after", after_bytes)] {
            let imported = runtime
                .import_docx(&bytes)
                .unwrap_or_else(|err| panic!("{label}: import_docx failed: {err:?}"));
            let snapshot_blob = runtime
                .export_snapshot_blob(&imported.doc_handle)
                .unwrap_or_else(|err| panic!("{label}: export_snapshot_blob failed: {err:?}"));
            decode_snapshot_blob(&snapshot_blob);
            runtime
                .import_snapshot_blob(&snapshot_blob)
                .unwrap_or_else(|err| panic!("{label}: import_snapshot_blob failed: {err:?}"));
        }
    }

    // The end-to-end apply_op_id propagation tests (runtime apply_edit →
    // tracked_view → extract changelets, and through the snapshot blob
    // roundtrip) live with the consuming application's pipeline tests
    // because they cross the engine/app seam into changelet extraction.

    #[test]
    fn tracked_view_preserves_revisions_while_view_normalizes_them() {
        let document_xml = wrap_body(
            r#"
    <w:p>
      <w:r><w:t xml:space="preserve">The party shall use </w:t></w:r>
      <w:del w:id="1" w:author="Alice" w:date="2026-04-08T00:00:00Z">
        <w:r><w:delText>reasonable efforts</w:delText></w:r>
      </w:del>
      <w:ins w:id="2" w:author="Alice" w:date="2026-04-08T00:00:00Z">
        <w:r><w:t>best efforts</w:t></w:r>
      </w:ins>
      <w:r><w:t xml:space="preserve"> to protect data.</w:t></w:r>
    </w:p>"#,
        );
        let bytes = build_docx(&document_xml);
        let runtime = SimpleRuntime::new();
        let imported = runtime.import_docx(&bytes).expect("import tracked docx");

        let tracked = runtime
            .tracked_view(&imported.doc_handle)
            .expect("tracked_view");
        let BlockNode::Paragraph(tracked_para) = &tracked.canonical.blocks[0].block else {
            panic!("expected paragraph");
        };
        assert!(
            tracked_para
                .segments
                .iter()
                .any(|seg| matches!(seg.status, TrackingStatus::Inserted(_))),
            "tracked_view should preserve inserted segments"
        );
        assert!(
            tracked_para
                .segments
                .iter()
                .any(|seg| matches!(seg.status, TrackingStatus::Deleted(_))),
            "tracked_view should preserve deleted segments"
        );

        let normalized = runtime.view(&imported.doc_handle).expect("view");
        let BlockNode::Paragraph(normalized_para) = &normalized.canonical.blocks[0].block else {
            panic!("expected paragraph");
        };
        assert!(
            normalized_para
                .segments
                .iter()
                .all(|seg| seg.status == TrackingStatus::Normal),
            "view() should normalize tracked changes away"
        );
        let text = normalized_para
            .segments
            .iter()
            .flat_map(|seg| seg.inlines.iter())
            .filter_map(|inline| match inline {
                InlineNode::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert_eq!(text, "The party shall use best efforts to protect data.");
    }

    #[test]
    fn single_document_view_reflects_tracked_edit_from_snapshot_state() {
        let document_xml = wrap_body(
            r#"
    <w:p>
      <w:r><w:t>The liability cap is one million dollars.</w:t></w:r>
    </w:p>"#,
        );
        let bytes = build_docx(&document_xml);
        let runtime = SimpleRuntime::new();
        let imported = runtime.import_docx(&bytes).expect("import clean docx");

        let tx = crate::edit::EditTransaction {
            steps: vec![crate::edit::EditStep::ReplaceParagraphText {
                block_id: NodeId::from("p_1"),
                rationale: Some("lower the liability cap".to_string()),
                replacement_role: None,
                expect: "one million dollars".to_string(),
                semantic_hash: None,
                content: crate::edit::ParagraphContent {
                    fragments: vec![crate::edit::ContentFragment::Text(
                        "The liability cap is five hundred thousand dollars.".to_string(),
                    )],
                },
            }],
            summary: Some("Lower liability cap".to_string()),
            materialization_mode: crate::edit::MaterializationMode::TrackedChange,
            revision: RevisionInfo {
                revision_id: 1,
                author: Some("Stemma".to_string()),
                date: Some("2026-04-09T00:00:00Z".to_string()),
                apply_op_id: None,
            },
        };

        runtime
            .apply_edit(&imported.doc_handle, &tx)
            .expect("apply_edit should succeed");

        let full_doc = runtime
            .single_document_view(&imported.doc_handle)
            .expect("single_document_view");
        let block = &full_doc.blocks[0];
        assert_eq!(block.change_type.as_str(), "modified");
        assert!(
            block
                .segments
                .iter()
                .any(|seg| matches!(seg, crate::InlineChange::Deleted { .. })),
            "single-document projection should show deleted tracked text after apply_edit",
        );
        assert!(
            block
                .segments
                .iter()
                .any(|seg| matches!(seg, crate::InlineChange::Inserted { .. })),
            "single-document projection should show inserted tracked text after apply_edit",
        );
    }

    #[test]
    fn single_document_view_reflects_direct_edit_without_tracked_segments() {
        let document_xml = wrap_body(
            r#"
    <w:p>
      <w:r><w:t>The liability cap is one million dollars.</w:t></w:r>
    </w:p>"#,
        );
        let bytes = build_docx(&document_xml);
        let runtime = SimpleRuntime::new();
        let imported = runtime.import_docx(&bytes).expect("import clean docx");

        let tx = crate::edit::EditTransaction {
            steps: vec![crate::edit::EditStep::ReplaceParagraphText {
                block_id: NodeId::from("p_1"),
                rationale: Some("lower the liability cap".to_string()),
                replacement_role: None,
                expect: "one million dollars".to_string(),
                semantic_hash: None,
                content: crate::edit::ParagraphContent {
                    fragments: vec![crate::edit::ContentFragment::Text(
                        "The liability cap is five hundred thousand dollars.".to_string(),
                    )],
                },
            }],
            summary: Some("Lower liability cap directly".to_string()),
            materialization_mode: crate::edit::MaterializationMode::Direct,
            revision: RevisionInfo {
                revision_id: 1,
                author: Some("Stemma".to_string()),
                date: Some("2026-04-09T00:00:00Z".to_string()),
                apply_op_id: None,
            },
        };

        runtime
            .apply_edit(&imported.doc_handle, &tx)
            .expect("apply_edit should succeed");

        let tracked = runtime
            .tracked_view(&imported.doc_handle)
            .expect("tracked_view after direct edit");
        let BlockNode::Paragraph(tracked_para) = &tracked.canonical.blocks[0].block else {
            panic!("expected paragraph");
        };
        assert!(
            tracked_para
                .segments
                .iter()
                .all(|seg| seg.status == TrackingStatus::Normal),
            "direct apply must not leave tracked segments behind",
        );

        let full_doc = runtime
            .single_document_view(&imported.doc_handle)
            .expect("single_document_view");
        let block = &full_doc.blocks[0];
        assert_eq!(block.change_type.as_str(), "unchanged");
        assert!(
            block
                .segments
                .iter()
                .all(|seg| matches!(seg, crate::InlineChange::Unchanged { .. })),
            "single-document projection should remain clean after direct apply",
        );
    }

    #[test]
    fn apply_edit_export_emits_well_formed_tracked_change_wrappers() {
        fn local_name(name: &str) -> &str {
            match name.rsplit_once(':') {
                Some((_, local)) => local,
                None => name,
            }
        }

        fn find_nested_same_type(root: &Element, tag: &str) -> Option<String> {
            fn walk(el: &Element, tag: &str, inside_parent: bool, path: &str) -> Option<String> {
                let name = local_name(&el.name);
                let current_path = format!("{path}/{}", el.name);

                if name == tag {
                    if inside_parent {
                        return Some(current_path);
                    }
                    for child in &el.children {
                        if let XMLNode::Element(child_el) = child
                            && let Some(found) = walk(child_el, tag, true, &current_path)
                        {
                            return Some(found);
                        }
                    }
                    return None;
                }

                for child in &el.children {
                    if let XMLNode::Element(child_el) = child
                        && let Some(found) = walk(child_el, tag, inside_parent, &current_path)
                    {
                        return Some(found);
                    }
                }
                None
            }

            walk(root, tag, false, "")
        }

        fn find_first_local<'a>(el: &'a Element, tag: &str) -> Option<&'a Element> {
            if local_name(&el.name) == tag {
                return Some(el);
            }
            for child in &el.children {
                if let XMLNode::Element(child_el) = child
                    && let Some(found) = find_first_local(child_el, tag)
                {
                    return Some(found);
                }
            }
            None
        }

        fn find_t_inside_del(root: &Element) -> Option<String> {
            fn search(el: &Element, inside_del: bool, path: &str) -> Option<String> {
                let name = local_name(&el.name);
                let current_path = format!("{path}/{}", el.name);
                let now_inside_del = inside_del || name == "del";

                if now_inside_del && name == "t" {
                    return Some(current_path);
                }

                for child in &el.children {
                    if let XMLNode::Element(child_el) = child
                        && let Some(found) = search(child_el, now_inside_del, &current_path)
                    {
                        return Some(found);
                    }
                }
                None
            }

            search(root, false, "")
        }

        fn has_deltext_inside_del(root: &Element) -> bool {
            fn search(el: &Element, inside_del: bool) -> bool {
                let name = local_name(&el.name);
                let now_inside_del = inside_del || name == "del";

                if now_inside_del && name == "delText" {
                    return true;
                }

                for child in &el.children {
                    if let XMLNode::Element(child_el) = child
                        && search(child_el, now_inside_del)
                    {
                        return true;
                    }
                }
                false
            }

            search(root, false)
        }

        let document_xml = wrap_body(
            r#"
    <w:p>
      <w:r><w:t>The tenant must maintain insurance coverage.</w:t></w:r>
    </w:p>"#,
        );
        let bytes = build_docx(&document_xml);
        let runtime = SimpleRuntime::new();
        let imported = runtime.import_docx(&bytes).expect("import clean docx");

        let tx = crate::edit::EditTransaction {
            steps: vec![crate::edit::EditStep::ReplaceParagraphText {
                block_id: NodeId::from("p_1"),
                rationale: Some("tighten insurance language".to_string()),
                replacement_role: None,
                expect: "The tenant must maintain insurance coverage.".to_string(),
                semantic_hash: None,
                content: crate::edit::ParagraphContent {
                    fragments: vec![crate::edit::ContentFragment::Text(
                        "The obligation is suspended.".to_string(),
                    )],
                },
            }],
            summary: Some("Tighten insurance language".to_string()),
            materialization_mode: crate::edit::MaterializationMode::TrackedChange,
            revision: RevisionInfo {
                revision_id: 7,
                author: Some("Stemma".to_string()),
                date: Some("2026-04-09T00:00:00Z".to_string()),
                apply_op_id: None,
            },
        };

        runtime
            .apply_edit(&imported.doc_handle, &tx)
            .expect("apply_edit should succeed");
        let exported = runtime
            .export_docx(&imported.doc_handle, ExportMode::Redline)
            .expect("export_docx should succeed");

        let archive = DocxArchive::read(&exported).expect("read exported docx");
        let root = word_xml::parse_document_xml(
            archive
                .get("word/document.xml")
                .expect("exported docx should contain word/document.xml"),
        )
        .expect("parse exported document.xml");
        let body = body_element(&root).expect("document should have body");
        let paragraph = body
            .children
            .iter()
            .find_map(|child| match child {
                XMLNode::Element(el) if local_name(&el.name) == "p" => Some(el),
                _ => None,
            })
            .expect("exported document should contain a paragraph");

        assert!(
            find_first_local(paragraph, "del").is_some(),
            "exported paragraph should contain w:del",
        );
        assert!(
            find_first_local(paragraph, "ins").is_some(),
            "exported paragraph should contain w:ins",
        );
        assert!(
            find_nested_same_type(paragraph, "del").is_none(),
            "w:del must not contain nested revision wrappers",
        );
        assert!(
            find_nested_same_type(paragraph, "ins").is_none(),
            "w:ins must not contain nested revision wrappers",
        );
        assert!(
            find_t_inside_del(paragraph).is_none(),
            "deleted tracked text must not serialize as w:t inside w:del",
        );
        assert!(
            has_deltext_inside_del(paragraph),
            "deleted tracked text must serialize as w:delText",
        );
    }

    #[test]
    fn test_strip_parenthesized_letter() {
        let mut inlines = text_inlines("(a)\tEvents of Default");
        let prefix = prefix_tuple(strip_literal_prefix(&mut inlines));
        assert_eq!(prefix, Some(("(a)".to_string(), false, true)));
        assert_eq!(extract_inline_text_simple(&inlines), "Events of Default");
    }

    #[test]
    fn test_strip_parenthesized_roman() {
        let mut inlines = text_inlines("(iv)\tDefinitions");
        let prefix = prefix_tuple(strip_literal_prefix(&mut inlines));
        assert_eq!(prefix, Some(("(iv)".to_string(), false, true)));
        assert_eq!(extract_inline_text_simple(&inlines), "Definitions");
    }

    #[test]
    fn test_strip_parenthesized_number() {
        let mut inlines = text_inlines("(12)\tSection twelve");
        let prefix = prefix_tuple(strip_literal_prefix(&mut inlines));
        assert_eq!(prefix, Some(("(12)".to_string(), false, true)));
        assert_eq!(extract_inline_text_simple(&inlines), "Section twelve");
    }

    #[test]
    fn test_strip_digit_period() {
        let mut inlines = text_inlines("1.\tFirst item");
        let prefix = prefix_tuple(strip_literal_prefix(&mut inlines));
        assert_eq!(prefix, Some(("1.".to_string(), false, true)));
        assert_eq!(extract_inline_text_simple(&inlines), "First item");
    }

    #[test]
    fn test_strip_letter_paren() {
        let mut inlines = text_inlines("a)\tLetter item");
        let prefix = prefix_tuple(strip_literal_prefix(&mut inlines));
        assert_eq!(prefix, Some(("a)".to_string(), false, true)));
        assert_eq!(extract_inline_text_simple(&inlines), "Letter item");
    }

    #[test]
    fn test_strip_space_separator() {
        let mut inlines = text_inlines("(a) Space separator");
        let prefix = prefix_tuple(strip_literal_prefix(&mut inlines));
        assert_eq!(prefix, Some(("(a)".to_string(), false, false)));
        assert_eq!(extract_inline_text_simple(&inlines), "Space separator");
    }

    #[test]
    fn test_no_prefix() {
        let mut inlines = text_inlines("No prefix here");
        let prefix = prefix_tuple(strip_literal_prefix(&mut inlines));
        assert_eq!(prefix, None);
        assert_eq!(extract_inline_text_simple(&inlines), "No prefix here");
    }

    #[test]
    fn test_empty_inlines() {
        let mut inlines: Vec<InlineNode> = vec![];
        let prefix = prefix_tuple(strip_literal_prefix(&mut inlines));
        assert_eq!(prefix, None);
    }

    #[test]
    fn test_invalid_enumerator_too_long() {
        let mut inlines = text_inlines("(toolong)\tNot");
        let prefix = prefix_tuple(strip_literal_prefix(&mut inlines));
        assert_eq!(prefix, None);
    }

    #[test]
    fn test_multi_run_prefix() {
        let mut inlines = multi_run_inlines(&["(a)", "\t", "Body text"]);
        let prefix = prefix_tuple(strip_literal_prefix(&mut inlines));
        assert_eq!(prefix, Some(("(a)".to_string(), false, true)));
        assert_eq!(extract_inline_text_simple(&inlines), "Body text");
    }

    #[test]
    fn test_prefix_only_no_body() {
        // Don't strip if "prefix" IS the entire content
        let mut inlines = text_inlines("(a)");
        let prefix = prefix_tuple(strip_literal_prefix(&mut inlines));
        assert_eq!(prefix, None);
        assert_eq!(extract_inline_text_simple(&inlines), "(a)");
    }

    #[test]
    fn test_strip_uppercase_letter_period() {
        let mut inlines = text_inlines("A.\tFirst clause");
        let prefix = prefix_tuple(strip_literal_prefix(&mut inlines));
        assert_eq!(prefix, Some(("A.".to_string(), false, true)));
        assert_eq!(extract_inline_text_simple(&inlines), "First clause");
    }

    #[test]
    fn test_strip_double_digit() {
        let mut inlines = text_inlines("12.\tTwelfth item");
        let prefix = prefix_tuple(strip_literal_prefix(&mut inlines));
        assert_eq!(prefix, Some(("12.".to_string(), false, true)));
        assert_eq!(extract_inline_text_simple(&inlines), "Twelfth item");
    }

    #[test]
    fn test_strip_double_letter_parens() {
        let mut inlines = text_inlines("(aa)\tDouble letter");
        let prefix = prefix_tuple(strip_literal_prefix(&mut inlines));
        assert_eq!(prefix, Some(("(aa)".to_string(), false, true)));
        assert_eq!(extract_inline_text_simple(&inlines), "Double letter");
    }

    #[test]
    fn test_no_strip_without_separator() {
        // Missing tab/space after prefix — should NOT match
        let mut inlines = text_inlines("(a)NoSpace");
        let prefix = prefix_tuple(strip_literal_prefix(&mut inlines));
        assert_eq!(prefix, None);
    }

    #[test]
    fn test_multibyte_not_prefix() {
        // Smart quotes and other multibyte chars should not match
        let mut inlines = text_inlines("\u{201c}some quoted text\u{201d}");
        let prefix = prefix_tuple(strip_literal_prefix(&mut inlines));
        assert_eq!(prefix, None);
    }

    #[test]
    fn test_digit_paren_prefix() {
        let mut inlines = text_inlines("3)\tThird item");
        let prefix = prefix_tuple(strip_literal_prefix(&mut inlines));
        assert_eq!(prefix, Some(("3)".to_string(), false, true)));
        assert_eq!(extract_inline_text_simple(&inlines), "Third item");
    }

    #[test]
    fn test_prefix_with_tab_only_after() {
        // Just a prefix + tab with no body = entire content is prefix, don't strip
        let mut inlines = text_inlines("(a)\t");
        let prefix = prefix_tuple(strip_literal_prefix(&mut inlines));
        assert_eq!(prefix, None);
    }

    #[test]
    fn test_strip_prefix_with_leading_whitespace() {
        // Word sometimes puts spaces/tabs before the manual enumeration
        let mut inlines = text_inlines("\t(e)\tIn the event");
        let prefix = prefix_tuple(strip_literal_prefix(&mut inlines));
        assert_eq!(prefix, Some(("(e)".to_string(), true, true)));
        assert_eq!(extract_inline_text_simple(&inlines), "In the event");
    }

    #[test]
    fn test_strip_prefix_with_leading_and_trailing_spaces() {
        let mut inlines = text_inlines("    (e)    In the event");
        let prefix = prefix_tuple(strip_literal_prefix(&mut inlines));
        assert_eq!(prefix, Some(("(e)".to_string(), false, false)));
        assert_eq!(extract_inline_text_simple(&inlines), "In the event");
    }

    // --- rPr element ordering per ECMA-376 §17.3.2.28 ---

    /// Regression test: rPr children must follow the conventional OOXML order.
    ///
    /// ECMA-376 Part 1 §17.3.2.28 defines CT_RPr via EG_RPrBase as xsd:choice
    /// with maxOccurs="unbounded" (technically any order is schema-valid).
    /// However, Word always emits elements in a conventional order and some
    /// consumers depend on it.
    ///
    /// Conventional order (positions from EG_RPrBase schema):
    ///   rStyle(1) → rFonts(2) → b(3) → i(5) → caps(7) → smallCaps(8) →
    ///   strike(9) → dstrike(10) → outline(11) → shadow(12) → emboss(13) →
    ///   imprint(14) → vanish(17) → color(19) → spacing(20) → sz(24) →
    ///   highlight(26) → u(27) → vertAlign(32) → lang(36) → rPrChange(40)
    #[test]
    fn rpr_element_ordering_follows_ooxml_convention() {
        use crate::domain::Mark;

        let marks = vec![Mark::Bold, Mark::Italic, Mark::Underline];
        let style_props = StyleProps {
            char_style_id: Some("Emphasis".into()),
            font_family: Some("Arial".into()),
            color: Some("FF0000".into()),
            font_size: Some(24),
            highlight: Some(HighlightColor::Yellow),
            lang: Some("en-US".into()),
            char_spacing: Some(10),
            strike: MarkValue::On,
            ..StyleProps::default()
        };

        let run = build_text_run("test", &marks, &style_props, false, None, &mut 100);

        // Extract the rPr element
        let rpr = run
            .children
            .iter()
            .find_map(|n| {
                if let XMLNode::Element(el) = n {
                    if el.name == "rPr" { Some(el) } else { None }
                } else {
                    None
                }
            })
            .expect("run should have rPr");

        // Extract child element names
        let child_names: Vec<&str> = rpr
            .children
            .iter()
            .filter_map(|n| {
                if let XMLNode::Element(el) = n {
                    Some(el.name.as_str())
                } else {
                    None
                }
            })
            .collect();

        // Expected order per ECMA-376 conventional ordering
        let expected = vec![
            "rStyle",    // position 1
            "rFonts",    // position 2
            "b",         // position 3
            "i",         // position 5
            "strike",    // position 9
            "color",     // position 19
            "spacing",   // position 20
            "sz",        // position 24
            "highlight", // position 26
            "u",         // position 27
            "lang",      // position 36
        ];

        assert_eq!(
            child_names, expected,
            "rPr children must follow ECMA-376 §17.3.2.28 conventional order"
        );
    }

    /// Regression test: boolean marks emitted in spec order regardless of input order.
    #[test]
    fn rpr_boolean_marks_emitted_in_spec_order() {
        use crate::domain::Mark;

        // Bold + Italic stay as marks; all 9 boolean marks are now on StyleProps.
        let marks = vec![Mark::Italic, Mark::Bold];
        let style_props = StyleProps {
            vanish: MarkValue::On,
            shadow: MarkValue::On,
            outline: MarkValue::On,
            imprint: MarkValue::On,
            emboss: MarkValue::On,
            double_strike: MarkValue::On,
            strike: MarkValue::On,
            small_caps: MarkValue::On,
            caps: MarkValue::On,
            ..StyleProps::default()
        };

        let run = build_text_run("test", &marks, &style_props, false, None, &mut 100);

        let rpr = run
            .children
            .iter()
            .find_map(|n| {
                if let XMLNode::Element(el) = n {
                    if el.name == "rPr" { Some(el) } else { None }
                } else {
                    None
                }
            })
            .expect("run should have rPr");

        let child_names: Vec<&str> = rpr
            .children
            .iter()
            .filter_map(|n| {
                if let XMLNode::Element(el) = n {
                    Some(el.name.as_str())
                } else {
                    None
                }
            })
            .collect();

        let expected = vec![
            "b",
            "i",
            "caps",
            "smallCaps",
            "strike",
            "dstrike",
            "outline",
            "shadow",
            "emboss",
            "imprint",
            "vanish",
        ];
        assert_eq!(
            child_names, expected,
            "boolean marks must follow spec order regardless of input order"
        );
    }

    // --- explicit-off roundtrip (§17.3.2.37) ---

    /// When a run carries `strike: MarkValue::Off`, the serializer must emit
    /// `<w:strike w:val="0"/>` so Word does not fall back to the character
    /// style's `<w:strike/>`. This is the core invariant for the
    /// strikethrough-leaking fix: explicit-off must survive the pipeline,
    /// not collapse into absent/inherit.
    #[test]
    fn rpr_explicit_off_emits_val_false() {
        let marks = vec![];
        let style_props = StyleProps {
            strike: MarkValue::Off,
            caps: MarkValue::Off,
            ..StyleProps::default()
        };

        let run = build_text_run("test", &marks, &style_props, false, None, &mut 100);

        let rpr = run
            .children
            .iter()
            .find_map(|n| {
                if let XMLNode::Element(el) = n {
                    if el.name == "rPr" { Some(el) } else { None }
                } else {
                    None
                }
            })
            .expect("run should have rPr");

        // Find <w:strike w:val="0"/>
        let strike_el = rpr
            .children
            .iter()
            .find_map(|n| {
                if let XMLNode::Element(el) = n {
                    if el.name == "strike" { Some(el) } else { None }
                } else {
                    None
                }
            })
            .expect("rPr must contain <w:strike> when strike=Off");

        let val = strike_el
            .attributes
            .iter()
            .find(|(k, _)| k.local_name == "val")
            .map(|(_, v)| v.as_str());
        assert_eq!(
            val,
            Some("0"),
            "w:strike must have w:val=\"0\" for explicit-off"
        );

        // Find <w:caps w:val="0"/>
        let caps_el = rpr
            .children
            .iter()
            .find_map(|n| {
                if let XMLNode::Element(el) = n {
                    if el.name == "caps" { Some(el) } else { None }
                } else {
                    None
                }
            })
            .expect("rPr must contain <w:caps> when caps=Off");

        let val = caps_el
            .attributes
            .iter()
            .find(|(k, _)| k.local_name == "val")
            .map(|(_, v)| v.as_str());
        assert_eq!(
            val,
            Some("0"),
            "w:caps must have w:val=\"0\" for explicit-off"
        );
    }

    /// MarkValue::Inherit must NOT emit anything — it means "no direct
    /// override, inherit from style chain". This distinguishes it from Off.
    #[test]
    fn rpr_inherit_emits_nothing() {
        let marks = vec![];
        let style_props = StyleProps {
            strike: MarkValue::Inherit,
            ..StyleProps::default()
        };

        let run = build_text_run("test", &marks, &style_props, false, None, &mut 100);

        // With all-default style_props, there should be no rPr at all (or empty rPr).
        let has_strike = run.children.iter().any(|n| {
            if let XMLNode::Element(el) = n
                && el.name == "rPr"
            {
                return el
                    .children
                    .iter()
                    .any(|c| matches!(c, XMLNode::Element(e) if e.name == "strike"));
            }
            false
        });
        assert!(
            !has_strike,
            "Inherit must not emit <w:strike> — it means 'no override'"
        );
    }

    // --- rPrChange inside deleted runs ---

    /// Verify that `build_text_run` emits `w:rPrChange` inside `w:rPr` even
    /// when `deleted_text = true`. Per OOXML §17.13.5.31, the previous
    /// formatting state must be recorded regardless of tracked-deletion
    /// context.
    #[test]
    fn rpr_change_emitted_inside_deleted_run() {
        use crate::domain::Mark;

        let fc = FormattingChange {
            previous_marks: vec![],
            previous_style_props: StyleProps::default(),
            previous_rpr_authored: crate::domain::RunRprAuthored::default(),
            revision_id: 77,
            author: "TestAuthor".to_string(),
            date: Some("2024-01-01T00:00:00Z".to_string()),
        };

        let run = build_text_run(
            "deleted bold text",
            &[Mark::Bold],
            &StyleProps::default(),
            true,      // deleted_text
            Some(&fc), // formatting_change present
            &mut 100,
        );

        // The run must have an rPr child.
        let rpr = run
            .children
            .iter()
            .find_map(|n| match n {
                XMLNode::Element(el) if el.name == "rPr" => Some(el),
                _ => None,
            })
            .expect("deleted run with formatting change must have w:rPr");

        // The rPr must contain rPrChange.
        let rpr_change = rpr
            .children
            .iter()
            .find_map(|n| match n {
                XMLNode::Element(el) if el.name == "rPrChange" => Some(el),
                _ => None,
            })
            .expect("w:rPr must contain w:rPrChange for a deleted run with formatting change");

        // rPrChange must carry the author attribute.
        let author_attr = attr_get(rpr_change, "w:author").expect("rPrChange must have w:author");
        assert_eq!(author_attr, "TestAuthor");

        // rPrChange must carry the date attribute.
        let date_attr = attr_get(rpr_change, "w:date").expect("rPrChange must have w:date");
        assert_eq!(date_attr, "2024-01-01T00:00:00Z");

        // rPrChange must contain an rPr child (the previous formatting state).
        let inner_rpr = rpr_change
            .children
            .iter()
            .find_map(|n| match n {
                XMLNode::Element(el) if el.name == "rPr" => Some(el),
                _ => None,
            })
            .expect("rPrChange must contain a child w:rPr (previous formatting)");

        // The previous formatting had no marks, so inner rPr should have no
        // boolean-mark children (empty previous state).
        let inner_child_names: Vec<&str> = inner_rpr
            .children
            .iter()
            .filter_map(|n| match n {
                XMLNode::Element(el) => Some(el.name.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            inner_child_names.is_empty(),
            "previous rPr should be empty when previous_marks is empty, got: {inner_child_names:?}"
        );

        // The run must use w:delText (not w:t) since deleted_text = true.
        let has_del_text = run
            .children
            .iter()
            .any(|n| matches!(n, XMLNode::Element(el) if el.name == "delText"));
        assert!(has_del_text, "deleted run must use w:delText, not w:t");

        // The run must NOT have w:t.
        let has_t = run
            .children
            .iter()
            .any(|n| matches!(n, XMLNode::Element(el) if el.name == "t"));
        assert!(!has_t, "deleted run must not have w:t");
    }

    /// Verify the full serialization path: a paragraph with a block-level
    /// deletion and inline formatting change produces `w:rPrChange` inside
    /// the `w:del` container.
    #[test]
    fn deleted_paragraph_with_formatting_change_emits_rpr_change() {
        use crate::domain::Mark;

        let mut para = make_test_paragraph();
        para.segments = vec![TrackedSegment {
            status: TrackingStatus::Normal,
            inlines: vec![InlineNode::from(TextNode {
                id: NodeId::from("t1"),
                text_role: None,
                text: "reformatted text".to_string(),
                marks: vec![Mark::Bold],
                style_props: StyleProps::default(),
                rpr_authored: crate::domain::RunRprAuthored::default(),
                formatting_change: Some(FormattingChange {
                    previous_marks: vec![],
                    previous_style_props: StyleProps::default(),
                    previous_rpr_authored: crate::domain::RunRprAuthored::default(),
                    revision_id: 78,
                    author: "Author1".to_string(),
                    date: Some("2024-06-01T00:00:00Z".to_string()),
                }),
            })],
        }];

        // Serialize as a block-level deletion (entire paragraph deleted).
        let block_status = TrackingStatus::Deleted(RevisionInfo {
            revision_id: 0,
            author: Some("Deleter".to_string()),
            date: Some("2024-06-02T00:00:00Z".to_string()),
            apply_op_id: None,
        });
        let p_el = serialize_paragraph_node(
            &para,
            Some(&block_status),
            false,
            &mut 100,
            &crate::serialize::BookmarkIdPolicy::default(),
            "base",
            None,
        )
        .expect("serialization should succeed");

        // Find the w:del container inside the paragraph.
        let del = p_el
            .children
            .iter()
            .find_map(|n| match n {
                XMLNode::Element(el) if el.name == "del" => Some(el),
                _ => None,
            })
            .expect("paragraph should contain a w:del element");

        // Find the w:r inside w:del.
        let run = del
            .children
            .iter()
            .find_map(|n| match n {
                XMLNode::Element(el) if el.name == "r" => Some(el),
                _ => None,
            })
            .expect("w:del should contain a w:r element");

        // Find w:rPr inside the run.
        let rpr = run
            .children
            .iter()
            .find_map(|n| match n {
                XMLNode::Element(el) if el.name == "rPr" => Some(el),
                _ => None,
            })
            .expect("run inside w:del must have w:rPr");

        // Verify w:rPrChange is present.
        let rpr_change = rpr
            .children
            .iter()
            .find_map(|n| match n {
                XMLNode::Element(el) if el.name == "rPrChange" => Some(el),
                _ => None,
            })
            .expect("w:rPr inside deleted run must contain w:rPrChange");

        assert_eq!(
            attr_get(rpr_change, "w:author").map(String::as_str),
            Some("Author1"),
            "rPrChange author should come from the formatting change, not the deletion"
        );

        // Verify the run uses w:delText.
        let has_del_text = run
            .children
            .iter()
            .any(|n| matches!(n, XMLNode::Element(el) if el.name == "delText"));
        assert!(has_del_text, "run inside w:del must use w:delText");
    }

    // --- Spacing serialization roundtrip ---

    /// Helper: make a minimal ParagraphNode for serialization tests.
    fn make_test_paragraph() -> ParagraphNode {
        ParagraphNode {
            id: NodeId("p1".into()),
            style_id: None,
            align: None,
            has_direct_align: false,
            indent: None,
            has_direct_indent: false,
            authored_indent: None,
            spacing: None,
            has_direct_spacing: false,
            authored_spacing: None,
            borders: None,
            keep_next: None,
            keep_lines: None,
            page_break_before: false,
            widow_control: None,
            contextual_spacing: None,
            shading: None,
            has_direct_keep_next: true,
            has_direct_keep_lines: true,
            has_direct_page_break_before: true,
            has_direct_widow_control: true,
            has_direct_contextual_spacing: true,
            has_direct_shading: true,
            has_direct_borders: true,
            tab_stops: vec![],
            effective_tab_stops_rel: vec![],
            segments: vec![],
            block_text_hash: None,
            numbering: None,
            has_direct_numbering: true,
            numbering_suppressed: false,
            materialized_numbering: None,
            rendered_text: None,
            literal_prefix: None,
            literal_prefix_marks: Vec::new(),
            literal_prefix_style_props: crate::domain::StyleProps::default(),
            literal_prefix_rpr_authored: crate::domain::RunRprAuthored::default(),
            literal_prefix_leading_rpr: None,
            literal_prefix_trailing_rpr: None,
            literal_prefix_leading_tab_twips: None,
            literal_prefix_leading_tab_count: 0,
            literal_prefix_leading_ws: String::new(),
            literal_prefix_trailing_ws: String::new(),
            literal_prefix_has_trailing_tab: false,
            literal_prefix_trailing_tab_stop_twips: None,
            outline_lvl: None,
            heading_level: None,
            para_mark_status: None,
            paragraph_mark_marks: vec![],
            paragraph_mark_style_props: StyleProps::default(),
            paragraph_mark_rpr_off: Default::default(),
            para_split: false,
            section_property_change: None,
            formatting_change: None,
            section_properties: None,
            mirror_indents: None,
            auto_space_de: None,
            auto_space_dn: None,
            bidi: None,
            text_alignment: None,
            suppress_auto_hyphens: None,
            snap_to_grid: None,
            overflow_punct: None,
            adjust_right_ind: None,
            word_wrap: None,
            frame_pr: None,
            para_id: None,
            text_id: None,
            text_direction: None,
            cnf_style: None,
            preserved_ppr: Vec::new(),
        }
    }

    #[test]
    fn build_paragraph_properties_spacing_roundtrip() {
        use crate::domain::{LineSpacingRule, ParagraphSpacing};

        let mut para = make_test_paragraph();
        para.spacing = Some(ParagraphSpacing {
            before: Some(120),
            after: Some(240),
            before_lines: None,
            after_lines: None,
            before_autospacing: None,
            after_autospacing: None,
            line: Some(360),
            line_rule: Some(LineSpacingRule::Exact),
        });
        para.has_direct_spacing = true;
        let ppr = build_paragraph_properties(&para, &mut 100, None).expect("pPr should be built");

        // Serialize to XML string for inspection
        let mut buf = Vec::new();
        ppr.write(&mut buf).unwrap();
        let xml = String::from_utf8(buf).unwrap();

        assert!(
            xml.contains("before=\"120\""),
            "before should be serialized: {xml}"
        );
        assert!(
            xml.contains("after=\"240\""),
            "after should be serialized: {xml}"
        );
        assert!(
            xml.contains("line=\"360\""),
            "line should be serialized: {xml}"
        );
        assert!(
            xml.contains("lineRule=\"exact\""),
            "lineRule should be serialized: {xml}"
        );
    }

    // --- Border serialization roundtrip ---

    #[test]
    fn build_paragraph_properties_borders_roundtrip() {
        use crate::domain::ParagraphBorders;

        let mut para = make_test_paragraph();
        para.borders = Some(ParagraphBorders {
            top: Some(Border {
                style: BorderStyle::Single,
                color: Some("FF0000".into()),
                size: Some(4),
                space: None,
                extra_attrs: Vec::new(),
            }),
            bottom: Some(Border {
                style: BorderStyle::Double,
                color: None,
                size: Some(8),
                space: None,
                extra_attrs: Vec::new(),
            }),
            left: None,
            right: None,
            between: None,
            bar: None,
        });
        let ppr = build_paragraph_properties(&para, &mut 100, None).expect("pPr should be built");

        let mut buf = Vec::new();
        ppr.write(&mut buf).unwrap();
        let xml = String::from_utf8(buf).unwrap();

        // Verify pBdr container exists with top and bottom edges
        assert!(xml.contains("pBdr"), "should contain pBdr: {xml}");
        assert!(
            xml.contains("\"single\""),
            "top should have single style: {xml}"
        );
        assert!(xml.contains("FF0000"), "top should have color: {xml}");
        assert!(
            xml.contains("\"double\""),
            "bottom should have double style: {xml}"
        );
    }

    /// `build_paragraph_properties` must emit the pBdr edges in the fixed Annex A
    /// CT_PBdr order (top, left, bottom, right, between, bar) regardless of the
    /// order the `ParagraphBorders` fields were populated. This is the rebuild-path
    /// guarantee the deleted `spec_para_borders_shading` ordering tests relied on
    /// (those asserted reordering on an UNTOUCHED reserialize, which is verbatim —
    /// the real ordering logic runs here, when an edit rebuilds the pPr).
    #[test]
    fn build_paragraph_properties_pbdr_edges_in_annex_a_order() {
        use crate::domain::ParagraphBorders;

        // Populate all six edges; the struct-literal order below is deliberately
        // NOT the schema order (between/bar/right named before top) to prove the
        // serializer imposes the canonical order rather than echoing field order.
        let edge = |c: &str| {
            Some(Border {
                style: BorderStyle::Single,
                color: Some(c.into()),
                size: Some(4),
                space: None,
                extra_attrs: Vec::new(),
            })
        };
        let mut para = make_test_paragraph();
        para.borders = Some(ParagraphBorders {
            between: edge("444444"),
            bar: edge("555555"),
            right: edge("333333"),
            bottom: edge("222222"),
            left: edge("111111"),
            top: edge("000000"),
        });

        let ppr = build_paragraph_properties(&para, &mut 100, None).expect("pPr should be built");
        let mut buf = Vec::new();
        ppr.write(&mut buf).unwrap();
        let xml = String::from_utf8(buf).unwrap();

        // The six edge start-tags must appear in CT_PBdr sequence order. Find each
        // within the pBdr element and assert strictly ascending byte positions (the
        // pBdr open tag precedes every edge, so the first edge's position is > 0).
        let pbdr_start = xml.find("<w:pBdr").expect("pBdr present");
        let pbdr = &xml[pbdr_start..];
        let mut last: Option<usize> = None;
        for tag in [
            "<w:top",
            "<w:left",
            "<w:bottom",
            "<w:right",
            "<w:between",
            "<w:bar",
        ] {
            let pos = pbdr
                .find(tag)
                .unwrap_or_else(|| panic!("pBdr must contain {tag}: {pbdr}"));
            if let Some(prev) = last {
                assert!(
                    pos > prev,
                    "pBdr edges must be in CT_PBdr order (top, left, bottom, right, between, bar); \
                     {tag} appeared out of order at {pos} (previous edge at {prev}): {pbdr}"
                );
            }
            last = Some(pos);
        }
    }

    // --- sectPr roundtrip through ParagraphNode serialization ---

    #[test]
    fn build_paragraph_properties_sect_pr_roundtrip() {
        let mut para = make_test_paragraph();
        para.section_properties = Some(crate::domain::SectionProperties {
            page_width: Some(12240),
            page_height: Some(15840),
            margin_top: Some(1440),
            margin_right: Some(1440),
            margin_bottom: Some(1440),
            margin_left: Some(1440),
            ..Default::default()
        });
        let ppr = build_paragraph_properties(&para, &mut 100, None)
            .expect("pPr should be built when sectPr present");

        // Serialize the built pPr to XML and check that sectPr is present.
        let mut buf = Vec::new();
        ppr.write(&mut buf).unwrap();
        let xml = String::from_utf8(buf).unwrap();

        assert!(
            xml.contains("sectPr"),
            "sectPr should be present in serialized pPr: {xml}"
        );
        assert!(
            xml.contains("pgSz"),
            "pgSz child should survive roundtrip: {xml}"
        );
        assert!(
            xml.contains("12240"),
            "page width should survive roundtrip: {xml}"
        );
        assert!(
            xml.contains("pgMar"),
            "pgMar child should survive roundtrip: {xml}"
        );
    }

    // --- cnfStyle roundtrip ---

    #[test]
    fn build_paragraph_properties_cnf_style_roundtrip() {
        use crate::domain::CnfStyle;

        let mut para = make_test_paragraph();
        para.cnf_style = Some(CnfStyle {
            val: Some("100000000000".to_string()),
            first_row: true,
            last_row: false,
            first_column: false,
            last_column: false,
            odd_v_band: false,
            even_v_band: false,
            odd_h_band: false,
            even_h_band: false,
            first_row_first_column: false,
            first_row_last_column: false,
            last_row_first_column: false,
            last_row_last_column: false,
        });
        let ppr = build_paragraph_properties(&para, &mut 100, None).expect("pPr with cnfStyle");
        let mut buf = Vec::new();
        ppr.write(&mut buf).unwrap();
        let xml = String::from_utf8(buf).unwrap();

        assert!(
            xml.contains("cnfStyle"),
            "cnfStyle should be serialized: {xml}"
        );
        assert!(xml.contains("100000000000"), "val should roundtrip: {xml}");
        assert!(
            xml.contains("firstRow"),
            "firstRow attr should be present: {xml}"
        );
    }

    // --- footnotePr / endnotePr roundtrip ---

    #[test]
    fn section_properties_footnote_pr_roundtrip() {
        use crate::domain::{
            NotePosition, NoteProperties, NumberFormat, RestartRule, SectionProperties,
        };

        let sp = SectionProperties {
            page_width: None,
            page_height: None,
            orientation: None,
            columns: None,
            column_space: None,
            column_defs: vec![],
            margin_top: None,
            margin_bottom: None,
            margin_left: None,
            margin_right: None,
            header_distance: None,
            footer_distance: None,
            gutter: None,
            rtl_gutter: None,
            section_type: None,
            page_borders: None,
            line_numbering: None,
            v_align: None,
            text_direction: None,
            page_number_type: None,
            doc_grid_type: None,
            doc_grid_line_pitch: None,
            doc_grid_char_space: None,
            title_page: None,
            bidi: None,
            form_prot: None,
            no_endnote: None,
            paper_size_code: None,
            column_separator: None,
            equal_width: None,
            footnote_pr: Some(NoteProperties {
                position: Some(NotePosition::BeneathText),
                num_fmt: Some(NumberFormat::LowerRoman),
                num_start: Some(2),
                num_restart: Some(RestartRule::EachSect),
            }),
            endnote_pr: Some(NoteProperties {
                position: Some(NotePosition::DocEnd),
                num_fmt: Some(NumberFormat::Decimal),
                num_start: None,
                num_restart: None,
            }),
            header_refs: vec![],
            footer_refs: vec![],
            paper_source: None,
            printer_settings_rid: None,
        };

        let el = section_properties_to_element(&sp, None, None, None);
        let mut buf = Vec::new();
        el.write(&mut buf).unwrap();
        let xml = String::from_utf8(buf).unwrap();

        assert!(
            xml.contains("footnotePr"),
            "footnotePr should be serialized: {xml}"
        );
        assert!(
            xml.contains("endnotePr"),
            "endnotePr should be serialized: {xml}"
        );
        assert!(
            xml.contains("beneathText"),
            "footnote position should roundtrip: {xml}"
        );
        assert!(
            xml.contains("lowerRoman"),
            "footnote numFmt should roundtrip: {xml}"
        );
        assert!(
            xml.contains("numStart"),
            "footnote numStart should roundtrip: {xml}"
        );
        assert!(
            xml.contains("docEnd"),
            "endnote position should roundtrip: {xml}"
        );

        // Now parse it back
        let parsed = crate::word_ir::parse_section_properties(&el, &Default::default());
        let fp = parsed.footnote_pr.expect("footnote_pr should parse back");
        assert_eq!(fp.position, Some(NotePosition::BeneathText));
        assert_eq!(fp.num_fmt, Some(NumberFormat::LowerRoman));
        assert_eq!(fp.num_start, Some(2));
        assert_eq!(fp.num_restart, Some(RestartRule::EachSect));

        let ep = parsed.endnote_pr.expect("endnote_pr should parse back");
        assert_eq!(ep.position, Some(NotePosition::DocEnd));
        assert_eq!(ep.num_fmt, Some(NumberFormat::Decimal));
    }

    // --- numId remap walks tables and story parts ---

    /// Helper: make a ParagraphNode with numbering info.
    fn make_numbered_paragraph(id: &str, num_id: u32, ilvl: u32) -> ParagraphNode {
        let mut p = make_test_paragraph();
        p.id = NodeId::from(id.to_string());
        p.numbering = Some(NumberingInfo {
            num_id,
            ilvl,
            synthesized_text: String::new(),
            is_bullet: false,
            restart_numbering: false,
        });
        p
    }

    /// Helper: wrap a BlockNode in a normal TrackedBlock.
    fn tracked(block: BlockNode) -> TrackedBlock {
        normal_tracked_block(block)
    }

    /// Helper: make a minimal table with one row and one cell containing the given blocks.
    fn make_table(cell_blocks: Vec<BlockNode>) -> TableNode {
        TableNode {
            id: NodeId::from("tbl1"),
            rows: vec![TableRowNode {
                id: NodeId::from("row1"),
                cells: vec![TableCellNode {
                    id: NodeId::from("cell1"),
                    blocks: cell_blocks,
                    grid_span: 1,
                    v_merge: VerticalMerge::None,
                    formatting: CellFormatting {
                        borders: None,
                        shading: None,
                        width: None,
                        v_align: None,
                        margins: None,
                        no_wrap: None,
                        text_direction: None,
                        tc_fit_text: None,
                        has_direct_borders: true,
                        authored_borders: None,
                        has_direct_shading: true,
                    },
                    formatting_change: None,
                    tracking_status: None,
                    row_sdt_wrapper: None,
                    content_sdt_wraps: Vec::new(),
                    cnf_style: None,
                    hide_mark: false,
                    preserved: Vec::new(),
                }],
                grid_before: 0,
                grid_after: 0,
                tracking_status: None,
                is_header: false,
                height: None,
                height_rule: None,
                formatting_change: None,
                para_id: None,
                text_id: None,
                cant_split: false,
                jc: None,
                w_before: None,
                w_after: None,
                cnf_style: None,
                tbl_pr_ex: None,
                cell_spacing: None,
                preserved: Vec::new(),
            }],
            structure_hash: String::new(),
            formatting: TableFormatting {
                style_id: None,
                tbl_look: None,
                borders: None,
                width: None,
                grid_cols: vec![],
                default_cell_margins: None,
                alignment: None,
                indent: None,
                layout: None,
                cell_spacing: None,
                positioning: None,
                overlap: None,
                row_band_size: None,
                col_band_size: None,
                has_direct_borders: true,
                has_direct_cell_margins: true,
                has_direct_alignment: true,
                has_direct_indent: true,
                has_direct_tbl_look: true,
                shading: None,
                bidi_visual: false,
                caption: None,
                description: None,
                preserved: Vec::new(),
            },
            formatting_change: None,
        }
    }

    #[test]
    fn remap_numids_reaches_paragraphs_inside_tables() {
        // Paragraph inside a table cell with numId=5
        let para = make_numbered_paragraph("p_in_table", 5, 0);
        let table = make_table(vec![BlockNode::from(para)]);
        let mut blocks = vec![tracked(BlockNode::from(table))];

        let mut remap = HashMap::new();
        remap.insert(5, 42);

        remap_numids_in_blocks(&mut blocks, &remap);

        // Extract the paragraph from inside the table and verify remapped numId.
        let BlockNode::Table(t) = &blocks[0].block else {
            panic!("expected table");
        };
        let BlockNode::Paragraph(p) = &t.rows[0].cells[0].blocks[0] else {
            panic!("expected paragraph in cell");
        };
        assert_eq!(
            p.numbering.as_ref().unwrap().num_id,
            42,
            "numId inside table cell should be remapped from 5 to 42"
        );
    }

    #[test]
    fn remap_numids_reaches_nested_tables() {
        // Paragraph inside a nested table (table within a table cell).
        let inner_para = make_numbered_paragraph("p_nested", 10, 1);
        let inner_table = make_table(vec![BlockNode::from(inner_para)]);
        let outer_table = make_table(vec![BlockNode::from(inner_table)]);
        let mut blocks = vec![tracked(BlockNode::from(outer_table))];

        let mut remap = HashMap::new();
        remap.insert(10, 99);

        remap_numids_in_blocks(&mut blocks, &remap);

        // Drill into outer -> cell -> inner table -> cell -> paragraph.
        let BlockNode::Table(outer) = &blocks[0].block else {
            panic!("expected outer table");
        };
        let BlockNode::Table(inner) = &outer.rows[0].cells[0].blocks[0] else {
            panic!("expected inner table in cell");
        };
        let BlockNode::Paragraph(p) = &inner.rows[0].cells[0].blocks[0] else {
            panic!("expected paragraph in inner cell");
        };
        assert_eq!(
            p.numbering.as_ref().unwrap().num_id,
            99,
            "numId inside nested table should be remapped from 10 to 99"
        );
    }

    #[test]
    fn remap_numids_reaches_story_parts() {
        // Verify that the remap helper works on story-part block slices
        // (headers, footers, footnotes, endnotes).
        let para = make_numbered_paragraph("p_header", 7, 0);
        let mut header_blocks = vec![tracked(BlockNode::from(para))];

        let mut remap = HashMap::new();
        remap.insert(7, 55);

        remap_numids_in_blocks(&mut header_blocks, &remap);

        let BlockNode::Paragraph(p) = &header_blocks[0].block else {
            panic!("expected paragraph");
        };
        assert_eq!(
            p.numbering.as_ref().unwrap().num_id,
            55,
            "numId in story-part paragraph should be remapped from 7 to 55"
        );
    }

    #[test]
    fn remap_numids_skips_paragraphs_without_numbering() {
        // Paragraphs without numbering should be left untouched (no panic).
        let mut para = make_test_paragraph();
        para.id = NodeId::from("p_no_num");
        let mut blocks = vec![tracked(BlockNode::from(para))];

        let mut remap = HashMap::new();
        remap.insert(1, 2);

        remap_numids_in_blocks(&mut blocks, &remap);

        let BlockNode::Paragraph(p) = &blocks[0].block else {
            panic!("expected paragraph");
        };
        assert!(
            p.numbering.is_none(),
            "paragraph without numbering should remain unchanged"
        );
    }

    #[test]
    fn remap_numids_skips_unmatched_numids() {
        // A paragraph whose numId is not in the remap map should keep its original value.
        let para = make_numbered_paragraph("p_keep", 100, 0);
        let mut blocks = vec![tracked(BlockNode::from(para))];

        let mut remap = HashMap::new();
        remap.insert(5, 42); // only remaps 5 -> 42, not 100

        remap_numids_in_blocks(&mut blocks, &remap);

        let BlockNode::Paragraph(p) = &blocks[0].block else {
            panic!("expected paragraph");
        };
        assert_eq!(
            p.numbering.as_ref().unwrap().num_id,
            100,
            "numId not in remap map should be preserved"
        );
    }

    #[test]
    fn remap_numids_reaches_table_inside_story_part() {
        // A numbered paragraph inside a table cell inside a story part (e.g.
        // header) must be remapped — this is the cross-cutting scenario that
        // This is the cross-cutting scenario originally missed.
        let para = make_numbered_paragraph("p_in_tbl_header", 3, 0);
        let table = make_table(vec![BlockNode::from(para)]);
        let mut header_blocks = vec![tracked(BlockNode::from(table))];

        let mut remap = HashMap::new();
        remap.insert(3, 77);

        remap_numids_in_blocks(&mut header_blocks, &remap);

        let BlockNode::Table(t) = &header_blocks[0].block else {
            panic!("expected table");
        };
        let BlockNode::Paragraph(p) = &t.rows[0].cells[0].blocks[0] else {
            panic!("expected paragraph inside table cell");
        };
        assert_eq!(
            p.numbering.as_ref().unwrap().num_id,
            77,
            "numId inside table cell in story part should be remapped from 3 to 77"
        );
    }

    // =========================================================================
    // bookmark-pairing guard tests (replaces the old repair_unpaired_bookmarks
    // tests, which encoded the banned silent-repair behavior: synthesizing
    // zero-span ends and deleting orphan ends collapses bookmark ranges and
    // masks torn pairs — fix-at-symptom)
    // =========================================================================

    fn orphans_of(xml: &[u8]) -> (HashSet<String>, HashSet<String>) {
        let root = word_xml::parse_document_xml(xml).unwrap();
        bookmark_orphans(&root)
    }

    /// An orphan the serialization introduced (not present in any input) is
    /// an engine bug: the guard must refuse with the part path and the
    /// orphan ids — never rewrite the part (I6b).
    #[test]
    fn guard_engine_introduced_orphan_fails_loudly() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:bookmarkStart w:id="42" w:name="torn"/>
      <w:r><w:t>hello</w:t></w:r>
      <w:bookmarkEnd w:id="7"/>
    </w:p>
  </w:body>
</w:document>"#;
        let (orphan_starts, orphan_ends) = orphans_of(xml);
        let err = check_part_bookmark_integrity(
            "word/document.xml",
            &orphan_starts,
            &orphan_ends,
            &HashSet::new(),
            &HashSet::new(),
        )
        .expect_err("new orphans must be refused");
        assert_eq!(err.code, ErrorCode::ValidationFailed);
        assert!(
            err.message.contains("word/document.xml")
                && err.message.contains("42")
                && err.message.contains("7"),
            "error must carry part path and orphan ids: {}",
            err.message
        );
    }

    /// Imbalance the INPUT already had passes through byte-faithfully (I6a):
    /// the orphan ids match an input part's own orphans, so the guard stays
    /// silent — opaque fidelity forbids "repairing" input content.
    #[test]
    fn guard_inherited_orphans_pass_through() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t>hello</w:t></w:r></w:p>
    <w:bookmarkEnd w:id="7"/>
  </w:body>
</w:document>"#;
        let (orphan_starts, orphan_ends) = orphans_of(xml);
        // The same imbalance existed in the input part.
        let inherited_ends: HashSet<String> = ["7".to_string()].into_iter().collect();
        check_part_bookmark_integrity(
            "word/document.xml",
            &orphan_starts,
            &orphan_ends,
            &HashSet::new(),
            &inherited_ends,
        )
        .expect("inherited orphans are the document's own state — no error");
    }

    /// A balanced part is silently fine.
    #[test]
    fn guard_paired_bookmarks_ok() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:bookmarkStart w:id="1" w:name="ok"/>
      <w:r><w:t>hi</w:t></w:r>
      <w:bookmarkEnd w:id="1"/>
    </w:p>
  </w:body>
</w:document>"#;
        let (orphan_starts, orphan_ends) = orphans_of(xml);
        assert!(orphan_starts.is_empty() && orphan_ends.is_empty());
        check_part_bookmark_integrity(
            "word/document.xml",
            &orphan_starts,
            &orphan_ends,
            &HashSet::new(),
            &HashSet::new(),
        )
        .expect("balanced part must pass");
    }

    /// Mixed shape: one inherited orphan (passes) plus one engine-introduced
    /// orphan (refuses) — the NEW id must be the one reported.
    #[test]
    fn guard_mixed_inherited_and_new_orphans_reports_only_new() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:bookmarkStart w:id="2" w:name="torn"/>
      <w:r><w:t>text</w:t></w:r>
      <w:bookmarkEnd w:id="9"/>
    </w:p>
  </w:body>
</w:document>"#;
        let (orphan_starts, orphan_ends) = orphans_of(xml);
        // The orphan end id=9 was already orphaned in the input; start id=2 is new.
        let inherited_ends: HashSet<String> = ["9".to_string()].into_iter().collect();
        let err = check_part_bookmark_integrity(
            "word/document.xml",
            &orphan_starts,
            &orphan_ends,
            &HashSet::new(),
            &inherited_ends,
        )
        .expect_err("the new orphan start must still be refused");
        assert!(
            err.message.contains("\"2\"") && !err.message.contains("\"9\""),
            "only the engine-introduced orphan id must be reported: {}",
            err.message
        );
    }

    // ── PendingParts save-path foundation ────────────────────────────────────
    //
    // These exercise the private save-path twin (`apply_pending_parts`) by
    // building a `DocxPackage` and a `PendingParts` directly — no verb stages
    // anything yet, so this is how we prove the channel works ahead of the leaf
    // verbs. The pure-core inertness (existing verbs => empty PendingParts) is
    // proved by `stemma-engine/tests/pending_parts_foundation.rs`.
    mod pending_parts_save {
        use super::super::*;
        use super::make_test_paragraph;
        use crate::docx::{DocxArchive, DocxFile};
        use crate::docx_package::DocxPackage;
        use crate::domain::{
            BlockNode, CanonDoc, DocFingerprint, DocMeta, DocPart, InlineNode, NodeId, OpaqueKind,
            ProofRef, RevisionInfo, StyleProps, TrackedSegment, TrackingStatus,
            normal_tracked_block,
        };
        use crate::edit::{PendingMedia, PendingParts, StyleOp};

        const STYLES_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

        fn empty_canon() -> CanonDoc {
            CanonDoc {
                id: NodeId::from("doc"),
                blocks: Vec::new(),
                meta: DocMeta {
                    schema_version: crate::domain::SCHEMA_VERSION_V0.to_string(),
                    docx_fingerprint: DocFingerprint("fp".to_string()),
                    internal_ids_version: crate::domain::INTERNAL_IDS_VERSION_V0.to_string(),
                },
                headers: Vec::new(),
                footers: Vec::new(),
                footnotes: Vec::new(),
                endnotes: Vec::new(),
                comments: Vec::new(),
                comments_extended: Vec::new(),
                body_section_properties: None,
                body_section_property_change: None,
                compat_settings: crate::domain::CompatSettings::default(),
                even_and_odd_headers: None,
                document_background: None,
                document_protection: None,
            }
        }

        fn minimal_pkg(styles_inner: Option<&str>) -> DocxPackage {
            let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#.to_vec();
            let root_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#.to_vec();
            let doc_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
</Relationships>"#.to_vec();
            let document = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body/></w:document>"#.to_vec();

            let mut files = vec![
                DocxFile {
                    name: "[Content_Types].xml".to_string(),
                    data: content_types,
                },
                DocxFile {
                    name: "_rels/.rels".to_string(),
                    data: root_rels,
                },
                DocxFile {
                    name: "word/_rels/document.xml.rels".to_string(),
                    data: doc_rels,
                },
                DocxFile {
                    name: "word/document.xml".to_string(),
                    data: document,
                },
            ];
            if let Some(inner) = styles_inner {
                let styles = format!(
                    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="{STYLES_NS}">{inner}</w:styles>"#
                )
                .into_bytes();
                files.push(DocxFile {
                    name: "word/styles.xml".to_string(),
                    data: styles,
                });
            }
            let archive = DocxArchive::from_parts(files);
            DocxPackage::from_archive(&archive).expect("minimal package parses")
        }

        /// One-paragraph CanonDoc with a single inserted drawing whose blip
        /// references `logical_rid`, so the save path has an IR rId to rewrite.
        fn doc_with_inserted_drawing(logical_rid: &str) -> CanonDoc {
            let drawing_xml = format!(
                r#"<w:drawing xmlns:w="{STYLES_NS}" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><a:blip r:embed="{logical_rid}"/></w:drawing>"#
            );
            let opaque = InlineNode::from(crate::domain::OpaqueInlineNode {
                id: NodeId::from("op1"),
                kind: OpaqueKind::Drawing,
                opaque_ref: "op_ref_1".to_string(),
                proof_ref: ProofRef {
                    part: DocPart::DocumentXml,
                    block_id: NodeId::from("p1"),
                    docx_anchor: String::new(),
                },
                wrapper_marks: Vec::new(),
                wrapper_style_props: StyleProps::default(),
                raw_xml: Some(drawing_xml.into_bytes()),
                content_hash: None,
            });
            let seg = TrackedSegment {
                status: TrackingStatus::Inserted(RevisionInfo {
                    revision_id: 1,
                    author: None,
                    date: None,
                    apply_op_id: None,
                }),
                inlines: vec![opaque],
            };
            let mut para = make_test_paragraph();
            para.segments = vec![seg];
            let mut doc = empty_canon();
            doc.blocks = vec![normal_tracked_block(BlockNode::from(para))];
            doc
        }

        fn first_drawing_rid(doc: &CanonDoc) -> Option<String> {
            let BlockNode::Paragraph(p) = &doc.blocks[0].block else {
                return None;
            };
            let InlineNode::OpaqueInline(o) = &p.segments[0].inlines[0] else {
                return None;
            };
            let raw = o.raw_xml.as_ref()?;
            crate::diff::find_blip_rid(std::str::from_utf8(raw).ok()?)
        }

        /// One-paragraph CanonDoc with a single opaque inline whose `raw_xml` is
        /// exactly `raw`. Used to exercise the unparseable-fragment preflight.
        fn doc_with_opaque_raw(kind: OpaqueKind, raw: &[u8]) -> CanonDoc {
            let opaque = InlineNode::from(crate::domain::OpaqueInlineNode {
                id: NodeId::from("op1"),
                kind,
                opaque_ref: "op_ref_1".to_string(),
                proof_ref: ProofRef {
                    part: DocPart::DocumentXml,
                    block_id: NodeId::from("p1"),
                    docx_anchor: String::new(),
                },
                wrapper_marks: Vec::new(),
                wrapper_style_props: StyleProps::default(),
                raw_xml: Some(raw.to_vec()),
                content_hash: None,
            });
            let seg = TrackedSegment {
                status: TrackingStatus::Normal,
                inlines: vec![opaque],
            };
            let mut para = make_test_paragraph();
            para.segments = vec![seg];
            let mut doc = empty_canon();
            doc.blocks = vec![normal_tracked_block(BlockNode::from(para))];
            doc
        }

        #[test]
        fn preflight_flags_unparseable_opaque_with_revisions() {
            // Malformed XML carrying a revision marker → the scan returns its id,
            // so `project` will refuse rather than silently leave the revision.
            let raw = br#"<w:sdt><w:sdtContent><w:ins w:id="1"><w:r><w:t>NEW</w:t></w:sdtContent></w:sdt>"#;
            let doc = doc_with_opaque_raw(OpaqueKind::Sdt, raw);
            assert_eq!(
                first_unparseable_opaque_with_revisions(&doc),
                Some(NodeId::from("op1"))
            );
        }

        #[test]
        fn preflight_ignores_parseable_and_marker_free_opaques() {
            // A well-formed opaque (parses fine) → not flagged.
            let ok = br#"<w:sdt><w:sdtContent><w:ins w:id="1"><w:r><w:t>NEW</w:t></w:r></w:ins></w:sdtContent></w:sdt>"#;
            assert_eq!(
                first_unparseable_opaque_with_revisions(&doc_with_opaque_raw(OpaqueKind::Sdt, ok)),
                None
            );
            // Malformed but with NO revision marker → not flagged (nothing to lose).
            let no_marker = br#"<w:sdt><w:sdtContent><w:r><w:t>NEW</w:sdtContent></w:sdt>"#;
            assert_eq!(
                first_unparseable_opaque_with_revisions(&doc_with_opaque_raw(
                    OpaqueKind::Sdt,
                    no_marker
                )),
                None
            );
        }

        fn png_bytes() -> Vec<u8> {
            // 8-byte PNG magic + a little payload — non-empty, content irrelevant.
            vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3, 4]
        }

        fn media(logical_rid: &str, bytes: Vec<u8>) -> PendingMedia {
            let bytes_sha256 = sha256_hex_bytes(&bytes);
            PendingMedia {
                logical_rid: logical_rid.to_string(),
                bytes,
                bytes_sha256,
                content_type: "image/png".to_string(),
                ext: "png".to_string(),
            }
        }

        // (b) A hand-built PendingMedia registers a part + rel, rewrites the
        // logical rId to the real rId, leaves no orphaned rId, and declares the
        // content type.
        #[test]
        fn media_registers_part_rel_and_rewrites_rid() {
            let mut pkg = minimal_pkg(None);
            let mut doc = doc_with_inserted_drawing("rIdLOGICAL");
            let before_rid = first_drawing_rid(&doc).expect("doc has a blip rId");
            assert_eq!(before_rid, "rIdLOGICAL");

            let pending = PendingParts {
                media: vec![media("rIdLOGICAL", png_bytes())],
                style_ops: Vec::new(),
                numbering_ops: Vec::new(),
                custom_xml: Vec::new(),

                ..Default::default()
            };
            apply_pending_parts(&mut doc, &mut pkg, &pending).expect("media applies");

            // A media part was written.
            let media_parts: Vec<&str> = pkg
                .part_names()
                .filter(|p| p.starts_with("word/media/"))
                .collect();
            assert_eq!(media_parts.len(), 1, "exactly one media part written");
            assert!(media_parts[0].ends_with(".png"));
            assert_eq!(
                pkg.get_part(media_parts[0]).unwrap(),
                png_bytes().as_slice(),
                "media bytes preserved"
            );

            // An image relationship was added, and the IR rId now points to it.
            let new_rid = first_drawing_rid(&doc).expect("doc still has a blip rId");
            assert_ne!(new_rid, "rIdLOGICAL", "logical rId must be rewritten");
            let rel = pkg
                .document_rels
                .find_by_id(&new_rid)
                .expect("rewritten rId resolves to a real relationship — no orphan");
            assert_eq!(rel.rel_type, IMAGE_REL_TYPE);
            assert_eq!(rel.target, media_parts[0].strip_prefix("word/").unwrap());

            // Content type for png is declared (Default or Override).
            assert!(
                pkg.content_types.has_default("png")
                    || pkg
                        .content_types
                        .has_override(&format!("/{}", media_parts[0])),
                "png content type must be declared"
            );
        }

        // (c) fail-loud: empty bytes, unknown/empty content-type, sha mismatch —
        // each errors with context and writes no orphaned part.
        #[test]
        fn media_empty_bytes_fails_loud_no_orphan() {
            let mut pkg = minimal_pkg(None);
            let mut doc = doc_with_inserted_drawing("rIdLOGICAL");
            let mut m = media("rIdLOGICAL", Vec::new());
            m.bytes_sha256 = sha256_hex_bytes(&[]); // honest digest of empty
            let pending = PendingParts {
                media: vec![m],
                style_ops: Vec::new(),
                numbering_ops: Vec::new(),
                custom_xml: Vec::new(),

                ..Default::default()
            };
            let err = apply_pending_parts(&mut doc, &mut pkg, &pending).unwrap_err();
            assert!(
                err.message.contains("empty image bytes"),
                "msg: {}",
                err.message
            );
            assert!(err.details.context.is_some());
            assert_eq!(
                pkg.part_names()
                    .filter(|p| p.starts_with("word/media/"))
                    .count(),
                0,
                "no media part written on failure"
            );
        }

        #[test]
        fn media_empty_content_type_fails_loud() {
            let mut pkg = minimal_pkg(None);
            let mut doc = doc_with_inserted_drawing("rIdLOGICAL");
            let mut m = media("rIdLOGICAL", png_bytes());
            m.content_type = "  ".to_string();
            let pending = PendingParts {
                media: vec![m],
                style_ops: Vec::new(),
                numbering_ops: Vec::new(),
                custom_xml: Vec::new(),

                ..Default::default()
            };
            let err = apply_pending_parts(&mut doc, &mut pkg, &pending).unwrap_err();
            assert!(
                err.message.contains("empty content-type"),
                "msg: {}",
                err.message
            );
            assert_eq!(
                pkg.part_names()
                    .filter(|p| p.starts_with("word/media/"))
                    .count(),
                0,
                "no media part written on failure"
            );
        }

        #[test]
        fn media_sha_mismatch_fails_loud() {
            let mut pkg = minimal_pkg(None);
            let mut doc = doc_with_inserted_drawing("rIdLOGICAL");
            let mut m = media("rIdLOGICAL", png_bytes());
            m.bytes_sha256 = "deadbeef".to_string();
            let pending = PendingParts {
                media: vec![m],
                style_ops: Vec::new(),
                numbering_ops: Vec::new(),
                custom_xml: Vec::new(),

                ..Default::default()
            };
            let err = apply_pending_parts(&mut doc, &mut pkg, &pending).unwrap_err();
            assert!(
                err.message.contains("sha256 mismatch"),
                "msg: {}",
                err.message
            );
            assert_eq!(
                pkg.part_names()
                    .filter(|p| p.starts_with("word/media/"))
                    .count(),
                0,
                "no media part written on failure"
            );
        }

        fn style_fragment(style_id: &str, name: &str) -> Vec<u8> {
            format!(
                r#"<w:style xmlns:w="{STYLES_NS}" w:type="paragraph" w:styleId="{style_id}"><w:name w:val="{name}"/></w:style>"#
            )
            .into_bytes()
        }

        // Style Create inserts a new w:style.
        #[test]
        fn style_create_inserts_new_style() {
            let mut pkg = minimal_pkg(Some(
                r#"<w:style w:type="paragraph" w:styleId="Normal"><w:name w:val="Normal"/></w:style>"#,
            ));
            let mut doc = empty_canon();
            let pending = PendingParts {
                media: Vec::new(),
                style_ops: vec![StyleOp::Create {
                    style_id: "MyStyle".to_string(),
                    style_xml: style_fragment("MyStyle", "My Style"),
                }],
                numbering_ops: Vec::new(),
                custom_xml: Vec::new(),
                ..Default::default()
            };
            apply_pending_parts(&mut doc, &mut pkg, &pending).expect("create applies");
            let styles = pkg.get_part("word/styles.xml").unwrap();
            let s = std::str::from_utf8(styles).unwrap();
            assert!(s.contains("MyStyle"), "new style present: {s}");
            assert!(s.contains("Normal"), "existing style preserved");
        }

        // When the package has no styles part, a Create BOOTSTRAPS one (mirrors
        // the settings.xml synthesis precedent) and registers the content-type
        // Override + the styles document relationship.
        #[test]
        fn style_create_bootstraps_absent_part() {
            let mut pkg = minimal_pkg(None);
            assert!(
                pkg.get_part("word/styles.xml").is_none(),
                "precondition: no styles part"
            );
            assert!(
                !pkg.content_types.has_override("/word/styles.xml"),
                "precondition: no styles override"
            );
            let mut doc = empty_canon();
            let pending = PendingParts {
                media: Vec::new(),
                style_ops: vec![StyleOp::Create {
                    style_id: "MyStyle".to_string(),
                    style_xml: style_fragment("MyStyle", "My Style"),
                }],
                numbering_ops: Vec::new(),
                custom_xml: Vec::new(),
                ..Default::default()
            };
            apply_pending_parts(&mut doc, &mut pkg, &pending)
                .expect("create into absent styles part bootstraps it");

            let styles = pkg
                .get_part("word/styles.xml")
                .expect("styles part synthesized");
            let s = std::str::from_utf8(styles).unwrap();
            assert!(s.contains("w:styles"), "synthesized root present: {s}");
            assert!(s.contains("MyStyle"), "authored style spliced in: {s}");

            assert!(
                pkg.content_types.has_override("/word/styles.xml"),
                "styles content-type override registered"
            );
            assert!(
                pkg.document_rels.entries.iter().any(|r| r.rel_type
                    == "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles"
                    && r.target == "styles.xml"),
                "styles document relationship registered"
            );
        }

        #[test]
        fn style_create_existing_fails_loud() {
            let mut pkg = minimal_pkg(Some(
                r#"<w:style w:type="paragraph" w:styleId="Dup"><w:name w:val="Dup"/></w:style>"#,
            ));
            let before = pkg.get_part("word/styles.xml").unwrap().to_vec();
            let mut doc = empty_canon();
            let pending = PendingParts {
                media: Vec::new(),
                style_ops: vec![StyleOp::Create {
                    style_id: "Dup".to_string(),
                    style_xml: style_fragment("Dup", "Dup"),
                }],
                numbering_ops: Vec::new(),
                custom_xml: Vec::new(),
                ..Default::default()
            };
            let err = apply_pending_parts(&mut doc, &mut pkg, &pending).unwrap_err();
            assert!(
                err.message.contains("already exists"),
                "msg: {}",
                err.message
            );
            assert_eq!(
                pkg.get_part("word/styles.xml").unwrap(),
                before.as_slice(),
                "styles.xml unchanged on failed Create"
            );
        }

        #[test]
        fn style_modify_missing_fails_loud() {
            let mut pkg = minimal_pkg(Some(
                r#"<w:style w:type="paragraph" w:styleId="Normal"><w:name w:val="Normal"/></w:style>"#,
            ));
            let mut doc = empty_canon();
            let pending = PendingParts {
                media: Vec::new(),
                style_ops: vec![StyleOp::Modify {
                    style_id: "Ghost".to_string(),
                    style_xml: style_fragment("Ghost", "Ghost"),
                }],
                numbering_ops: Vec::new(),
                custom_xml: Vec::new(),
                ..Default::default()
            };
            let err = apply_pending_parts(&mut doc, &mut pkg, &pending).unwrap_err();
            assert!(
                err.message.contains("no style with that id exists"),
                "msg: {}",
                err.message
            );
        }

        #[test]
        fn style_malformed_fragment_fails_loud() {
            let mut pkg = minimal_pkg(Some(
                r#"<w:style w:type="paragraph" w:styleId="Normal"><w:name w:val="Normal"/></w:style>"#,
            ));
            let mut doc = empty_canon();
            let pending = PendingParts {
                media: Vec::new(),
                style_ops: vec![StyleOp::Create {
                    style_id: "Bad".to_string(),
                    style_xml: b"<w:style not closed".to_vec(),
                }],
                numbering_ops: Vec::new(),
                custom_xml: Vec::new(),
                ..Default::default()
            };
            let err = apply_pending_parts(&mut doc, &mut pkg, &pending).unwrap_err();
            assert!(
                err.message.contains("malformed XML"),
                "msg: {}",
                err.message
            );
        }

        // Style ordering: an authored Modify, applied AFTER the styles merge,
        // wins a base/target style-id collision. We model the merge having
        // already preferred the target's "Heading1", then prove our Modify
        // overwrites that with the authored definition. This is the contract
        // that `apply_pending_parts` runs after `merge_styles_xml_preferring_target`.
        #[test]
        fn authored_style_wins_collision_after_merge() {
            // Simulate the post-merge styles.xml: it has Heading1 from a
            // "target wins" merge with the target's definition.
            let mut pkg = minimal_pkg(Some(
                r#"<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="TARGET Heading"/></w:style>"#,
            ));
            let mut doc = empty_canon();
            let authored = format!(
                r#"<w:style xmlns:w="{STYLES_NS}" w:type="paragraph" w:styleId="Heading1"><w:name w:val="AUTHORED Heading"/></w:style>"#
            )
            .into_bytes();
            let pending = PendingParts {
                media: Vec::new(),
                style_ops: vec![StyleOp::Modify {
                    style_id: "Heading1".to_string(),
                    style_xml: authored,
                }],
                numbering_ops: Vec::new(),
                custom_xml: Vec::new(),
                ..Default::default()
            };
            apply_pending_parts(&mut doc, &mut pkg, &pending).expect("authored modify applies");
            let s = String::from_utf8(pkg.get_part("word/styles.xml").unwrap().to_vec()).unwrap();
            assert!(
                s.contains("AUTHORED Heading"),
                "authored style must win over the merged target definition: {s}"
            );
            assert!(
                !s.contains("TARGET Heading"),
                "merged target definition must be replaced, not duplicated: {s}"
            );
        }
    }

    /// `merge_target_numbering` must refuse to emit a document with dangling
    /// `w:numPr` references rather than silently drop the merge. A block that
    /// carries `NumberingInfo` asserts "my rendered label depends on a
    /// `<w:num>` definition existing in the output package" — if that
    /// definition cannot be sourced (from base or target), continuing would
    /// export a list that displays as plain, unnumbered text in Word with no
    /// error anywhere in the pipeline.
    mod merge_target_numbering_tests {
        use super::super::*;
        use super::make_test_paragraph;
        use crate::docx::{DocxArchive, DocxFile};
        use crate::docx_package::DocxPackage;
        use crate::domain::{
            BlockNode, CanonDoc, DocFingerprint, DocMeta, NodeId, NumberingInfo,
            normal_tracked_block,
        };

        fn empty_canon() -> CanonDoc {
            CanonDoc {
                id: NodeId::from("doc"),
                blocks: Vec::new(),
                meta: DocMeta {
                    schema_version: crate::domain::SCHEMA_VERSION_V0.to_string(),
                    docx_fingerprint: DocFingerprint("fp".to_string()),
                    internal_ids_version: crate::domain::INTERNAL_IDS_VERSION_V0.to_string(),
                },
                headers: Vec::new(),
                footers: Vec::new(),
                footnotes: Vec::new(),
                endnotes: Vec::new(),
                comments: Vec::new(),
                comments_extended: Vec::new(),
                body_section_properties: None,
                body_section_property_change: None,
                compat_settings: crate::domain::CompatSettings::default(),
                even_and_odd_headers: None,
                document_background: None,
                document_protection: None,
            }
        }

        /// One-paragraph `CanonDoc` whose paragraph references `num_id` via a
        /// live `NumberingInfo` — the shape a merged doc has when a block
        /// (inserted or re-pointed by a modify) needs its numbering sourced
        /// from elsewhere.
        fn doc_needing_num_id(num_id: u32) -> CanonDoc {
            let mut para = make_test_paragraph();
            para.numbering = Some(NumberingInfo {
                num_id,
                ilvl: 0,
                synthesized_text: "1.".to_string(),
                is_bullet: false,
                restart_numbering: false,
            });
            let mut doc = empty_canon();
            doc.blocks = vec![normal_tracked_block(BlockNode::from(para))];
            doc
        }

        /// Empty `DocxPackage` with no `word/numbering.xml` part — merge must
        /// source the needed numId entirely from the target archive.
        fn base_pkg_without_numbering() -> DocxPackage {
            let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#
                .to_vec();
            let root_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#
                .to_vec();
            let doc_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#
                .to_vec();
            let document = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body/></w:document>"#.to_vec();
            let archive = DocxArchive::from_parts(vec![
                DocxFile {
                    name: "[Content_Types].xml".to_string(),
                    data: content_types,
                },
                DocxFile {
                    name: "_rels/.rels".to_string(),
                    data: root_rels,
                },
                DocxFile {
                    name: "word/_rels/document.xml.rels".to_string(),
                    data: doc_rels,
                },
                DocxFile {
                    name: "word/document.xml".to_string(),
                    data: document,
                },
            ]);
            DocxPackage::from_archive(&archive).expect("minimal package parses")
        }

        /// Target archive with no `word/numbering.xml` part at all.
        fn target_archive_without_numbering() -> DocxArchive {
            DocxArchive::from_parts(vec![DocxFile {
                name: "word/document.xml".to_string(),
                data: br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body/></w:document>"#
                    .to_vec(),
            }])
        }

        /// Target archive whose `word/numbering.xml` part is present but not
        /// well-formed XML.
        fn target_archive_with_malformed_numbering() -> DocxArchive {
            DocxArchive::from_parts(vec![
                DocxFile {
                    name: "word/document.xml".to_string(),
                    data: br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body/></w:document>"#
                        .to_vec(),
                },
                DocxFile {
                    name: "word/numbering.xml".to_string(),
                    data: b"not xml".to_vec(),
                },
            ])
        }

        #[test]
        fn errors_when_needed_and_target_has_no_numbering_part() {
            let mut doc = doc_needing_num_id(5);
            let mut base_pkg = base_pkg_without_numbering();
            let target_archive = target_archive_without_numbering();

            let err = merge_target_numbering(&mut doc, &mut base_pkg, &target_archive)
                .expect_err("a block needs numId 5 but neither base nor target defines it");

            assert_eq!(err.code, ErrorCode::InvalidDocx);
            assert!(
                err.message.contains("word/numbering.xml"),
                "error should name the missing part: {}",
                err.message
            );
            assert!(
                err.message.contains('5'),
                "error should name the unresolved numId: {}",
                err.message
            );
        }

        #[test]
        fn errors_when_target_numbering_part_is_malformed() {
            let mut doc = doc_needing_num_id(5);
            let mut base_pkg = base_pkg_without_numbering();
            let target_archive = target_archive_with_malformed_numbering();

            let err = merge_target_numbering(&mut doc, &mut base_pkg, &target_archive)
                .expect_err("target's word/numbering.xml is not well-formed XML");

            assert_eq!(err.code, ErrorCode::InvalidDocx);
            assert!(
                err.message.contains("word/numbering.xml"),
                "error should name the unparseable part: {}",
                err.message
            );
        }

        #[test]
        fn no_op_when_no_block_references_numbering() {
            // No paragraph carries a NumberingInfo — the target's missing
            // numbering.xml is irrelevant and must not be treated as an error.
            let mut doc = empty_canon();
            doc.blocks = vec![normal_tracked_block(BlockNode::from(make_test_paragraph()))];
            let mut base_pkg = base_pkg_without_numbering();
            let target_archive = target_archive_without_numbering();

            merge_target_numbering(&mut doc, &mut base_pkg, &target_archive)
                .expect("no needed numIds means nothing to source");
        }
    }
}
