//! Integration tests for inserting a paragraph already AT a target list level
//! via the v4 paragraph-content `list: {num_id, ilvl}` field.
//!
//! Domain rule: an agent authoring a list sub-point should be able to do it as a
//! SINGLE tracked insert. The inserted paragraph carries its `w:numPr`
//! (numId + ilvl, §17.3.1.19) from the start — no follow-up `set_numbering`
//! (which is refused on a freshly-inserted paragraph). The insert is a normal
//! tracked insert (`w:ins`):
//!   - the serialized inserted paragraph has `w:numPr` with the requested
//!     numId/ilvl, sitting inside a `w:ins`-tracked paragraph;
//!   - `accept_all` == the document WITH the new list item present at the right
//!     level;
//!   - `reject_all` == the original document (the insert vanishes);
//!   - referencing a numId the document does NOT use fails loud — the engine
//!     never fabricates a numbering definition.
//!
//! The fixture (mirroring `list_ops.rs`) carries a real `word/numbering.xml`
//! with `numId=1` (decimal) and `numId=2` (bullet).

use stemma::Resolution;
use stemma::api::Document;
use stemma::docx::DocxArchive;
use stemma::domain::{BlockNode, CanonDoc, InlineNode, ParagraphNode};
use stemma::edit_v4::parse_transaction;
use stemma::runtime::ExportOptions;

// ─── Fixture: a doc with a decimal list (numId=1) + bullet list (numId=2) ─────

