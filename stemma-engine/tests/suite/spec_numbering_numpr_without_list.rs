//! A direct `w:numPr` that names no list: no `w:numId`, with or without a
//! `w:ilvl`.
//!
//! CT_NumPr (§17.3.1.19) makes both children optional, and documents in the
//! wild carry both shapes. Word 16, probed on the oracle with the shapes
//! below, opens them without repair and reads them like this:
//!
//! - such a `w:numPr` neither starts nor removes numbering: a paragraph
//!   whose style names a list keeps that list, and one whose style names
//!   none stays unnumbered;
//! - a written `w:ilvl` selects the level in the list the style gives;
//! - the same holds for a `w:pPrChange`'s previous numPr (the Original view);
//! - Reject All does not write the shape back: with no level it writes no
//!   numPr, with a level and a style list it writes the complete pair, and
//!   with a level and no style list it writes an explicit removal (numId 0).
//!
//! An untouched paragraph keeps what it wrote.

use stemma::api::Document;
use stemma::{ExportOptions, Resolution};

// numId 1: "1." / "(a)".
const NUMBERING: &str = r#"<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:multiLevelType w:val="multilevel"/><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/><w:lvlJc w:val="left"/></w:lvl><w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="lowerLetter"/><w:lvlText w:val="(%2)"/><w:lvlJc w:val="left"/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#;

// "Listed" names list 1 and writes no level.
const STYLES: &str = r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style><w:style w:type="paragraph" w:customStyle="1" w:styleId="Listed"><w:name w:val="Listed"/><w:basedOn w:val="Normal"/><w:pPr><w:numPr><w:numId w:val="1"/></w:numPr></w:pPr></w:style></w:styles>"#;

const EMPTY: &str = "<w:numPr/>";
const LEVEL_ONE_ONLY: &str = r#"<w:numPr><w:ilvl w:val="1"/></w:numPr>"#;
const DIRECT_ITEM: &str = r#"<w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr>"#;

