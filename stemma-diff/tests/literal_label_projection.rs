//! A typed-in enumeration label ("3.") is real paragraph text: comparing a
//! paragraph whose label changed ("3." → "6.") records the label change as a
//! tracked deletion and insertion. Reject All of that redline restores the
//! base reading and Accept All the target's — and the in-memory projection
//! of the returned redline must agree with projecting the saved and reopened
//! redline, since both are the same document.

use std::io::Write;

use stemma::api::Document;
use stemma::{ExportOptions, Resolution};

fn docx(body: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"></Relationships>"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = zip::write::FileOptions::default();
        for (name, part) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", document_xml.as_str()),
        ] {
            zip.start_file(name, opts).expect("start part");
            zip.write_all(part.as_bytes()).expect("write part");
        }
        zip.finish().expect("finish package");
    }
    buf
}

fn reopened(document: &Document) -> Document {
    let bytes = document
        .serialize(&ExportOptions::default())
        .expect("serialize");
    Document::parse(&bytes).expect("reopen")
}

fn first_block_text(document: &Document) -> String {
    document.read().blocks[0].text.clone()
}

#[test]
fn reject_all_of_a_compared_label_change_restores_the_base_label_in_memory_and_reopened() {
    let base = Document::parse(&docx(
        r#"<w:p><w:r><w:t>3. The article proposes five principles.</w:t></w:r></w:p>"#,
    ))
    .expect("parse base");
    let target = Document::parse(&docx(
        r#"<w:p><w:r><w:t>6. The article proposes five principles.</w:t></w:r></w:p>"#,
    ))
    .expect("parse target");

    let redline = stemma_diff::diff_as(&base, &target, "compare").expect("compare");

    let reopened_redline = reopened(&redline);
    for (reading, document) in [
        ("in-memory redline", &redline),
        ("reopened redline", &reopened_redline),
    ] {
        let rejected = document.project(Resolution::RejectAll).expect("reject all");
        assert_eq!(
            first_block_text(&rejected),
            "3. The article proposes five principles.",
            "{reading}: Reject All restores the base label"
        );
        assert_eq!(
            first_block_text(&reopened(&rejected)),
            "3. The article proposes five principles.",
            "{reading}: the rejected document saves the base label"
        );
        let accepted = document.project(Resolution::AcceptAll).expect("accept all");
        assert_eq!(
            first_block_text(&accepted),
            "6. The article proposes five principles.",
            "{reading}: Accept All keeps the target label"
        );
        assert_eq!(
            first_block_text(&reopened(&accepted)),
            "6. The article proposes five principles.",
            "{reading}: the accepted document saves the target label"
        );
    }
}
