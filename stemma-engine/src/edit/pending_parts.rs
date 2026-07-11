//! Per-apply transient: OPC parts an authoring verb wants staged into the
//! save path (image binaries, styles.xml fragments) alongside the typed-IR
//! mutation it makes to the [`crate::domain::CanonDoc`].
//!
//! # Why this exists
//!
//! `apply_transaction(&CanonDoc) -> (CanonDoc, PendingParts)` is the **pure
//! twin** of the save path: it has no [`crate::docx_package::DocxPackage`] in
//! scope by design. But some verbs (insert an image, create a style) need to
//! add real OPC parts and relationships, not just mutate the body IR. Those
//! verbs cannot reach the package from the pure core, so they **stage** the
//! parts here, and the save path (`runtime::apply_pending_parts`, invoked from
//! `serialize_canonical_docx`) applies them against the live `DocxPackage`.
//!
//! This generalizes the existing `copy_target_media_for_inserted_drawings`
//! precedent: same intent (copy a binary, register a rel, rewrite the IR rId),
//! but the parts are **supplied by a verb** instead of inferred from a second
//! archive.
//!
//! # Transient — never persisted
//!
//! `PendingParts` is a per-apply value derived **entirely** from the
//! `EditTransaction`. It carries no durable identity and MUST NOT be
//! serialized: the [`crate::runtime::EditSnapshot`] "do not persist" contract
//! says the only durable artifacts are the DOCX bytes plus the
//! `EditTransaction` history. Replaying the same transaction re-derives the
//! same `PendingParts` deterministically, so there is nothing here worth
//! persisting. Deliberately NO `Serialize`/`Deserialize`.

/// OPC parts staged by the verbs in one `apply_transaction` call, to be
/// applied to the live `DocxPackage` at save time.
///
/// Empty for all current (foundation-era) verbs: the channel exists, but no
/// shipped verb writes to it yet.
///
/// # Public only because it rides the `apply_transaction` return type
///
/// `apply_transaction` is `pub` and is called from integration-test crates,
/// which can only destructure its `(CanonDoc, PendingParts)` tuple if the type
/// is nameable. So this is `pub` out of necessity, **not** because it is part
/// of a stable, caller-constructed API: it is a per-apply transient that the
/// save path consumes and discards. Treat it as internal/unstable — callers
/// bind it and pass it straight to the save path (or ignore it). It carries no
/// `Serialize`/`Deserialize` precisely so it cannot be persisted.
#[derive(Debug, Default, Clone)]
pub struct PendingParts {
    /// Image (or other media) binaries to add as `word/media/*` parts with an
    /// image relationship, rewriting the staged `logical_rid` to the real rId
    /// the package assigns.
    pub media: Vec<PendingMedia>,
    /// `word/styles.xml` create/modify operations, spliced after the
    /// base/target style merge so an authored style wins a style-id collision.
    pub style_ops: Vec<StyleOp>,
    /// `word/numbering.xml` create-definition operations: author a brand-new
    /// `w:num`/`w:abstractNum` pair (cloning the levels of an existing list) so
    /// a verb can split a numbered list into two independently-counted lists.
    /// Materialized by `runtime::apply_pending_numbering_ops` at save time.
    pub numbering_ops: Vec<NumberingOp>,
    /// Custom-XML datastore parts to author (or reuse) for content-control data
    /// bindings (`w:dataBinding`). Each entry, keyed by its `store_item_id`,
    /// becomes a `customXml/item*.xml` data part plus its `itemProps`,
    /// content-type Overrides, and a `customXml` relationship from
    /// document.xml. Materialized by `runtime::apply_pending_custom_xml` at save
    /// time. A direct generalization of the styles/numbering part-bootstrap.
    pub custom_xml: Vec<CustomXmlPart>,
    /// Body-level (block) content-control text fills. A block `w:sdt` keeps its
    /// bytes in the serialize scaffold (`BodyTemplate.opaque_children`), NOT on
    /// the IR node, so `apply_transaction` (pure over `&CanonDoc`) cannot splice
    /// them. The `sdt_text_fill` verb validates the target block exists and mints
    /// the tracked-change ids here; the save path
    /// (`runtime::apply_pending_opaque_child_text_sets`) reads the scaffold node
    /// by `body_index`, sets its content-control value, and writes it back — the
    /// same PendingParts seam custom-XML/media/styles already use.
    pub opaque_child_text_sets: Vec<OpaqueChildTextSet>,
}