fn para(text: &str, ppr: &str) -> String {
    format!(r#"<w:p><w:pPr>{ppr}</w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>"#)
}

fn styled(style: &str, rest: &str) -> String {
    format!(r#"<w:pStyle w:val="{style}"/>{rest}"#)
}

fn ppr_change(previous: &str) -> String {
    format!(
        r#"<w:pPrChange w:id="900" w:author="Reviewer" w:date="2026-01-05T10:00:00Z"><w:pPr>{previous}</w:pPr></w:pPrChange>"#
    )
}

fn make_docx(body: &str) -> Vec<u8> {
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/></Relationships>"#;

    use std::io::Write;
    use zip::write::FileOptions;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        for (name, xml) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", document.as_str()),
            ("word/styles.xml", STYLES),
            ("word/numbering.xml", NUMBERING),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(xml.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

fn document_xml(docx: &[u8]) -> String {
    use std::io::Read;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(docx)).unwrap();
    let mut xml = String::new();
    zip.by_name("word/document.xml")
        .unwrap()
        .read_to_string(&mut xml)
        .unwrap();
    xml
}

fn save(doc: &Document) -> Vec<u8> {
    doc.serialize(&ExportOptions::default()).expect("serialize")
}

/// The `w:pPr` of the paragraph whose text is `text`, as saved.
fn saved_ppr(xml: &str, text: &str) -> String {
    let xml = xml.replace(" />", "/>");
    let end = xml
        .find(&format!(">{text}</w:t>"))
        .unwrap_or_else(|| panic!("{text} saved"));
    let start = xml[..end]
        .rfind("<w:p>")
        .max(xml[..end].rfind("<w:p "))
        .expect("paragraph start");
    let paragraph = &xml[start..end];
    let Some(ppr_start) = paragraph.find("<w:pPr>") else {
        return String::new();
    };
    let ppr_end = paragraph.rfind("</w:pPr>").expect("pPr end") + "</w:pPr>".len();
    paragraph[ppr_start..ppr_end].to_string()
}

/// `(num_id, ilvl, marker)` per block, in document order.
fn lists(doc: &Document) -> Vec<Option<(u32, u32, String)>> {
    doc.read()
        .blocks
        .iter()
        .map(|block| {
            block
                .list
                .as_ref()
                .map(|list| (list.num_id, list.ilvl, list.marker_text.clone()))
        })
        .collect()
}

fn item(num_id: u32, ilvl: u32, marker: &str) -> Option<(u32, u32, String)> {
    Some((num_id, ilvl, marker.to_string()))
}

fn open(paragraphs: &[String]) -> Document {
    Document::parse(&make_docx(&paragraphs.concat())).expect("a numPr without a list opens")
}

/// Three paragraphs in style "Listed"; the middle one adds `probe`.
fn styled_trio(probe: &str) -> Document {
    open(&[
        para("Harbour dues", &styled("Listed", "")),
        para("Probe item", &styled("Listed", probe)),
        para("Pilot fees", &styled("Listed", "")),
    ])
}

/// Two directly numbered paragraphs around an unstyled one carrying `probe`.
fn plain_trio(probe: &str) -> Document {
    open(&[
        para("Harbour dues", DIRECT_ITEM),
        para("Probe item", probe),
        para("Pilot fees", DIRECT_ITEM),
    ])
}

#[test]
fn an_empty_numpr_keeps_the_style_list() {
    assert_eq!(
        lists(&styled_trio(EMPTY)),
        vec![item(1, 0, "1."), item(1, 0, "2."), item(1, 0, "3.")]
    );
}

#[test]
fn a_level_alone_selects_the_level_in_the_style_list() {
    assert_eq!(
        lists(&styled_trio(LEVEL_ONE_ONLY)),
        vec![item(1, 0, "1."), item(1, 1, "(a)"), item(1, 0, "2.")]
    );
}

#[test]
fn without_a_style_list_neither_shape_numbers_the_paragraph() {
    for probe in [EMPTY, LEVEL_ONE_ONLY] {
        assert_eq!(
            lists(&plain_trio(probe)),
            vec![item(1, 0, "1."), None, item(1, 0, "2.")],
            "{probe}"
        );
    }
}

#[test]
fn an_untouched_paragraph_keeps_the_numpr_it_wrote() {
    for (probe, saved) in [
        (
            EMPTY,
            r#"<w:pPr><w:pStyle w:val="Listed"/><w:numPr/></w:pPr>"#,
        ),
        (
            LEVEL_ONE_ONLY,
            r#"<w:pPr><w:pStyle w:val="Listed"/><w:numPr><w:ilvl w:val="1"/></w:numPr></w:pPr>"#,
        ),
    ] {
        let xml = document_xml(&save(&styled_trio(probe)));
        assert_eq!(saved_ppr(&xml, "Probe item"), saved, "{probe}");
    }
    let xml = document_xml(&save(&plain_trio(LEVEL_ONE_ONLY)));
    assert_eq!(
        saved_ppr(&xml, "Probe item"),
        r#"<w:pPr><w:numPr><w:ilvl w:val="1"/></w:numPr></w:pPr>"#
    );
}

#[test]
fn a_saved_document_reopens_to_the_same_lists() {
    for probe in [EMPTY, LEVEL_ONE_ONLY] {
        let doc = styled_trio(probe);
        let reopened = Document::parse(&save(&doc)).expect("reopen");
        assert_eq!(lists(&reopened), lists(&doc), "{probe}");
    }
}

/// The probe paragraph is in style "Listed" now and was, before the tracked
/// change, in the same style with `previous` beside it.
fn changed_from(previous: &str) -> Document {
    open(&[
        para("Harbour dues", &styled("Listed", "")),
        para(
            "Probe item",
            &styled("Listed", &ppr_change(&styled("Listed", previous))),
        ),
        para("Pilot fees", &styled("Listed", "")),
    ])
}

#[test]
fn reject_all_writes_the_complete_pair_for_a_previous_level_alone() {
    let doc = changed_from(LEVEL_ONE_ONLY);
    assert_eq!(
        lists(&doc),
        vec![item(1, 0, "1."), item(1, 0, "2."), item(1, 0, "3.")],
        "the current state has no direct numPr"
    );
    let rejected = doc.project(Resolution::RejectAll).expect("reject all");
    assert_eq!(
        lists(&rejected),
        vec![item(1, 0, "1."), item(1, 1, "(a)"), item(1, 0, "2.")]
    );
    assert_eq!(
        saved_ppr(&document_xml(&save(&rejected)), "Probe item"),
        r#"<w:pPr><w:pStyle w:val="Listed"/><w:numPr><w:ilvl w:val="1"/><w:numId w:val="1"/></w:numPr></w:pPr>"#
    );
}

#[test]
fn reject_all_writes_no_numpr_for_a_previous_empty_one() {
    let rejected = changed_from(EMPTY)
        .project(Resolution::RejectAll)
        .expect("reject all");
    assert_eq!(
        lists(&rejected),
        vec![item(1, 0, "1."), item(1, 0, "2."), item(1, 0, "3.")]
    );
    assert_eq!(
        saved_ppr(&document_xml(&save(&rejected)), "Probe item"),
        r#"<w:pPr><w:pStyle w:val="Listed"/></w:pPr>"#
    );
}

/// Word writes `<w:ilvl w:val="1"/><w:numId w:val="0"/>` here. The removal
/// this engine writes carries level 0; both number nothing.
#[test]
fn reject_all_writes_a_removal_for_a_previous_level_alone_with_no_style_list() {
    let doc = open(&[
        para("Harbour dues", DIRECT_ITEM),
        para(
            "Probe item",
            &format!(r#"<w:jc w:val="center"/>{}"#, ppr_change(LEVEL_ONE_ONLY)),
        ),
        para("Pilot fees", DIRECT_ITEM),
    ]);
    let rejected = doc.project(Resolution::RejectAll).expect("reject all");
    assert_eq!(
        lists(&rejected),
        vec![item(1, 0, "1."), None, item(1, 0, "2.")]
    );
    assert_eq!(
        saved_ppr(&document_xml(&save(&rejected)), "Probe item"),
        r#"<w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="0"/></w:numPr></w:pPr>"#
    );
}

#[test]
fn accept_all_keeps_the_current_state() {
    let accepted = changed_from(LEVEL_ONE_ONLY)
        .project(Resolution::AcceptAll)
        .expect("accept all");
    assert_eq!(
        lists(&accepted),
        vec![item(1, 0, "1."), item(1, 0, "2."), item(1, 0, "3.")]
    );
}
