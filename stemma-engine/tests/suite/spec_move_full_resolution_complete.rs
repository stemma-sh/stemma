//! Full resolution is move-complete (ECMA-376 §17.13.5.21–.28).
//!
//! Domain rule: accept-all and reject-all resolve EVERY pending tracked
//! change, a move included — a move is one atomic revision whose accept keeps
//! the destination content and whose reject restores the source, and either
//! way ALL of its markup (`w:moveFrom`/`w:moveTo` containers and the
//! `w:move*Range*` delimiters) leaves the document. The serialized output of
//! a full resolution therefore re-imports with ZERO pending revisions.
//!
//! Regression: the model projection settled the move CONTENT by segment
//! status but left the zero-width move markup (`DecorationType::MoveRange`)
//! in place, so a document whose move rode along with other revisions came
//! out of `--accept-all` still carrying a pending half-move that no further
//! resolution could clear.

use std::collections::HashSet;
use std::io::Write;

use stemma::api::Document;
use stemma::{ExportOptions, Resolution, ResolveSelectionAction, RevisionKind};

fn make_docx_with_body(body: &str) -> Vec<u8> {
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let document_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;

    let mut bytes = Vec::new();
    {
        use zip::write::FileOptions;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
        let options: FileOptions = FileOptions::default();
        zip.start_file("[Content_Types].xml", options).unwrap();
        zip.write_all(content_types.as_bytes()).unwrap();
        zip.start_file("_rels/.rels", options).unwrap();
        zip.write_all(root_rels.as_bytes()).unwrap();
        zip.start_file("word/_rels/document.xml.rels", options)
            .unwrap();
        zip.write_all(document_rels.as_bytes()).unwrap();
        zip.start_file("word/document.xml", options).unwrap();
        zip.write_all(document.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    bytes
}

const DATE: &str = r#"w:date="2026-07-01T10:00:00Z""#;

/// An inline (run-level) move riding alongside an insertion, a deletion, and
/// a run-formatting change — the mixed-document shape a real multi-party
/// redline carries.
fn mixed_doc_with_inline_move() -> Vec<u8> {
    let body = format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Payment terms depend on </w:t></w:r><w:moveFromRangeStart w:id="101" w:name="move1" w:author="M. Chen" {DATE}/><w:moveFrom w:id="102" w:author="M. Chen" {DATE}><w:r><w:t>the effective date</w:t></w:r></w:moveFrom><w:moveFromRangeEnd w:id="101"/><w:r><w:t xml:space="preserve"> in all cases.</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">Delivery is governed by </w:t></w:r><w:moveToRangeStart w:id="103" w:name="move1" w:author="M. Chen" {DATE}/><w:moveTo w:id="104" w:author="M. Chen" {DATE}><w:r><w:t>the effective date</w:t></w:r></w:moveTo><w:moveToRangeEnd w:id="103"/><w:r><w:t xml:space="preserve"> henceforth.</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">Notices go </w:t></w:r><w:ins w:id="110" w:author="L. Marsh" {DATE}><w:r><w:t xml:space="preserve">by registered mail </w:t></w:r></w:ins><w:r><w:t>to the addresses below.</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">The warranty period is </w:t></w:r><w:del w:id="111" w:author="O. Counsel" {DATE}><w:r><w:delText xml:space="preserve">twenty-four </w:delText></w:r></w:del><w:r><w:t>months.</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">Compliance is </w:t></w:r><w:r><w:rPr><w:b/><w:rPrChange w:id="112" w:author="F. Reyes" {DATE}><w:rPr/></w:rPrChange></w:rPr><w:t>mandatory</w:t></w:r><w:r><w:t xml:space="preserve"> for both parties.</w:t></w:r></w:p>"#
    );
    make_docx_with_body(&body)
}

fn pending_count(bytes: &[u8]) -> usize {
    let doc = Document::parse(bytes).expect("re-parse resolved output");
    stemma::enumerate_revisions(&doc.snapshot().canonical).len()
}

fn document_xml(bytes: &[u8]) -> String {
    use std::io::Read;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("zip");
    let mut xml = String::new();
    archive
        .by_name("word/document.xml")
        .expect("document.xml")
        .read_to_string(&mut xml)
        .expect("utf8");
    xml
}

fn assert_no_move_markup(xml: &str, context: &str) {
    for marker in [
        "<w:moveFrom",
        "<w:moveTo",
        "moveFromRangeStart",
        "moveFromRangeEnd",
        "moveToRangeStart",
        "moveToRangeEnd",
    ] {
        assert!(
            !xml.contains(marker),
            "§17.13.5.21–.28: a full resolution settles a move as a unit — {marker} must not \
             survive {context}. XML: {xml}"
        );
    }
}

#[test]
fn accept_all_resolves_inline_move_completely_in_mixed_document() {
    let bytes = mixed_doc_with_inline_move();
    let doc = Document::parse(&bytes).expect("parse");
    assert_eq!(
        stemma::enumerate_revisions(&doc.snapshot().canonical).len(),
        4,
        "fixture census: one move + insert + delete + format_run"
    );

    let accepted = doc.project(Resolution::AcceptAll).expect("accept-all");
    let out = accepted
        .serialize(&ExportOptions::default())
        .expect("serialize");

    let text = accepted.to_text();
    assert!(
        text.contains("Payment terms depend on  in all cases.")
            || text.contains("Payment terms depend on in all cases."),
        "accept removes the moved text from the source. text: {text}"
    );
    assert!(
        text.contains("Delivery is governed by the effective date henceforth."),
        "accept keeps the moved text at the destination. text: {text}"
    );

    assert_no_move_markup(&document_xml(&out), "accept-all");
    assert_eq!(
        pending_count(&out),
        0,
        "accept-all output must re-import with zero pending revisions"
    );
}

#[test]
fn reject_all_resolves_inline_move_completely_in_mixed_document() {
    let bytes = mixed_doc_with_inline_move();
    let doc = Document::parse(&bytes).expect("parse");

    let rejected = doc.project(Resolution::RejectAll).expect("reject-all");
    let out = rejected
        .serialize(&ExportOptions::default())
        .expect("serialize");

    let text = rejected.to_text();
    assert!(
        text.contains("Payment terms depend on the effective date in all cases."),
        "reject restores the moved text at the source. text: {text}"
    );
    assert!(
        text.contains("Delivery is governed by  henceforth.")
            || text.contains("Delivery is governed by henceforth."),
        "reject removes the moved text from the destination. text: {text}"
    );

    assert_no_move_markup(&document_xml(&out), "reject-all");
    assert_eq!(
        pending_count(&out),
        0,
        "reject-all output must re-import with zero pending revisions"
    );
}

/// Selectively resolving an UNRELATED revision must carry the still-pending
/// move through unchanged: same one-atomic-move census on re-import, and
/// never a same-polarity double-wrap (`w:del` nested in `w:moveFrom`,
/// `w:ins` in `w:moveTo`) — that nesting is unrepresentable in the model
/// (import quarantines it as nested tracking), so serializing it turns a
/// clean pending move into an unresolvable quarantined block.
#[test]
fn selective_resolution_of_unrelated_revision_preserves_pending_move() {
    let bytes = mixed_doc_with_inline_move();
    let doc = Document::parse(&bytes).expect("parse");
    let insert_id = stemma::enumerate_revisions(&doc.snapshot().canonical)
        .into_iter()
        .find(|revision| revision.author.as_deref() == Some("L. Marsh"))
        .expect("the unrelated insert is enumerable")
        .revision_id;

    let resolved = doc
        .project(Resolution::Selective {
            ids: HashSet::from([insert_id]),
            action: ResolveSelectionAction::Accept,
        })
        .expect("selective accept of the unrelated insert");
    let out = resolved
        .serialize(&ExportOptions::default())
        .expect("serialize");

    let reparsed = Document::parse(&out).expect("re-parse");
    let census = stemma::enumerate_revisions(&reparsed.snapshot().canonical);
    assert!(
        census.iter().all(|revision| revision.revision_id != 0),
        "no quarantined (census-only id 0) records may appear — the untouched move must \
         re-import as a first-class revision, not nested tracking. census: {census:?}"
    );
    assert_eq!(
        census
            .iter()
            .filter(|revision| revision.kind == RevisionKind::Move)
            .count(),
        1,
        "the untouched move survives as ONE atomic move revision. census: {census:?}"
    );
    assert_eq!(
        census.len(),
        3,
        "exactly the move + delete + format_run remain pending. census: {census:?}"
    );
}

/// The paragraph-pair (block-level) move shape stays complete too: accept
/// drops the emptied source paragraph, reject restores it, and neither leaves
/// move markup or a pending census behind.
#[test]
fn full_resolution_resolves_paragraph_pair_move_completely() {
    let body = format!(
        r#"<w:p><w:moveFromRangeStart w:id="2" w:name="pmove" w:author="A" {DATE}/><w:moveFrom w:id="3" w:author="A" {DATE}><w:r><w:t>Relocated sentence.</w:t></w:r></w:moveFrom><w:moveFromRangeEnd w:id="2"/></w:p><w:p><w:r><w:t>Anchor sentence.</w:t></w:r></w:p><w:p><w:moveToRangeStart w:id="4" w:name="pmove" w:author="A" {DATE}/><w:moveTo w:id="5" w:author="A" {DATE}><w:r><w:t>Relocated sentence.</w:t></w:r></w:moveTo><w:moveToRangeEnd w:id="4"/></w:p>"#
    );
    let bytes = make_docx_with_body(&body);
    let doc = Document::parse(&bytes).expect("parse");

    let accepted = doc.project(Resolution::AcceptAll).expect("accept-all");
    let out = accepted
        .serialize(&ExportOptions::default())
        .expect("serialize accepted");
    assert_no_move_markup(&document_xml(&out), "accept-all (paragraph pair)");
    assert_eq!(pending_count(&out), 0, "accept-all leaves nothing pending");

    let rejected = doc.project(Resolution::RejectAll).expect("reject-all");
    let out = rejected
        .serialize(&ExportOptions::default())
        .expect("serialize rejected");
    assert_no_move_markup(&document_xml(&out), "reject-all (paragraph pair)");
    assert_eq!(pending_count(&out), 0, "reject-all leaves nothing pending");
}
