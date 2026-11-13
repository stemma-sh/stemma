//! A tracked formatting change on the run of a note mark (`w:footnoteRef`,
//! `w:annotationRef`, `w:separator`: a glyph Word renders in its run's
//! formatting) is a revision like any run's (`w:rPrChange`, ISO 29500-1
//! §17.13.5.31): it is listed, survives saving and reopening, and resolves
//! by Accept All, Reject All and its own id. So does one on a run holding
//! only a mark that renders nothing (`w:lastRenderedPageBreak`, which Word
//! writes alone in a run).
use std::io::{Cursor, Read, Write};

use stemma::api::{Document, ExportOptions};
use stemma::{Resolution, ResolveSelectionAction};

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

/// A run holding `content`, now plain, that was italic before the change.
fn run_was_italic(content: &str) -> String {
    format!(
        r#"<w:r><w:rPr><w:rPrChange w:id="30" w:author="Editor" w:date="2026-11-13T00:00:00Z"><w:rPr><w:i/></w:rPr></w:rPrChange></w:rPr>{content}</w:r>"#
    )
}

/// A document whose one footnote and one comment carry `footnote_mark`,
/// `separator` and `comment_mark` as their mark runs.
fn package(footnote_mark: &str, separator: &str, comment_mark: &str) -> Vec<u8> {
    let parts = [
        ("[Content_Types].xml", r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/footnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"/><Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/></Types>"#.to_string()),
        ("_rels/.rels", r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#.to_string()),
        ("word/_rels/document.xml.rels", r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes" Target="footnotes.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/></Relationships>"#.to_string()),
        ("word/document.xml", format!(r#"<w:document xmlns:w="{W}" xmlns:r="{REL}"><w:body><w:p><w:commentRangeStart w:id="0"/><w:r><w:t>The pier is open daily.</w:t></w:r><w:commentRangeEnd w:id="0"/><w:r><w:commentReference w:id="0"/></w:r><w:r><w:footnoteReference w:id="1"/></w:r></w:p><w:sectPr/></w:body></w:document>"#)),
        ("word/footnotes.xml", format!(r#"<w:footnotes xmlns:w="{W}"><w:footnote w:type="separator" w:id="-1"><w:p>{separator}</w:p></w:footnote><w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote><w:footnote w:id="1"><w:p>{footnote_mark}<w:r><w:t xml:space="preserve"> Weather permitting.</w:t></w:r></w:p></w:footnote></w:footnotes>"#)),
        ("word/comments.xml", format!(r#"<w:comments xmlns:w="{W}"><w:comment w:id="0" w:author="Reviewer" w:initials="R"><w:p>{comment_mark}<w:r><w:t>Check the season.</w:t></w:r></w:p></w:comment></w:comments>"#)),
    ];
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, contents) in parts {
        writer
            .start_file(name, zip::write::FileOptions::default())
            .unwrap();
        writer.write_all(contents.as_bytes()).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

const FOOTNOTE_MARK: &str = r#"<w:r><w:footnoteRef/></w:r>"#;
const SEPARATOR: &str = r#"<w:r><w:separator/></w:r>"#;
const COMMENT_MARK: &str = r#"<w:r><w:annotationRef/></w:r>"#;

fn part(bytes: &[u8], name: &str) -> String {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut xml = String::new();
    archive
        .by_name(name)
        .unwrap()
        .read_to_string(&mut xml)
        .unwrap();
    xml
}

fn saved(document: &Document) -> Vec<u8> {
    document
        .serialize(&ExportOptions::default())
        .expect("serialize")
}

/// The `w:rPr` children's names of each run in `xml` holding `marker`.
fn mark_rprs(xml: &str, marker: &str) -> Vec<String> {
    let root = xmltree::Element::parse(xml.as_bytes()).unwrap();
    fn walk(node: &xmltree::Element, marker: &str, out: &mut Vec<String>) {
        let elements = || {
            node.children
                .iter()
                .filter_map(xmltree::XMLNode::as_element)
        };
        if node.name == "r" && elements().any(|c| c.name == marker) {
            out.push(node.get_child("rPr").map_or(String::new(), |rpr| {
                rpr.children
                    .iter()
                    .filter_map(xmltree::XMLNode::as_element)
                    .map(|c| c.name.clone())
                    .collect::<Vec<_>>()
                    .join(",")
            }));
        }
        for child in elements() {
            walk(child, marker, out);
        }
    }
    let mut out = Vec::new();
    walk(&root, marker, &mut out);
    out
}

/// The note mark `marker` in story part `part_name`, its run carrying the
/// change: one revision, through saving, reopening and every resolution.
fn assert_mark_change_round_trips(bytes: &[u8], part_name: &str, marker: &str) {
    let read = Document::parse(bytes).expect("parse");
    let as_saved = saved(&read);
    assert_eq!(
        mark_rprs(&part(&as_saved, part_name), marker),
        vec!["rPrChange"],
        "{marker}: written back"
    );
    let reopened = Document::parse(&as_saved).expect("reopen");
    for (form, document) in [("as read", read), ("saved and reopened", reopened)] {
        let revisions = document.revisions();
        assert_eq!(revisions.len(), 1, "{marker} {form}: {revisions:?}");
        let id = revisions[0].revision_id;
        for (what, resolution, want) in [
            ("Accept All", Resolution::AcceptAll, ""),
            ("Reject All", Resolution::RejectAll, "i"),
            (
                "accept by id",
                Resolution::Selective {
                    ids: [id].into_iter().collect(),
                    action: ResolveSelectionAction::Accept,
                },
                "",
            ),
            (
                "reject by id",
                Resolution::Selective {
                    ids: [id].into_iter().collect(),
                    action: ResolveSelectionAction::Reject,
                },
                "i",
            ),
        ] {
            let resolved = document.project(resolution).expect("resolve");
            let xml = part(&saved(&resolved), part_name);
            assert_eq!(
                mark_rprs(&xml, marker),
                vec![want],
                "{marker} {form}, {what}: {xml}"
            );
            assert!(resolved.revisions().is_empty(), "{marker} {form}, {what}");
        }
    }
}

#[test]
fn a_footnote_mark_run_formatting_change_round_trips() {
    let bytes = package(&run_was_italic("<w:footnoteRef/>"), SEPARATOR, COMMENT_MARK);
    assert_mark_change_round_trips(&bytes, "word/footnotes.xml", "footnoteRef");
}

#[test]
fn a_separator_run_formatting_change_round_trips() {
    let bytes = package(
        FOOTNOTE_MARK,
        &run_was_italic("<w:separator/>"),
        COMMENT_MARK,
    );
    assert_mark_change_round_trips(&bytes, "word/footnotes.xml", "separator");
}

#[test]
fn a_comment_mark_run_formatting_change_round_trips() {
    let bytes = package(
        FOOTNOTE_MARK,
        SEPARATOR,
        &run_was_italic("<w:annotationRef/>"),
    );
    assert_mark_change_round_trips(&bytes, "word/comments.xml", "annotationRef");
}

/// A run holding only `marker`, a mark that renders nothing, whose
/// formatting changed: the change is carried on the mark like a note
/// mark's.
fn assert_lone_mark_change_round_trips(marker: &str) {
    let body = format!(
        r#"<w:p>{}<w:r><w:t>The pier is open daily.</w:t></w:r></w:p>"#,
        run_was_italic(&format!("<w:{marker}/>"))
    );
    let bytes = package(FOOTNOTE_MARK, SEPARATOR, COMMENT_MARK);
    let document =
        part(&bytes, "word/document.xml").replace("<w:body>", &format!("<w:body>{body}"));
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let mut archive = zip::ZipArchive::new(Cursor::new(&bytes)).unwrap();
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).unwrap();
        let name = file.name().to_string();
        let mut contents = String::new();
        file.read_to_string(&mut contents).unwrap();
        if name == "word/document.xml" {
            contents = document.clone();
        }
        writer
            .start_file(name, zip::write::FileOptions::default())
            .unwrap();
        writer.write_all(contents.as_bytes()).unwrap();
    }
    let bytes = writer.finish().unwrap().into_inner();
    assert_mark_change_round_trips(&bytes, "word/document.xml", marker);
}

/// Word writes `w:lastRenderedPageBreak` alone in a run.
#[test]
fn a_change_on_a_run_holding_only_a_page_break_mark_round_trips() {
    assert_lone_mark_change_round_trips("lastRenderedPageBreak");
}

#[test]
fn a_change_on_a_run_holding_only_a_soft_hyphen_round_trips() {
    assert_lone_mark_change_round_trips("softHyphen");
}

/// A soft hyphen alone in its run (§17.3.3.29) renders, when the line breaks
/// there, as a hyphen in that run's formatting: the run's formatting is the
/// hyphen's and survives an edit of its paragraph, saving and reopening.
#[test]
fn a_run_holding_only_a_soft_hyphen_keeps_its_formatting() {
    let body = r#"<w:p><w:r><w:t>Hyphen</w:t></w:r><w:r><w:rPr><w:b/><w:color w:val="C00000"/></w:rPr><w:softHyphen/></w:r><w:r><w:t>ation is allowed.</w:t></w:r></w:p>"#;
    let bytes = package(FOOTNOTE_MARK, SEPARATOR, COMMENT_MARK);
    let document_xml =
        part(&bytes, "word/document.xml").replace("<w:body>", &format!("<w:body>{body}"));
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let mut archive = zip::ZipArchive::new(Cursor::new(&bytes)).unwrap();
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).unwrap();
        let name = file.name().to_string();
        let mut contents = String::new();
        file.read_to_string(&mut contents).unwrap();
        if name == "word/document.xml" {
            contents = document_xml.clone();
        }
        writer
            .start_file(name, zip::write::FileOptions::default())
            .unwrap();
        writer.write_all(contents.as_bytes()).unwrap();
    }
    let bytes = writer.finish().unwrap().into_inner();

    let read = Document::parse(&bytes).expect("parse");
    let as_saved = saved(&read);
    assert_eq!(
        mark_rprs(&part(&as_saved, "word/document.xml"), "softHyphen"),
        vec!["b,color"]
    );
    let target = read.read().blocks[0].id.to_string();
    let json = serde_json::json!({"ops":[{"op":"replace","target":target,"expect":"allowed",
        "content":{"type":"paragraph","content":[{"type":"text","text":"Hyphenation is permitted."}]}}],
        "revision":{"author":"Editor"}})
    .to_string();
    let edited = read
        .apply(
            &stemma::edit_v4::parse_transaction(&json)
                .unwrap()
                .into_edit_transaction()
                .unwrap(),
        )
        .expect("edit");
    let reopened = Document::parse(&saved(&edited)).expect("reopen");
    let accepted = edited.project(Resolution::AcceptAll).expect("accept");
    for document in [&edited, &reopened, &accepted] {
        assert_eq!(
            mark_rprs(&part(&saved(document), "word/document.xml"), "softHyphen"),
            vec!["b,color"]
        );
    }
}