/// A body-level content-control value fill staged for the save path (RFC-0002
/// §Phase-2 block-SDT plumbing).
///
/// Addressed by the frozen import-time `body_index` (the key the serializer
/// resolves `opaque_children` by, stable across sibling insert/delete). The
/// tracked-change ids are minted at verb time from the transaction's whole-doc
/// counter so they are unique across the document, exactly like an inline splice.
/// Fails loud at save time (no silent fallback) when the `body_index` is absent
/// or its `w:sdtContent` has no text paragraph to fill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpaqueChildTextSet {
    /// The body index of the target block-level `w:sdt`.
    pub body_index: usize,
    /// The value to set (whole-value replace of the control's text).
    pub value: String,
    /// Tracked (`w:del`+`w:ins`) vs direct replace.
    pub tracked: bool,
    /// Revision author (empty string if none).
    pub author: String,
    /// Revision date (RFC-3339), if any.
    pub date: Option<String>,
    /// The pre-minted `[w:del id, w:ins id]` for the tracked change.
    pub revision_ids: [u32; 2],
}

/// A custom-XML datastore part staged by the content-control data-binding verb.
///
/// `store_item_id` is the `storeItemID` GUID the verb wrote into the control's
/// `w:dataBinding`; the save path authors a `customXml/item*.xml` data part
/// whose `itemProps` carries this id as its `ds:itemID`, so Word resolves the
/// binding's `storeItemID` to a real, well-formed part. The `root_element` is
/// the local name of the datastore document element (a deterministic skeleton
/// the bound XPath can address). Multiple bindings sharing one `store_item_id`
/// collapse to a single authored part (dedup by id at save time).
///
/// Fails loud at save time (no silent fallback) when `store_item_id` is empty
/// (a datastore part with no item id is unresolvable).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomXmlPart {
    /// The `storeItemID` GUID (matches the control's `w:dataBinding`).
    pub store_item_id: String,
    /// Local name of the datastore document element (the XPath root the binding
    /// addresses), e.g. `root`. Used to author a minimal well-formed skeleton.
    pub root_element: String,
    /// Optional default namespace URI for the datastore root element. `None`
    /// authors a no-namespace skeleton.
    pub namespace: Option<String>,
}

/// A `word/numbering.xml` mutation staged by a verb.
///
/// The only variant today is `CreateDefinition`, the save-time half of the
/// list-split verb. The pure verb core holds a `CanonDoc`, not the numbering
/// part, so it CANNOT know which `numId`s the part already defines (orphan
/// definitions, style-linked lists, and story-only lists routinely occupy ids
/// no body paragraph references). It therefore re-points the split tail at a
/// sentinel PLACEHOLDER id (`verbs::numbering::SPLIT_PLACEHOLDER_NUM_ID_BASE`)
/// rather than guessing a real one; the save path
/// (`runtime::apply_pending_numbering_ops`), which CAN see the part, allocates
/// the real `numId` against the part's authoritative population, allocates the
/// real `abstractNumId`, **clones** the source list's `<w:abstractNum>` (so
/// every `<w:lvl>` format is preserved verbatim — opaque preservation), appends
/// a fresh `<w:num>` pointing at the clone, and rewrites the placeholder in the
/// re-pointed paragraphs' live `w:numPr`. This mirrors how [`PendingMedia`]'s
/// `logical_rid` placeholder becomes a real rId at save time.
///
/// Fails loud at save time (no silent fallback) when:
/// - the staged `placeholder_num_id` collides with a real `<w:num>` in
///   numbering.xml — the reserved sentinel range overlapped a real definition,
///   which is a programmer bug, not a default;
/// - `cloned_from_num_id` has no resolvable `<w:abstractNum>` (a split of a
///   list whose definition is missing is invalid — we refuse rather than
///   fabricate empty levels).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NumberingOp {
    /// Author a new list definition by cloning the abstractNum of an existing
    /// list. The verb stages a PLACEHOLDER `numId`; the save path allocates the
    /// real `numId` + `abstractNumId` and clones the source levels.
    CreateDefinition {
        /// The sentinel placeholder `w:numId` the verb wrote into the split
        /// tail's IR, to be rewritten by the save path to the real id it
        /// allocates. See `verbs::numbering::SPLIT_PLACEHOLDER_NUM_ID_BASE`.
        placeholder_num_id: u32,
        /// The existing `w:numId` whose `<w:abstractNum>` levels are cloned so
        /// the new list renders identically to the source.
        cloned_from_num_id: u32,
    },
}

