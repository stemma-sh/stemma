//! The block guard hashes the **(visible char, status-class) stream**
//! plus the class-tagged anchor inventory, and carries a scheme version.
//!
//! The reader of the markup sees tracked text *as tracked*, so the guard must
//! answer "has what I was looking at changed?" for tracked MEANING, not just
//! bytes. Pinned here:
//!
//!   - the v1 blindness theorem: a status change over identical bytes (the
//!     exact transition stacking will create) moved NOTHING in the v1
//!     formula but moves the v2 guard;
//!   - resolving a tombstone moves the v2 guard (accepting a deletion removes
//!     (char, d) pairs — and shifts span ordinals — which v1 never saw);
//!   - segmentation-insensitivity is preserved: same text, same classes,
//!     different segment topology → identical guard (adjacent same-class
//!     runs coalesce; attribution is NOT hashed);
//!   - scheme versioning: a legacy v1 guard (bare hex) still validates under
//!     the formula it was minted with — stored transactions keep replaying;
//!   - the v2 guard is what the read view mints and the write path accepts.
//!
//! Daily, corpus-free.

use stemma::api::Document;
use stemma::edit::{
    ContentFragment, EditStep, EditTransaction, MaterializationMode, ParagraphContent,
    ResolvedSpanSelector,
};
use stemma::semantic_hash::{GUARD_SCHEME_V2_PREFIX, block_guard, block_semantic_hash_for_block};
use stemma::{BlockNode, RevisionInfo};

// ─── Fixtures ──────────────────────────────────────────────────────────────

fn make_docx_with_body(body_inner: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
    );
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

fn first_block(doc: &Document) -> BlockNode {
    doc.snapshot().canonical.blocks[0].block.clone()
}

fn revision() -> RevisionInfo {
    RevisionInfo {
        revision_id: 60,
        identity: 0,
        author: Some("guard-test".to_string()),
        date: Some("2026-06-09T00:00:00Z".to_string()),
        apply_op_id: None,
    }
}

// ─── The v1 blindness theorem ────────────────────────────────────────────────

#[test]
fn status_change_over_identical_bytes_moves_v2_but_was_invisible_to_v1() {
    // Same visible byte stream "Hello world." — once all-Normal, once with
    // "world" pending-inserted. An editor who read the first and edits after
    // the document became the second is targeting a DIFFERENT enumeration
    // (one span vs three); the guard must move. v1 hashed text only and was
    // blind to exactly this transition — the one stacking (step 3a) creates.
    let plain = Document::parse(&make_docx_with_body(
        r#"<w:p><w:r><w:t xml:space="preserve">Hello world.</w:t></w:r></w:p>"#,
    ))
    .expect("parse");
    let tracked = Document::parse(&make_docx_with_body(
        r#"<w:p><w:r><w:t xml:space="preserve">Hello </w:t></w:r><w:ins w:id="1" w:author="A" w:date="2026-01-01T00:00:00Z"><w:r><w:t>world</w:t></w:r></w:ins><w:r><w:t>.</w:t></w:r></w:p>"#,
    ))
    .expect("parse");

    let plain_block = first_block(&plain);
    let tracked_block = first_block(&tracked);

    // The load-bearing assertion: v2 sees the status classes. (v1's
    // status-blindness is witnessed cleanly by the tombstone test below —
    // on THIS fixture v1 incidentally differed too, because it hashed one
    // atom per TextNode and the run topologies differ; that accidental
    // sensitivity was never a contract and disappears whenever the topology
    // matches, which is exactly when the blindness bites.)
    assert_ne!(
        block_guard(&plain_block),
        block_guard(&tracked_block),
        "v2 must distinguish tracked meaning over identical bytes"
    );
}

#[test]
fn resolving_a_tombstone_moves_the_v2_guard() {
    // Accepting a deletion removes the (char, d) pairs from the stream — and
    // removes a span from the enumeration, shifting later ordinals. v1
    // skipped Deleted segments entirely, so accept moved nothing.
    let doc = Document::parse(&make_docx_with_body(
        r#"<w:p><w:r><w:t xml:space="preserve">Keep </w:t></w:r><w:del w:id="2" w:author="A" w:date="2026-01-01T00:00:00Z"><w:r><w:delText xml:space="preserve">gone </w:delText></w:r></w:del><w:r><w:t>tail.</w:t></w:r></w:p>"#,
    ))
    .expect("parse");
    let accepted = doc.read_accepted().expect("accept");

    let before = first_block(&doc);
    let after = first_block(&accepted);
    assert_eq!(
        block_semantic_hash_for_block(&before),
        block_semantic_hash_for_block(&after),
        "v1 premise: accepting the deletion moved nothing (the old blind spot)"
    );
    assert_ne!(
        block_guard(&before),
        block_guard(&after),
        "v2: accepting a deletion changes the (char, class) stream and must move the guard"
    );
}

// ─── Segmentation- and attribution-insensitivity preserved ──────────────────

