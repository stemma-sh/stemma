//! Accepting a tracked MOVE of a bookmarked range drops the bookmark — and
//! that is WORD'S behavior, not a stemma defect.
//!
//! THE CORRECTION (confirmed against real Word). An earlier review read
//! "accept-all drops the moved bookmark, orphaning its REF"
//! as a stemma corruption bug and proposed a serializer fix (carry the
//! bookmark into the `w:moveTo` copy — "emit both"). Real Word REFUTED that:
//!
//!   - Word's OWN native tracked move (authored by Word COM: bookmark a
//!     paragraph, turn track-changes on, cut+paste it) accepted by Word →
//!     the bookmark is GONE, the REF orphaned ("Error! Reference source not
//!     found"). Word does not duplicate the bookmark into the moveTo and does
//!     not re-anchor it on accept.
//!   - A doc with the bookmark carried into BOTH move halves (same name in
//!     moveFrom and moveTo) → Word /accept STILL drops it. "Emit both" would
//!     have passed every internal test and corrupted nothing it could see,
//!     while Word still orphaned the REF.
//!
//! So there is no serializer fix that makes Word preserve a moved bookmark
//! through accept — Word itself doesn't. The accept-fidelity contract is
//! therefore that OUR accept MATCHES Word: the moved bookmark drops. (Reject
//! restores the source, which keeps it — the regression floor.)
//!
//! User-facing consequence (real, but inherent to Word, not a stemma bug):
//! tracked-moving a REF-target heading breaks the cross-reference once
//! accepted. The mitigation lives at the agent/product layer — re-anchor the
//! bookmark at the destination (the `insert_bookmark` verb) if the REF must
//! survive — NOT in the serializer. SKILL guidance updated accordingly.
//!
//! Daily, corpus-free.

use stemma::api::Document;
use stemma::edit::{EditStep, EditTransaction, InsertPosition, MaterializationMode};
use stemma::{NodeId, RevisionInfo};

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

/// Three paragraphs; the FIRST carries a bookmark "BM" around its run.
const BODY: &str = concat!(
    r#"<w:p><w:bookmarkStart w:id="1" w:name="BM"/><w:r><w:t>Definitions section.</w:t></w:r><w:bookmarkEnd w:id="1"/></w:p>"#,
    r#"<w:p><w:r><w:t>Intro paragraph.</w:t></w:r></w:p>"#,
    r#"<w:p><w:r><w:t>Scope paragraph.</w:t></w:r></w:p>"#,
);

fn bookmark_names(docx: &[u8]) -> Vec<String> {
    let xml = String::from_utf8(
        stemma::docx::DocxArchive::read(docx)
            .expect("zip")
            .get("word/document.xml")
            .expect("document.xml")
            .to_vec(),
    )
    .expect("utf8");
    let mut out = Vec::new();
    for chunk in xml.split("<w:bookmarkStart").skip(1) {
        if let Some(i) = chunk.find("w:name=\"") {
            let rest = &chunk[i + 8..];
            if let Some(end) = rest.find('"') {
                out.push(rest[..end].to_string());
            }
        }
    }
    out.sort();
    out
}

fn serialize(doc: &Document) -> Vec<u8> {
    doc.serialize(&stemma::ExportOptions {
        mode: stemma::ExportMode::Redline,
        validator_level: stemma::ValidatorLevel::Blocking,
        validator: None,
    })
    .expect("serialize")
}

/// Move the bookmarked first paragraph to after the third, tracked.
fn move_first_after_third() -> Document {
    let doc = Document::parse(&make_docx_with_body(BODY)).expect("parse");
    let ids: Vec<NodeId> = doc.read().blocks.iter().map(|b| b.id.clone()).collect();
    doc.apply(&EditTransaction {
        steps: vec![EditStep::MoveBlockRange {
            from_block_id: ids[0].clone(),
            to_block_id: ids[0].clone(),
            dest_anchor_id: ids[2].clone(),
            dest_position: InsertPosition::After,
            rationale: None,
            expect: None,
            semantic_hash: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 50,
            author: Some("mover".to_string()),
            date: Some("2026-06-13T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    })
    .expect("tracked move applies")
}

/// The loss happens at consumption (resolution of the serialized artifact), so
/// the repro round-trips as a host does: serialize the move, re-import, resolve,
/// serialize again — the bytes the user keeps.
fn resolved_bookmark_names(moved: &Document, accept: bool) -> Vec<String> {
    let moved_bytes = serialize(moved);
    let reparsed = Document::parse(&moved_bytes).expect("re-parse the move");
    let resolved = if accept {
        reparsed.read_accepted().expect("accept-all")
    } else {
        reparsed.read_rejected().expect("reject-all")
    };
    bookmark_names(&serialize(&resolved))
}

#[test]
fn rejecting_a_move_keeps_the_bookmark() {
    // The regression floor: reject-all restores the source, which keeps its
    // bookmark. This must never break.
    let moved = move_first_after_third();
    assert!(
        resolved_bookmark_names(&moved, /*accept=*/ false).contains(&"BM".to_string()),
        "reject-all must keep the bookmark on the restored source paragraph"
    );
}

#[test]
fn accepting_a_move_drops_the_bookmark_matching_word() {
    // Accept-all drops the moved bookmark — the Word-faithful outcome (Word's
    // own native move drops it identically; verified against real Word). Our
    // accept MATCHES Word here; that is the accept-fidelity contract. A test
    // asserting the bookmark SURVIVES would encode a non-Word expectation.
    let moved = move_first_after_third();
    assert!(
        !resolved_bookmark_names(&moved, /*accept=*/ true).contains(&"BM".to_string()),
        "accept-all drops the moved bookmark, matching Word; preserving it here \
         would diverge from what real Word consumes"
    );
}
