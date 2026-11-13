//! A resolution removes a note definition only when it removes the note's
//! last reference. A definition that had no reference in the input is the
//! input's own state, and both resolution paths (the model's projection and
//! the byte path) leave it, as they leave a pre-existing unreferenced
//! comment.

use std::io::Write;

use stemma::api::Document;
use stemma::docx::DocxArchive;
use stemma::normalize::{normalize_docx, reject_all_docx};
use stemma::{ExportOptions, Resolution};

use crate::common::read_zip_entry;

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn docx() -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="{W}"><w:body><w:p><w:r><w:t>Body text</w:t></w:r><w:r><w:footnoteReference w:id="1"/></w:r><w:ins w:id="9" w:author="Ann" w:date="2026-01-01T00:00:00Z"><w:r><w:t> and more</w:t></w:r></w:ins></w:p><w:sectPr/></w:body></w:document>"#
    );
    let footnotes_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:footnotes xmlns:w="{W}"><w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote><w:footnote w:id="1"><w:p><w:r><w:t>Referenced note.</w:t></w:r></w:p></w:footnote><w:footnote w:id="2"><w:p><w:r><w:t>Note nothing refers to.</w:t></w:r></w:p></w:footnote></w:footnotes>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/footnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdFn" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes" Target="footnotes.xml"/></Relationships>"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions = zip::write::FileOptions::default();
        for (name, data) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", document_xml.as_str()),
            ("word/footnotes.xml", footnotes_xml.as_str()),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

fn footnote_ids(bytes: &[u8]) -> Vec<String> {
    let xml = read_zip_entry(bytes, "word/footnotes.xml").expect("footnotes.xml");
    xml.split("<w:footnote ")
        .skip(1)
        .filter(|rest| !rest.starts_with("w:type"))
        .map(|rest| {
            rest.split("w:id=\"")
                .nth(1)
                .unwrap()
                .split('"')
                .next()
                .unwrap()
                .to_string()
        })
        .collect()
}

#[test]
fn a_note_nothing_referred_to_survives_both_resolution_paths() {
    let bytes = docx();
    let doc = Document::parse(&bytes).expect("parse");
    for resolution in [Resolution::AcceptAll, Resolution::RejectAll] {
        let resolved = doc
            .project(resolution)
            .unwrap()
            .serialize(&ExportOptions::default())
            .unwrap();
        assert_eq!(footnote_ids(&resolved), ["1", "2"]);
    }
    let archive = DocxArchive::read(&bytes).unwrap();
    for (resolved, _) in [
        normalize_docx(&archive).unwrap(),
        reject_all_docx(&archive).unwrap(),
    ] {
        assert_eq!(footnote_ids(&resolved.write().unwrap()), ["1", "2"]);
    }
}