#[test]
fn guard_is_insensitive_to_segment_topology_and_attribution() {
    // One insertion of "ab" vs two adjacent insertions "a" + "b" by two
    // DIFFERENT authors: same (char, class) stream, identical guard. The
    // guard tracks status classes, never segmentation or attribution — that
    // remains the deliberate design (the `expect` predicate covers the
    // ordinal-vs-segmentation hole at resolution time).
    let one = Document::parse(&make_docx_with_body(
        r#"<w:p><w:r><w:t xml:space="preserve">x </w:t></w:r><w:ins w:id="1" w:author="A" w:date="2026-01-01T00:00:00Z"><w:r><w:t>ab</w:t></w:r></w:ins></w:p>"#,
    ))
    .expect("parse");
    let two = Document::parse(&make_docx_with_body(
        r#"<w:p><w:r><w:t xml:space="preserve">x </w:t></w:r><w:ins w:id="1" w:author="A" w:date="2026-01-01T00:00:00Z"><w:r><w:t>a</w:t></w:r></w:ins><w:ins w:id="2" w:author="B" w:date="2026-02-01T00:00:00Z"><w:r><w:t>b</w:t></w:r></w:ins></w:p>"#,
    ))
    .expect("parse");

    assert_eq!(
        block_guard(&first_block(&one)),
        block_guard(&first_block(&two)),
        "same (char, class) stream must hash identically regardless of segment topology or authors"
    );
}

// ─── Scheme versioning ───────────────────────────────────────────────────────

