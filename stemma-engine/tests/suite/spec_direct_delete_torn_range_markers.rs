//! Direct-mode physical-removal edits must not orphan a range-marker half.
//!
//! A bookmark / comment / permission range is a `start`/`end` pair joined by a
//! part-local id (ECMA-376 §17.13.6). A `MaterializationMode::Direct` edit that
//! PHYSICALLY removes content — a table row delete, a block-range delete — can
//! remove the content holding one half of a pair while the other half lives in
//! surviving content, tearing the pair. A lone `bookmarkStart`/`bookmarkEnd` is
//! schema-invalid and the post-serialization pairing guard
//! (`enforce_story_bookmark_integrity`) refuses the whole document.
//!
//! The engine's rule (matching Word, and the accept/reject resolution path via
//! `tracked_model::collapse_resolution_torn_range_markers`): collapse the torn
//! range to a POINT at the surviving half — re-insert the removed half adjacent
//! to the survivor so the marker survives as an empty range, never an orphan.
//!
//! Both tests synthesize their DOCX in memory (corpus-free, daily tier).

use std::io::Read;

use stemma::ExportOptions;
use stemma::api::Document;
use stemma::domain::{BlockNode, NodeId, RevisionInfo};
use stemma::edit::{EditStep, EditTransaction, MaterializationMode, TableOp};
use zip::ZipArchive;

/// Pack a `word/document.xml` body into a minimal, valid DOCX.
fn pack(body_inner_xml: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner_xml}<w:sectPr/></w:body></w:document>"#
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

fn direct_txn(steps: Vec<EditStep>) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: MaterializationMode::Direct,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("Direct".to_string()),
            date: Some("2026-07-09T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

fn document_xml_of(docx: &[u8]) -> String {
    let mut zip = ZipArchive::new(std::io::Cursor::new(docx)).expect("open DOCX zip");
    let mut file = zip
        .by_name("word/document.xml")
        .expect("word/document.xml present");
    let mut out = String::new();
    file.read_to_string(&mut out).expect("read document.xml");
    out
}

/// The id of the first body block whose node is a table.
fn first_table_id(doc: &Document) -> NodeId {
    doc.snapshot()
        .canonical
        .blocks
        .iter()
        .find_map(|tb| match &tb.block {
            BlockNode::Table(t) => Some(t.id.clone()),
            _ => None,
        })
        .expect("document has a table block")
}

/// `(bookmarkStart count, bookmarkEnd count)` in the serialized document XML.
fn bookmark_marker_counts(xml: &str) -> (usize, usize) {
    (
        xml.matches("<w:bookmarkStart ").count(),
        xml.matches("<w:bookmarkEnd ").count(),
    )
}

/// (i) A bookmark opens inside table row 0 and closes in a body paragraph AFTER
/// the table. A Direct `DeleteRow(0)` physically removes the row holding the
/// `bookmarkStart`, leaving the `bookmarkEnd` behind. The edit must serialize
/// cleanly with a BALANCED bookmark marker set — never a lone end.
#[test]
fn direct_delete_row_collapses_bookmark_torn_across_row_boundary() {
    let body = r#"<w:tbl>
        <w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr>
        <w:tr>
            <w:tc><w:p>
                <w:bookmarkStart w:id="0" w:name="span"/>
                <w:r><w:t>row zero cell</w:t></w:r>
            </w:p></w:tc>
        </w:tr>
        <w:tr>
            <w:tc><w:p><w:r><w:t>row one cell</w:t></w:r></w:p></w:tc>
        </w:tr>
    </w:tbl>
    <w:p>
        <w:r><w:t>body paragraph</w:t></w:r>
        <w:bookmarkEnd w:id="0"/>
    </w:p>"#;

    let base = Document::parse(&pack(body)).expect("parse");
    // Precondition: the input is a WHOLE pair (one start, one end).
    assert_eq!(
        bookmark_marker_counts(&document_xml_of(&pack(body))),
        (1, 1),
        "fixture starts with a balanced bookmark pair"
    );

    let table = first_table_id(&base);
    let edited = base
        .apply(&direct_txn(vec![EditStep::TableStructureOp {
            block_id: table,
            semantic_hash: None,
            op: TableOp::DeleteRow { row_index: 0 },
            rationale: None,
        }]))
        .expect("Direct DeleteRow applies");

    // Without the fix this serialize FAILS: the pairing guard refuses the lone
    // bookmarkEnd ("serialization introduced unpaired bookmarks").
    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize must succeed: the torn pair is collapsed, not orphaned");

    let (starts, ends) = bookmark_marker_counts(&document_xml_of(&bytes));
    assert_eq!(
        starts, ends,
        "bookmarkStart/bookmarkEnd must be balanced (collapsed pair or none), got {starts} start(s) / {ends} end(s)"
    );
    // The surviving end's partner was re-materialized (collapse to a point), so
    // the pair is preserved rather than silently dropped.
    assert_eq!(
        (starts, ends),
        (1, 1),
        "the range collapses to a point: both halves survive adjacent"
    );
}

/// (ii) A bookmark opens in one body paragraph and closes in the NEXT. A Direct
/// `DeleteBlockRange` over only the first paragraph physically removes the
/// `bookmarkStart`, leaving the `bookmarkEnd`. Same rule: collapse to a point,
/// serialize clean, balanced markers.
#[test]
fn direct_delete_block_range_collapses_bookmark_with_half_in_deleted_paragraph() {
    let body = r#"<w:p>
        <w:bookmarkStart w:id="0" w:name="span"/>
        <w:r><w:t>first paragraph</w:t></w:r>
    </w:p>
    <w:p>
        <w:r><w:t>second paragraph</w:t></w:r>
        <w:bookmarkEnd w:id="0"/>
    </w:p>"#;

    let base = Document::parse(&pack(body)).expect("parse");
    assert_eq!(
        bookmark_marker_counts(&document_xml_of(&pack(body))),
        (1, 1),
        "fixture starts with a balanced bookmark pair"
    );

    let first_para = base.read().blocks[0].id.clone();
    let edited = base
        .apply(&direct_txn(vec![EditStep::DeleteBlockRange {
            from_block_id: first_para.clone(),
            to_block_id: first_para,
            rationale: None,
            expect: "first paragraph".to_string(),
            semantic_hash: None,
        }]))
        .expect("Direct DeleteBlockRange applies");

    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize must succeed: the torn pair is collapsed, not orphaned");

    let (starts, ends) = bookmark_marker_counts(&document_xml_of(&bytes));
    assert_eq!(
        starts, ends,
        "bookmarkStart/bookmarkEnd must be balanced, got {starts} start(s) / {ends} end(s)"
    );
    assert_eq!(
        (starts, ends),
        (1, 1),
        "the range collapses to a point: both halves survive adjacent"
    );
}
