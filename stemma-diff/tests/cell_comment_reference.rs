//! A comment reference is a zero-width marker, not content, so comparing a
//! table-cell paragraph that only gains one must leave every other object in
//! the paragraph where it is. The witness is a legacy form field with its
//! bookmark: the redline inserts the reference and keeps the field whole.

use std::io::Write;

use stemma::api::Document;
use stemma::domain::{BlockNode, FieldKind, InlineNode, OpaqueKind, TrackingStatus};
use stemma::{ExportOptions, Resolution};

const FORM_FIELD: &str = r#"<w:r><w:fldChar w:fldCharType="begin"><w:ffData><w:name w:val="Text106"/><w:enabled/><w:calcOnExit w:val="0"/><w:textInput/></w:ffData></w:fldChar></w:r><w:bookmarkStart w:id="1" w:name="Text106"/><w:r><w:instrText xml:space="preserve"> FORMTEXT </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:bookmarkEnd w:id="1"/>"#;

const COMMENT_REFERENCE: &str = r#"<w:r><w:commentReference w:id="1"/></w:r>"#;

/// One table row (with a Word row identity, so the two rows are compared
/// cell by cell) holding `cell_paragraph`, plus a comments part.
fn docx(cell_paragraph: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml"><w:body><w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid><w:tr w14:paraId="0C2E718A" w14:textId="77777777"><w:tc><w:tcPr><w:tcW w:w="4000" w:type="dxa"/></w:tcPr><w:p w14:paraId="647B77EC" w14:textId="77777777">{cell_paragraph}</w:p></w:tc></w:tr></w:tbl><w:p/><w:sectPr/></w:body></w:document>"#
    );
    let comments_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:comment w:id="1" w:author="Reviewer" w:date="2026-01-01T00:00:00Z"><w:p><w:r><w:t>Check the name.</w:t></w:r></w:p></w:comment></w:comments>"#;
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/></Relationships>"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = zip::write::FileOptions::default();
        for (name, body) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", document_xml.as_str()),
            ("word/comments.xml", comments_xml),
        ] {
            zip.start_file(name, opts).expect("start part");
            zip.write_all(body.as_bytes()).expect("write part");
        }
        zip.finish().expect("finish package");
    }
    buf
}

/// The cell paragraph's objects as (field part or "comment reference",
/// whether its segment is Normal / Inserted / Deleted).
fn cell_objects(document: &Document) -> Vec<(String, &'static str)> {
    let BlockNode::Table(table) = &document.snapshot().canonical.blocks[0].block else {
        panic!("the fixture starts with a table");
    };
    let BlockNode::Paragraph(paragraph) = &table.rows[0].cells[0].blocks[0] else {
        panic!("the cell holds a paragraph");
    };
    paragraph
        .segments
        .iter()
        .flat_map(|segment| {
            let status = match segment.status {
                TrackingStatus::Normal => "normal",
                TrackingStatus::Inserted(_) => "inserted",
                TrackingStatus::Deleted(_) => "deleted",
                TrackingStatus::InsertedThenDeleted(_) => "inserted-then-deleted",
            };
            segment
                .inlines
                .iter()
                .filter_map(move |inline| match inline {
                    InlineNode::OpaqueInline(opaque) => match &opaque.kind {
                        OpaqueKind::Field(field) => {
                            Some((format!("{:?}", field.field_kind), status))
                        }
                        OpaqueKind::CommentReference(_) => {
                            Some(("comment reference".to_string(), status))
                        }
                        _ => None,
                    },
                    _ => None,
                })
        })
        .collect()
}

fn field(kind: FieldKind) -> (String, &'static str) {
    (format!("{kind:?}"), "normal")
}

#[test]
fn a_cell_paragraph_gaining_a_comment_reference_keeps_its_field_whole() {
    let base = Document::parse(&docx(FORM_FIELD)).expect("parse base");
    let target =
        Document::parse(&docx(&format!("{COMMENT_REFERENCE}{FORM_FIELD}"))).expect("parse target");

    let redline = stemma_diff::diff_as(&base, &target, "compare").expect("compare");
    let reopened = Document::parse(
        &redline
            .serialize(&ExportOptions::default())
            .expect("serialize"),
    )
    .expect("reopen");
    let fields = [
        field(FieldKind::Begin),
        field(FieldKind::Instruction),
        field(FieldKind::Separate),
        field(FieldKind::End),
    ];
    // The redline inserts the reference and keeps the field whole.
    let mut expected = vec![("comment reference".to_string(), "inserted")];
    expected.extend(fields.iter().cloned());
    assert_eq!(cell_objects(&redline), expected);

    // Saved, a `w:ins` holding only the reference of a comment whose range
    // holds no inserted text is no revision to Word (Word 16, verified): it
    // reopens untracked, and every resolution keeps it and the field whole.
    let mut kept = vec![("comment reference".to_string(), "normal")];
    kept.extend(fields.iter().cloned());
    assert_eq!(cell_objects(&reopened), kept);
    for resolution in [Resolution::AcceptAll, Resolution::RejectAll] {
        let resolved = reopened.project(resolution).expect("resolve");
        assert_eq!(cell_objects(&resolved), kept);
    }
}
