//! MS-DOCX §2.6.2.4: an unchanged paragraph version must not identify changed
//! text. Clearing the optional version is valid; the paragraph identity stays.
use std::io::{Cursor, Read, Write};

use stemma::api::Document;
use stemma::{ExportOptions, Resolution, ResolveSelectionAction};
use xmltree::{Element, XMLNode};
use zip::write::FileOptions;

const W14: &str = "http://schemas.microsoft.com/office/word/2010/wordml";

fn fixture() -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let parts = [
        (
            "[Content_Types].xml",
            r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/settings.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml"/></Types>"#,
        ),
        (
            "_rels/.rels",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#,
        ),
        (
            "word/_rels/document.xml.rels",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings" Target="settings.xml"/></Relationships>"#,
        ),
        (
            "word/settings.xml",
            r#"<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml"><w14:docId w14:val="00112233"/></w:settings>"#,
        ),
        (
            "word/document.xml",
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" mc:Ignorable="w14"><w:body><w:p w14:paraId="00000011" w14:textId="00000021"><w:del w:id="7" w:author="Text editor"><w:r><w:delText>removed</w:delText></w:r></w:del></w:p><w:p w14:paraId="00000012" w14:textId="00000022"><w:r><w:rPr><w:b/><w:rPrChange w:id="9" w:author="Style editor"><w:rPr><w:b w:val="0"/></w:rPr></w:rPrChange></w:rPr><w:t>Stable.</w:t></w:r></w:p><w:p w14:paraId="00000013" w14:textId="00000023"><w:r><w:t xml:space="preserve">Kept </w:t></w:r><w:ins w:id="8" w:author="Text editor"><w:r><w:t>addition</w:t></w:r></w:ins></w:p><w:sectPr/></w:body></w:document>"#,
        ),
    ];
    for (name, text) in parts {
        writer.start_file(name, FileOptions::default()).unwrap();
        writer.write_all(text.as_bytes()).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn replace_fixture_xml(needle: &str, replacement: &str) -> Vec<u8> {
    replace_main_xml(&fixture(), needle, replacement)
}

fn replace_main_xml(source: &[u8], needle: &str, replacement: &str) -> Vec<u8> {
    let mut archive = zip::ZipArchive::new(Cursor::new(source)).unwrap();
    let mut output = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).unwrap();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        if entry.name() == "word/document.xml" {
            let xml = String::from_utf8(bytes).unwrap();
            assert_eq!(xml.matches(needle).count(), 1);
            bytes = xml.replace(needle, replacement).into_bytes();
        }
        output
            .start_file(entry.name(), FileOptions::default())
            .unwrap();
        output.write_all(&bytes).unwrap();
    }
    output.finish().unwrap().into_inner()
}

fn attr<'a>(element: &'a Element, local: &str) -> Option<&'a str> {
    element
        .attributes
        .iter()
        .find(|(name, _)| name.local_name == local && name.namespace.as_deref() == Some(W14))
        .map(|(_, value)| value.as_str())
}

fn versions(bytes: &[u8]) -> Vec<(String, Option<String>)> {
    let archive = stemma::docx::DocxArchive::read(bytes).unwrap();
    let root = Element::parse(archive.get("word/document.xml").unwrap()).unwrap();
    root.get_child("body")
        .unwrap()
        .children
        .iter()
        .filter_map(XMLNode::as_element)
        .filter(|element| element.name == "p")
        .map(|paragraph| {
            (
                attr(paragraph, "paraId").unwrap().to_owned(),
                attr(paragraph, "textId").map(str::to_owned),
            )
        })
        .collect()
}

fn paragraph_version(bytes: &[u8], identity: &str) -> Option<String> {
    let archive = stemma::docx::DocxArchive::read(bytes).unwrap();
    let root = Element::parse(archive.get("word/document.xml").unwrap()).unwrap();
    fn find<'a>(root: &'a Element, identity: &str, matches: &mut Vec<&'a Element>) {
        if root.name == "p" && attr(root, "paraId") == Some(identity) {
            matches.push(root);
        }
        for child in root.children.iter().filter_map(XMLNode::as_element) {
            find(child, identity, matches);
        }
    }
    let mut matches = Vec::new();
    find(&root, identity, &mut matches);
    assert_eq!(
        matches.len(),
        1,
        "the original paragraph identity survives uniquely"
    );
    attr(matches[0], "textId").map(str::to_owned)
}

fn check(document: &Document, changed: &[usize]) {
    let expected = ["00000021", "00000022", "00000023"];
    let bytes = document.serialize(&ExportOptions::default()).unwrap();
    let reopened = Document::parse(&bytes)
        .unwrap()
        .serialize(&ExportOptions::default())
        .unwrap();
    for output in [&bytes, &reopened] {
        let rows = versions(output);
        assert_eq!(rows.len(), 3);
        for (index, (identity, version)) in rows.iter().enumerate() {
            assert_eq!(identity, &format!("{:08X}", 0x11 + index));
            if changed.contains(&index) {
                assert_ne!(
                    version.as_deref(),
                    Some(expected[index]),
                    "changed text cannot retain its earlier version"
                );
            } else {
                assert_eq!(
                    version.as_deref(),
                    Some(expected[index]),
                    "unchanged text retains its version"
                );
            }
        }
    }
}

fn select(document: &Document, wire: u32, action: ResolveSelectionAction) -> Document {
    let identity = document
        .revisions()
        .iter()
        .find(|row| row.wire_id == wire)
        .unwrap()
        .revision_id;
    document
        .project(Resolution::Selective {
            ids: [identity].into_iter().collect(),
            action,
        })
        .unwrap()
}

