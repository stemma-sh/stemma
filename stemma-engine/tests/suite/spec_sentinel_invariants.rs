//! Real-structure SENTINEL CORPUS — relational invariants run DAILY over the
//! structural classes that today only the DOCX corpus / Word-oracle tiers touch.
//!
//! # Why this file exists
//!
//! The daily merge gate (`just -f stemma-engine/Justfile gate`) is hermetic: it has no
//! DOCX corpus and no Word VM. The ~679 checked-in fixtures are swept by
//! `roundtrip_fidelity.rs`, and `redline_fixpoint_daily.rs` runs the fixpoint
//! chain over a curated category list — but BOTH skip gracefully when the corpus
//! is absent, so in the hermetic gate they check **zero real structures**. That
//! means a whole structural class (a tracked move across a paragraph boundary, a
//! pre-existing ins/del spanning a paragraph mark, a field inside a changed run,
//! a footnote-body edit, a header/footer redline, a nested-table cell change, the
//! Tier-2 formatting bundle) can regress while the daily gate stays green.
//!
//! This file closes that blind spot. Every witness is a small **in-memory**
//! (before, after) DOCX pair (the existing `pack(document_xml)` idiom — no
//! corpus dependency), so the relational invariants below run on EVERY
//! `cargo test -p stemma`, ratcheting the daily gate toward Word.
//!
//! # Invariants asserted per witness (catalog numbers from
//! `stemma-engine/docs/testing_strategy.md`)
//!
//! - **#6** accept/reject: `accept_all(merge_diff(A,B)) == text(B)` and
//!   `reject_all(merge_diff(A,B)) == text(A)`, in canonical space.
//! - **#7** fixpoint: `diff -> merge -> accept_all -> re-diff(merged, B)` is empty.
//! - **#10** identity: `diff(A,A)` empty AND `redline(A,A)` emits no tracked spans.
//! - **#18 (Tier-2)** formatting parity: `style_id` / `heading_level` /
//!   `num_id` / `ilvl` / `Bold` / opaque survive the accept projection on the
//!   DIFF/redline path (the catalog only covers this via Word today).
//! - **#13 (hermetic proxy)** `serialize(redline(A,B))` is validator-clean
//!   (no Error-severity `validate_docx` findings) — the in-process stand-in for
//!   "opens clean in Word".
//!
//! Tests encode the **domain rule** (what accept/reject *should* yield per
//! ECMA-376 §17.13.5), not whatever the code happens to produce today. A witness
//! that exposes a real bug is a coverage WIN: it is either fixed at the source or
//! `#[ignore]`d with a precise description of where the data first goes wrong.
//!
//! Daily tier, corpus-free.

use stemma::edit::{
    ContentFragment, EditStep, EditTransaction, MaterializationMode, ParagraphContent,
    apply_transaction,
};
use stemma::{
    BlockNode, CanonDoc, DiffChange, DocxRuntime, ExportMode, InlineNode, Mark, RevisionInfo,
    SimpleRuntime, TransactionMeta, accept_all, diff_documents, merge_diff,
    redline_extract::{RedlineSpan, extract_redline},
};

use crate::common;

// ── packaging: minimal in-memory DOCX (the repo-wide test idiom) ───────────

/// Pack a `word/document.xml` body into a minimal single-part DOCX.
fn pack(document_xml: &str) -> Vec<u8> {
    pack_full(document_xml, &[], &[])
}

/// Pack `word/document.xml` plus extra parts (e.g. footnotes / headers). Each
/// part contributes a `[Content_Types]` override AND a `document.xml.rels`
/// relationship pointing at it.
fn pack_with_parts(document_xml: &str, extra: &[ExtraPart]) -> Vec<u8> {
    pack_full(document_xml, extra, &[])
}

/// An internal OPC part (footnotes / header / footer) referenced from
/// `document.xml` by a relationship.
#[derive(Clone)]
struct ExtraPart {
    /// Part name inside the package, e.g. `word/footnotes.xml`.
    name: &'static str,
    /// `<Override>` content-type for `[Content_Types].xml`.
    content_type: &'static str,
    /// Relationship entry (id, type, target) for `document.xml.rels`.
    rel: (&'static str, &'static str, &'static str),
    xml: String,
}

/// An external relationship (e.g. a hyperlink) — `TargetMode="External"`, no
/// part, no content-type override.
#[derive(Clone, Copy)]
struct ExternalRel {
    id: &'static str,
    rel_type: &'static str,
    target: &'static str,
}

fn pack_full(document_xml: &str, parts: &[ExtraPart], external_rels: &[ExternalRel]) -> Vec<u8> {
    use std::io::Write;
    use zip::write::FileOptions;

    let overrides: String = parts
        .iter()
        .map(|p| {
            format!(
                r#"<Override PartName="/{}" ContentType="{}"/>"#,
                p.name, p.content_type
            )
        })
        .collect();
    let content_types = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>{overrides}</Types>"#
    );

    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;

