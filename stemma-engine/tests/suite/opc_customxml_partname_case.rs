//! OPC part-name equivalence is ASCII case-INSENSITIVE (ECMA-376 Part 2 §9.1).
//!
//! Wild Word packages exist whose custom-XML datastore lives under an uppercase
//! directory — `customXML/item1.xml` — with the document relationship target
//! spelled to match (`../customXML/item1.xml`). Per §9.1 that part name is
//! equivalent to `customXml/item1.xml`; a conforming consumer must treat them as
//! the same part when deciding part existence, uniqueness, and rel-target
//! resolution.
//!
//! Regression: authoring a NEW content-control data-binding on such a package
//! (`WrapInContentControl` + `DataBinding`) allocated the next `customXml/itemN`
//! index by scanning part names case-SENSITIVELY. It failed to see the uppercase
//! `customXML/item1.xml`, reused index 1, and (a) clobbered the pre-existing
//! datastore via the case-insensitive `set_part`, and (b) authored a document
//! relationship whose lowercase target `../customXml/item1.xml` no longer matched
//! the uppercase-spelled stored part, so serialize died with I-REL-003
//! ("target … does not exist in the package").
//!
//! Domain rule: the allocator, the itemID dedup scan, and the I-REL-003
//! validator all compare part names case-insensitively; existing parts keep
//! their original spelling on write, and newly authored parts use the canonical
//! lowercase `customXml/` directory.

use stemma::api::Document;
use stemma::domain::{BlockNode, CanonDoc, NodeId, RevisionInfo, SdtControl};
use stemma::edit::{DataBinding, EditStep, EditTransaction, MaterializationMode, SdtSpec};
use stemma::{ExportMode, ExportOptions, ValidatorLevel};

use std::io::Write;
use zip::write::FileOptions;

/// A single-paragraph DOCX that already carries a custom-XML datastore under an
/// UPPERCASE `customXML/` directory, with the document relationship target
/// spelled to match. This is the wild layout §9.1 makes legal.
fn make_docx_with_uppercase_customxml(text: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" xmlns:w15="http://schemas.microsoft.com/office/word/2012/wordml"><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#
    );
    // The existing datastore's ds:itemID — distinct from the one we later bind,
    // so the new binding authors a genuinely new part rather than reusing this.
    let existing_item_id = "{11111111-2222-3333-4444-555555555555}";
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/customXML/itemProps1.xml" ContentType="application/vnd.openxmlformats-officedocument.customXmlProperties+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    // The document rel target is spelled with the uppercase directory, matching
    // the stored part exactly — legal per §9.1.
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId100" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/customXml" Target="../customXML/item1.xml"/></Relationships>"#;
    let item1 =
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><root xmlns="urn:existing"/>"#;
    let item_props1 = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><ds:datastoreItem ds:itemID="{existing_item_id}" xmlns:ds="http://schemas.openxmlformats.org/officeDocument/2006/customXml"><ds:schemaRefs/></ds:datastoreItem>"#
    );
    let item1_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/customXmlProps" Target="itemProps1.xml"/></Relationships>"#;

    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        let mut file = |name: &str, body: &str| {
            zip.start_file(name, opts).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        };
        file("[Content_Types].xml", content_types);
        file("_rels/.rels", rels);
        file("word/_rels/document.xml.rels", doc_rels);
        file("word/document.xml", &document_xml);
        // The uppercase-directory datastore triad.
        file("customXML/item1.xml", item1);
        file("customXML/itemProps1.xml", &item_props1);
        file("customXML/_rels/item1.xml.rels", item1_rels);
        zip.finish().unwrap();
    }
    buf
}

fn first_block_id(canon: &CanonDoc) -> NodeId {
    match &canon.blocks[0].block {
        BlockNode::Paragraph(p) => p.id.clone(),
        _ => panic!("expected a paragraph"),
    }
}

fn txn(steps: Vec<EditStep>) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("OPC".to_string()),
            date: Some("2026-07-09T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

/// Resolve a relationship target relative to its source part's directory,
/// normalizing `../` segments (the resolution the OPC validator performs).
fn resolve(base_dir: &str, target: &str) -> String {
    let combined = format!("{base_dir}{target}");
    let mut out: Vec<&str> = Vec::new();
    for seg in combined.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    out.join("/")
}

#[test]
fn data_binding_on_uppercase_customxml_layout_authors_fresh_part_and_serializes_clean() {
    let base = Document::parse(&make_docx_with_uppercase_customxml(
        "The Counterparty shall sign.",
    ))
    .expect("parse wild uppercase-customXML package");
    let block_id = first_block_id(&base.snapshot().canonical);

    // Bind with a FRESH storeItemID (not the existing datastore's) so the save
    // path must author a new datastore part, not reuse item1.
    let fresh_store_id = "{D4D4D4D4-0000-4000-8000-BADA55BADA55}";
    let binding = DataBinding {
        xpath: "/ns:root[1]/ns:party[1]".to_string(),
        store_item_id: fresh_store_id.to_string(),
        prefix_mappings: Some("xmlns:ns='urn:contract'".to_string()),
    };
    let edited = base
        .apply(&txn(vec![EditStep::WrapInContentControl {
            block_id,
            expect: "Counterparty".to_string(),
            semantic_hash: None,
            spec: SdtSpec {
                tag: Some("party".to_string()),
                alias: Some("Counterparty".to_string()),
                control: SdtControl::RichText,
                binding: Some(binding),
            },
            rationale: None,
        }]))
        .expect("apply data-binding wrap");

    // Serialize under Blocking validation — this is where the case-sensitive
    // I-REL-003 resolution used to reject the authored datastore relationship.
    let bytes = edited
        .serialize(&ExportOptions {
            mode: ExportMode::Redline,
            validator_level: ValidatorLevel::Blocking,
            validator: None,
        })
        .expect("serialize+validate Blocking-clean on uppercase-customXML layout");

    let archive = stemma::docx::DocxArchive::read(&bytes).expect("read out");
    let names: Vec<String> = archive.list().map(|s| s.to_string()).collect();

    // The pre-existing uppercase datastore part keeps its original spelling and
    // is NOT clobbered by the new authoring.
    assert!(
        names.iter().any(|n| n == "customXML/item1.xml"),
        "the pre-existing uppercase customXML/item1.xml must survive with its \
         original case; parts={names:?}"
    );

    // The newly authored datastore uses the canonical lowercase directory and a
    // FRESH index (2), not a case-variant collision with item1.
    assert!(
        names.iter().any(|n| n == "customXml/item2.xml"),
        "a fresh customXml/item2.xml datastore part must be authored (canonical \
         lowercase dir, next free index); parts={names:?}"
    );

    // Every internal relationship target resolves to a stored part
    // case-insensitively (§9.1) — including the pre-existing uppercase target
    // and the newly authored lowercase one.
    let doc_rels = String::from_utf8(
        archive
            .get("word/_rels/document.xml.rels")
            .expect("document rels")
            .to_vec(),
    )
    .unwrap();
    for target in ["../customXML/item1.xml", "../customXml/item2.xml"] {
        assert!(
            doc_rels.contains(target),
            "expected document rel target {target:?}; rels={doc_rels}"
        );
        let resolved = resolve("word/", target);
        assert!(
            names.iter().any(|n| n.eq_ignore_ascii_case(&resolved)),
            "rel target {target:?} resolves to {resolved:?}, which must exist \
             case-insensitively; parts={names:?}"
        );
    }
}
