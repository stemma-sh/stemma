//! Package hygiene: serialization emits no unreferenced (orphan) parts.
//!
//! THE CONTRACT (OPC, ISO 29500-2): every part the serializer writes into the
//! package is reachable from a relationship — `_rels/.rels` →
//! `word/document.xml` → `word/_rels/document.xml.rels` → story parts. A part
//! with no inbound relationship is dead weight: Word ignores it silently, but
//! third-party validators flag the package as malformed (the official
//! Anthropic DOCX skill's validator calls it CRITICAL), and it leaks internal
//! bookkeeping names into user files.
//!
//! KNOWN GAP: the importer
//! synthesizes blank default header/footer STORIES for documents that have
//! none (so the header/footer verbs have a story to address —
//! `import.rs` `synthesized-blank-*`), and the serializer then writes those
//! parts and their [Content_Types] overrides WITHOUT writing a
//! `headerReference`/`footerReference` or a relationship. Either the blank
//! stories should be omitted at serialization while still blank, or they
//! should be properly referenced once realized. Repro: any minimal doc —
//! parse → serialize → `word/synthesized-blank-header-default.xml` orphan.

use stemma::api::Document;

fn make_minimal_docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Hello.</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;
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

/// Every part name referenced from any .rels part (targets resolved relative
/// to the rels' owner directory).
fn referenced_parts(docx: &[u8]) -> Vec<String> {
    let archive = stemma::docx::DocxArchive::read(docx).expect("zip");
    let rels_parts: Vec<String> = archive
        .list()
        .filter(|n| n.ends_with(".rels"))
        .map(str::to_string)
        .collect();
    let mut out = Vec::new();
    for name in rels_parts {
        let xml = String::from_utf8(archive.get(&name).expect("part").to_vec()).expect("utf8");
        // Owner dir: "_rels/.rels" → "", "word/_rels/document.xml.rels" → "word/".
        let owner_dir = name
            .rsplit_once("_rels/")
            .map(|(prefix, _)| prefix.to_string())
            .unwrap_or_default();
        for chunk in xml.split("Target=\"").skip(1) {
            let target = chunk.split('"').next().expect("closing quote");
            if target.starts_with("http") {
                continue; // external
            }
            out.push(format!("{owner_dir}{target}"));
        }
    }
    out
}

#[test]
#[ignore = "serializer writes synthesized-blank header/footer parts with no \
            relationship or headerReference — orphan parts in every package"]
fn serialization_emits_no_unreferenced_parts() {
    // The verbatim passthrough (parse → serialize, no edits) is clean; the
    // orphans appear on the EDITED path, where serialization rebuilds the
    // package from the canonical doc — including the synthesized blank
    // header/footer stories the importer added.
    let doc = Document::parse(&make_minimal_docx()).expect("parse");
    let view = doc.read();
    let block = &view.blocks[0];
    let edited = doc
        .apply(&stemma::edit::EditTransaction {
            steps: vec![stemma::edit::EditStep::ReplaceParagraphText {
                block_id: block.id.clone(),
                rationale: None,
                replacement_role: None,
                expect: "Hello.".to_string(),
                semantic_hash: Some(block.guard.clone()),
                content: stemma::edit::ParagraphContent {
                    fragments: vec![stemma::edit::ContentFragment::Text(
                        "Hello there.".to_string(),
                    )],
                },
            }],
            summary: None,
            materialization_mode: stemma::edit::MaterializationMode::TrackedChange,
            revision: stemma::RevisionInfo {
                revision_id: 1,
                identity: 0,
                author: Some("orphan-test".to_string()),
                date: Some("2026-06-12T00:00:00Z".to_string()),
                apply_op_id: None,
            },
        })
        .expect("apply one tracked edit");
    let bytes = edited
        .serialize(&stemma::ExportOptions {
            mode: stemma::ExportMode::Redline,
            validator_level: stemma::ValidatorLevel::Blocking,
            validator: None,
        })
        .expect("serialize");

    let archive = stemma::docx::DocxArchive::read(&bytes).expect("zip");
    let referenced = referenced_parts(&bytes);
    let part_names: Vec<String> = archive.list().map(str::to_string).collect();
    let orphans: Vec<String> = part_names
        .into_iter()
        .filter(|n| {
            n != "[Content_Types].xml"
                && !n.ends_with(".rels")
                && !referenced.iter().any(|r| r == n)
        })
        .collect();
    assert!(
        orphans.is_empty(),
        "every non-rels part must be the target of some relationship; \
         orphan parts: {orphans:?}"
    );
}
