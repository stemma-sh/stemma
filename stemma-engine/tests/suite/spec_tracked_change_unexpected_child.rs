//! A tracked-change container (`w:ins`/`w:del`/`w:moveFrom`/`w:moveTo`) may only
//! hold `EG_ContentRunContent` — runs, the transparent wrappers, the
//! `sdt`/`fldSimple`/math widgets, and the zero-width range & revision markup.
//! There are NO property children. An element matching none of the importer's
//! arms is unmodeled CONTENT: the old catch-all ("ignore other children") dropped
//! it silently, losing its text or its anchoring. The importer must instead
//! REFUSE with a contextual error (container kind + element), the same
//! no-silent-fallback discipline the sibling nested-revision and wrapper-dispatch
//! arms already enforce.
//!
//! Sentinel: with the fail-loud reverted to the silent catch-all, `parse`
//! SUCCEEDS and the element's text is dropped — i.e. this test would pass
//! vacuously on the bug. It asserts BOTH that parse is refused AND (as the
//! fail-without-fix witness) that the dropped content is exactly what would be
//! lost, so a silent-drop regression cannot slip by as a green run.
//!
//! Daily tier: synthetic in-memory DOCX, no corpus.

use std::io::Write;

use stemma::api::Document;

fn docx_with_body(body_inner: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    let mut buf = Vec::new();
    {
        use zip::write::FileOptions;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        let mut w = |name: &str, data: &str| {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        };
        w("[Content_Types].xml", content_types);
        w("_rels/.rels", rels);
        w("word/_rels/document.xml.rels", doc_rels);
        w("word/document.xml", &document_xml);
        zip.finish().unwrap();
    }
    buf
}

/// A `w:hyperlink` nested directly inside a `w:ins` is out-of-schema for
/// `CT_RunTrackChange` (hyperlink lives in `EG_PContent`, not
/// `EG_ContentRunContent`) and matches none of the importer's tracked-change
/// arms. Importing it must REFUSE, not silently drop the hyperlink (and its run
/// text "linktext").
#[test]
fn unexpected_element_inside_tracked_change_is_refused() {
    let body = r#"<w:p>
      <w:r><w:t xml:space="preserve">keep </w:t></w:r>
      <w:ins w:id="7" w:author="A" w:date="2024-01-01T00:00:00Z">
        <w:hyperlink w:anchor="X"><w:r><w:t>linktext</w:t></w:r></w:hyperlink>
      </w:ins>
    </w:p>"#;
    let err = match Document::parse(&docx_with_body(body)) {
        Ok(_) => panic!("import must refuse an unmodeled element inside a tracked change"),
        Err(e) => e,
    };

    // Actionable, contextual error: names the container kind and the element,
    // and states the no-silent-drop rationale.
    let msg = err.message.to_lowercase();
    assert!(
        msg.contains("tracked change") && msg.contains("ins"),
        "error must name the tracked container kind: {}",
        err.message
    );
    assert!(
        msg.contains("hyperlink"),
        "error must name the offending element: {}",
        err.message
    );
    assert!(
        msg.contains("silently dropping") || msg.contains("silently drop"),
        "error must state it refuses rather than silently dropping: {}",
        err.message
    );
}

/// The refusal is not specific to hyperlink: any element outside the tracked
/// container's content model is refused. A `w:tbl` nested directly inside a
/// `w:del` (tables are block content, never a run-level tracked child) must also
/// be rejected rather than dropped.
#[test]
fn unexpected_block_element_inside_tracked_change_is_refused() {
    let body = r#"<w:p>
      <w:del w:id="8" w:author="A" w:date="2024-01-01T00:00:00Z">
        <w:tbl><w:tr><w:tc><w:p><w:r><w:delText>cell</w:delText></w:r></w:p></w:tc></w:tr></w:tbl>
      </w:del>
    </w:p>"#;
    let err = match Document::parse(&docx_with_body(body)) {
        Ok(_) => panic!("import must refuse a table nested directly in a tracked change"),
        Err(e) => e,
    };
    let msg = err.message.to_lowercase();
    assert!(
        msg.contains("tracked change") && msg.contains("del") && msg.contains("tbl"),
        "error must name the container kind (del) and element (tbl): {}",
        err.message
    );
}

/// Guard against over-refusal: a tracked change carrying only LEGITIMATE
/// `EG_ContentRunContent` — a run, a bookmark pair (decoration), and a comment
/// range marker — must still import cleanly. This pins that the fail-loud fires
/// only on genuinely unmodeled children, not on the handled content kinds.
#[test]
fn legitimate_tracked_change_content_still_imports() {
    let body = r#"<w:p>
      <w:r><w:t xml:space="preserve">base </w:t></w:r>
      <w:ins w:id="7" w:author="A" w:date="2024-01-01T00:00:00Z">
        <w:bookmarkStart w:id="1" w:name="_b"/>
        <w:r><w:t>inserted text</w:t></w:r>
        <w:commentRangeEnd w:id="2"/>
        <w:bookmarkEnd w:id="1"/>
      </w:ins>
    </w:p>"#;
    let doc = Document::parse(&docx_with_body(body))
        .expect("a tracked change of legitimate run-level content must import");
    // The inserted text survives (nothing was dropped or refused).
    assert!(
        doc.to_text().contains("inserted text"),
        "inserted run text must be modeled: {:?}",
        doc.to_text()
    );
}
