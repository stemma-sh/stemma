//! A textbox inside a header or footer is reachable by the textbox ops.
//!
//! Domain rule: a running label such as a tab divider's "TAB 2" is often a
//! textbox anchored in a header part (§17.10.4 `w:hdr` holds the same
//! paragraph content as the body, drawings included). Header stories reuse the
//! body's block ids, so an op names the story by part name (`header_part` /
//! `footer_part`), as `edit_header` does. `opaque_text_edit` then edits the
//! textbox's text, tracked or direct, and `set_image_attrs` resizes it — in
//! that header only.

use stemma::api::Document;
use stemma::domain::StoryScope;
use stemma::opaque_targets::opaque_text_targets;
use stemma::{ExportOptions, Resolution};

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const DRAWING_NS: &str = r#"xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape""#;

/// A header part whose only paragraph anchors a textbox reading `label`.
fn header_xml(label: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:hdr xmlns:w="{W}" {DRAWING_NS}><w:p><w:r><w:drawing><wp:anchor behindDoc="0" distT="0" distB="0" distL="0" distR="0" simplePos="0" locked="0" layoutInCell="1" allowOverlap="1" relativeHeight="1"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="margin"><wp:posOffset>0</wp:posOffset></wp:positionH><wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV><wp:extent cx="914400" cy="320040"/><wp:wrapNone/><wp:docPr id="1" name="Text Box 1"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="320040"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></wps:spPr><wps:txbx><w:txbxContent><w:p><w:r><w:rPr><w:rFonts w:ascii="Times New Roman" w:hAnsi="Times New Roman"/><w:sz w:val="24"/></w:rPr><w:t>{label}</w:t></w:r></w:p></w:txbxContent></wps:txbx><wps:bodyPr/></wps:wsp></a:graphicData></a:graphic></wp:anchor></w:drawing></w:r></w:p></w:hdr>"#
    )
}

/// A header part whose textbox is a legacy VML shape (`w:pict`), sized by its
/// style, with its label split across runs as a PDF converter writes it.
fn vml_header_xml() -> String {
    let run = |text: &str| {
        format!(
            r#"<w:r><w:rPr><w:rFonts w:ascii="Times New Roman" w:hAnsi="Times New Roman"/><w:sz w:val="24"/></w:rPr><w:t xml:space="preserve">{text}</w:t></w:r>"#
        )
    };
    let label: String = ["T", "A", "B", " ", "2"].iter().map(|t| run(t)).collect();
    format!(
        r##"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:hdr xmlns:w="{W}" xmlns:v="urn:schemas-microsoft-com:vml"><w:p><w:r><w:pict><v:shape style="position:absolute;margin-left:89pt;margin-top:72.38pt;width:35.12pt;height:14pt;z-index:-1" type="#_x0000_t202" filled="f" stroked="f"><v:textbox inset="0,0,0,0"><w:txbxContent><w:p>{label}</w:p></w:txbxContent></v:textbox></v:shape></w:pict></w:r></w:p></w:hdr>"##
    )
}

/// Two sections, each with its own default header: "TAB 1" and "TAB 2".
fn tabbed_docx() -> Vec<u8> {
    tabbed_docx_with(header_xml("TAB 2"))
}

/// The two-section document with `header2` as the second header part.
fn tabbed_docx_with(header2: String) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}" xmlns:r="{R}"><w:body><w:p><w:pPr><w:sectPr><w:headerReference w:type="default" r:id="rIdH1"/></w:sectPr></w:pPr><w:r><w:t>First tab.</w:t></w:r></w:p><w:p><w:r><w:t>Second tab.</w:t></w:r></w:p><w:sectPr><w:headerReference w:type="default" r:id="rIdH2"/></w:sectPr></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/><Override PartName="/word/header2.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdH1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/><Relationship Id="rIdH2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header2.xml"/></Relationships>"#;
    let (h1, h2) = (header_xml("TAB 1"), header2);
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions = zip::write::FileOptions::default();
        for (name, data) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", document_xml.as_str()),
            ("word/header1.xml", h1.as_str()),
            ("word/header2.xml", h2.as_str()),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