fn make_two_list_docx(paras: &[(&str, Option<(u32, u32)>)]) -> Vec<u8> {
    let mut document_xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#,
    );
    for (text, numbering) in paras {
        document_xml.push_str("<w:p>");
        if let Some((num_id, ilvl)) = numbering {
            document_xml.push_str(&format!(
                r#"<w:pPr><w:numPr><w:ilvl w:val="{ilvl}"/><w:numId w:val="{num_id}"/></w:numPr></w:pPr>"#
            ));
        }
        document_xml.push_str(&format!(r#"<w:r><w:t>{text}</w:t></w:r></w:p>"#));
    }
    document_xml.push_str("<w:sectPr/></w:body></w:document>");

    let numbering_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl><w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="lowerLetter"/><w:lvlText w:val="(%2)"/></w:lvl></w:abstractNum><w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/><w:lvlText w:val="&#8226;"/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num><w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num></w:numbering>"#;

    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId10" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/></Relationships>"#;

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
        zip.start_file("word/numbering.xml", opts).unwrap();
        zip.write_all(numbering_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

fn parse(paras: &[(&str, Option<(u32, u32)>)]) -> (Document, Vec<String>) {
    let doc = Document::parse(&make_two_list_docx(paras)).expect("parse two-list docx");
    let ids = doc.read().blocks.iter().map(|b| b.id.to_string()).collect();
    (doc, ids)
}

/// Apply a v4 insert that authors a sub-point after `anchor_id` at
/// `{num_id, ilvl}`. Returns the edited Document.
fn insert_subpoint(
    doc: &Document,
    anchor_id: &str,
    text: &str,
    num_id: u32,
    ilvl: u32,
) -> Result<Document, stemma::RuntimeError> {
    let json = format!(
        r#"{{
          "ops": [{{
            "op": "insert",
            "target": {{ "anchor": "{anchor_id}", "position": "after" }},
            "content": [{{
              "type": "paragraph",
              "role": "default",
              "content": [{{ "type": "text", "text": "{text}" }}],
              "list": {{ "num_id": {num_id}, "ilvl": {ilvl} }}
            }}]
          }}],
          "revision": {{ "author": "Counsel", "date": "2026-06-05T00:00:00Z" }}
        }}"#
    );
    let txn = parse_transaction(&json)
        .expect("schema check passes")
        .into_edit_transaction()
        .expect("adapter translates the list field");
    doc.apply(&txn)
}

/// Flatten a paragraph's text content (Text inlines only — enough to match the
/// plain-text inserts these tests author).
fn paragraph_text(p: &ParagraphNode) -> String {
    p.segments
        .iter()
        .flat_map(|s| s.inlines.iter())
        .filter_map(|i| match i {
            InlineNode::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect()
}

/// The paragraph texts of `doc` projected at `resolution`, in document order.
fn para_texts(doc: &Document, resolution: Resolution) -> Vec<String> {
    let resolved = doc.project(resolution).expect("project");
    resolved
        .snapshot()
        .canonical
        .blocks
        .iter()
        .filter_map(|b| match &b.block {
            BlockNode::Paragraph(p) => Some(paragraph_text(p)),
            _ => None,
        })
        .collect()
}

/// `(num_id, ilvl)` of the first paragraph whose accepted text equals `text`.
fn numbering_of_text(doc: &Document, text: &str) -> Option<(u32, u32)> {
    let resolved = doc.project(Resolution::AcceptAll).expect("project");
    let canon: &CanonDoc = &resolved.snapshot().canonical;
    for b in &canon.blocks {
        if let BlockNode::Paragraph(p) = &b.block
            && paragraph_text(p) == text
        {
            return p.numbering.as_ref().map(|n| (n.num_id, n.ilvl));
        }
    }
    None
}

// ─── Invariant 1: inserted paragraph carries the requested numPr ──────────────

#[test]
fn inserted_subpoint_carries_requested_numbering() {
    // Parent decimal item at numId=1, ilvl=0. Insert a sub-point one level
    // deeper (ilvl=1) on the SAME list (numId=1) — the agent reads the parent's
    // list.num_id and inserts with ilvl = parent.ilvl + 1.
    let (doc, ids) = parse(&[("Parent item.", Some((1, 0)))]);
    let edited = insert_subpoint(&doc, &ids[0], "Freshly nested", 1, 1).expect("insert sub-point");

    assert_eq!(
        numbering_of_text(&edited, "Freshly nested"),
        Some((1, 1)),
        "inserted sub-point is on numId=1 at ilvl=1"
    );
}

// ─── Invariant: serialized inserted para has numPr inside a w:ins ─────────────

#[test]
fn serialized_subpoint_has_numpr_inside_w_ins() {
    let (doc, ids) = parse(&[("Parent item.", Some((1, 0)))]);
    let edited = insert_subpoint(&doc, &ids[0], "Freshly nested", 1, 1).expect("insert sub-point");

    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize tracked insert");
    let archive = DocxArchive::read(&bytes).expect("read docx");
    let xml = String::from_utf8(
        archive
            .get("word/document.xml")
            .expect("document.xml")
            .to_vec(),
    )
    .expect("utf8");

    // The new paragraph's run is tracked-inserted (w:ins) and the paragraph
    // carries its numPr at the requested level.
    assert!(
        xml.contains("Freshly nested"),
        "inserted text present in serialized output"
    );
    assert!(
        xml.contains("<w:ins"),
        "the inserted run is a tracked insert (w:ins): {xml}"
    );
    // The serializer emits self-closing tags with a space (`<w:numId w:val="1" />`);
    // match the numPr coordinates without depending on that exact whitespace.
    assert!(
        xml.contains(r#"<w:numId w:val="1""#),
        "inserted paragraph references numId=1: {xml}"
    );
    assert!(
        xml.contains(r#"<w:ilvl w:val="1""#),
        "inserted paragraph sits at ilvl=1: {xml}"
    );

    // Structural check: the numPr and the w:ins both live in the SAME paragraph
    // (the freshly-inserted one), i.e. the inserted text's paragraph is the one
    // that gained the level-1 numPr. We locate the paragraph by its text.
    let subpoint_p_start = xml.find("Freshly nested").expect("text present");
    let p_open = xml[..subpoint_p_start]
        .rfind("<w:p>")
        .or_else(|| xml[..subpoint_p_start].rfind("<w:p "))
        .expect("paragraph open before the inserted text");
    let p_close = xml[subpoint_p_start..]
        .find("</w:p>")
        .map(|o| subpoint_p_start + o)
        .expect("paragraph close after the inserted text");
    let subpoint_para = &xml[p_open..p_close];
    assert!(
        subpoint_para.contains(r#"<w:ilvl w:val="1""#)
            && subpoint_para.contains(r#"<w:numId w:val="1""#),
        "the inserted paragraph itself carries the level-1 numPr: {subpoint_para}"
    );
    assert!(
        subpoint_para.contains("<w:ins"),
        "the inserted paragraph's content is tracked-inserted: {subpoint_para}"
    );
}

// ─── Invariant 2: accept == new item present; reject == original ──────────────

#[test]
fn accept_has_subpoint_reject_is_original() {
    let (doc, ids) = parse(&[("Parent item.", Some((1, 0))), ("Tail.", Some((1, 0)))]);

    let original_accept = para_texts(&doc, Resolution::AcceptAll);
    let edited = insert_subpoint(&doc, &ids[0], "Freshly nested", 1, 1).expect("insert sub-point");

    let accepted = para_texts(&edited, Resolution::AcceptAll);
    assert_eq!(
        accepted,
        vec![
            "Parent item.".to_string(),
            "Freshly nested".to_string(),
            "Tail.".to_string()
        ],
        "accept-all keeps the new list item, in position after its anchor"
    );

    let rejected = para_texts(&edited, Resolution::RejectAll);
    assert_eq!(
        rejected, original_accept,
        "reject-all restores the original document (the insert vanishes)"
    );
}

// ─── Invariant 3: an absent numId fails loud (no fabricated definition) ───────

#[test]
fn unknown_num_id_is_refused_loud() {
    // numId=99 is not used by any paragraph in the document. The engine must
    // refuse rather than fabricate a numbering definition.
    let (doc, ids) = parse(&[("Parent item.", Some((1, 0)))]);
    let err = insert_subpoint(&doc, &ids[0], "Orphan", 99, 0)
        .err()
        .expect("unknown numId must be refused");
    let msg = format!("{err:?}");
    assert!(
        err.message.contains("num_id 99")
            || err.message.contains("99")
            || msg.contains("InsertListNumIdUnknown"),
        "the refusal names the unknown numId and the in-use ids: {err:?} / {}",
        err.message
    );
}

// ─── Invariant: ilvl out of bounds is refused at the wire edge ────────────────

#[test]
fn out_of_range_ilvl_is_refused_at_schema() {
    let (_doc, ids) = parse(&[("Parent item.", Some((1, 0)))]);
    let json = format!(
        r#"{{
          "ops": [{{
            "op": "insert",
            "target": {{ "anchor": "{}", "position": "after" }},
            "content": [{{
              "type": "paragraph",
              "role": "default",
              "content": [{{ "type": "text", "text": "too deep" }}],
              "list": {{ "num_id": 1, "ilvl": 9 }}
            }}]
          }}],
          "revision": {{ "author": "Counsel" }}
        }}"#,
        ids[0]
    );
    let err = parse_transaction(&json).expect_err("ilvl 9 must be refused at the schema layer");
    let msg = format!("{err}");
    assert!(
        msg.contains("0..=8") || msg.contains("ilvl"),
        "the schema refusal names the list-level bound: {msg}"
    );
}
