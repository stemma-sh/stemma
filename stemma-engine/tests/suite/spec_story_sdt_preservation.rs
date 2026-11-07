//! Story-part block content controls survive the edit-path rebuild (§17.5.2).
//!
//! DOMAIN RULE: header/footer parts are rebuilt wholesale from the IR whenever
//! any edit is applied to the document. A `w:sdt` wrapping story blocks —
//! Word's page-number gallery (`w:docPartObj`/`w:docPartGallery`) is the
//! canonical real-world case — is part of the document's content model. The
//! rebuild must re-emit the envelope (`w:sdtPr` verbatim, `w:sdtContent`
//! enclosing the same blocks), not silently unwrap it (CLAUDE.md "no silent
//! fallbacks"). Content alone surviving is not enough: the envelope is what
//! makes Word treat the range as a content control.
//!
//! The wrapper rides the same model as `WrapBlocksInContentControl`
//! (`TrackedBlock::block_sdt_wrap`), so accept/reject semantics are inherited:
//! SDT structure is untracked and survives resolution unchanged.

use std::io::Write;

use stemma::ExportOptions;
use stemma::RevisionInfo;
use stemma::api::Document;
use stemma::edit::{EditStep, EditTransaction, MaterializationMode};
use zip::write::FileOptions;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

/// A footer part whose content is wrapped in Word's page-number gallery SDT.
fn page_number_footer(inner_blocks: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:ftr xmlns:w="{W_NS}"><w:sdt><w:sdtPr><w:id w:val="123456"/><w:docPartObj><w:docPartGallery w:val="Page Numbers (Bottom of Page)"/><w:docPartUnique/></w:docPartObj></w:sdtPr><w:sdtContent>{inner_blocks}</w:sdtContent></w:sdt></w:ftr>"#
    )
}

const PAGE_FIELD_PARA: &str =
    r#"<w:p><w:fldSimple w:instr=" PAGE "><w:r><w:t>1</w:t></w:r></w:fldSimple></w:p>"#;

