//! Paragraph boundaries own deletion history; moved runs retain their source
//! session even when the surviving paragraph has another rsidRDefault.
use std::io::{Cursor, Read, Write};

use stemma::api::{Document, ExportOptions};
use stemma::{Resolution, ResolveSelectionAction};
use xmltree::{Element, XMLNode};
use zip::write::FileOptions;

const LEFT: &str = r#"<w:r><w:t xml:space="preserve">left </w:t></w:r>"#;
const RIGHT: &str = r#"<w:r><w:t>right</w:t></w:r>"#;
const DELETED_MARK: &str = r#"<w:del w:id="11" w:author="Later" w:date="2026-02-03T04:05:00Z"/>"#;
const INSERTED_MARK: &str = r#"<w:ins w:id="11" w:author="Later" w:date="2026-02-03T04:05:00Z"/>"#;

fn body(mark: &str, left: &str, right: &str) -> String {
    format!(
        r#"<w:p w:rsidR="00112233" w:rsidRDefault="00112233" w:rsidDel="00445566"><w:pPr><w:rPr>{mark}</w:rPr></w:pPr>{left}</w:p><w:p w:rsidR="00778899" w:rsidRDefault="00778899">{right}</w:p>"#
    )
}

fn fixture(body: &str) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let parts = [
        ("[Content_Types].xml", r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/settings.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml"/></Types>"#.to_string()),
        ("_rels/.rels", r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#.to_string()),
        ("word/_rels/document.xml.rels", r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings" Target="settings.xml"/></Relationships>"#.to_string()),
        ("word/settings.xml", r#"<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:rsids><w:rsidRoot w:val="00112233"/><w:rsid w:val="00112233"/><w:rsid w:val="00445566"/><w:rsid w:val="00778899"/></w:rsids></w:settings>"#.to_string()),
        ("word/document.xml", format!(r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#)),
    ];
    for (name, contents) in parts {
        writer.start_file(name, FileOptions::default()).unwrap();
        writer.write_all(contents.as_bytes()).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn main_xml(bytes: &[u8]) -> Element {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut xml = Vec::new();
    archive
        .by_name("word/document.xml")
        .unwrap()
        .read_to_end(&mut xml)
        .unwrap();
    Element::parse(xml.as_slice()).unwrap()
}

fn paragraphs(root: &Element) -> Vec<&Element> {
    root.get_child("body")
        .unwrap()
        .children
        .iter()
        .filter_map(XMLNode::as_element)
        .filter(|node| node.name == "p")
        .collect()
}

fn descendants<'a>(node: &'a Element, name: &str, out: &mut Vec<&'a Element>) {
    if node.name == name {
        out.push(node);
    }
    for child in node.children.iter().filter_map(XMLNode::as_element) {
        descendants(child, name, out);
    }
}

fn attr<'a>(node: &'a Element, local: &str) -> Option<&'a str> {
    let matches: Vec<_> = node
        .attributes
        .iter()
        .filter(|(name, _)| {
            name.local_name == local
                && name.namespace.as_deref()
                    == Some("http://schemas.openxmlformats.org/wordprocessingml/2006/main")
        })
        .collect();
    assert!(matches.len() <= 1, "unique expanded attribute name");
    matches.first().map(|(_, value)| value.as_str())
}

fn projected_roots(bytes: &[u8], resolution: Resolution) -> Vec<Element> {
    let projected = Document::parse(bytes).unwrap().project(resolution).unwrap();
    let output = projected.serialize(&ExportOptions::default()).unwrap();
    let reopened = Document::parse(&output).unwrap();
    let resaved = reopened.serialize(&ExportOptions::default()).unwrap();
    vec![main_xml(&output), main_xml(&resaved)]
}

fn selective(bytes: &[u8], wire: u32, action: ResolveSelectionAction) -> Resolution {
    let document = Document::parse(bytes).unwrap();
    let identity = document
        .revisions()
        .iter()
        .find(|row| row.wire_id == wire)
        .unwrap()
        .revision_id;
    Resolution::Selective {
        ids: [identity].into_iter().collect(),
        action,
    }
}

/// ECMA-376 17.3.1: an omitted run rsidR inherits its paragraph's
/// rsidRDefault. Compare effective provenance, not one equivalent wire form.
fn text_sessions(root: &Element) -> Vec<(char, String)> {
    let mut result = Vec::new();
    for paragraph in paragraphs(root) {
        let inherited = attr(paragraph, "rsidRDefault").unwrap();
        let mut runs = Vec::new();
        descendants(paragraph, "r", &mut runs);
        for run in runs {
            let session = attr(run, "rsidR").unwrap_or(inherited);
            let text = run.get_child("t").unwrap().get_text().unwrap();
            result.extend(text.chars().map(|ch| (ch, session.to_string())));
        }
    }
    result
}

fn expected_sessions(left: &str) -> Vec<(char, String)> {
    "left "
        .chars()
        .map(|ch| (ch, left.to_string()))
        .chain("right".chars().map(|ch| (ch, "00778899".to_string())))
        .collect()
}

#[test]
fn full_reject_clears_the_rejected_paragraph_deletion_session() {
    let bytes = fixture(&body(DELETED_MARK, LEFT, RIGHT));
    for root in projected_roots(&bytes, Resolution::RejectAll) {
        assert!(attr(paragraphs(&root)[0], "rsidDel").is_none());
    }
}

#[test]
fn selective_reject_clears_the_rejected_paragraph_deletion_session() {
    let bytes = fixture(&body(DELETED_MARK, LEFT, RIGHT));
    let resolution = selective(&bytes, 11, ResolveSelectionAction::Reject);
    for root in projected_roots(&bytes, resolution) {
        assert!(attr(paragraphs(&root)[0], "rsidDel").is_none());
    }
}

#[test]
fn rejecting_nested_mark_deletion_clears_its_session_but_keeps_insertion() {
    let mark = format!(
        r#"<w:ins w:id="10" w:author="Origin" w:date="2026-02-02T04:05:00Z"/>{DELETED_MARK}"#
    );
    let left = format!(
        r#"<w:ins w:id="10" w:author="Origin" w:date="2026-02-02T04:05:00Z">{LEFT}</w:ins>"#
    );
    let bytes = fixture(&body(&mark, &left, RIGHT));
    for root in projected_roots(
        &bytes,
        selective(&bytes, 11, ResolveSelectionAction::Reject),
    ) {
        assert!(attr(paragraphs(&root)[0], "rsidDel").is_none());
        let mut insertion = Vec::new();
        descendants(paragraphs(&root)[0], "ins", &mut insertion);
        assert!(
            !insertion.is_empty(),
            "the independent insertion remains pending"
        );
        assert!(
            insertion
                .iter()
                .all(|node| attr(node, "author").unwrap() == "Origin")
        );
    }
}

#[test]
fn rejecting_another_revision_keeps_pending_paragraph_deletion_history() {
    let right = format!(
        r#"<w:ins w:id="21" w:author="Other" w:date="2026-02-04T04:05:00Z">{RIGHT}</w:ins>"#
    );
    let bytes = fixture(&body(DELETED_MARK, LEFT, &right));
    for root in projected_roots(
        &bytes,
        selective(&bytes, 21, ResolveSelectionAction::Reject),
    ) {
        assert_eq!(attr(paragraphs(&root)[0], "rsidDel").unwrap(), "00445566");
    }
}

#[test]
fn full_reject_keeps_history_when_there_is_no_paragraph_deletion() {
    let bytes = fixture(&body("", LEFT, RIGHT));
    for root in projected_roots(&bytes, Resolution::RejectAll) {
        assert_eq!(attr(paragraphs(&root)[0], "rsidDel").unwrap(), "00445566");
    }
}

#[test]
fn full_accept_mark_merge_preserves_both_implicit_run_sessions() {
    let bytes = fixture(&body(DELETED_MARK, LEFT, RIGHT));
    for root in projected_roots(&bytes, Resolution::AcceptAll) {
        assert_eq!(text_sessions(&root), expected_sessions("00112233"));
    }
}

#[test]
fn selective_accept_mark_merge_preserves_both_implicit_run_sessions() {
    let bytes = fixture(&body(DELETED_MARK, LEFT, RIGHT));
    for root in projected_roots(
        &bytes,
        selective(&bytes, 11, ResolveSelectionAction::Accept),
    ) {
        assert_eq!(text_sessions(&root), expected_sessions("00112233"));
    }
}

#[test]
fn rejecting_inserted_boundary_preserves_both_implicit_run_sessions() {
    let bytes = fixture(&body(INSERTED_MARK, LEFT, RIGHT));
    for root in projected_roots(
        &bytes,
        selective(&bytes, 11, ResolveSelectionAction::Reject),
    ) {
        assert_eq!(text_sessions(&root), expected_sessions("00112233"));
    }
}

#[test]
fn mark_merge_preserves_an_explicit_run_session() {
    let left = r#"<w:r w:rsidR="00445566"><w:t xml:space="preserve">left </w:t></w:r>"#;
    let bytes = fixture(&body(DELETED_MARK, left, RIGHT));
    for root in projected_roots(&bytes, Resolution::AcceptAll) {
        assert_eq!(text_sessions(&root), expected_sessions("00445566"));
    }
}

#[test]
fn equal_paragraph_defaults_do_not_materialize_unneeded_run_sessions() {
    let bytes = fixture(&body(DELETED_MARK, LEFT, RIGHT).replace("00778899", "00112233"));
    for root in projected_roots(&bytes, Resolution::AcceptAll) {
        let mut runs = Vec::new();
        descendants(&root, "r", &mut runs);
        assert!(runs.iter().all(|run| attr(run, "rsidR").is_none()));
    }
}

#[test]
fn mark_merge_preserves_inherited_hyperlink_run_session() {
    let left = r#"<w:hyperlink w:anchor="destination"><w:r><w:t xml:space="preserve">left </w:t></w:r></w:hyperlink>"#;
    let right = format!(
        r#"<w:bookmarkStart w:id="4" w:name="destination"/>{RIGHT}<w:bookmarkEnd w:id="4"/>"#
    );
    let bytes = fixture(&body(DELETED_MARK, left, &right));
    for root in projected_roots(&bytes, Resolution::AcceptAll) {
        assert_eq!(text_sessions(&root), expected_sessions("00112233"));
    }
}

#[test]
fn mark_merge_preserves_inherited_simple_field_run_session() {
    let left = r#"<w:fldSimple w:instr=" DATE "><w:r><w:t xml:space="preserve">left </w:t></w:r></w:fldSimple>"#;
    let bytes = fixture(&body(DELETED_MARK, left, RIGHT));
    for root in projected_roots(&bytes, Resolution::AcceptAll) {
        assert_eq!(text_sessions(&root), expected_sessions("00112233"));
    }
}

#[test]
fn mark_merge_preserves_inherited_break_session() {
    let right = format!(r#"<w:r><w:br/></w:r>{RIGHT}"#);
    let bytes = fixture(&body(DELETED_MARK, LEFT, &right));
    for root in projected_roots(&bytes, Resolution::AcceptAll) {
        let paragraph = paragraphs(&root)[0];
        let default = attr(paragraph, "rsidRDefault").unwrap();
        let mut runs = Vec::new();
        descendants(paragraph, "r", &mut runs);
        let run = runs
            .into_iter()
            .find(|run| run.get_child("br").is_some())
            .unwrap();
        assert_eq!(attr(run, "rsidR").unwrap_or(default), "00778899");
    }
}

#[test]
fn mark_merge_preserves_inherited_empty_run_session() {
    let right = format!(r#"<w:r><w:rPr><w:lang w:val="en-US"/></w:rPr></w:r>{RIGHT}"#);
    let bytes = fixture(&body(DELETED_MARK, LEFT, &right));
    for root in projected_roots(&bytes, Resolution::AcceptAll) {
        let paragraph = paragraphs(&root)[0];
        let default = attr(paragraph, "rsidRDefault").unwrap();
        let mut runs = Vec::new();
        descendants(paragraph, "r", &mut runs);
        let run = runs
            .into_iter()
            .find(|run| run.get_child("t").is_none())
            .unwrap();
        assert_eq!(attr(run, "rsidR").unwrap_or(default), "00778899");
    }
}

#[test]
fn mark_merge_preserves_the_following_hyperlink_session() {
    let left = format!(
        r#"<w:bookmarkStart w:id="4" w:name="destination"/>{LEFT}<w:bookmarkEnd w:id="4"/>"#
    );
    let right = r#"<w:hyperlink w:anchor="destination"><w:r><w:t>right</w:t></w:r></w:hyperlink>"#;
    let bytes = fixture(&body(DELETED_MARK, &left, right));
    for root in projected_roots(&bytes, Resolution::AcceptAll) {
        assert_eq!(text_sessions(&root), expected_sessions("00112233"));
        let mut bookmarks = Vec::new();
        descendants(&root, "bookmarkStart", &mut bookmarks);
        assert!(
            bookmarks.iter().all(|node| attr(node, "rsidR").is_none()),
            "paragraph-level markers are not run carriers"
        );
    }
}

#[test]
fn mark_merge_preserves_the_following_field_session() {
    let right = r#"<w:fldSimple w:instr=" DATE "><w:r><w:t>right</w:t></w:r></w:fldSimple>"#;
    let bytes = fixture(&body(DELETED_MARK, LEFT, right));
    for root in projected_roots(&bytes, Resolution::AcceptAll) {
        assert_eq!(text_sessions(&root), expected_sessions("00112233"));
    }
}

#[test]
fn mark_merge_preserves_the_following_literal_prefix_session() {
    let right = r#"<w:r><w:t>1. right</w:t></w:r>"#;
    let bytes = fixture(&body(DELETED_MARK, LEFT, right));
    let expected: Vec<_> = "left "
        .chars()
        .map(|ch| (ch, "00112233".to_string()))
        .chain("1. right".chars().map(|ch| (ch, "00778899".to_string())))
        .collect();
    for root in projected_roots(&bytes, Resolution::AcceptAll) {
        assert_eq!(text_sessions(&root), expected);
    }
}

#[test]
fn moving_a_textbox_preserves_its_own_paragraph_session_context() {
    let right = format!(
        r#"<w:r><w:pict xmlns:v="urn:schemas-microsoft-com:vml"><v:shape id="box" style="width:30pt;height:15pt"><v:textbox><w:txbxContent><w:p w:rsidRDefault="00445566"><w:r><w:t>inside</w:t></w:r></w:p></w:txbxContent></v:textbox></v:shape></w:pict></w:r>{RIGHT}"#
    );
    let bytes = fixture(&body(DELETED_MARK, LEFT, &right));
    for root in projected_roots(&bytes, Resolution::AcceptAll) {
        let main = paragraphs(&root)[0];
        let mut runs = Vec::new();
        descendants(main, "r", &mut runs);
        let outer = runs
            .iter()
            .find(|run| run.get_child("pict").is_some())
            .unwrap();
        assert_eq!(attr(outer, "rsidR"), Some("00778899"));
        let mut all_paragraphs = Vec::new();
        descendants(main, "p", &mut all_paragraphs);
        let nested = all_paragraphs
            .into_iter()
            .find(|paragraph| attr(paragraph, "rsidRDefault") == Some("00445566"))
            .unwrap();
        let inner = nested.get_child("r").unwrap();
        assert!(
            attr(inner, "rsidR").is_none(),
            "the textbox's inherited session did not change"
        );
        assert_eq!(inner.get_child("t").unwrap().get_text().unwrap(), "inside");
    }
}

#[test]
fn mark_merge_preserves_a_synthesized_field_session() {
    use std::sync::Arc;
    use stemma::domain::{BlockNode, InlineNode, OpaqueKind};
    let right = r#"<w:fldSimple w:instr=" DATE "><w:r><w:t>right</w:t></w:r></w:fldSimple>"#;
    let bytes = fixture(&body(DELETED_MARK, LEFT, right));
    let document = Document::parse(&bytes).unwrap();
    let mut snapshot = document.snapshot().clone();
    let canonical = Arc::make_mut(&mut snapshot.canonical);
    let BlockNode::Paragraph(paragraph) = &mut canonical.blocks[1].block else {
        panic!("following paragraph")
    };
    let field = paragraph
        .segments
        .iter_mut()
        .flat_map(|segment| &mut segment.inlines)
        .find_map(|inline| match inline {
            InlineNode::OpaqueInline(opaque) if matches!(opaque.kind, OpaqueKind::Field(_)) => {
                Some(opaque)
            }
            _ => None,
        })
        .unwrap();
    // Exercise the supported typed field builder, not imported raw XML.
    field.raw_xml = None;
    field.content_hash = None;
    let authored = document.with_derived_snapshot(snapshot);
    let before = authored.serialize(&ExportOptions::default()).unwrap();
    assert_eq!(
        text_sessions(&main_xml(&before)),
        expected_sessions("00112233")
    );
    let after = authored
        .project(Resolution::AcceptAll)
        .unwrap()
        .serialize(&ExportOptions::default())
        .unwrap();
    assert_eq!(
        text_sessions(&main_xml(&after)),
        expected_sessions("00112233")
    );
}

#[test]
fn mark_merge_preserves_a_synthesized_hyperlink_session() {
    use std::sync::Arc;
    use stemma::domain::{BlockNode, InlineNode, OpaqueKind};
    let left = format!(
        r#"<w:bookmarkStart w:id="4" w:name="destination"/>{LEFT}<w:bookmarkEnd w:id="4"/>"#
    );
    let right = r#"<w:hyperlink w:anchor="destination"><w:r><w:t>right</w:t></w:r></w:hyperlink>"#;
    let bytes = fixture(&body(DELETED_MARK, &left, right));
    let document = Document::parse(&bytes).unwrap();
    let mut snapshot = document.snapshot().clone();
    let canonical = Arc::make_mut(&mut snapshot.canonical);
    let BlockNode::Paragraph(paragraph) = &mut canonical.blocks[1].block else {
        panic!("following paragraph")
    };
    let hyperlink = paragraph
        .segments
        .iter_mut()
        .flat_map(|segment| &mut segment.inlines)
        .find_map(|inline| match inline {
            InlineNode::OpaqueInline(opaque) if matches!(opaque.kind, OpaqueKind::Hyperlink(_)) => {
                Some(opaque)
            }
            _ => None,
        })
        .unwrap();
    hyperlink.raw_xml = None;
    hyperlink.content_hash = None;
    let OpaqueKind::Hyperlink(data) = &mut hyperlink.kind else {
        panic!("hyperlink")
    };
    data.runs.clear();
    let authored = document.with_derived_snapshot(snapshot);
    let before = authored.serialize(&ExportOptions::default()).unwrap();
    assert_eq!(
        text_sessions(&main_xml(&before)),
        expected_sessions("00112233")
    );
    let after = authored
        .project(Resolution::AcceptAll)
        .unwrap()
        .serialize(&ExportOptions::default())
        .unwrap();
    assert_eq!(
        text_sessions(&main_xml(&after)),
        expected_sessions("00112233")
    );
}
