//! A tracked formatting change on a field part's run (`w:rPrChange` in the
//! `w:rPr` of a `w:fldChar` or `w:instrText` run, ISO 29500-1 §17.13.5.31)
//! is a revision like any run's: Word lists it, Accept All keeps the run's
//! formatting and Reject All restores the formatting it records. The same
//! holds for any run whose content imports as an object, such as a
//! footnote reference.
use std::io::{Cursor, Read, Write};

use stemma::api::{Document, ExportOptions};
use stemma::{Resolution, ResolveSelectionAction};
use zip::write::FileOptions;

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn package(body: &str, footnotes: bool) -> Vec<u8> {
    let mut overrides = r#"<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>"#.to_string();
    let mut rels = String::new();
    if footnotes {
        overrides.push_str(r#"<Override PartName="/word/footnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"/>"#);
        rels.push_str(r#"<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes" Target="footnotes.xml"/>"#);
    }
    let mut parts = vec![
        ("[Content_Types].xml".to_string(), format!(r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/>{overrides}</Types>"#)),
        ("_rels/.rels".to_string(), r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#.to_string()),
        ("word/_rels/document.xml.rels".to_string(), format!(r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{rels}</Relationships>"#)),
        ("word/document.xml".to_string(), format!(r#"<w:document xmlns:w="{W}"><w:body>{body}<w:sectPr/></w:body></w:document>"#)),
    ];
    if footnotes {
        parts.push(("word/footnotes.xml".to_string(), format!(r#"<w:footnotes xmlns:w="{W}"><w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote><w:footnote w:id="1"><w:p><w:r><w:footnoteRef/></w:r><w:r><w:t xml:space="preserve"> Open until dusk.</w:t></w:r></w:p></w:footnote></w:footnotes>"#)));
    }
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, contents) in parts {
        writer.start_file(name, FileOptions::default()).unwrap();
        writer.write_all(contents.as_bytes()).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

/// A run carrying `content`, now plain, that was italic before the change.
fn run_was_italic(id: u32, content: &str) -> String {
    format!(
        r#"<w:r><w:rPr><w:rPrChange w:id="{id}" w:author="Editor" w:date="2026-11-12T00:00:00Z"><w:rPr><w:i/></w:rPr></w:rPrChange></w:rPr>{content}</w:r>"#
    )
}

/// "Chapter: <STYLEREF field> is open daily.", each of the field's four
/// part runs carrying a formatting change from italic to plain.
fn field_body() -> String {
    format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Chapter: </w:t></w:r>{}{}{}<w:r><w:t>Harbour Walks</w:t></w:r>{}<w:r><w:t xml:space="preserve"> is open daily.</w:t></w:r></w:p>"#,
        run_was_italic(10, r#"<w:fldChar w:fldCharType="begin"/>"#),
        run_was_italic(
            11,
            r#"<w:instrText xml:space="preserve"> STYLEREF  "heading 1" </w:instrText>"#
        ),
        run_was_italic(12, r#"<w:fldChar w:fldCharType="separate"/>"#),
        run_was_italic(13, r#"<w:fldChar w:fldCharType="end"/>"#),
    )
}

fn main_xml(bytes: &[u8]) -> String {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut xml = String::new();
    archive
        .by_name("word/document.xml")
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

/// The `w:rPr` children of each run holding `marker` (a part's element
/// name), in order, as written.
fn part_rprs(xml: &str, marker: &str) -> Vec<String> {
    let root = xmltree::Element::parse(xml.as_bytes()).unwrap();
    let mut out = Vec::new();
    fn walk(node: &xmltree::Element, marker: &str, out: &mut Vec<String>) {
        if node.name == "r"
            && node
                .children
                .iter()
                .filter_map(xmltree::XMLNode::as_element)
                .any(|child| child.name == marker)
        {
            let rpr = node.get_child("rPr").map_or(String::new(), |rpr| {
                rpr.children
                    .iter()
                    .filter_map(xmltree::XMLNode::as_element)
                    .map(|child| child.name.clone())
                    .collect::<Vec<_>>()
                    .join(",")
            });
            out.push(rpr);
        }
        for child in node
            .children
            .iter()
            .filter_map(xmltree::XMLNode::as_element)
        {
            walk(child, marker, out);
        }
    }
    walk(&root, marker, &mut out);
    out
}

#[test]
fn a_field_part_run_formatting_change_is_a_revision() {
    let document = Document::parse(&package(&field_body(), false)).expect("parse");
    let revisions = document.revisions();
    assert_eq!(revisions.len(), 4, "one per field part run: {revisions:?}");

    // It survives saving and reopening.
    let reopened = Document::parse(&saved(&document)).expect("reopen");
    assert_eq!(reopened.revisions().len(), 4);
    let xml = main_xml(&saved(&reopened));
    assert_eq!(part_rprs(&xml, "fldChar"), vec!["rPrChange"; 3], "{xml}");
    assert_eq!(part_rprs(&xml, "instrText"), vec!["rPrChange"], "{xml}");

    // Accept All keeps the runs as they are; Reject All restores italic.
    let accepted = main_xml(&saved(&document.project(Resolution::AcceptAll).unwrap()));
    assert_eq!(part_rprs(&accepted, "fldChar"), vec![""; 3], "{accepted}");
    assert_eq!(part_rprs(&accepted, "instrText"), vec![""], "{accepted}");
    let rejected = main_xml(&saved(&document.project(Resolution::RejectAll).unwrap()));
    assert_eq!(part_rprs(&rejected, "fldChar"), vec!["i"; 3], "{rejected}");
    assert_eq!(part_rprs(&rejected, "instrText"), vec!["i"], "{rejected}");
}

#[test]
fn a_field_part_run_formatting_change_resolves_on_its_own() {
    let document = Document::parse(&package(&field_body(), false)).expect("parse");
    let code = document
        .revisions()
        .iter()
        .find(|revision| revision.excerpt.contains("field code"))
        .map(|revision| revision.revision_id)
        .expect("the field code run's revision");
    let rejected = document
        .project(Resolution::Selective {
            ids: [code].into_iter().collect(),
            action: ResolveSelectionAction::Reject,
        })
        .expect("reject the field code's formatting");
    assert_eq!(rejected.revisions().len(), 3);
    let xml = main_xml(&saved(&rejected));
    assert_eq!(part_rprs(&xml, "instrText"), vec!["i"], "{xml}");
    assert_eq!(part_rprs(&xml, "fldChar"), vec!["rPrChange"; 3], "{xml}");
}

/// One run holding an object (`content`, its element `marker`), plain now
/// and italic before: one revision that enumerates, survives saving and
/// reopening, and resolves by Accept All, Reject All and its own id.
fn assert_object_run_change_round_trips(content: &str, marker: &str, footnotes: bool) {
    assert_object_run_change_round_trips_in("", content, "", marker, footnotes);
}

/// [`assert_object_run_change_round_trips`] with the changed run between
/// the plain runs `before` and `after`.
fn assert_object_run_change_round_trips_in(
    before: &str,
    content: &str,
    after: &str,
    marker: &str,
    footnotes: bool,
) {
    let body = format!(
        r#"<w:p><w:r><w:t xml:space="preserve">The pier is open daily. </w:t></w:r>{before}{}{after}</w:p>"#,
        run_was_italic(20, content)
    );
    let bytes = package(&body, footnotes);
    let read = Document::parse(&bytes).expect("parse");
    let reopened = Document::parse(&saved(&read)).expect("reopen");
    for (form, document) in [("as read", read), ("saved and reopened", reopened)] {
        let revisions = document.revisions();
        assert_eq!(revisions.len(), 1, "{marker} {form}: {revisions:?}");
        let id = revisions[0].revision_id;
        let rprs = |resolution: Resolution| {
            let resolved = document.project(resolution).expect("resolve");
            let xml = main_xml(&saved(&resolved));
            (part_rprs(&xml, marker), resolved.revisions().len(), xml)
        };
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
            let (got, left, xml) = rprs(resolution);
            assert_eq!(got, vec![want], "{marker} {form}, {what}: {xml}");
            assert_eq!(left, 0, "{marker} {form}, {what}: revisions left");
        }
    }
}

#[test]
fn a_footnote_reference_run_formatting_change_round_trips() {
    assert_object_run_change_round_trips(
        r#"<w:footnoteReference w:id="1"/>"#,
        "footnoteReference",
        true,
    );
}

#[test]
fn a_symbol_run_formatting_change_round_trips() {
    assert_object_run_change_round_trips(
        r#"<w:sym w:font="Wingdings" w:char="F0E0"/>"#,
        "sym",
        false,
    );
}

#[test]
fn a_drawing_run_formatting_change_round_trips() {
    assert_object_run_change_round_trips(
        r#"<w:drawing><wp:inline xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" distT="0" distB="0" distL="0" distR="0"><wp:extent cx="457200" cy="457200"/><wp:docPr id="7" name="Pier marker"/><a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"/></a:graphic></wp:inline></w:drawing>"#,
        "drawing",
        false,
    );
}

#[test]
fn a_field_code_run_formatting_change_round_trips() {
    assert_object_run_change_round_trips_in(
        r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r>"#,
        r#"<w:instrText xml:space="preserve"> PAGE </w:instrText>"#,
        r#"<w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>3</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r>"#,
        "instrText",
        false,
    );
}
