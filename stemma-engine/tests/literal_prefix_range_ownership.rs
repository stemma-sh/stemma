//! Position-bound ranges must retain their original text owners when another
//! paragraph is edited (ISO 29500-1 §§17.13.2,17.13.8).
use std::io::{Cursor, Read, Write};
use stemma::api::{Document, ExportOptions};
use stemma::edit_v4::parse_transaction;
use xmltree::{Element, XMLNode};
use zip::write::FileOptions;

fn fixture(start: &str, end: &str) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let parts = [
        ("[Content_Types].xml", r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#.to_string()),
        ("_rels/.rels", r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#.to_string()),
        ("word/document.xml", format!(r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p>{start}<w:r><w:t>i</w:t></w:r>{end}<w:r><w:t>. Appendix</w:t></w:r></w:p><w:p><w:r><w:t>Other body.</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#)),
    ];
    for (name, text) in parts {
        writer.start_file(name, FileOptions::default()).unwrap();
        writer.write_all(text.as_bytes()).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn range_positions(bytes: &[u8], marker: &str) -> Vec<usize> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut xml = Vec::new();
    archive
        .by_name("word/document.xml")
        .unwrap()
        .read_to_end(&mut xml)
        .unwrap();
    let root = Element::parse(xml.as_slice()).unwrap();
    let paragraph = root.get_child("body").unwrap();
    fn visit(node: &Element, marker: &str, offset: &mut usize, positions: &mut Vec<usize>) {
        if node.name.starts_with(marker) {
            positions.push(*offset);
        }
        if node.name == "t" {
            *offset += node.get_text().unwrap().chars().count();
        }
        for child in node.children.iter().filter_map(XMLNode::as_element) {
            visit(child, marker, offset, positions);
        }
    }
    let mut positions = Vec::new();
    visit(paragraph, marker, &mut 0, &mut positions);
    positions
}

fn unrelated_edit_preserves_range(start: &str, end: &str, marker: &str) {
    let source = fixture(start, end);
    assert_eq!(range_positions(&source, marker), vec![0, 1]);
    let document = Document::parse(&source).unwrap();
    let view = document.read();
    let last = view.blocks.last().unwrap();
    let wire = serde_json::json!({
        "ops":[{"op":"replace", "target":last.id, "guard":last.guard,
            "content":{"type":"paragraph", "content":[{"type":"text", "text":"Changed body."}]}}],
        "revision":{"author":"Range test", "date":"2026-01-01T00:00:00Z"}
    });
    let transaction = parse_transaction(&wire.to_string())
        .unwrap()
        .into_edit_transaction()
        .unwrap();
    let edited = document.apply(&transaction).unwrap();
    let output = edited.serialize(&ExportOptions::default()).unwrap();
    assert_eq!(range_positions(&output, marker), vec![0, 1]);
    let reopened = Document::parse(&output)
        .unwrap()
        .serialize(&ExportOptions::default())
        .unwrap();
    assert_eq!(range_positions(&reopened, marker), vec![0, 1]);
    for resolution in [stemma::Resolution::AcceptAll, stemma::Resolution::RejectAll] {
        let projected = edited.project(resolution).unwrap();
        let output = projected.serialize(&ExportOptions::default()).unwrap();
        let expected = if marker == "proofErr" {
            vec![]
        } else {
            vec![0, 1]
        };
        assert_eq!(range_positions(&output, marker), expected);
        let reopened = Document::parse(&output)
            .unwrap()
            .serialize(&ExportOptions::default())
            .unwrap();
        assert_eq!(range_positions(&reopened, marker), expected);
    }
}

#[test]
fn unrelated_edit_preserves_spelling_range_on_literal_label() {
    unrelated_edit_preserves_range(
        r#"<w:proofErr w:type="spellStart"/>"#,
        r#"<w:proofErr w:type="spellEnd"/>"#,
        "proofErr",
    );
}

#[test]
fn unrelated_edit_preserves_bookmark_range_on_literal_label() {
    unrelated_edit_preserves_range(
        r#"<w:bookmarkStart w:id="1" w:name="Label"/>"#,
        r#"<w:bookmarkEnd w:id="1"/>"#,
        "bookmark",
    );
}

#[test]
fn unrelated_edit_preserves_permission_range_on_literal_label() {
    unrelated_edit_preserves_range(
        r#"<w:permStart w:id="1" w:edGrp="everyone"/>"#,
        r#"<w:permEnd w:id="1"/>"#,
        "perm",
    );
}

fn first_paragraph_text(bytes: &[u8]) -> String {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let root = Element::parse(archive.by_name("word/document.xml").unwrap()).unwrap();
    fn text(node: &Element) -> String {
        if node.name == "t" {
            return node.get_text().unwrap_or_default().into_owned();
        }
        node.children
            .iter()
            .filter_map(XMLNode::as_element)
            .map(text)
            .collect()
    }
    text(root.get_child("body").unwrap().get_child("p").unwrap())
}

fn remove_prefix_bookmark(keep_spelling_range: bool) {
    let (before, after) = if keep_spelling_range {
        (
            r#"<w:proofErr w:type="spellStart"/><w:bookmarkStart w:id="1" w:name="Label"/>"#,
            r#"<w:bookmarkEnd w:id="1"/><w:proofErr w:type="spellEnd"/>"#,
        )
    } else {
        (
            r#"<w:bookmarkStart w:id="1" w:name="Label"/>"#,
            r#"<w:bookmarkEnd w:id="1"/>"#,
        )
    };
    let document = Document::parse(&fixture(before, after)).unwrap();
    let view = document.read();
    let first = &view.blocks[0];
    let wire = serde_json::json!({
        "ops":[{"op":"remove_bookmark","target":first.id,"name":"Label","semantic_hash":first.guard}],
        "revision":{"author":"Range test", "date":"2026-01-01T00:00:00Z"}
    });
    let transaction = parse_transaction(&wire.to_string())
        .unwrap()
        .into_edit_transaction()
        .unwrap();
    let removed = document.apply(&transaction).unwrap();
    let output = removed.serialize(&ExportOptions::default()).unwrap();
    for bytes in [
        &output,
        &Document::parse(&output)
            .unwrap()
            .serialize(&ExportOptions::default())
            .unwrap(),
    ] {
        assert_eq!(first_paragraph_text(bytes), "i. Appendix");
        assert_eq!(range_positions(bytes, "bookmark"), Vec::<usize>::new());
        assert_eq!(
            range_positions(bytes, "proofErr"),
            if keep_spelling_range {
                vec![0, 1]
            } else {
                vec![]
            }
        );
    }
}

#[test]
fn bookmark_removal_clears_its_prefix_position_references() {
    remove_prefix_bookmark(false);
}

#[test]
fn bookmark_removal_preserves_an_enclosing_spelling_range() {
    remove_prefix_bookmark(true);
}

#[test]
fn comment_deletion_clears_its_prefix_position_references() {
    let source = fixture(
        r#"<w:commentRangeStart w:id="0"/>"#,
        r#"<w:commentRangeEnd w:id="0"/>"#,
    );
    let mut archive = zip::ZipArchive::new(Cursor::new(source)).unwrap();
    let mut parts = Vec::new();
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).unwrap();
        let mut text = String::new();
        file.read_to_string(&mut text).unwrap();
        if file.name() == "word/document.xml" {
            text = text.replace(
                "</w:p><w:p>",
                r#"<w:r><w:commentReference w:id="0"/></w:r></w:p><w:p>"#,
            );
        } else if file.name() == "[Content_Types].xml" {
            text = text.replace("</Types>", r#"<Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/></Types>"#);
        }
        parts.push((file.name().to_string(), text));
    }
    parts.push(("word/_rels/document.xml.rels".into(),r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdComments" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/></Relationships>"#.into()));
    parts.push(("word/comments.xml".into(),r#"<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:comment w:id="0" w:author="Reviewer"><w:p><w:r><w:t>Note.</w:t></w:r></w:p></w:comment></w:comments>"#.into()));
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, text) in parts {
        writer.start_file(name, FileOptions::default()).unwrap();
        writer.write_all(text.as_bytes()).unwrap();
    }
    let bytes = writer.finish().unwrap().into_inner();
    assert_eq!(range_positions(&bytes, "commentRange"), vec![0, 1]);
    let document = Document::parse(&bytes).unwrap();
    let wire =
        r#"{"ops":[{"op":"comment_delete","comment_id":"0"}],"revision":{"author":"Range test"}}"#;
    let transaction = parse_transaction(wire)
        .unwrap()
        .into_edit_transaction()
        .unwrap();
    let removed = document.apply(&transaction).unwrap();
    let output = removed.serialize(&ExportOptions::default()).unwrap();
    for bytes in [
        &output,
        &Document::parse(&output)
            .unwrap()
            .serialize(&ExportOptions::default())
            .unwrap(),
    ] {
        assert_eq!(first_paragraph_text(bytes), "i. Appendix");
        assert_eq!(range_positions(bytes, "comment"), Vec::<usize>::new());
    }
}

#[test]
fn comparison_rebinds_prefix_markers_to_the_emitted_owner() {
    let source = fixture(
        r#"<w:bookmarkStart w:id="1" w:name="Label"/>"#,
        r#"<w:bookmarkEnd w:id="1"/>"#,
    );
    let mut archive = zip::ZipArchive::new(Cursor::new(&source)).unwrap();
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).unwrap();
        let mut text = String::new();
        file.read_to_string(&mut text).unwrap();
        if file.name() == "word/document.xml" {
            text = text.replace(
                "<w:body><w:p>",
                r#"<w:body><w:p/><w:p><w:pPr><w:spacing w:after="200"/></w:pPr>"#,
            );
        }
        writer
            .start_file(file.name(), FileOptions::default())
            .unwrap();
        writer.write_all(text.as_bytes()).unwrap();
    }
    let target = writer.finish().unwrap().into_inner();
    let base = Document::parse(&source).unwrap();
    let target = Document::parse(&target).unwrap();
    let compared = stemma_diff::diff(&base, &target).unwrap();
    for resolution in [stemma::Resolution::AcceptAll, stemma::Resolution::RejectAll] {
        let output = compared
            .project(resolution)
            .unwrap()
            .serialize(&ExportOptions::default())
            .unwrap();
        assert_eq!(range_positions(&output, "bookmark"), vec![0, 1]);
    }
}