/// One media binary to stage into the package at save time.
///
/// `logical_rid` is the placeholder rId the verb wrote into the IR drawing
/// XML. The save path allocates a real rId, registers the relationship, and
/// rewrites the IR `logical_rid -> real rId`. `bytes_sha256` lets the save
/// path deduplicate identical binaries across a transaction.
/// See [`PendingParts`] for the public-only-by-necessity rationale. The leaf
/// image-insert verb (a follow-up commit) is the first producer; foundation-era
/// code never constructs this, so the fields are lint-exempt until then.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct PendingMedia {
    /// Placeholder relationship id referenced by the inserted drawing's
    /// `r:embed` in the IR; rewritten to the real rId at save time.
    pub logical_rid: String,
    /// Raw image bytes.
    pub bytes: Vec<u8>,
    /// Lowercase hex SHA-256 of `bytes`, for dedup.
    pub bytes_sha256: String,
    /// Content type for the part (e.g. `image/png`).
    pub content_type: String,
    /// File extension without the dot (e.g. `png`), used for the media part
    /// name and the content-type Default entry.
    pub ext: String,
}

/// A `word/styles.xml` mutation staged by a verb.
///
/// `style_xml` is the full serialized `<w:style …>…</w:style>` fragment.
/// `Create` requires the styleId be absent; `Modify` requires it be present.
/// Both are enforced loudly at save time (no silent insert-or-update).
///
/// Foundation-era: the save path (`runtime::apply_pending_style_ops`) and the
/// unit tests construct/match these, but no shipped verb builds them yet, so
/// the lib otherwise never constructs the variants. The leaf verbs that author
/// styles will. Suppress the dead-code lint until then.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum StyleOp {
    /// Insert a brand-new `w:style`. Fails loud if the styleId already exists.
    Create {
        style_id: String,
        style_xml: Vec<u8>,
    },
    /// Replace an existing `w:style` matched by `w:styleId`. Fails loud if the
    /// styleId is absent.
    Modify {
        style_id: String,
        style_xml: Vec<u8>,
    },
    /// Set the document DEFAULT run properties
    /// (`w:docDefaults/w:rPrDefault/w:rPr`). Unlike `Create`/`Modify` this op has
    /// NO styleId — it targets the docDefaults block, not a `w:style`. The save
    /// path (`runtime::apply_pending_style_ops`) find-or-creates the
    /// `docDefaults/rPrDefault/rPr` chain and PROPERTY-MERGES only the named
    /// children (`font_family` → `w:rFonts`, `font_size_half_points` → `w:sz`),
    /// preserving every other rPrDefault child. At least one field must be
    /// `Some` (the verb refuses an all-`None` op).
    SetDocDefaults {
        font_family: Option<String>,
        font_size_half_points: Option<u32>,
    },
}