/// Body doc + footer part + footerReference (mirrors
/// blindspot_editfooter_story.rs::make_footer_docx).
fn make_docx_with_footer(footer_xml: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>
<w:p><w:r><w:t>Body paragraph.</w:t></w:r></w:p>
<w:sectPr>
<w:footerReference w:type="default" r:id="rIdF1"/>
<w:pgSz w:w="12240" w:h="15840"/>
</w:sectPr>
</w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdF1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/></Relationships>"#;

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
        zip.start_file("word/footer1.xml", opts).unwrap();
        zip.write_all(footer_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

/// Any edit forces the story-part rebuild (an un-edited export returns the
/// original bytes and proves nothing — see roundtrip_fidelity.rs module docs).
fn reserialize_trigger() -> EditTransaction {
    EditTransaction {
        steps: vec![EditStep::SetDocDefaults {
            font_family: Some("Calibri".to_string()),
            font_size_half_points: None,
            rationale: Some("story sdt reserialize trigger".to_string()),
        }],
        materialization_mode: MaterializationMode::Direct,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("fid".into()),
            date: Some("2026-07-02T00:00:00Z".into()),
            apply_op_id: None,
        },
        summary: Some("reserialize trigger".to_string()),
    }
}

fn part_of(docx: &[u8], part: &str) -> String {
    let archive = stemma::docx::DocxArchive::read(docx).expect("read docx archive");
    let bytes = archive
        .get(part)
        .unwrap_or_else(|| panic!("{part} present"));
    String::from_utf8(bytes.to_vec()).expect("part utf-8")
}

fn edit_and_serialize(docx: &[u8]) -> Vec<u8> {
    let doc = Document::parse(docx).expect("parse");
    let edited = doc.apply(&reserialize_trigger()).expect("apply trigger");
    edited
        .serialize(&ExportOptions::default())
        .expect("serialize")
}

fn assert_footer_keeps_gallery_envelope(footer_xml: &str) {
    assert_eq!(
        footer_xml.matches("<w:sdt>").count(),
        1,
        "footer must carry exactly one content-control envelope; footer1.xml: {footer_xml}"
    );
    assert!(
        footer_xml.contains("Page Numbers (Bottom of Page)"),
        "the page-number gallery sdtPr (docPartGallery) must survive the story rebuild \
         verbatim; footer1.xml: {footer_xml}"
    );
    let sdt_content = footer_xml
        .find("<w:sdtContent>")
        .expect("sdtContent present");
    let sdt_close = footer_xml
        .find("</w:sdtContent>")
        .expect("sdtContent close present");
    let page_para = footer_xml.find(" PAGE ").expect("PAGE field present");
    assert!(
        sdt_content < page_para && page_para < sdt_close,
        "the PAGE-field paragraph must be enclosed by the sdtContent envelope; \
         footer1.xml: {footer_xml}"
    );
}

#[test]
fn untouched_story_row_keeps_editing_session_attributes() {
    check_story_row_session_attributes();
}

pub(super) fn check_story_row_session_attributes() {
    // ISO 29500-1 17.4.78: these values record row editing sessions.
    // An unrelated body edit must not erase supplied row provenance.
    let attributes = [
        ("w:rsidR", "00110022"),
        ("w:rsidRPr", "00330044"),
        ("w:rsidTr", "00550066"),
        ("w:rsidDel", "00770088"),
    ];
    let row_attributes = attributes
        .iter()
        .map(|(key, value)| format!("{key}=\"{value}\""))
        .collect::<Vec<_>>()
        .join(" ");
    let footer = format!(
        r#"<w:ftr xmlns:w="{W_NS}"><w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="3600"/></w:tblGrid><w:tr {row_attributes}><w:tc><w:tcPr><w:tcW w:w="3600" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>Retained footer entry</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:ftr>"#
    );
    let source = make_docx_with_footer(&footer);
    let original = Document::parse(&source).unwrap();
    let canonical = &original.snapshot().canonical;
    assert!(stemma::roundtrip_compare::compare_canon_docs(canonical, canonical).is_empty());
    for erase in [false, true] {
        let mut changed = (**canonical).clone();
        let stemma::BlockNode::Table(table) = &mut changed.footers[0].blocks[0].block else {
            panic!("footer table");
        };
        if erase {
            table.rows[0].source_row_attrs.clear();
        } else {
            table.rows[0].source_row_attrs[0].1 = "009900AA".to_string();
        }
        let differences = stemma::roundtrip_compare::compare_canon_docs(canonical, &changed);
        assert!(
            differences
                .iter()
                .any(|difference| difference.path.contains("source_row_attrs")),
            "loss or alteration of row session provenance is not equivalent"
        );
    }
    let edited = original.apply(&reserialize_trigger()).unwrap();
    let bytes = edited.serialize(&ExportOptions::default()).unwrap();
    let reopened = Document::parse(&bytes).unwrap();
    let accepted = reopened.project(stemma::Resolution::AcceptAll).unwrap();
    let rejected = reopened.project(stemma::Resolution::RejectAll).unwrap();
    for candidate in [&edited, &reopened, &accepted, &rejected] {
        let output = candidate.serialize(&ExportOptions::default()).unwrap();
        let xml = part_of(&output, "word/footer1.xml");
        let root = xmltree::Element::parse(xml.as_bytes()).unwrap();
        let row = root.get_child("tbl").unwrap().get_child("tr").unwrap();
        for (key, value) in attributes {
            let actual = row
                .attributes
                .iter()
                .find(|(name, _)| {
                    name.namespace.as_deref() == Some(W_NS)
                        && name.local_name == key.trim_start_matches("w:")
                })
                .map(|(_, value)| value.as_str());
            assert_eq!(
                actual,
                Some(value),
                "untouched footer row must retain {key}"
            );
        }
    }
}

/// Word's page-number footer gallery survives an unrelated document edit.
#[test]
fn footer_page_number_sdt_survives_edit_rebuild() {
    let docx = make_docx_with_footer(&page_number_footer(PAGE_FIELD_PARA));
    let out = edit_and_serialize(&docx);

    let footer_xml = part_of(&out, "word/footer1.xml");
    assert_footer_keeps_gallery_envelope(&footer_xml);

    let report = stemma::api::validate(&out);
    assert!(
        report.ok,
        "output must validate clean; issues: {:?}",
        report.issues
    );
}

/// Idempotence: re-importing our own output and editing again must not
/// duplicate or nest the envelope.
#[test]
fn footer_sdt_envelope_is_stable_across_reimport() {
    let docx = make_docx_with_footer(&page_number_footer(PAGE_FIELD_PARA));
    let once = edit_and_serialize(&docx);
    let twice = edit_and_serialize(&once);

    let footer_xml = part_of(&twice, "word/footer1.xml");
    assert_footer_keeps_gallery_envelope(&footer_xml);
}

/// A multi-block envelope keeps its full span: both paragraphs inside ONE
/// sdtContent, and a trailing unwrapped paragraph stays outside it.
#[test]
fn footer_sdt_spanning_two_paragraphs_keeps_span_and_boundary() {
    let two_paras = format!(r#"{PAGE_FIELD_PARA}<w:p><w:r><w:t>Confidential</w:t></w:r></w:p>"#);
    let footer_xml_in = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:ftr xmlns:w="{W_NS}"><w:sdt><w:sdtPr><w:id w:val="123456"/><w:docPartObj><w:docPartGallery w:val="Page Numbers (Bottom of Page)"/><w:docPartUnique/></w:docPartObj></w:sdtPr><w:sdtContent>{two_paras}</w:sdtContent></w:sdt><w:p><w:r><w:t>Outside the control</w:t></w:r></w:p></w:ftr>"#
    );
    let docx = make_docx_with_footer(&footer_xml_in);
    let out = edit_and_serialize(&docx);

    let footer_xml = part_of(&out, "word/footer1.xml");
    assert_eq!(
        footer_xml.matches("<w:sdt>").count(),
        1,
        "exactly one envelope; footer1.xml: {footer_xml}"
    );
    let close = footer_xml
        .find("</w:sdtContent>")
        .expect("sdtContent close present");
    let confidential = footer_xml
        .find("Confidential")
        .expect("second wrapped paragraph present");
    let outside = footer_xml
        .find("Outside the control")
        .expect("unwrapped paragraph present");
    assert!(
        confidential < close,
        "second paragraph must stay INSIDE the envelope (span=2); footer1.xml: {footer_xml}"
    );
    assert!(
        close < outside,
        "trailing paragraph must stay OUTSIDE the envelope; footer1.xml: {footer_xml}"
    );
}

/// ECMA-376 §17.5.2.29/.34: each enclosing control owns its own properties
/// and content. An unrelated edit must not flatten a nested control, including
/// when its first block is also the first block of the outer control.
#[test]
fn nested_footer_controls_keep_properties_and_exact_content_boundaries() {
    check_nested_footer_control_lifecycle();
}

pub(super) fn check_nested_footer_control_lifecycle() {
    fn shape(node: &xmltree::Element) -> serde_json::Value {
        let mut attributes: Vec<_> = node
            .attributes
            .iter()
            .map(|(name, value)| {
                (
                    name.namespace.as_deref(),
                    name.local_name.as_str(),
                    value.as_str(),
                )
            })
            .collect();
        attributes.sort();
        let children: Vec<_> = node
            .children
            .iter()
            .map(|child| match child {
                xmltree::XMLNode::Element(element) => shape(element),
                other => serde_json::json!(["non_element", format!("{other:?}")]),
            })
            .collect();
        serde_json::json!([node.namespace, node.name, attributes, children])
    }
    fn footer_shape(bytes: &[u8]) -> serde_json::Value {
        let xml = part_of(bytes, "word/footer1.xml");
        let root = xmltree::Element::parse(xml.as_bytes()).unwrap();
        // A serializer may declare unused extension namespaces as Ignorable.
        // This witness has exclusively WML payload, so that root declaration
        // does not change MCE processing. Keep every payload attribute exact.
        for (name, prefixes) in root.attributes.iter() {
            assert_eq!(
                name.namespace.as_deref(),
                Some("http://schemas.openxmlformats.org/markup-compatibility/2006")
            );
            assert_eq!(name.local_name, "Ignorable");
            for prefix in prefixes.split_whitespace() {
                let uri = root.namespaces.as_ref().unwrap().get(prefix).unwrap();
                assert_ne!(uri, W_NS);
            }
        }
        fn assert_wml_payload(node: &xmltree::Element) {
            assert_eq!(node.namespace.as_deref(), Some(W_NS));
            assert!(
                node.attributes
                    .iter()
                    .all(|(name, _)| name.namespace.as_deref() == Some(W_NS))
            );
            for child in &node.children {
                if let xmltree::XMLNode::Element(child) = child {
                    assert_wml_payload(child);
                }
            }
        }
        for child in &root.children {
            if let xmltree::XMLNode::Element(child) = child {
                assert_wml_payload(child);
            }
        }
        let mut result = shape(&root);
        result[2] = serde_json::json!([]);
        result
    }
    for before_inner in ["", "<w:p><w:r><w:t>Before inner</w:t></w:r></w:p>"] {
        let footer = format!(
            r#"<w:ftr xmlns:w="{W_NS}"><w:sdt><w:sdtPr><w:id w:val="101"/><w:alias w:val="Outer"/></w:sdtPr><w:sdtContent>{before_inner}<w:sdt><w:sdtPr><w:id w:val="202"/><w:docPartObj><w:docPartGallery w:val="Page Numbers (Bottom of Page)"/><w:docPartUnique/></w:docPartObj></w:sdtPr><w:sdtEndPr><w:rPr><w:b/></w:rPr></w:sdtEndPr><w:sdtContent>{PAGE_FIELD_PARA}</w:sdtContent></w:sdt><w:p><w:r><w:t>After inner</w:t></w:r></w:p></w:sdtContent></w:sdt><w:p><w:r><w:t>Outside outer</w:t></w:r></w:p></w:ftr>"#
        );
        let source = make_docx_with_footer(&footer);
        assert!(
            stemma::api::validate(&source).ok,
            "synthetic input must be valid"
        );
        let expected = footer_shape(&source);
        let output = edit_and_serialize(&source);
        let xml = part_of(&output, "word/footer1.xml");
        assert_eq!(
            xml.matches("<w:sdt>").count(),
            2,
            "both structural control envelopes must survive the unrelated edit"
        );
        assert_eq!(
            footer_shape(&output),
            expected,
            "complete footer payload and nesting"
        );
        for resolution in [stemma::Resolution::AcceptAll, stemma::Resolution::RejectAll] {
            let resolved = Document::parse(&output)
                .unwrap()
                .project(resolution)
                .unwrap()
                .serialize(&ExportOptions::default())
                .unwrap();
            assert!(stemma::api::validate(&resolved).ok);
            assert_eq!(
                footer_shape(&resolved),
                expected,
                "resolution cannot unwrap structural controls"
            );
            assert_eq!(
                footer_shape(&edit_and_serialize(&resolved)),
                expected,
                "nested wrappers remain stable after re-import and another edit"
            );
        }
    }
}
