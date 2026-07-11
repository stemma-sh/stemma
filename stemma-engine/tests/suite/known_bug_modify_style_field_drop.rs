//! REGRESSION TEST (formerly an #[ignore]d known-bug doc; fixed by the
//! merge-preserve splice):
//! `ModifyStyle` used to replace the whole `w:style` by styleId with a
//! fragment built purely from the `StyleDefinition` subset, silently dropping
//! every field the subset cannot represent (`w:tabs`, `w:next`, `w:qFormat`,
//! `w:outlineLvl`, the `w:default` attribute, …) — the write-side sibling of
//! the v4 style-op deny_unknown_fields class.
//!
//! DOMAIN RULE (CLAUDE.md "no silent fallbacks"): modifying a style changes
//! the fields the caller authored and PRESERVES the rest. The fix is the
//! merge-preserve splice in `apply_pending_style_ops`: fragment children
//! replace same-named children of the existing element in place (recursing
//! one level into pPr/rPr; schema-ordered insertion for new children).
//! Corollary contract: omitting a field means "leave it alone" — removal is
//! not expressible via ModifyStyle.

use std::io::Write;

use stemma::ExportOptions;
use stemma::RevisionInfo;
use stemma::api::Document;
use stemma::edit::{
    EditStep, EditTransaction, MaterializationMode, StyleDefinition, StyleParaProps, StyleRunProps,
    StyleType,
};
use zip::write::FileOptions;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn make_docx_with_styles(styles_xml: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}"><w:body><w:p><w:pPr><w:pStyle w:val="ClauseText"/></w:pPr><w:r><w:t>Clause body.</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;
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
        zip.start_file("word/styles.xml", opts).unwrap();
        zip.write_all(styles_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

/// A style with a tab stop, qFormat, and next — fields ModifyStyle's
/// StyleDefinition cannot express. Changing only the font must not lose them.
#[test]
fn modify_style_preserves_fields_the_definition_does_not_model() {
    let styles = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="{W_NS}"><w:style w:type="paragraph" w:styleId="ClauseText"><w:name w:val="Clause Text"/><w:next w:val="ClauseText"/><w:qFormat/><w:pPr><w:tabs><w:tab w:val="left" w:pos="1440"/></w:tabs><w:spacing w:after="120"/></w:pPr><w:rPr><w:sz w:val="22"/></w:rPr></w:style></w:styles>"#
    );
    let docx = make_docx_with_styles(&styles);
    let doc = Document::parse(&docx).expect("parse");

    // Modify only the font of the existing style.
    let txn = EditTransaction {
        steps: vec![EditStep::ModifyStyle {
            style_id: "ClauseText".to_string(),
            def: StyleDefinition {
                style_id: "ClauseText".to_string(),
                style_type: StyleType::Para,
                based_on: None,
                name: "Clause Text".to_string(),
                run_props: StyleRunProps {
                    font_family: Some("Georgia".to_string()),
                    ..StyleRunProps::default()
                },
                para_props: StyleParaProps::default(),
            },
            rationale: None,
        }],
        materialization_mode: MaterializationMode::Direct,
        revision: RevisionInfo {
            revision_id: 1,
            author: Some("fid".into()),
            date: Some("2026-07-02T00:00:00Z".into()),
            apply_op_id: None,
        },
        summary: Some("change ClauseText font only".to_string()),
    };
    let edited = doc.apply(&txn).expect("apply ModifyStyle");
    let out = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");

    let archive = stemma::docx::DocxArchive::read(&out).expect("read output");
    let styles_out =
        String::from_utf8(archive.get("word/styles.xml").expect("styles.xml").to_vec())
            .expect("utf-8");

    assert!(
        styles_out.contains("Georgia"),
        "the authored change must land; styles.xml: {styles_out}"
    );
    for needle in [r#"w:pos="1440""#, "<w:qFormat", "<w:next"] {
        assert!(
            styles_out.contains(needle),
            "ModifyStyle must preserve the existing style's unmodeled field \
             {needle:?} (no silent field drop); styles.xml: {styles_out}"
        );
    }
}