#[test]
fn save_without_text_changes_retains_versions() {
    check(&Document::parse(&fixture()).unwrap(), &[]);
}

#[test]
fn accepting_deleted_text_invalidates_only_its_version() {
    let document = Document::parse(&fixture())
        .unwrap()
        .project(Resolution::AcceptAll)
        .unwrap();
    check(&document, &[0]);
}

#[test]
fn rejecting_inserted_text_invalidates_only_its_version() {
    let document = Document::parse(&fixture())
        .unwrap()
        .project(Resolution::RejectAll)
        .unwrap();
    check(&document, &[2]);
}

#[test]
fn selected_text_resolution_invalidates_only_changed_versions() {
    let document = Document::parse(&fixture()).unwrap();
    check(&select(&document, 7, ResolveSelectionAction::Accept), &[0]);
    check(&select(&document, 8, ResolveSelectionAction::Reject), &[2]);
}

#[test]
fn selected_formatting_resolution_retains_text_versions() {
    let document = Document::parse(&fixture()).unwrap();
    for action in [
        ResolveSelectionAction::Accept,
        ResolveSelectionAction::Reject,
    ] {
        check(&select(&document, 9, action), &[]);
    }
}

#[test]
fn hyperlink_text_resolution_invalidates_its_paragraph_version() {
    let deleted = r#"<w:del w:id="7" w:author="Text editor"><w:r><w:delText>removed</w:delText></w:r></w:del>"#;
    let bytes = replace_fixture_xml(
        deleted,
        &format!(r#"<w:hyperlink w:anchor="local">{deleted}</w:hyperlink>"#),
    );
    let document = Document::parse(&bytes).unwrap();
    check(&document.project(Resolution::AcceptAll).unwrap(), &[0]);
    check(&select(&document, 7, ResolveSelectionAction::Accept), &[0]);
}

#[test]
fn authored_text_replacement_invalidates_its_paragraph_version() {
    let bytes = replace_fixture_xml(
        r#"<w:rPrChange w:id="9" w:author="Style editor"><w:rPr><w:b w:val="0"/></w:rPr></w:rPrChange>"#,
        "",
    );
    let document = Document::parse(&bytes).unwrap();
    let view = document.read();
    let block = &view.blocks[1];
    let wire = serde_json::json!({
        "revision":{"author":"Version editor", "date":"2026-01-01T00:00:00Z"},
        "ops":[{"op":"replace", "target":block.id, "guard":block.guard,
            "content":{"type":"paragraph", "content":[{"type":"text", "text":"Changed."}]}}]
    });
    let transaction = stemma::edit_v4::parse_transaction(&wire.to_string())
        .unwrap()
        .into_edit_transaction()
        .unwrap();
    check(&document.apply(&transaction).unwrap(), &[1]);
}

#[test]
fn authored_cell_text_invalidates_only_the_cell_paragraph_version() {
    let bytes = replace_fixture_xml(
        "<w:body>",
        r#"<w:body><w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="3000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="3000" w:type="dxa"/></w:tcPr><w:p w14:paraId="00000041" w14:textId="00000051"><w:r><w:t>Cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
    );
    let document = Document::parse(&bytes).unwrap();
    let view = document.read();
    let wire = serde_json::json!({
        "revision":{"author":"Version editor"},
        "ops":[{"op":"table_op", "target":view.blocks[0].id,
            "table_op":{"kind":"set_cell_text", "row_index":0, "col_index":0, "text":"New cell"}}]
    });
    let transaction = stemma::edit_v4::parse_transaction(&wire.to_string())
        .unwrap()
        .into_edit_transaction()
        .unwrap();
    let edited = document.apply(&transaction).unwrap();
    check(&edited, &[]);
    let bytes = edited.serialize(&ExportOptions::default()).unwrap();
    assert_ne!(
        paragraph_version(&bytes, "00000041").as_deref(),
        Some("00000051")
    );
}

#[test]
fn merging_text_invalidates_the_surviving_paragraph_version() {
    let bytes = replace_fixture_xml(
        r#"<w:p w14:paraId="00000011" w14:textId="00000021"><w:del w:id="7" w:author="Text editor"><w:r><w:delText>removed</w:delText></w:r></w:del></w:p>"#,
        r#"<w:p w14:paraId="00000011" w14:textId="00000021"><w:pPr><w:rPr><w:del w:id="10" w:author="Text editor"/></w:rPr></w:pPr><w:r><w:t xml:space="preserve">First </w:t></w:r></w:p>"#,
    );
    let in_cell = replace_main_xml(
        &bytes,
        "<w:body>",
        r#"<w:body><w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="3000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="3000" w:type="dxa"/></w:tcPr>"#,
    );
    let in_cell = replace_main_xml(&in_cell, "<w:sectPr/>", "</w:tc></w:tr></w:tbl><w:sectPr/>");
    for source in [bytes, in_cell] {
        let document = Document::parse(&source).unwrap();
        for merged in [
            document.project(Resolution::AcceptAll).unwrap(),
            select(&document, 10, ResolveSelectionAction::Accept),
        ] {
            let bytes = merged.serialize(&ExportOptions::default()).unwrap();
            assert_ne!(
                paragraph_version(&bytes, "00000012").as_deref(),
                Some("00000022")
            );
            assert_eq!(
                paragraph_version(&bytes, "00000013").as_deref(),
                Some("00000023")
            );
            let reopened = Document::parse(&bytes)
                .unwrap()
                .serialize(&ExportOptions::default())
                .unwrap();
            assert_eq!(
                paragraph_version(&reopened, "00000012"),
                paragraph_version(&bytes, "00000012")
            );
        }
    }
}
