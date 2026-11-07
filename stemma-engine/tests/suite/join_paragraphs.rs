//! Two paragraphs can be joined.
//!
//! Domain rule (§17.13.5.15, a deleted paragraph mark): joining a paragraph
//! with the one after it deletes the paragraph mark between them, as Delete
//! at the end of the first paragraph does in Word. Tracked, Accept gives one
//! paragraph holding both texts and Reject gives back the two; direct, the
//! join is made at once. A paragraph with no paragraph after it is refused.

use stemma::ExportOptions;
use stemma::api::Document;

fn docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t xml:space="preserve">The first term is to begin from 1 September 2021. </w:t></w:r></w:p><w:p><w:r><w:t>(Please note the start date.)</w:t></w:r></w:p><w:p><w:r><w:t>Before the table.</w:t></w:r></w:p><w:tbl><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc><w:p/></w:tc></w:tr></w:tbl><w:p/><w:sectPr/></w:body></w:document>"#;
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions = zip::write::FileOptions::default();
        for (name, data) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/document.xml", document_xml),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

fn join(
    doc: &Document,
    target: &str,
    with: &str,
    expect: &str,
    mode: &str,
) -> Result<Document, String> {
    let json = serde_json::json!({"ops":[{"op":"join_paragraphs","target":target,"with":with,"expect":expect}],
        "materialization_mode":mode,"revision":{"author":"Editor"}})
    .to_string();
    let txn = stemma::edit_v4::parse_transaction(&json)
        .map_err(|e| format!("{e:?}"))?
        .into_edit_transaction()
        .map_err(|e| format!("{e:?}"))?;
    doc.apply(&txn).map_err(|e| e.to_string())
}

fn texts(doc: &Document) -> Vec<String> {
    doc.read().blocks.iter().map(|b| b.text.clone()).collect()
}

#[test]
fn two_paragraphs_are_joined_tracked_and_direct() {
    let doc = Document::parse(&docx()).expect("parse");
    let first = doc.read().blocks[0].id.to_string();
    let second = doc.read().blocks[1].id.to_string();
    let joined = "The first term is to begin from 1 September 2021. (Please note the start date.)";
    for mode in ["tracked_change", "direct"] {
        let edited = join(&doc, &first, &second, "begin from 1 September 2021.", mode)
            .unwrap_or_else(|e| panic!("{mode}: {e}"));
        let accepted = edited.read_accepted().expect("accept");
        assert_eq!(texts(&accepted)[0], joined, "{mode}");
        assert_eq!(
            texts(&accepted)[1],
            "Before the table.",
            "{mode}: the next paragraph is unchanged"
        );
        if mode == "tracked_change" {
            assert_eq!(
                texts(&edited.read_rejected().expect("reject"))[..2],
                texts(&doc)[..2],
                "reject gives back both paragraphs"
            );
        } else {
            assert_eq!(texts(&edited)[0], joined, "direct: joined at once");
        }
        let reopened = Document::parse(&edited.serialize(&ExportOptions::default()).expect("save"))
            .expect("reopen");
        let differences = stemma::roundtrip_compare::compare_canon_docs(
            &edited.snapshot().canonical,
            &reopened.snapshot().canonical,
        );
        assert!(
            differences.is_empty(),
            "{mode}: {:?}",
            &differences[..differences.len().min(2)]
        );
    }
}

#[test]
fn a_paragraph_before_a_table_is_not_joined() {
    let doc = Document::parse(&docx()).expect("parse");
    let read = doc.read();
    let (first, before_table, table) = (
        read.blocks[0].id.to_string(),
        read.blocks[2].id.to_string(),
        read.blocks[3].id.to_string(),
    );
    let Err(err) = join(
        &doc,
        &before_table,
        &table,
        "Before the table.",
        "tracked_change",
    ) else {
        panic!("a table follows");
    };
    assert!(err.contains("no paragraph after it"), "{err}");
    let Err(err) = join(&doc, &first, &before_table, "begin from", "tracked_change") else {
        panic!("not the next paragraph");
    };
    assert!(
        err.contains("a paragraph joins the paragraph right after it"),
        "{err}"
    );
}

/// Two paragraphs in one table cell join the same way (a sentence split over
/// two paragraphs of one cell).
#[test]
fn two_paragraphs_in_a_cell_are_joined() {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:tbl><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t xml:space="preserve">Delivery takes place on the agreed date. </w:t></w:r></w:p><w:p><w:r><w:t>If no date is agreed, delivery takes place within thirty days.</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/><w:sectPr/></w:body></w:document>"#;
    let bytes = {
        let mut archive = stemma::docx::DocxArchive::read(&docx()).unwrap();
        archive.upsert("word/document.xml", document_xml.as_bytes().to_vec());
        archive.write().unwrap()
    };
    let doc = Document::parse(&bytes).expect("parse");
    let cell = &doc.read().blocks[0].cells[0];
    let (first, second) = (
        cell.paragraphs[0].block_id.clone(),
        cell.paragraphs[1].block_id.clone(),
    );
    let joined = "Delivery takes place on the agreed date. If no date is agreed, delivery takes place within thirty days.";
    for mode in ["tracked_change", "direct"] {
        let edited = join(&doc, &first, &second, "on the agreed date.", mode)
            .unwrap_or_else(|e| panic!("{mode}: {e}"));
        let accepted = edited.read_accepted().expect("accept");
        let cell = &accepted.read().blocks[0].cells[0];
        assert_eq!(cell.paragraphs.len(), 1, "{mode}");
        assert_eq!(cell.text, joined, "{mode}");
        if mode == "tracked_change" {
            let rejected = edited.read_rejected().expect("reject");
            assert_eq!(rejected.read().blocks[0].cells[0].paragraphs.len(), 2);
        }
        let reopened = Document::parse(&edited.serialize(&ExportOptions::default()).expect("save"))
            .expect("reopen");
        let differences = stemma::roundtrip_compare::compare_canon_docs(
            &edited.snapshot().canonical,
            &reopened.snapshot().canonical,
        );
        assert!(
            differences.is_empty(),
            "{mode}: {:?}",
            &differences[..differences.len().min(2)]
        );
    }
}