/// The textbox target in the header part `part`: (host block id, drawing id).
fn header_textbox(doc: &Document, part: &str) -> (String, String) {
    let targets: Vec<_> = opaque_text_targets(&doc.snapshot().canonical)
        .into_iter()
        .filter(
            |t| matches!(&t.location, StoryScope::Header { part_path, .. } if part_path == part),
        )
        .collect();
    assert_eq!(targets.len(), 1, "one textbox in {part}");
    (
        targets[0].host_block_id.to_string(),
        targets[0].opaque_id.to_string(),
    )
}

/// The text of every textbox target per header part, in part order.
fn header_texts(doc: &Document) -> Vec<(String, String)> {
    let mut texts: Vec<_> = opaque_text_targets(&doc.snapshot().canonical)
        .into_iter()
        .filter_map(|t| match t.location {
            StoryScope::Header { part_path, .. } => Some((part_path, t.text)),
            _ => None,
        })
        .collect();
    texts.sort();
    texts
}

fn apply(doc: &Document, ops: &str, tracked: bool) -> Result<Document, stemma::RuntimeError> {
    let mode = if tracked {
        ""
    } else {
        r#","materialization_mode":"direct""#
    };
    let json = format!(r#"{{"ops":[{ops}],"revision":{{"author":"Counsel"}}{mode}}}"#);
    let transaction = stemma::edit_v4::parse_transaction(&json)
        .expect("parse")
        .into_edit_transaction()
        .expect("adapt");
    doc.apply(&transaction)
}

fn part_xml(doc: &Document, part: &str) -> String {
    let bytes = doc.serialize(&ExportOptions::default()).expect("serialize");
    String::from_utf8(
        stemma::docx::DocxArchive::read(&bytes)
            .expect("read")
            .get(&format!("word/{part}"))
            .expect("part")
            .to_vec(),
    )
    .expect("utf-8")
}

fn relabel(block: &str, drawing: &str) -> String {
    format!(
        r#"{{"op":"opaque_text_edit","header_part":"header2.xml","target":"{block}","opaque_id":"{drawing}","find":"TAB 2","replacement":"RESOURCE DIRECTORY"}}"#
    )
}

#[test]
fn a_header_textbox_edit_is_tracked_and_resolves_in_that_header_only() {
    let doc = Document::parse(&tabbed_docx()).expect("parse");
    let (block, drawing) = header_textbox(&doc, "header2.xml");
    let edited =
        apply(&doc, &relabel(&block, &drawing), true).expect("the header textbox is editable");

    let xml = part_xml(&edited, "header2.xml");
    assert!(
        xml.contains(">TAB 2</w:delText>") && xml.contains(">RESOURCE DIRECTORY</w:t>"),
        "a tracked replacement inside the header textbox: {xml}"
    );
    assert!(
        xml.contains(r#"w:ascii="Times New Roman""#),
        "the textbox keeps its font: {xml}"
    );

    let reopened = Document::parse(&edited.serialize(&ExportOptions::default()).expect("save"))
        .expect("reopen");
    for (resolution, label) in [
        (Resolution::AcceptAll, "RESOURCE DIRECTORY"),
        (Resolution::RejectAll, "TAB 2"),
    ] {
        let projected = reopened.project(resolution).expect("project");
        assert_eq!(
            header_texts(&projected),
            vec![
                ("header1.xml".to_string(), "TAB 1".to_string()),
                ("header2.xml".to_string(), label.to_string()),
            ],
            "after resolving, header2 reads {label:?}"
        );
    }
}

#[test]
fn a_direct_header_textbox_edit_changes_the_text() {
    let doc = Document::parse(&tabbed_docx()).expect("parse");
    let (block, drawing) = header_textbox(&doc, "header2.xml");
    let edited = apply(&doc, &relabel(&block, &drawing), false).expect("direct edit");
    assert_eq!(
        header_texts(&edited),
        vec![
            ("header1.xml".to_string(), "TAB 1".to_string()),
            ("header2.xml".to_string(), "RESOURCE DIRECTORY".to_string()),
        ]
    );
    assert!(
        edited.revisions().is_empty(),
        "a direct edit leaves no revision"
    );
}

#[test]
fn a_header_textbox_can_be_widened() {
    let doc = Document::parse(&tabbed_docx()).expect("parse");
    let (block, drawing) = header_textbox(&doc, "header2.xml");
    let edited = apply(
        &doc,
        &format!(
            r#"{{"op":"set_image_attrs","header_part":"header2.xml","target":"{block}","drawing_id":"{drawing}","resize":{{"cx":2743200,"cy":320040}}}}"#
        ),
        true,
    )
    .expect("the header textbox resizes");
    assert!(
        part_xml(&edited, "header2.xml").contains(r#"<wp:extent cx="2743200" cy="320040""#),
        "header2's textbox is wider"
    );
    assert!(
        part_xml(&edited, "header1.xml").contains(r#"<wp:extent cx="914400" cy="320040""#),
        "header1's textbox is unchanged"
    );
}

#[test]
fn a_story_address_is_checked() {
    let doc = Document::parse(&tabbed_docx()).expect("parse");
    let (block, drawing) = header_textbox(&doc, "header2.xml");
    let both = format!(
        r#"{{"op":"opaque_text_edit","header_part":"header2.xml","footer_part":"footer1.xml","target":"{block}","opaque_id":"{drawing}","find":"TAB 2","replacement":"X"}}"#
    );
    let json = format!(r#"{{"ops":[{both}],"revision":{{"author":"Counsel"}}}}"#);
    let refused = stemma::edit_v4::parse_transaction(&json)
        .map_err(|e| e.to_string())
        .and_then(|t| {
            t.into_edit_transaction()
                .map(|_| ())
                .map_err(|e| e.to_string())
        });
    assert!(
        refused.as_ref().is_err_and(|e| e.contains("both")),
        "naming two stories is refused: {refused:?}"
    );

    // Without the story, the header's block id is not a body block.
    let body = format!(
        r#"{{"op":"opaque_text_edit","target":"{block}","opaque_id":"{drawing}","find":"TAB 2","replacement":"X"}}"#
    );
    assert!(
        apply(&doc, &body, true).is_err(),
        "a header block is not in the body"
    );
    let missing = relabel(&block, &drawing).replace("header2.xml", "header9.xml");
    match apply(&doc, &missing, true) {
        Ok(_) => panic!("an unknown part is refused"),
        Err(err) => assert!(err.message.contains("header9.xml"), "{}", err.message),
    }
}

/// The same edit on a legacy VML textbox whose label spans five runs, as a
/// PDF converter writes it: the text is replaced across the runs, and the
/// shape widens through its style (1 pt = 12,700 EMU), keeping its position.
#[test]
fn a_vml_header_textbox_is_edited_and_widened() {
    let doc = Document::parse(&tabbed_docx_with(vml_header_xml())).expect("parse");
    let (block, drawing) = header_textbox(&doc, "header2.xml");
    let ops = format!(
        r#"{},{{"op":"set_image_attrs","header_part":"header2.xml","target":"{block}","drawing_id":"{drawing}","resize":{{"cx":2032000,"cy":177800}}}}"#,
        relabel(&block, &drawing)
    );
    let edited = apply(&doc, &ops, true).expect("the VML textbox is edited and resized");
    let xml = part_xml(&edited, "header2.xml");
    assert!(
        xml.contains(
            "position:absolute;margin-left:89pt;margin-top:72.38pt;width:160pt;height:14pt;z-index:-1"
        ),
        "only the size changed, in points: {xml}"
    );
    let accepted = edited.project(Resolution::AcceptAll).expect("accept");
    assert_eq!(
        header_texts(&accepted)[1],
        ("header2.xml".to_string(), "RESOURCE DIRECTORY".to_string())
    );
}

/// `set_textbox_text` reaches a header textbox the same way.
#[test]
fn a_header_textbox_can_be_rewritten_wholesale() {
    let doc = Document::parse(&tabbed_docx()).expect("parse");
    let (block, drawing) = header_textbox(&doc, "header2.xml");
    let edited = apply(
        &doc,
        &format!(
            r#"{{"op":"set_textbox_text","header_part":"header2.xml","target":"{block}","drawing_id":"{drawing}","paragraphs":["RESOURCE DIRECTORY"]}}"#
        ),
        true,
    )
    .expect("the header textbox is rewritten");
    assert_eq!(
        header_texts(&edited),
        vec![
            ("header1.xml".to_string(), "TAB 1".to_string()),
            ("header2.xml".to_string(), "RESOURCE DIRECTORY".to_string()),
        ]
    );
}
