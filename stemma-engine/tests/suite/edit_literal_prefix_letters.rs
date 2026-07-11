//! Literal-prefix LETTER renumbering on insert, driven END-TO-END from real
//! DOCX bytes.
//!
//! When a new paragraph is inserted after a `(b)`-style literal-prefix list item,
//! the engine reassigns its placeholder label to the next letter in the same
//! decorated format. Arabic renumbering is pinned in
//! `edit_basic.rs::insert_literal_prefix_gets_anchor_plus_one_arabic`; this file
//! mirrors that for the LETTER path, but constructs the list as real DOCX run
//! text (the importer's `strip_literal_prefix` derives the `literal_prefix` from
//! the leading run text) rather than hand-built IR.
//!
//! DOMAIN RULE: the inserted item's label is `anchor_letter + 1`, rendered with
//! the SAME prefix/suffix decoration as the anchor (`(b)` -> `(c)`). When the
//! resulting number is out of single-letter range (after `(z)` there is no
//! `(aa)`), the engine must fail loudly with `UnsupportedParagraphRole` naming
//! the supported formats — never silently emit a malformed label.
//!
//! No hand-built IR. Corpus-free, daily tier.

use stemma::api::Document;
use stemma::domain::{BlockNode, CanonDoc, NodeId, RevisionInfo};
use stemma::edit::{
    BlockSpec, EditError, EditStep, EditTransaction, InsertPosition, MaterializationMode,
    ParagraphBlockSpec, apply_transaction, parse_paragraph_markup,
};
use stemma::vocabulary;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

// ─── Synthetic-docx helper ───────────────────────────────────────────────────

fn make_docx(body_inner: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}" xmlns:r="{R_NS}"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
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

fn parse(body: &str) -> CanonDoc {
    (*Document::parse(&make_docx(body))
        .expect("parse")
        .snapshot()
        .canonical)
        .clone()
}

fn test_revision() -> RevisionInfo {
    RevisionInfo {
        revision_id: 100,
        author: Some("Test Author".to_string()),
        date: Some("2026-06-07T00:00:00Z".to_string()),
        apply_op_id: None,
    }
}

/// The literal-prefix of the i-th block (None if not a literal-prefix paragraph).
fn prefix_at(canon: &CanonDoc, idx: usize) -> Option<String> {
    match &canon.blocks[idx].block {
        BlockNode::Paragraph(p) => p.literal_prefix.clone(),
        _ => None,
    }
}

fn block_id_at(canon: &CanonDoc, idx: usize) -> NodeId {
    match &canon.blocks[idx].block {
        BlockNode::Paragraph(p) => p.id.clone(),
        BlockNode::Table(t) => t.id.clone(),
        BlockNode::OpaqueBlock(o) => o.id.clone(),
    }
}

/// Resolve the literal-prefix role id the imported list paragraphs cluster into
/// (so the inserted paragraph inherits the anchor's prefix as a placeholder).
fn literal_prefix_role(canon: &CanonDoc) -> String {
    let vocab = vocabulary::extract_vocabulary(canon);
    vocab
        .paragraph_roles
        .iter()
        .find(|r| r.numbering_source == Some(vocabulary::NumberingSource::LiteralPrefix))
        .expect("the imported list must expose a literal-prefix role")
        .id
        .clone()
}

/// Insert one paragraph after `anchor` using the document's literal-prefix role.
fn insert_after(canon: &CanonDoc, anchor: NodeId) -> Result<CanonDoc, EditError> {
    let role = literal_prefix_role(canon);
    let tx = EditTransaction {
        steps: vec![EditStep::InsertParagraphs {
            anchor_block_id: anchor,
            position: InsertPosition::After,
            rationale: None,
            blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                role: Some(role),
                content: parse_paragraph_markup("Inserted item body").unwrap(),
                restart_numbering: false,
                list: None,
            })],
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };
    apply_transaction(canon, &tx).map(|(d, _)| d)
}

// ─── Single-letter increment, decoration preserved ───────────────────────────

#[test]
fn insert_after_b_letter_item_gets_c_same_decoration() {
    // "(a) First..., (b) Second..."; insert AFTER "(b)". The new item must be
    // "(c)" (anchor letter b=2, +1 -> c), same "(...)" decoration. This is the
    // next increment beyond edit_basic's after-"(a)" case.
    let body = concat!(
        r#"<w:p><w:r><w:t xml:space="preserve">(a) First item body</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t xml:space="preserve">(b) Second item body</w:t></w:r></w:p>"#,
    );
    let canon = parse(body);
    assert_eq!(
        prefix_at(&canon, 0).as_deref(),
        Some("(a)"),
        "fixture import"
    );
    assert_eq!(
        prefix_at(&canon, 1).as_deref(),
        Some("(b)"),
        "fixture import"
    );
    let anchor = block_id_at(&canon, 1);

    let result = insert_after(&canon, anchor).expect("insert after (b)");
    // p0, p1, then the inserted paragraph at index 2.
    assert_eq!(
        prefix_at(&result, 2).as_deref(),
        Some("(c)"),
        "inserting after (b) must yield (c), same parenthesis decoration"
    );
    // Existing labels untouched (downstream renumbering is the caller's job).
    assert_eq!(prefix_at(&result, 0).as_deref(), Some("(a)"));
    assert_eq!(prefix_at(&result, 1).as_deref(), Some("(b)"));
}

#[test]
fn insert_after_uppercase_letter_item_increments_in_kind() {
    // "A.<tab>...", "B.<tab>..."; inserting after "B." yields "C." — the
    // LabelKind (upper letter) and the "." suffix are preserved.
    let body = concat!(
        r#"<w:p><w:r><w:t xml:space="preserve">A.&#9;First item body</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t xml:space="preserve">B.&#9;Second item body</w:t></w:r></w:p>"#,
    );
    let canon = parse(body);
    assert_eq!(
        prefix_at(&canon, 0).as_deref(),
        Some("A."),
        "fixture import"
    );
    assert_eq!(
        prefix_at(&canon, 1).as_deref(),
        Some("B."),
        "fixture import"
    );
    let anchor = block_id_at(&canon, 1);

    let result = insert_after(&canon, anchor).expect("insert after B.");
    assert_eq!(
        prefix_at(&result, 2).as_deref(),
        Some("C."),
        "inserting after B. must yield C. (uppercase kind + '.' suffix preserved)"
    );
}

// ─── Out-of-range: after (z) there is no single-letter successor ─────────────

#[test]
fn insert_after_z_letter_item_fails_out_of_range() {
    // The anchor "(z)" is letter 26; the successor would be number 27, which has
    // no single-letter rendering. The engine must fail loudly with
    // UnsupportedParagraphRole — never emit "(aa)" or a malformed label.
    let body =
        r#"<w:p><w:r><w:t xml:space="preserve">(z) Final single letter item</w:t></w:r></w:p>"#;
    let canon = parse(body);
    assert_eq!(
        prefix_at(&canon, 0).as_deref(),
        Some("(z)"),
        "fixture import"
    );
    let anchor = block_id_at(&canon, 0);

    let err = insert_after(&canon, anchor).expect_err("inserting after (z) must fail");
    assert!(
        matches!(err, EditError::UnsupportedParagraphRole { .. }),
        "expected UnsupportedParagraphRole for out-of-range letter, got: {err:?}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("out of range") || msg.contains("single-letter"),
        "error must explain the single-letter range limit: {msg}"
    );
}