    let mut doc_rel_entries = String::new();
    for p in parts {
        let (id, ty, target) = p.rel;
        doc_rel_entries.push_str(&format!(
            r#"<Relationship Id="{id}" Type="{ty}" Target="{target}"/>"#
        ));
    }
    for r in external_rels {
        doc_rel_entries.push_str(&format!(
            r#"<Relationship Id="{}" Type="{}" Target="{}" TargetMode="External"/>"#,
            r.id, r.rel_type, r.target
        ));
    }
    let doc_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{doc_rel_entries}</Relationships>"#
    );

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
        for p in parts {
            zip.start_file(p.name, opts).unwrap();
            zip.write_all(p.xml.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

/// Wrap a body fragment in a full `w:document`. Declares the namespaces the
/// witnesses use (w, w14 for paraId, r for hyperlinks).
fn document(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    )
}

// ── text extraction in canonical space ─────────────────────────────────────

/// All paragraph text across the document (recursing into tables), space-joined.
/// Matches the `all_text` helper used by the accept/reject spec tests.
fn all_text(doc: &CanonDoc) -> String {
    common::all_paragraphs(doc)
        .iter()
        .map(|p| common::paragraph_text(p))
        .collect::<Vec<_>>()
        .join(" ")
}

fn revision() -> RevisionInfo {
    RevisionInfo {
        revision_id: 1,
        identity: 0,
        author: Some("sentinel".to_string()),
        date: Some("2026-06-01T00:00:00Z".to_string()),
        apply_op_id: None,
    }
}

fn redline_meta() -> TransactionMeta {
    TransactionMeta {
        author: "sentinel".to_string(),
        reason: Some("sentinel invariant".to_string()),
        timestamp_utc: Some("2026-06-01T00:00:00Z".to_string()),
    }
}

// ── the canonical-space relational harness (#6, #7, #10, #18) ──────────────

/// Import a before/after pair through the diff -> merge_diff path and assert the
/// relational invariants in canonical space. `expected_accept` / `expected_reject`
/// are the domain-rule texts (B and A normalized through the same extractor),
/// computed by the caller from the imported docs.
struct Pair {
    /// Human description of the structural class this witness covers.
    class: &'static str,
    a: CanonDoc,
    b: CanonDoc,
}

impl Pair {
    fn build(class: &'static str, a_bytes: &[u8], b_bytes: &[u8]) -> Self {
        let rt = SimpleRuntime::new();
        let a = std::sync::Arc::unwrap_or_clone(
            rt.import_docx(a_bytes)
                .unwrap_or_else(|e| panic!("[{class}] import A: {e:?}"))
                .canonical,
        );
        let b = std::sync::Arc::unwrap_or_clone(
            rt.import_docx(b_bytes)
                .unwrap_or_else(|e| panic!("[{class}] import B: {e:?}"))
                .canonical,
        );
        Pair { class, a, b }
    }

    /// Merge A→B into a tracked-change document via the diff/merge path.
    fn merged(&self) -> CanonDoc {
        let diff = diff_documents(&self.a, &self.b)
            .unwrap_or_else(|e| panic!("[{}] diff_documents A→B: {e}", self.class));
        merge_diff(&self.a, &self.b, &diff, &revision())
            .unwrap_or_else(|e| panic!("[{}] merge_diff: {e:?}", self.class))
            .doc
    }

    /// #6: accept_all(merge) == text(B); reject_all(merge) == text(A).
    fn assert_accept_reject(&self) {
        let class = self.class;
        let want_b = all_text(&self.b);
        let want_a = all_text(&self.a);

        let mut accepted = self.merged();
        accept_all(&mut accepted);
        assert_eq!(
            all_text(&accepted),
            want_b,
            "[{class}] #6 accept_all must equal target B text"
        );

        let mut rejected = self.merged();
        stemma::reject_all_with_styles(&mut rejected, None);
        assert_eq!(
            all_text(&rejected),
            want_a,
            "[{class}] #6 reject_all must equal base A text"
        );
    }

    /// #7: diff -> merge -> accept_all -> re-diff(merged, B) is empty.
    fn assert_fixpoint(&self) {
        let class = self.class;
        let mut merged = self.merged();
        accept_all(&mut merged);
        let fixpoint = diff_documents(&merged, &self.b)
            .unwrap_or_else(|e| panic!("[{class}] fixpoint re-diff: {e}"));
        assert!(
            fixpoint.changes.is_empty(),
            "[{class}] #7 fixpoint violated: accept_all(merge(A,B)) still differs from B by {} change(s): {}",
            fixpoint.changes.len(),
            describe_changes(&fixpoint.changes),
        );
    }

    /// #10 identity: diff(A,A) empty AND diff(B,B) empty.
    fn assert_identity(&self) {
        let class = self.class;
        for (label, doc) in [("A", &self.a), ("B", &self.b)] {
            let diff = diff_documents(doc, doc)
                .unwrap_or_else(|e| panic!("[{class}] identity diff({label},{label}): {e}"));
            assert!(
                diff.changes.is_empty(),
                "[{class}] #10 identity diff({label},{label}) produced {} change(s): {}",
                diff.changes.len(),
                describe_changes(&diff.changes),
            );
        }
    }

    fn assert_core(&self) {
        // Guard against a vacuous witness: a diff pair whose A and B already
        // collapse to the same text would make accept/reject/fixpoint pass
        // trivially. Every diff-pair witness must encode a real A→B change.
        assert_ne!(
            all_text(&self.a),
            all_text(&self.b),
            "[{}] witness is vacuous — A and B have identical text",
            self.class
        );
        self.assert_identity();
        self.assert_accept_reject();
        self.assert_fixpoint();
    }
}

fn describe_changes(changes: &[DiffChange]) -> String {
    changes
        .iter()
        .take(8)
        .map(|c| format!("{c:?}").chars().take(140).collect::<String>())
        .collect::<Vec<_>>()
        .join(" | ")
}

// ── runtime redline harness (#13 validator-clean + #10 redline-no-markup) ──

/// Run the full runtime redline pipeline (import A, import B, diff_and_redline,
/// export Redline) and return the exported bytes. This is the path that covers
/// non-body stories (headers / footers / footnotes), which the canonical-space
/// `diff_documents` harness does not.
fn redline_export(class: &str, a_bytes: &[u8], b_bytes: &[u8]) -> Vec<u8> {
    let rt = SimpleRuntime::new();
    let ia = rt
        .import_docx(a_bytes)
        .unwrap_or_else(|e| panic!("[{class}] import A: {e:?}"));
    let ib = rt
        .import_docx(b_bytes)
        .unwrap_or_else(|e| panic!("[{class}] import B: {e:?}"));
    let apply = rt
        .diff_and_redline(&ia.doc_handle, &ib.doc_handle, redline_meta())
        .unwrap_or_else(|e| panic!("[{class}] diff_and_redline: {e:?}"));
    assert!(apply.applied, "[{class}] redline must be applied");
    rt.export_docx(&ia.doc_handle, ExportMode::Redline)
        .unwrap_or_else(|e| panic!("[{class}] export_docx Redline: {e:?}"))
}

/// #13 hermetic proxy: the serialized redline output must carry no
/// Error-severity validator findings (the in-process stand-in for "opens clean
/// in Word"). A finding here means we emitted structurally invalid OOXML.
fn assert_validator_clean(class: &str, bytes: &[u8]) {
    let validation = stemma::docx_validate::validate_docx(bytes);
    let errors: Vec<String> = validation.errors().map(|f| format!("{f}")).collect();
    assert!(
        errors.is_empty(),
        "[{class}] #13 serialize(redline) is not validator-clean ({} error finding(s)):\n  {}",
        errors.len(),
        errors.join("\n  "),
    );
}

/// #10 (redline path): `redline(A,A)` must emit zero tracked-change spans.
fn assert_redline_identity(class: &str, a_bytes: &[u8]) {
    let exported = redline_export(class, a_bytes, a_bytes);
    let extract =
        extract_redline(&exported).unwrap_or_else(|e| panic!("[{class}] extract_redline: {e}"));
    let phantom = extract.body.iter().any(|p| {
        p.spans
            .iter()
            .any(|s| matches!(s, RedlineSpan::Inserted(_) | RedlineSpan::Deleted(_)))
    });
    assert!(
        !phantom,
        "[{class}] #10 redline(A,A) emitted phantom tracked-change markup"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  WITNESSES
//  Each builds a minimal (before, after) pair for one structural class and
//  states why the expected accept/reject text is correct per the domain rule.
// ═══════════════════════════════════════════════════════════════════════════

// (a) Tracked MOVE across a paragraph boundary — modelled as a content diff:
//     a sentence that exists in paragraph 1 of A is relocated to paragraph 2 in
//     B. The diff/merge path must reach a state where accept == B (sentence in
//     P2) and reject == A (sentence in P1). Fixpoint must hold.
fn move_across_paragraph() -> (Vec<u8>, Vec<u8>) {
    let a = pack(&document(
        r#"<w:p><w:r><w:t xml:space="preserve">Alpha MOVEME tail</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">Beta body</w:t></w:r></w:p>"#,
    ));
    let b = pack(&document(
        r#"<w:p><w:r><w:t xml:space="preserve">Alpha tail</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">Beta MOVEME body</w:t></w:r></w:p>"#,
    ));
    (a, b)
}

// (a) Tracked MOVE across a table-cell boundary — a token moves out of cell
//     (r1,c1) into cell (r1,c2). accept == B, reject == A.
fn move_across_table_cell() -> (Vec<u8>, Vec<u8>) {
    let cell = |t: &str| {
        format!(
            r#"<w:tc><w:tcPr><w:tcW w:w="2400" w:type="dxa"/></w:tcPr><w:p><w:r><w:t xml:space="preserve">{t}</w:t></w:r></w:p></w:tc>"#
        )
    };
    let table = |c1: &str, c2: &str| {
        format!(
            r#"<w:tbl><w:tblPr><w:tblW w:w="4800" w:type="dxa"/></w:tblPr><w:tblGrid><w:gridCol w:w="2400"/><w:gridCol w:w="2400"/></w:tblGrid><w:tr>{}{}</w:tr></w:tbl><w:p/>"#,
            cell(c1),
            cell(c2)
        )
    };
    let a = pack(&document(&table("Left TOKEN", "Right")));
    let b = pack(&document(&table("Left", "Right TOKEN")));
    (a, b)
}

// (b) Pre-existing ins/del spanning a paragraph mark. A already carries a
//     deleted paragraph mark (w:p[0]/pPr/rPr/del) plus a tracked insertion in
//     the following paragraph. Domain rule (§17.13.5.15): accepting the deleted
//     para-mark MERGES the two paragraphs; accepting the w:ins keeps inserted
//     text. We assert accept/reject text over the IMPORT-preserved canonical of
//     A itself (this witness is a static tracked-change doc, not a diff pair).
fn preexisting_change_across_paragraph_mark() -> Vec<u8> {
    pack(&document(
        r#"<w:p><w:pPr><w:rPr><w:del w:id="1" w:author="x" w:date="2026-01-01T00:00:00Z"/></w:rPr></w:pPr><w:r><w:t xml:space="preserve">First</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">Second </w:t></w:r><w:ins w:id="2" w:author="x" w:date="2026-01-01T00:00:00Z"><w:r><w:t xml:space="preserve">added</w:t></w:r></w:ins></w:p>"#,
    ))
}

// (b) Pre-existing ins/del at a table-cell boundary: a tracked insertion inside
//     a cell and a tracked deletion inside the neighbouring cell.
fn preexisting_change_across_table_cell() -> Vec<u8> {
    pack(&document(
        r#"<w:tbl><w:tblPr><w:tblW w:w="4800" w:type="dxa"/></w:tblPr><w:tblGrid><w:gridCol w:w="2400"/><w:gridCol w:w="2400"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="2400" w:type="dxa"/></w:tcPr><w:p><w:r><w:t xml:space="preserve">Keep </w:t></w:r><w:ins w:id="1" w:author="x" w:date="2026-01-01T00:00:00Z"><w:r><w:t xml:space="preserve">INS</w:t></w:r></w:ins></w:p></w:tc><w:tc><w:tcPr><w:tcW w:w="2400" w:type="dxa"/></w:tcPr><w:p><w:del w:id="2" w:author="x" w:date="2026-01-01T00:00:00Z"><w:r><w:delText xml:space="preserve">DEL</w:delText></w:r></w:del><w:r><w:t xml:space="preserve">stay</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/>"#,
    ))
}

// (c) A hyperlink inside a CHANGED run: A has body text "see link here", B
//     changes the surrounding words but the run carries a w:hyperlink whose
//     visible text changes. The hyperlink opaque must survive accept; accept==B.
fn hyperlink_in_changed_run() -> (Vec<u8>, Vec<u8>) {
    let body = |lead: &str, link: &str| {
        format!(
            r#"<w:p><w:r><w:t xml:space="preserve">{lead} </w:t></w:r><w:hyperlink r:id="rIdLink"><w:r><w:t xml:space="preserve">{link}</w:t></w:r></w:hyperlink><w:r><w:t xml:space="preserve"> end</w:t></w:r></w:p>"#
        )
    };
    // The hyperlink references an EXTERNAL target relationship; without it the
    // package is invalid (a dangling r:id), so the witness ships the rel.
    let link_rel = ExternalRel {
        id: "rIdLink",
        rel_type: "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink",
        target: "https://example.com/anchor",
    };
    let a = pack_full(
        &document(&body("Please review the", "old anchor")),
        &[],
        &[link_rel],
    );
    let b = pack_full(
        &document(&body("Kindly examine the", "new anchor")),
        &[],
        &[link_rel],
    );
    (a, b)
}

// (c) A field (w:fldSimple) inside a CHANGED run: surrounding text changes while
//     the PAGE field stays. The field is opaque and must survive accept.
fn field_in_changed_run() -> (Vec<u8>, Vec<u8>) {
    let body = |lead: &str| {
        format!(
            r#"<w:p><w:r><w:t xml:space="preserve">{lead} page </w:t></w:r><w:fldSimple w:instr=" PAGE "><w:r><w:t>1</w:t></w:r></w:fldSimple></w:p>"#
        )
    };
    let a = pack(&document(&body("See")));
    let b = pack(&document(&body("Refer to")));
    (a, b)
}

// (c) A block-level SDT (content control) next to a CHANGED paragraph. A
//     body-level w:sdt imports as a whole OpaqueBlock(Sdt) (import.rs §1237 —
//     the inner text is referenced by body index, never parsed into the
//     canonical model), so the structural class this witnesses is "an opaque
//     content control survives the diff/redline path while an adjacent run
//     changes": the SDT opaque inventory must NOT shrink across accept, and the
//     body text change must satisfy accept==B / reject==A.
fn sdt_adjacent_to_changed_run() -> (Vec<u8>, Vec<u8>) {
    let sdt = r#"<w:sdt><w:sdtPr><w:alias w:val="field"/><w:id w:val="101"/></w:sdtPr><w:sdtContent><w:p><w:r><w:t xml:space="preserve">control value</w:t></w:r></w:p></w:sdtContent></w:sdt>"#;
    let body =
        |t: &str| format!(r#"<w:p><w:r><w:t xml:space="preserve">{t}</w:t></w:r></w:p>{sdt}"#);
    let a = pack(&document(&body("draft text")));
    let b = pack(&document(&body("final text")));
    (a, b)
}

/// Count OpaqueBlock(Sdt) blocks at the top level of a document.
fn count_sdt_opaque(doc: &CanonDoc) -> usize {
    doc.blocks
        .iter()
        .filter(|tb| {
            matches!(
                &tb.block,
                BlockNode::OpaqueBlock(o) if matches!(o.kind, stemma::OpaqueKind::Sdt)
            )
        })
        .count()
}

// (d) Footnote-body edit: the footnote story text changes between A and B. The
//     redline path must carry tracked changes in word/footnotes.xml and stay
//     validator-clean.
fn footnote_body_edit() -> (Vec<u8>, Vec<u8>) {
    let footnotes = |note: &str| ExtraPart {
        name: "word/footnotes.xml",
        content_type: "application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml",
        rel: (
            "rIdFn",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes",
            "footnotes.xml",
        ),
        xml: format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote><w:footnote w:id="1"><w:p><w:r><w:rPr><w:rStyle w:val="FootnoteReference"/></w:rPr><w:footnoteRef/></w:r><w:r><w:t xml:space="preserve">{note}</w:t></w:r></w:p></w:footnote></w:footnotes>"#
        ),
    };
    let doc = r#"<w:p><w:r><w:t xml:space="preserve">Body text</w:t></w:r><w:r><w:rPr><w:rStyle w:val="FootnoteReference"/></w:rPr><w:footnoteReference w:id="1"/></w:r></w:p>"#;
    let a = pack_with_parts(&document(doc), &[footnotes("original note")]);
    let b = pack_with_parts(&document(doc), &[footnotes("revised note")]);
    (a, b)
}

// (e) Header/footer redline: the header story text changes between A and B. The
//     redline path must carry tracked changes in word/header1.xml.
fn header_redline() -> (Vec<u8>, Vec<u8>) {
    let header = |t: &str| ExtraPart {
        name: "word/header1.xml",
        content_type: "application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml",
        rel: (
            "rIdHdr",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header",
            "header1.xml",
        ),
        xml: format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:r><w:t xml:space="preserve">{t}</w:t></w:r></w:p></w:hdr>"#
        ),
    };
    // Reference the header from the section so it is a live story.
    let body = r#"<w:p><w:r><w:t xml:space="preserve">Body</w:t></w:r></w:p><w:sectPr><w:headerReference w:type="default" r:id="rIdHdr"/></w:sectPr>"#;
    let doc = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>{body}</w:body></w:document>"#
    );
    let a = pack_with_parts(&doc, &[header("Original Header")]);
    let b = pack_with_parts(&doc, &[header("Modified Header")]);
    (a, b)
}

// (f) Nested-table cell change: an inner table sits inside an outer cell; the
//     inner cell's text changes A→B. accept==B, reject==A, fixpoint holds.
fn nested_table_cell_change() -> (Vec<u8>, Vec<u8>) {
    let inner = |t: &str| {
        format!(
            r#"<w:tbl><w:tblPr><w:tblW w:w="2000" w:type="dxa"/></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t xml:space="preserve">{t}</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/>"#
        )
    };
    let outer = |inner_xml: &str| {
        format!(
            r#"<w:tbl><w:tblPr><w:tblW w:w="4800" w:type="dxa"/></w:tblPr><w:tblGrid><w:gridCol w:w="4800"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="4800" w:type="dxa"/></w:tcPr>{inner_xml}</w:tc></w:tr></w:tbl><w:p/>"#
        )
    };
    let a = pack(&document(&outer(&inner("inner OLD"))));
    let b = pack(&document(&outer(&inner("inner NEW"))));
    (a, b)
}

// (g) Heading-style + numbering + bold runs (the Tier-2 formatting bundle).
//     A→B edits the body text of a Heading1, numbered (numId/ilvl), bold-run
//     paragraph. The accept projection must preserve style_id="Heading1",
//     heading_level, numbering(num_id,ilvl), and the Bold mark — these are
//     exactly the Tier-2 attributes that #18 only covers via Word today.
fn formatting_tier_bundle() -> (Vec<u8>, Vec<u8>) {
    let para = |t: &str| {
        format!(
            r#"<w:p><w:pPr><w:pStyle w:val="Heading1"/><w:numPr><w:ilvl w:val="0"/><w:numId w:val="3"/></w:numPr></w:pPr><w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">{t}</w:t></w:r></w:p>"#
        )
    };
    // numId=3 must be defined by word/numbering.xml or the importer drops the
    // NumberingInfo (numbering is Some only when definitions resolve, per
    // import.rs synthesize). Heading1 is a built-in style; heading_level is
    // derived from the "HeadingN" style id, so no styles.xml part is required.
    let numbering = ExtraPart {
        name: "word/numbering.xml",
        content_type:
            "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml",
        rel: (
            "rIdNum",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering",
            "numbering.xml",
        ),
        xml: r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum><w:num w:numId="3"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#.to_string(),
    };
    let numbering_b = numbering.clone();
    let a = pack_with_parts(
        &document(&para("Heading old text")),
        std::slice::from_ref(&numbering),
    );
    let b = pack_with_parts(
        &document(&para("Heading new text")),
        std::slice::from_ref(&numbering_b),
    );
    (a, b)
}

// ── tests ──────────────────────────────────────────────────────────────────

#[test]
fn sentinel_untouched_equal_format_run_boundaries_survive_text_edit() {
    let source = pack(&document(
        r#"<w:p><w:pPr><w:jc w:val="both"/></w:pPr><w:r><w:t xml:space="preserve">Alpha </w:t></w:r><w:r><w:t xml:space="preserve">Beta </w:t></w:r><w:r><w:t>Gamma</w:t></w:r></w:p>"#,
    ));
    let runtime = SimpleRuntime::new();
    let canon = std::sync::Arc::unwrap_or_clone(
        runtime
            .import_docx(&source)
            .expect("import run-boundary sentinel")
            .canonical,
    );
    let block_id = canon
        .blocks
        .iter()
        .find_map(|tracked| match &tracked.block {
            BlockNode::Paragraph(paragraph) => Some(paragraph.id.clone()),
            _ => None,
        })
        .expect("sentinel must contain a paragraph");
    let transaction = EditTransaction {
        steps: vec![EditStep::ReplaceParagraphText {
            block_id: block_id.clone(),
            expect: "Alpha Beta Gamma".to_string(),
            content: ParagraphContent {
                fragments: vec![ContentFragment::Text("Alpha Beta Delta".to_string())],
            },
            rationale: None,
            replacement_role: None,
            semantic_hash: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: revision(),
    };
    let (edited, _) = apply_transaction(&canon, &transaction).expect("apply sentinel edit");
    let paragraph = edited
        .blocks
        .iter()
        .find_map(|tracked| match &tracked.block {
            BlockNode::Paragraph(paragraph) if paragraph.id == block_id => Some(paragraph),
            _ => None,
        })
        .expect("edited sentinel paragraph");
    let untouched_runs: Vec<&str> = paragraph
        .segments
        .iter()
        .filter(|segment| matches!(segment.status, stemma::TrackingStatus::Normal))
        .flat_map(|segment| segment.inlines.iter())
        .filter_map(|inline| match inline {
            InlineNode::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        untouched_runs,
        vec!["Alpha ", "Beta "],
        "an edit to Gamma must retain both untouched source runs"
    );

    let mut accepted = edited.clone();
    accept_all(&mut accepted);
    assert_eq!(all_text(&accepted), "Alpha Beta Delta");
    let mut rejected = edited;
    // This minimal witness has no styles part, so `None` is exact here.
    stemma::reject_all_with_styles(&mut rejected, None);
    assert_eq!(all_text(&rejected), "Alpha Beta Gamma");
}

#[test]
fn sentinel_split_literal_prefix_runs_survive_rebuild() {
    let source = pack(&document(
        r#"<w:p><w:pPr><w:jc w:val="both"/></w:pPr><w:r><w:t>1</w:t></w:r><w:r><w:t>.</w:t></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:t>Body text here.</w:t></w:r></w:p>"#,
    ));
    let rebuilt = stemma::api::Document::parse(&source)
        .expect("parse split-prefix sentinel")
        .serialize(&stemma::ExportOptions::default())
        .expect("serialize split-prefix sentinel");
    let xml = extract_part(&rebuilt, "word/document.xml");
    let paragraph = &xml
        [xml.find("<w:p>").expect("paragraph start")..xml.find("</w:p>").expect("paragraph end")];
    let run_count = paragraph.matches("<w:r>").count() + paragraph.matches("<w:r ").count();
    assert_eq!(
        run_count, 4,
        "identity rebuild must retain both label runs, the separator run, and the body run: {paragraph}"
    );
    assert!(
        paragraph.contains(">1</w:t>") && paragraph.contains(">.</w:t>"),
        "the split label must not be consolidated into one run: {paragraph}"
    );
}

#[test]
fn sentinel_move_across_paragraph_boundary() {
    let (a, b) = move_across_paragraph();
    Pair::build("move-across-paragraph", &a, &b).assert_core();
    assert_validator_clean(
        "move-across-paragraph",
        &redline_export("move-across-paragraph", &a, &b),
    );
}

#[test]
fn sentinel_move_across_table_cell_boundary() {
    let (a, b) = move_across_table_cell();
    Pair::build("move-across-table-cell", &a, &b).assert_core();
    assert_validator_clean(
        "move-across-table-cell",
        &redline_export("move-across-table-cell", &a, &b),
    );
}

#[test]
fn sentinel_preexisting_change_across_paragraph_mark() {
    // Static tracked-change doc: import-preserved canonical, then accept/reject.
    // Domain rule (§17.13.5.15): accepting the deleted para mark merges P1 into
    // P2 and keeps the w:ins text → "First Second added". Rejecting restores the
    // two separate paragraphs and drops the insertion → "First Second ".
    let bytes = preexisting_change_across_paragraph_mark();
    let rt = SimpleRuntime::new();
    let canon = std::sync::Arc::unwrap_or_clone(rt.import_docx(&bytes).expect("import").canonical);

    let mut accepted = canon.clone();
    accept_all(&mut accepted);
    let accepted_text = all_text(&accepted);
    assert!(
        accepted_text.contains("First")
            && accepted_text.contains("Second")
            && accepted_text.contains("added"),
        "#6 accept of pre-existing para-mark del + ins must keep 'First', 'Second', 'added'; got {accepted_text:?}"
    );
    // Accepting the deleted para mark merges into ONE top-level paragraph.
    let accepted_top = accepted
        .blocks
        .iter()
        .filter(|tb| matches!(&tb.block, BlockNode::Paragraph(_)))
        .count();
    assert_eq!(
        accepted_top, 1,
        "#6 accepting a deleted paragraph mark must merge two paragraphs into one"
    );

    let mut rejected = canon.clone();
    stemma::reject_all_with_styles(&mut rejected, None);
    let rejected_text = all_text(&rejected);
    assert!(
        rejected_text.contains("First")
            && rejected_text.contains("Second")
            && !rejected_text.contains("added"),
        "#6 reject must keep base text and drop the insertion 'added'; got {rejected_text:?}"
    );
    let rejected_top = rejected
        .blocks
        .iter()
        .filter(|tb| matches!(&tb.block, BlockNode::Paragraph(_)))
        .count();
    assert_eq!(
        rejected_top, 2,
        "#6 rejecting a deleted paragraph mark must keep the two paragraphs separate"
    );
}

#[test]
fn sentinel_preexisting_change_across_table_cell() {
    let bytes = preexisting_change_across_table_cell();
    let rt = SimpleRuntime::new();
    let canon = std::sync::Arc::unwrap_or_clone(rt.import_docx(&bytes).expect("import").canonical);

    // accept: insertion stays, deletion removed → "Keep INS" + "stay"
    let mut accepted = canon.clone();
    accept_all(&mut accepted);
    let acc = all_text(&accepted);
    assert!(
        acc.contains("Keep") && acc.contains("INS") && acc.contains("stay") && !acc.contains("DEL"),
        "#6 accept of cell ins/del: keep INS+stay, drop DEL; got {acc:?}"
    );

    // reject: insertion removed, deletion restored → "Keep" + "DELstay"
    let mut rejected = canon.clone();
    stemma::reject_all_with_styles(&mut rejected, None);
    let rej = all_text(&rejected);
    assert!(
        rej.contains("Keep") && rej.contains("DEL") && rej.contains("stay") && !rej.contains("INS"),
        "#6 reject of cell ins/del: keep DEL+stay, drop INS; got {rej:?}"
    );
}

#[test]
fn sentinel_hyperlink_in_changed_run() {
    let (a, b) = hyperlink_in_changed_run();
    let pair = Pair::build("hyperlink-in-changed-run", &a, &b);
    pair.assert_core();

    // The hyperlink imports as an OpaqueInline(Hyperlink). It must survive the
    // accept projection (opaque preservation) even though the surrounding run
    // text changes A→B.
    let is_link = |k: &stemma::OpaqueKind| matches!(k, stemma::OpaqueKind::Hyperlink(_));
    assert_eq!(
        count_opaque_inline(&pair.a, is_link),
        1,
        "fixture A should import exactly one hyperlink opaque inline"
    );
    let mut accepted = pair.merged();
    accept_all(&mut accepted);
    assert_eq!(
        count_opaque_inline(&accepted, is_link),
        1,
        "#18/I1: hyperlink opaque must survive the accept projection"
    );

    assert_redline_identity("hyperlink-in-changed-run", &a);
    assert_validator_clean(
        "hyperlink-in-changed-run",
        &redline_export("hyperlink-in-changed-run", &a, &b),
    );
}

#[test]
fn sentinel_field_in_changed_run() {
    let (a, b) = field_in_changed_run();
    let pair = Pair::build("field-in-changed-run", &a, &b);
    pair.assert_core();

    // The PAGE field imports as an OpaqueInline(Field). It must survive accept
    // even though the lead run text changes A→B.
    let is_field = |k: &stemma::OpaqueKind| matches!(k, stemma::OpaqueKind::Field(_));
    assert_eq!(
        count_opaque_inline(&pair.a, is_field),
        1,
        "fixture A should import exactly one field opaque inline"
    );
    let mut accepted = pair.merged();
    accept_all(&mut accepted);
    assert_eq!(
        count_opaque_inline(&accepted, is_field),
        1,
        "#18/I1: field opaque must survive the accept projection"
    );

    assert_validator_clean(
        "field-in-changed-run",
        &redline_export("field-in-changed-run", &a, &b),
    );
}

#[test]
fn sentinel_sdt_adjacent_to_changed_run() {
    let (a, b) = sdt_adjacent_to_changed_run();
    let pair = Pair::build("sdt-adjacent-to-changed-run", &a, &b);
    pair.assert_core();

    // Opaque preservation: the SDT opaque block must still be present after the
    // accept projection — a diff/merge must never silently drop a content
    // control while editing an adjacent run.
    assert_eq!(
        count_sdt_opaque(&pair.a),
        1,
        "fixture A should import exactly one OpaqueBlock(Sdt)"
    );
    let mut accepted = pair.merged();
    accept_all(&mut accepted);
    assert_eq!(
        count_sdt_opaque(&accepted),
        1,
        "#18/I1 opaque preservation: SDT content control must survive the accept projection"
    );

    assert_validator_clean(
        "sdt-adjacent-to-changed-run",
        &redline_export("sdt-adjacent-to-changed-run", &a, &b),
    );
}

#[test]
fn sentinel_footnote_body_edit() {
    let (a, b) = footnote_body_edit();
    // Footnote story lives outside the body, so use the runtime redline path.
    let exported = redline_export("footnote-body-edit", &a, &b);
    assert_validator_clean("footnote-body-edit", &exported);
    let extract = extract_redline(&exported).expect("extract_redline");
    let (deleted, inserted) = extract
        .tracked_changes_in("word/footnotes.xml")
        .expect("footnotes part should carry tracked changes");
    assert!(
        deleted.join(" ").contains("original"),
        "#6 footnote edit: deleted text should contain 'original', got {deleted:?}"
    );
    assert!(
        inserted.join(" ").contains("revised"),
        "#6 footnote edit: inserted text should contain 'revised', got {inserted:?}"
    );
    // #10: redline(A,A) over the footnote story emits no markup.
    assert_redline_identity("footnote-body-edit", &a);
}

#[test]
fn sentinel_header_redline() {
    let (a, b) = header_redline();
    let exported = redline_export("header-redline", &a, &b);
    assert_validator_clean("header-redline", &exported);
    let extract = extract_redline(&exported).expect("extract_redline");
    let (deleted, inserted) = extract
        .tracked_changes_in("word/header1.xml")
        .expect("header part should carry tracked changes");
    assert!(
        deleted.join(" ").contains("Original"),
        "#6 header redline: deleted should contain 'Original', got {deleted:?}"
    );
    assert!(
        inserted.join(" ").contains("Modified"),
        "#6 header redline: inserted should contain 'Modified', got {inserted:?}"
    );
    assert_redline_identity("header-redline", &a);
}

#[test]
fn sentinel_nested_table_cell_change() {
    let (a, b) = nested_table_cell_change();
    let pair = Pair::build("nested-table-cell-change", &a, &b);
    pair.assert_core();
    assert_validator_clean(
        "nested-table-cell-change",
        &redline_export("nested-table-cell-change", &a, &b),
    );
}

#[test]
fn sentinel_formatting_tier_bundle() {
    let (a, b) = formatting_tier_bundle();
    let pair = Pair::build("formatting-tier-bundle", &a, &b);
    // Core relational invariants first.
    pair.assert_core();

    // #18 Tier-2: the accept projection must preserve the formatting bundle.
    let mut accepted = pair.merged();
    accept_all(&mut accepted);
    let para = first_paragraph(&accepted);

    assert_eq!(
        para.style_id.as_deref(),
        Some("Heading1"),
        "#18 Tier-2: style_id must survive accept on the diff path"
    );
    assert!(
        para.heading_level.is_some(),
        "#18 Tier-2: heading_level must survive accept (style is Heading1)"
    );
    let numbering = para
        .numbering
        .as_ref()
        .expect("#18 Tier-2: numbering must survive accept");
    assert_eq!(
        (numbering.num_id, numbering.ilvl),
        (3, 0),
        "#18 Tier-2: num_id/ilvl must survive accept on the diff path"
    );
    assert!(
        paragraph_has_bold(para),
        "#18 Tier-2: Bold mark must survive accept on the diff path"
    );

    assert_validator_clean(
        "formatting-tier-bundle",
        &redline_export("formatting-tier-bundle", &a, &b),
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tracked-content serializer content-model witnesses
// ═══════════════════════════════════════════════════════════════════════════

/// The `word/document.xml` string of a packaged DOCX.
fn document_xml(bytes: &[u8]) -> String {
    common::read_zip_entry(bytes, "word/document.xml")
        .expect("exported DOCX must contain word/document.xml")
}

/// Canonical (normalized) text of a DOCX, via a fresh import.
fn imported_text(bytes: &[u8]) -> String {
    let rt = SimpleRuntime::new();
    let canon =
        std::sync::Arc::unwrap_or_clone(rt.import_docx(bytes).expect("import for text").canonical);
    all_text(&canon)
}

/// #14/#12 through serialize→reparse: re-import the exported redline bytes and
/// assert `accept_all` yields the target text and `reject_all` the base text.
fn assert_reparse_accept_reject(class: &str, redline: &[u8], want_accept: &str, want_reject: &str) {
    let rt = SimpleRuntime::new();
    let canon = std::sync::Arc::unwrap_or_clone(
        rt.import_docx(redline)
            .unwrap_or_else(|e| panic!("[{class}] re-import redline: {e:?}"))
            .canonical,
    );
    let mut accepted = canon.clone();
    accept_all(&mut accepted);
    assert_eq!(
        all_text(&accepted),
        want_accept,
        "[{class}] accept_all through serialize→reparse must equal target text"
    );
    let mut rejected = canon;
    stemma::reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        all_text(&rejected),
        want_reject,
        "[{class}] reject_all through serialize→reparse must equal base text"
    );
}

/// Extract each `<w:del …>…</w:del>` region. `<w:del ` (with the trailing space
/// before the `w:id` attribute) never matches `<w:delText>`/`<w:delInstrText>`.
/// The witnesses below produce non-nested deletions, so an open→next-close scan
/// is exact.
fn del_spans(xml: &str) -> Vec<String> {
    let mut spans = Vec::new();
    let mut rest = xml;
    while let Some(open) = rest.find("<w:del ") {
        let after = &rest[open..];
        let Some(close_rel) = after.find("</w:del>") else {
            break;
        };
        let end = close_rel + "</w:del>".len();
        spans.push(after[..end].to_string());
        rest = &after[end..];
    }
    spans
}

// Run-level widget (w:pgNum) emitted BARE at paragraph level. A paragraph
//   carries hidden page-number placeholder runs
//   (`<w:r><w:rPr><w:vanish/><w:sz w:val="24"/></w:rPr><w:pgNum/></w:r>`).
//   `w:pgNum` is EG_RunInnerContent (§17.3.3.22, CT_Empty) and is only legal
//   inside `w:r`; when the serializer dropped the run wrapper it emitted
//   `<w:pgNum/>` as a direct child of `<w:p>` and Word refused the file. The
//   bare-emission path is the whole-document rebuild, forced here by a tracked
//   edit to a DIFFERENT paragraph (the plain unedited roundtrip is verbatim and
//   would not reproduce it).
fn pgnum_widget_whole_doc_rebuild() -> (Vec<u8>, Vec<u8>) {
    let widget = r#"<w:r><w:rPr><w:vanish/><w:sz w:val="24"/></w:rPr><w:pgNum/></w:r>"#;
    let pgnum_para = format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Page </w:t></w:r>{widget}<w:r><w:t xml:space="preserve"> of </w:t></w:r>{widget}</w:p>"#
    );
    let other = |t: &str| format!(r#"<w:p><w:r><w:t xml:space="preserve">{t}</w:t></w:r></w:p>"#);
    let a = pack(&document(&format!("{pgnum_para}{}", other("draft body"))));
    let b = pack(&document(&format!("{pgnum_para}{}", other("final body"))));
    (a, b)
}

#[test]
fn sentinel_pgnum_widget_survives_whole_doc_rebuild() {
    let class = "pgnum-widget-rebuild";
    let (a, b) = pgnum_widget_whole_doc_rebuild();

    let exported = redline_export(class, &a, &b);
    let xml = document_xml(&exported);

    // (a) every <w:pgNum> is inside a <w:r> (rPr immediately precedes it, </w:r>
    //     immediately follows) — none emitted bare at paragraph level.
    let pgnum_total = xml.matches("<w:pgNum").count();
    assert_eq!(
        pgnum_total, 2,
        "[{class}] both page-number widgets must survive the rebuild"
    );
    // (a) every <w:pgNum> is preceded by a </w:rPr> — i.e. it sits inside a
    //     <w:r> after that run's rPr, never bare under <w:p> (a bare widget
    //     would follow </w:r> or <w:p>, not </w:rPr>). Equality with the total
    //     pgNum count proves NONE is emitted bare.
    assert_eq!(
        xml.matches("</w:rPr><w:pgNum").count(),
        pgnum_total,
        "[{class}] (a) every <w:pgNum> must sit inside a <w:r> after its <w:rPr>, not bare under <w:p>"
    );
    // (b) each wrapping run's rPr keeps w:vanish — the widgets stay hidden.
    assert!(
        xml.matches("<w:rPr><w:vanish").count() >= pgnum_total,
        "[{class}] (b) each wrapping run's rPr must preserve w:vanish (hidden)"
    );

    // (c) validator-clean (opens clean in Word — hermetic proxy).
    assert_validator_clean(class, &exported);

    // (d) accept/reject text identity through serialize→reparse.
    assert_reparse_accept_reject(class, &exported, &imported_text(&b), &imported_text(&a));
}

// Tracked DELETE of an inline content control (w:sdt). The SDT is preserved
//   as opaque raw XML; when the deletion wraps it in `w:del`, the SDT's own
//   descendant runs must switch to the deleted-text content model — `w:t` →
//   `w:delText` (§17.4.20, the I-TC-001 rule). Left as `w:t`, Word opens the file
//   only after repair and a programmatic accept-all crashes Word.
fn tracked_delete_of_inline_sdt() -> (Vec<u8>, Vec<u8>) {
    let sdt = r#"<w:sdt><w:sdtPr><w:alias w:val="Clause"/><w:tag w:val="ctl"/><w:id w:val="55"/></w:sdtPr><w:sdtContent><w:r><w:t xml:space="preserve">secret clause</w:t></w:r></w:sdtContent></w:sdt>"#;
    // A has the inline content control; B removes it (a tracked deletion).
    let a = pack(&document(&format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Keep </w:t></w:r>{sdt}<w:r><w:t xml:space="preserve"> tail</w:t></w:r></w:p>"#
    )));
    let b = pack(&document(
        r#"<w:p><w:r><w:t xml:space="preserve">Keep </w:t></w:r><w:r><w:t xml:space="preserve"> tail</w:t></w:r></w:p>"#,
    ));
    (a, b)
}

// Tracked REPLACE of an inline content control's content. Both A and B carry
//   the SAME `w:sdt` (same `w:sdtPr>w:id`), only its inner text differs; the diff
//   sees two unequal opaques and emits the original inside `w:del` and the
//   replacement inside `w:ins`. Cloned from a shared source, both copies would
//   carry one `w:sdtPr>w:id` — but ECMA-376 §17.5.2.18 makes that id the SDT's
//   unique identity, and while both copies are live (before accept/reject) two
//   tags claim it. The inserted copy must be re-id'd; the deleted original keeps
//   the source id (it survives reject and restores A's byte-shape).
fn tracked_replace_of_inline_sdt() -> (Vec<u8>, Vec<u8>) {
    let sdt = |txt: &str| {
        format!(
            r#"<w:sdt><w:sdtPr><w:alias w:val="Clause"/><w:tag w:val="ctl"/><w:id w:val="55"/></w:sdtPr><w:sdtContent><w:r><w:t xml:space="preserve">{txt}</w:t></w:r></w:sdtContent></w:sdt>"#
        )
    };
    let body = |inner: &str| {
        format!(
            r#"<w:p><w:r><w:t xml:space="preserve">Keep </w:t></w:r>{inner}<w:r><w:t xml:space="preserve"> tail</w:t></w:r></w:p>"#
        )
    };
    let a = pack(&document(&body(&sdt("secret clause"))));
    let b = pack(&document(&body(&sdt("public clause"))));
    (a, b)
}

/// Extract each `<w:ins …>…</w:ins>` region (mirror of [`del_spans`]). The
/// witness produces non-nested insertions, so an open→next-close scan is exact.
fn ins_spans(xml: &str) -> Vec<String> {
    let mut spans = Vec::new();
    let mut rest = xml;
    while let Some(open) = rest.find("<w:ins ") {
        let after = &rest[open..];
        let Some(close_rel) = after.find("</w:ins>") else {
            break;
        };
        let end = close_rel + "</w:ins>".len();
        spans.push(after[..end].to_string());
        rest = &after[end..];
    }
    spans
}

/// Every `w:sdtPr>w:id` value in the document, in order. `<w:id w:val="` is the
/// CT_DecimalNumber element form used by the SDT id (§17.5.2.18) — distinct from
/// the `w:id="…"` attribute the tracked-change containers carry, so this never
/// matches a `w:del`/`w:ins` id.
fn sdt_id_vals(xml: &str) -> Vec<String> {
    let needle = "<w:id w:val=\"";
    let mut ids = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find(needle) {
        let after = &rest[i + needle.len()..];
        let end = after.find('"').expect("w:val attribute must close");
        ids.push(after[..end].to_string());
        rest = &after[end..];
    }
    ids
}

#[test]
fn sentinel_tracked_replace_of_inline_sdt_reids_inserted_copy() {
    let class = "tracked-replace-inline-sdt";
    let (a, b) = tracked_replace_of_inline_sdt();

    let exported = redline_export(class, &a, &b);
    let xml = document_xml(&exported);

    // The source SDT id, present on BOTH A and B.
    let source_id = "55";

    // (a)+(b) the del-wrapped and ins-wrapped SDT carry DIFFERENT w:sdtPr>w:id,
    //     and the deleted copy keeps the source id (it IS the original).
    let del = del_spans(&xml);
    let ins = ins_spans(&xml);
    let del_sdt_id = del
        .iter()
        .find_map(|s| sdt_id_vals(s).into_iter().next())
        .unwrap_or_else(|| panic!("[{class}] the deleted content control must carry an sdt id"));
    let ins_sdt_id = ins
        .iter()
        .find_map(|s| sdt_id_vals(s).into_iter().next())
        .unwrap_or_else(|| panic!("[{class}] the inserted content control must carry an sdt id"));
    assert_eq!(
        del_sdt_id, source_id,
        "[{class}] (b) the deleted copy must keep the source sdt id"
    );
    assert_ne!(
        del_sdt_id, ins_sdt_id,
        "[{class}] (a) the deleted and inserted SDT copies must not share one w:sdtPr>w:id \
         (§17.5.2.18 — two live tags cannot claim one identity)"
    );

    // (c) every SDT id in the exported document is unique.
    let all_ids = sdt_id_vals(&xml);
    let unique: std::collections::HashSet<_> = all_ids.iter().collect();
    assert_eq!(
        all_ids.len(),
        unique.len(),
        "[{class}] (c) all sdt ids in the export must be unique, found {all_ids:?}"
    );

    // (d) accept/reject text identity through serialize→reparse. The SDT content
    //     is opaque (not in canonical text), so both sides read the same wrapper
    //     text; the reject side must restore A, the accept side B.
    assert_reparse_accept_reject(class, &exported, &imported_text(&b), &imported_text(&a));

    // (d′) the SURVIVING SDT identity: reject restores the source id (A's copy),
    //      accept keeps the re-id'd replacement — the same value Word would keep,
    //      so canonical and Word accept/reject agree (no id asymmetry).
    let rt = SimpleRuntime::new();
    let canon =
        std::sync::Arc::unwrap_or_clone(rt.import_docx(&exported).expect("re-import").canonical);
    let surviving_sdt_id = |doc: &CanonDoc| -> Vec<String> {
        common::all_paragraphs(doc)
            .iter()
            .flat_map(|p| p.all_inlines_owned())
            .filter_map(|inline| match inline {
                InlineNode::OpaqueInline(o) => o.raw_xml.as_ref().and_then(|raw| {
                    sdt_id_vals(&String::from_utf8_lossy(raw))
                        .into_iter()
                        .next()
                }),
                _ => None,
            })
            .collect()
    };
    let mut accepted = canon.clone();
    accept_all(&mut accepted);
    let mut rejected = canon;
    stemma::reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        surviving_sdt_id(&rejected),
        vec![source_id.to_string()],
        "[{class}] (d′) reject_all must restore the source SDT id"
    );
    assert_eq!(
        surviving_sdt_id(&accepted),
        vec![ins_sdt_id.clone()],
        "[{class}] (d′) accept_all must keep the re-id'd inserted SDT id"
    );

    // (e) validator-clean (#13 hermetic proxy — opens clean in Word).
    assert_validator_clean(class, &exported);
}

#[test]
fn sentinel_tracked_delete_of_inline_sdt_uses_deltext() {
    let class = "tracked-delete-inline-sdt";
    let (a, b) = tracked_delete_of_inline_sdt();

    let exported = redline_export(class, &a, &b);
    let xml = document_xml(&exported);

    // (a) no <w:t>/<w:instrText> anywhere inside any <w:del> (this witness has no
    //     w:txbxContent, so the textbox exemption is not exercised here).
    let dels = del_spans(&xml);
    assert!(
        !dels.is_empty(),
        "[{class}] the removed content control must serialize inside a <w:del>"
    );
    for span in &dels {
        assert!(
            !span.contains("<w:t>") && !span.contains("<w:t ") && !span.contains("<w:instrText"),
            "[{class}] (a) run text inside <w:del> must be <w:delText>/<w:delInstrText>, found plain w:t/w:instrText:\n{span}"
        );
    }
    // The deleted SDT's inner text must have become delText.
    assert!(
        xml.contains("<w:delText"),
        "[{class}] the deleted content-control text must serialize as <w:delText>"
    );

    // (b) validator-clean — after the I-TC-001 sdt-descendant fix this proves the
    //     deleted-text content model over sdtContent hermetically.
    assert_validator_clean(class, &exported);

    // (c)/(d) accept/reject text identity through serialize→reparse.
    assert_reparse_accept_reject(class, &exported, &imported_text(&b), &imported_text(&a));
}

/// Count opaque INLINE nodes across all paragraphs whose kind matches `pred`.
fn count_opaque_inline(doc: &CanonDoc, pred: impl Fn(&stemma::OpaqueKind) -> bool) -> usize {
    common::all_paragraphs(doc)
        .iter()
        .flat_map(|p| p.all_inlines_owned())
        .filter(|inline| match inline {
            InlineNode::OpaqueInline(o) => pred(&o.kind),
            _ => false,
        })
        .count()
}

// ── small accessors for the formatting-tier assertions ─────────────────────

fn first_paragraph(doc: &CanonDoc) -> &stemma::ParagraphNode {
    doc.blocks
        .iter()
        .find_map(|tb| match &tb.block {
            BlockNode::Paragraph(p) => Some(p),
            _ => None,
        })
        .expect("document should have a top-level paragraph")
}

/// True if any text inline in the paragraph carries the Bold mark.
fn paragraph_has_bold(p: &stemma::ParagraphNode) -> bool {
    p.all_inlines_owned().iter().any(|inline| match inline {
        InlineNode::Text(t) => t.marks.contains(&Mark::Bold),
        _ => false,
    })
}

// (h) numbering.xml with a picture bullet (CT_Numbering root sequence).
//     ECMA-376 §17.9 CT_Numbering is an xsd:sequence:
//         numPicBullet*, abstractNum*, num*, numIdMacAtCleanup?
//     The base numbering part carries its numPicBullet FIRST (correct source
//     order). The redline export path (merge_target_numbering) re-parses and
//     re-emits numbering.xml, and must preserve that sequence — if numPicBullet
//     lands after abstractNum/num, Word repairs the file and can drop the
//     picture bullets. A→B edits only the body text of the numbered paragraph,
//     so the export runs the numbering-merge rewrite while every numId still
//     resolves. numId=10 is referenced by the paragraph; the second
//     abstractNum/num pair (id 1 / numId 11) is unreferenced but present so the
//     witness proves *all* abstractNum precede *all* num, both after the single
//     numPicBullet.
fn numbering_with_pic_bullet() -> (Vec<u8>, Vec<u8>) {
    let para = |t: &str| {
        format!(
            r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="10"/></w:numPr></w:pPr><w:r><w:t xml:space="preserve">{t}</w:t></w:r></w:p>"#
        )
    };
    // Source order: numPicBullet, then two abstractNum, then two num.
    let numbering = ExtraPart {
        name: "word/numbering.xml",
        content_type:
            "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml",
        rel: (
            "rIdNum",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering",
            "numbering.xml",
        ),
        xml: r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:numPicBullet w:numPicBulletId="0"><w:pict/></w:numPicBullet><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum><w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="o"/></w:lvl></w:abstractNum><w:num w:numId="10"><w:abstractNumId w:val="0"/></w:num><w:num w:numId="11"><w:abstractNumId w:val="1"/></w:num></w:numbering>"#.to_string(),
    };
    let numbering_b = numbering.clone();
    let a = pack_with_parts(
        &document(&para("Item old")),
        std::slice::from_ref(&numbering),
    );
    let b = pack_with_parts(
        &document(&para("Item new")),
        std::slice::from_ref(&numbering_b),
    );
    (a, b)
}

#[test]
fn sentinel_numbering_pic_bullet_root_order() {
    let class = "numbering-pic-bullet-root-order";
    let (a, b) = numbering_with_pic_bullet();

    // Core relational invariants over the diff/merge path.
    Pair::build(class, &a, &b).assert_core();

    // Drive the redline export — this is the path that re-emits numbering.xml
    // via merge_target_numbering.
    let exported = redline_export(class, &a, &b);

    // (b) #13 hermetic proxy: the emitted package opens clean.
    assert_validator_clean(class, &exported);

    // (a) CT_Numbering root sequence: numPicBullet* precede abstractNum*,
    //     which precede num*. Assert on the actually-emitted part, not the IR.
    let numbering_xml = extract_part(&exported, "word/numbering.xml");
    let pic = numbering_xml
        .find("<w:numPicBullet")
        .expect("emitted numbering.xml must retain the numPicBullet element");
    let first_abstract = numbering_xml
        .find("<w:abstractNum ")
        .expect("emitted numbering.xml must retain abstractNum elements");
    let first_num = numbering_xml
        .find("<w:num ")
        .expect("emitted numbering.xml must retain num elements");
    // All three groups present with the fixture's cardinality (nothing dropped).
    assert_eq!(
        numbering_xml.matches("<w:numPicBullet").count(),
        1,
        "[{class}] numPicBullet must survive the rewrite:\n{numbering_xml}"
    );
    assert_eq!(
        numbering_xml.matches("<w:abstractNum ").count(),
        2,
        "[{class}] both abstractNum must survive the rewrite:\n{numbering_xml}"
    );
    assert_eq!(
        numbering_xml.matches("<w:num ").count(),
        2,
        "[{class}] both num must survive the rewrite:\n{numbering_xml}"
    );
    // Schema order: numPicBullet before every abstractNum and num. Because the
    // groups are contiguous, numPicBullet < first abstractNum < first num proves
    // the full sequence. Pre-fix, numPicBullet sorted last (after num) and this
    // fails.
    assert!(
        pic < first_abstract,
        "[{class}] (a) CT_Numbering sequence violated: numPicBullet must precede abstractNum:\n{numbering_xml}"
    );
    assert!(
        first_abstract < first_num,
        "[{class}] (a) CT_Numbering sequence violated: abstractNum must precede num:\n{numbering_xml}"
    );

    // (c) Numbering survives: the numbered paragraph still resolves its numPr
    //     (numId=10) after export + reparse.
    let rt = SimpleRuntime::new();
    let reimported = std::sync::Arc::unwrap_or_clone(
        rt.import_docx(&exported)
            .unwrap_or_else(|e| panic!("[{class}] reimport exported redline: {e:?}"))
            .canonical,
    );
    let resolves = common::all_paragraphs(&reimported)
        .iter()
        .any(|p| p.numbering.as_ref().is_some_and(|n| n.num_id == 10));
    assert!(
        resolves,
        "[{class}] (c) numbered paragraph must still resolve numPr numId=10 after redline export + reparse"
    );
}

/// Read a single archive part out of an exported DOCX as a UTF-8 string.
fn extract_part(docx_bytes: &[u8], part: &str) -> String {
    use std::io::Read;
    let mut zip =
        zip::ZipArchive::new(std::io::Cursor::new(docx_bytes)).expect("open exported zip");
    let mut file = zip
        .by_name(part)
        .unwrap_or_else(|_| panic!("exported package must contain {part}"));
    let mut s = String::new();
    file.read_to_string(&mut s).expect("read part as UTF-8");
    s
}
