//! OOXML spec-compliance tests for authored bookmarks (§17.13.6).
//!
//! These encode behavioral constraints from ECMA-376 / ISO 29500, not merely
//! what the code happens to do:
//!
//! - §17.13.6.2 `w:bookmarkStart` and §17.13.6.1 `w:bookmarkEnd`: every bookmark
//!   is a *pair* — a start carrying `w:id` + `w:name` and a matching end carrying
//!   the same `w:id`. The start must precede the end in document order, and the
//!   two must share one `w:id` (the value that links them).
//! - The bookmark `w:name` must round-trip exactly through serialize -> reimport.
//! - Bookmark names are unique *within a paragraph* in our v1 authoring grammar
//!   (Word treats the first declaration as the reference target; we refuse a
//!   duplicate rather than silently shadow).
//!
//! Daily tier (`spec_*`): corpus-free, all env unset.

use stemma::ExportOptions;
use stemma::api::Document;
use stemma::domain::*;
use stemma::edit::*;
use stemma::runtime::ErrorCode;

fn make_test_docx(paragraphs: &[&str]) -> Vec<u8> {
    let mut document_xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#,
    );
    for para in paragraphs {
        document_xml.push_str(&format!(r#"<w:p><w:r><w:t>{para}</w:t></w:r></w:p>"#));
    }
    document_xml.push_str("<w:sectPr/></w:body></w:document>");

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

fn txn(steps: Vec<EditStep>) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("Spec".to_string()),
            date: Some("2026-06-01T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

/// Ordered list of `(local_name, w:id, Option<w:name>)` for every bookmark
/// decoration in the first paragraph, in document order. The serialized
/// document.xml is parsed so we observe what a consumer (Word) sees.
fn serialized_bookmark_markers(doc: &Document) -> Vec<(String, String, Option<String>)> {
    let bytes = doc.serialize(&ExportOptions::default()).expect("serialize");
    let xml = extract_document_xml(&bytes);
    let mut out = Vec::new();
    let mut rest = xml.as_str();
    while let Some(pos) = rest.find("<w:bookmark") {
        let tail = &rest[pos..];
        let end = tail.find("/>").or_else(|| tail.find('>')).unwrap();
        let element = &tail[..end];
        let local = if element.starts_with("<w:bookmarkStart") {
            "bookmarkStart"
        } else if element.starts_with("<w:bookmarkEnd") {
            "bookmarkEnd"
        } else {
            rest = &tail[1..];
            continue;
        };
        let id = attr_value(element, "w:id").expect("bookmark carries w:id");
        let name = attr_value(element, "w:name");
        out.push((local.to_string(), id, name));
        rest = &tail[end..];
    }
    out
}

fn extract_document_xml(docx: &[u8]) -> String {
    use std::io::Read;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(docx)).expect("open zip");
    let mut file = zip
        .by_name("word/document.xml")
        .expect("document.xml present");
    let mut s = String::new();
    file.read_to_string(&mut s).expect("read document.xml");
    s
}

fn attr_value(xml: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let start = xml.find(&needle)? + needle.len();
    let rest = &xml[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn first_block_id(doc: &Document) -> NodeId {
    doc.read().blocks[0].id.clone()
}

/// §17.13.6 — an authored bookmark serializes as a `bookmarkStart` (carrying
/// `w:id` + `w:name`) that PRECEDES a `bookmarkEnd` carrying the SAME `w:id`.
#[test]
fn spec_bookmark_start_precedes_end_with_shared_id() {
    let base = Document::parse(&make_test_docx(&["alpha beta gamma"])).expect("parse");
    let block = first_block_id(&base);
    let edited = base
        .apply(&txn(vec![EditStep::InsertBookmark {
            block_id: block,
            expect: "beta".to_string(),
            semantic_hash: None,
            name: "Term".to_string(),
            rationale: None,
        }]))
        .expect("insert");

    let markers = serialized_bookmark_markers(&edited);
    assert_eq!(markers.len(), 2, "exactly a start and an end");

    let (kind0, id0, name0) = &markers[0];
    let (kind1, id1, name1) = &markers[1];

    // start precedes end (document order).
    assert_eq!(kind0, "bookmarkStart", "start must come first");
    assert_eq!(kind1, "bookmarkEnd", "end must come second");

    // §17.13.6.2: the start carries w:name; §17.13.6.1: the end does not.
    assert_eq!(name0.as_deref(), Some("Term"));
    assert_eq!(name1.as_deref(), None, "bookmarkEnd carries no w:name");

    // The pair shares one w:id.
    assert_eq!(id0, id1, "start and end must share one w:id");
}

/// The bookmark `w:name` survives serialize -> reimport byte-exact.
#[test]
fn spec_bookmark_name_roundtrips_exact() {
    let base = Document::parse(&make_test_docx(&["one two three"])).expect("parse");
    let block = first_block_id(&base);
    let name = "Cross_Ref.Target-1";
    let edited = base
        .apply(&txn(vec![EditStep::InsertBookmark {
            block_id: block,
            expect: "two".to_string(),
            semantic_hash: None,
            name: name.to_string(),
            rationale: None,
        }]))
        .expect("insert");

    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let reimported = Document::parse(&bytes).expect("reimport");
    let markers = serialized_bookmark_markers(&reimported);
    let start = markers
        .iter()
        .find(|(k, _, _)| k == "bookmarkStart")
        .expect("start present");
    assert_eq!(start.2.as_deref(), Some(name), "name preserved exactly");
}

/// Two distinct authored bookmarks get two distinct `w:id`s (no collision), and
/// each end pairs with its own start.
#[test]
fn spec_distinct_bookmarks_have_distinct_ids() {
    let base = Document::parse(&make_test_docx(&["one two three four five"])).expect("parse");
    let block = first_block_id(&base);
    let edited = base
        .apply(&txn(vec![
            EditStep::InsertBookmark {
                block_id: block.clone(),
                expect: "two".to_string(),
                semantic_hash: None,
                name: "A".to_string(),
                rationale: None,
            },
            EditStep::InsertBookmark {
                block_id: block,
                expect: "four".to_string(),
                semantic_hash: None,
                name: "B".to_string(),
                rationale: None,
            },
        ]))
        .expect("two inserts");

    let markers = serialized_bookmark_markers(&edited);
    let start_ids: Vec<&String> = markers
        .iter()
        .filter(|(k, _, _)| k == "bookmarkStart")
        .map(|(_, id, _)| id)
        .collect();
    assert_eq!(start_ids.len(), 2);
    assert_ne!(
        start_ids[0], start_ids[1],
        "distinct bookmarks => distinct ids"
    );
}

/// Name uniqueness within a paragraph: a second bookmark with the same name is
/// refused (no silent shadowing of the reference target).
#[test]
fn spec_duplicate_name_in_paragraph_refused() {
    let base = Document::parse(&make_test_docx(&["alpha beta gamma"])).expect("parse");
    let block = first_block_id(&base);
    let once = base
        .apply(&txn(vec![EditStep::InsertBookmark {
            block_id: block.clone(),
            expect: "alpha".to_string(),
            semantic_hash: None,
            name: "Same".to_string(),
            rationale: None,
        }]))
        .expect("first insert");
    let err = match once.apply(&txn(vec![EditStep::InsertBookmark {
        block_id: block,
        expect: "gamma".to_string(),
        semantic_hash: None,
        name: "Same".to_string(),
        rationale: None,
    }])) {
        Ok(_) => panic!("duplicate name must be refused"),
        Err(e) => e,
    };
    assert_eq!(err.code, ErrorCode::UnsupportedEdit);
}