#[test]
fn the_read_view_mints_v2_guards_and_the_write_path_accepts_them() {
    let doc = Document::parse(&make_docx_with_body(
        r#"<w:p><w:r><w:t xml:space="preserve">The term is thirty days.</w:t></w:r></w:p>"#,
    ))
    .expect("parse");
    let view = doc.read();
    assert!(
        view.blocks[0].guard.starts_with(GUARD_SCHEME_V2_PREFIX),
        "minted guards carry the scheme version: {}",
        view.blocks[0].guard
    );

    // And the whole mint→edit loop works on the v2 scheme (span op, which
    // REQUIRES the guard).
    doc.apply(&EditTransaction {
        steps: vec![EditStep::ReplaceSpanText {
            block_id: view.blocks[0].id.clone(),
            guard: view.blocks[0].guard.clone(),
            expect: None,
            span: ResolvedSpanSelector::Handle("s_0".to_string()),
            content: ParagraphContent {
                fragments: vec![ContentFragment::Text("The term is sixty days.".to_string())],
            },
            rationale: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: revision(),
    })
    .expect("a v2-minted guard validates");
}

#[test]
fn a_legacy_v1_guard_still_validates_under_its_own_scheme() {
    // A stored transaction minted before the reform carries a bare-hex v1
    // guard. It must validate under the v1 formula it was minted with —
    // replay keeps working; the v1 blind spots apply only to such callers.
    let doc = Document::parse(&make_docx_with_body(
        r#"<w:p><w:r><w:t xml:space="preserve">The term is thirty days.</w:t></w:r></w:p>"#,
    ))
    .expect("parse");
    let view = doc.read();
    let legacy_guard = block_semantic_hash_for_block(&first_block(&doc));
    assert!(
        !legacy_guard.starts_with(GUARD_SCHEME_V2_PREFIX),
        "fixture sanity: v1 guards are bare hex"
    );

    doc.apply(&EditTransaction {
        steps: vec![EditStep::ReplaceSpanText {
            block_id: view.blocks[0].id.clone(),
            guard: legacy_guard,
            expect: None,
            span: ResolvedSpanSelector::Handle("s_0".to_string()),
            content: ParagraphContent {
                fragments: vec![ContentFragment::Text("The term is sixty days.".to_string())],
            },
            rationale: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: revision(),
    })
    .expect("a legacy v1 guard validates under the v1 formula");
}

#[test]
fn a_stale_v2_guard_fails_with_a_v2_actual() {
    // Mismatch errors compare like with like: a stale v2 guard reports the
    // block's CURRENT v2 guard, not a v1 hash.
    let doc = Document::parse(&make_docx_with_body(
        r#"<w:p><w:r><w:t xml:space="preserve">Alpha bravo.</w:t></w:r></w:p>"#,
    ))
    .expect("parse");
    let view = doc.read();

    let err = doc
        .apply(&EditTransaction {
            steps: vec![EditStep::ReplaceSpanText {
                block_id: view.blocks[0].id.clone(),
                guard: format!("{GUARD_SCHEME_V2_PREFIX}{}", "0".repeat(64)),
                expect: None,
                span: ResolvedSpanSelector::Handle("s_0".to_string()),
                content: ParagraphContent {
                    fragments: vec![ContentFragment::Text("Alpha charlie.".to_string())],
                },
                rationale: None,
            }],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: revision(),
        })
        .err()
        .expect("a stale v2 guard must refuse");
    assert_eq!(err.code, stemma::ErrorCode::StaleEdit, "{err:?}");
}

// ─── Breadth verbs dispatch on the guard scheme too ─────────────────────────
//
// The v2 migration updated the four core `check_block_guard` sites but missed
// the breadth-verb family (formatting / numbering / bookmarks / notes /
// comments / styles / table ops, landed before v2 existed), which compared the
// view's v2 guard against the raw v1 formula — a false StaleEdit on every
// guarded breadth-verb call from a well-behaved read→write client. Pinned on
// two representative verbs; the fix is the shared `check_block_guard` dispatch,
// so one verb per validation shape suffices.

const PARA_AND_TABLE: &str = r#"<w:p><w:r><w:t>Service levels apply.</w:t></w:r></w:p><w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="3000"/><w:gridCol w:w="3000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="3000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>Metric</w:t></w:r></w:p></w:tc><w:tc><w:tcPr><w:tcW w:w="3000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>99.5%</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;

fn apply_one_step(doc: &Document, step: EditStep) -> Result<Document, stemma::RuntimeError> {
    doc.apply(&EditTransaction {
        steps: vec![step],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: revision(),
    })
}

/// (id, v2 view guard) of the unique block whose text contains `needle`.
fn block_with(doc: &Document, needle: &str) -> (stemma::NodeId, String) {
    let view = doc.read();
    let b = view
        .blocks
        .iter()
        .find(|b| b.text.contains(needle))
        .unwrap_or_else(|| panic!("no block contains {needle:?}"));
    assert!(
        b.guard.starts_with(GUARD_SCHEME_V2_PREFIX),
        "the read view mints v2 guards; got {}",
        b.guard
    );
    (b.id.clone(), b.guard.clone())
}

fn table_cell_step(block_id: stemma::NodeId, guard: Option<String>) -> EditStep {
    EditStep::TableStructureOp {
        block_id,
        semantic_hash: guard,
        op: stemma::edit::TableOp::SetCellText {
            row_index: 0,
            col_index: 1,
            text: "99.9%".to_string(),
        },
        rationale: None,
    }
}

fn para_format_step(block_id: stemma::NodeId, guard: Option<String>) -> EditStep {
    EditStep::SetParagraphFormatting {
        block_id,
        semantic_hash: guard,
        patch: stemma::edit::ParagraphFormattingPatch {
            align: Some(stemma::Alignment::Center),
            ..Default::default()
        },
        rationale: None,
    }
}

#[test]
fn breadth_verbs_accept_the_v2_view_guard() {
    // read → pass the guard back → apply: the fresh-snapshot loop must never
    // refuse. Before the check_block_guard dispatch this was a false StaleEdit
    // on both verbs.
    let doc = Document::parse(&make_docx_with_body(PARA_AND_TABLE)).expect("parse");

    let (table_id, table_guard) = block_with(&doc, "Metric");
    apply_one_step(&doc, table_cell_step(table_id, Some(table_guard)))
        .expect("a fresh v2 view guard must be accepted by table_op");

    let (para_id, para_guard) = block_with(&doc, "Service levels");
    apply_one_step(&doc, para_format_step(para_id, Some(para_guard)))
        .expect("a fresh v2 view guard must be accepted by set_para_format");
}

#[test]
fn breadth_verbs_accept_a_legacy_v1_guard_under_the_v1_formula() {
    // Stored v1 guards (bare hex) keep working on the breadth verbs too.
    let doc = Document::parse(&make_docx_with_body(PARA_AND_TABLE)).expect("parse");
    let (table_id, _) = block_with(&doc, "Metric");

    let v1 = doc
        .snapshot()
        .canonical
        .blocks
        .iter()
        .find_map(|tb| match &tb.block {
            BlockNode::Table(t) if t.id == table_id => {
                Some(block_semantic_hash_for_block(&tb.block))
            }
            _ => None,
        })
        .expect("table node");
    assert!(
        !v1.starts_with(GUARD_SCHEME_V2_PREFIX),
        "v1 guards are bare hex"
    );

    apply_one_step(&doc, table_cell_step(table_id, Some(v1)))
        .expect("a legacy v1 guard must validate under the v1 formula");
}

#[test]
fn breadth_verbs_refuse_a_wrong_guard_under_either_scheme() {
    let doc = Document::parse(&make_docx_with_body(PARA_AND_TABLE)).expect("parse");
    let (table_id, table_guard) = block_with(&doc, "Metric");

    // A v2 guard from a DIFFERENT block: scheme right, content stale.
    let (_, other_guard) = block_with(&doc, "Service levels");
    assert_ne!(table_guard, other_guard);
    let err = apply_one_step(&doc, table_cell_step(table_id.clone(), Some(other_guard)))
        .err()
        .expect("a stale v2 guard must refuse");
    assert_eq!(err.code, stemma::ErrorCode::StaleEdit, "{err:?}");

    // Garbage that matches neither scheme refuses too (never silently passes).
    let err = apply_one_step(
        &doc,
        table_cell_step(table_id, Some("deadbeef".to_string())),
    )
    .err()
    .expect("a wrong v1-shaped guard must refuse");
    assert_eq!(err.code, stemma::ErrorCode::StaleEdit, "{err:?}");
}
