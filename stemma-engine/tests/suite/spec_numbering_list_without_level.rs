//! A direct `w:numPr` that names a list (`w:numId`) but no level (`w:ilvl`).
//!
//! CT_NumPr (§17.3.1.19) makes both children optional. Word 16, probed on the
//! oracle with the shapes below:
//!
//! - the paragraph takes the nearest `w:ilvl` along its paragraph style's
//!   basedOn chain, even when that style names another list or none, and
//!   through styles that write only `w:numId` or no numPr; with no style
//!   level it is at level 0;
//! - except: when that nearest `w:ilvl` sits in a style naming no list below
//!   a style that names one, Word ignores it and uses level 0;
//! - the same holds for a `w:pPrChange`'s previous numPr (the Original view),
//!   resolved against the previous style;
//! - Reject All writes the resolved level into the restored live numPr;
//! - a style that writes only `w:ilvl` gives no numbering of its own.
//!
//! An untouched paragraph keeps what it wrote: no level is invented on save.

use stemma::api::Document;
use stemma::domain::{NodeId, RevisionInfo};
use stemma::edit::{EditStep, EditTransaction, MaterializationMode, NumberingChange};
use stemma::{ExportOptions, Resolution};

// numId 1: "1." / "a)" / "i." / "A>"; numId 2: "I-" / "1]" / "(a)".
const NUMBERING: &str = r#"<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="10"><w:multiLevelType w:val="multilevel"/><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/><w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr></w:lvl><w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="lowerLetter"/><w:lvlText w:val="%2)"/><w:pPr><w:ind w:left="1440" w:hanging="360"/></w:pPr></w:lvl><w:lvl w:ilvl="2"><w:start w:val="1"/><w:numFmt w:val="lowerRoman"/><w:lvlText w:val="%3."/><w:pPr><w:ind w:left="2160" w:hanging="360"/></w:pPr></w:lvl><w:lvl w:ilvl="3"><w:start w:val="1"/><w:numFmt w:val="upperLetter"/><w:lvlText w:val="%4&gt;"/><w:pPr><w:ind w:left="2880" w:hanging="360"/></w:pPr></w:lvl></w:abstractNum><w:abstractNum w:abstractNumId="20"><w:multiLevelType w:val="multilevel"/><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="upperRoman"/><w:lvlText w:val="%1-"/><w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr></w:lvl><w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%2]"/><w:pPr><w:ind w:left="1440" w:hanging="360"/></w:pPr></w:lvl><w:lvl w:ilvl="2"><w:start w:val="1"/><w:numFmt w:val="lowerLetter"/><w:lvlText w:val="(%3)"/><w:pPr><w:ind w:left="2160" w:hanging="360"/></w:pPr></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="10"/></w:num><w:num w:numId="2"><w:abstractNumId w:val="20"/></w:num></w:numbering>"#;

const STYLES: &str = r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style><w:style w:type="paragraph" w:customStyle="1" w:styleId="LevelTwo"><w:name w:val="Level Two"/><w:basedOn w:val="Normal"/><w:pPr><w:numPr><w:ilvl w:val="2"/></w:numPr></w:pPr></w:style><w:style w:type="paragraph" w:customStyle="1" w:styleId="OwnList"><w:name w:val="Own List"/><w:basedOn w:val="Normal"/><w:pPr><w:numPr><w:ilvl w:val="1"/><w:numId w:val="2"/></w:numPr></w:pPr></w:style><w:style w:type="paragraph" w:customStyle="1" w:styleId="Plain"><w:name w:val="Plain"/><w:basedOn w:val="Normal"/></w:style><w:style w:type="paragraph" w:customStyle="1" w:styleId="ParentLevelThree"><w:name w:val="Parent Level Three"/><w:basedOn w:val="Normal"/><w:pPr><w:numPr><w:ilvl w:val="3"/></w:numPr></w:pPr></w:style><w:style w:type="paragraph" w:customStyle="1" w:styleId="ChildListNoLevel"><w:name w:val="Child List No Level"/><w:basedOn w:val="ParentLevelThree"/><w:pPr><w:numPr><w:numId w:val="2"/></w:numPr></w:pPr></w:style><w:style w:type="paragraph" w:customStyle="1" w:styleId="ChildNoNumPr"><w:name w:val="Child No NumPr"/><w:basedOn w:val="ParentLevelThree"/></w:style><w:style w:type="paragraph" w:customStyle="1" w:styleId="ParentOwnList"><w:name w:val="Parent Own List"/><w:basedOn w:val="Normal"/><w:pPr><w:numPr><w:ilvl w:val="2"/><w:numId w:val="2"/></w:numPr></w:pPr></w:style><w:style w:type="paragraph" w:customStyle="1" w:styleId="ChildLevelOne"><w:name w:val="Child Level One"/><w:basedOn w:val="ParentOwnList"/><w:pPr><w:numPr><w:ilvl w:val="1"/></w:numPr></w:pPr></w:style></w:styles>"#;

fn num(num_id: u32, ilvl: Option<u32>) -> String {
    let level = ilvl.map_or(String::new(), |ilvl| format!(r#"<w:ilvl w:val="{ilvl}"/>"#));
    format!(r#"<w:numPr>{level}<w:numId w:val="{num_id}"/></w:numPr>"#)
}

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

fn numbering_step(block_id: &str, change: NumberingChange) -> EditTransaction {
    transaction(EditStep::SetParagraphNumbering {
        block_id: NodeId::from(block_id),
        semantic_hash: None,
        change,
        rationale: None,
    })
}

fn transaction(step: EditStep) -> EditTransaction {
    EditTransaction {
        steps: vec![step],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("Editor".to_string()),
            date: Some("2026-06-05T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

fn block_id(doc: &Document, index: usize) -> String {
    doc.read().blocks[index].id.to_string()
}

fn no_style_doc() -> Document {
    Document::parse(&make_docx(
        &[
            para("Opening item", &num(1, Some(0))),
            para("Nested item", &num(1, Some(1))),
            para("Probe item", &num(1, None)),
            para("Nested after", &num(1, Some(1))),
            para("Opening after", &num(1, Some(0))),
        ]
        .concat(),
    ))
    .expect("a list without a level opens")
}

#[test]
fn without_a_style_level_the_list_is_at_level_zero() {
    let doc = no_style_doc();
    assert_eq!(
        lists(&doc),
        vec![
            item(1, 0, "1."),
            item(1, 1, "a)"),
            item(1, 0, "2."),
            item(1, 1, "a)"),
            item(1, 0, "3."),
        ]
    );
}

#[test]
fn the_level_comes_from_the_paragraph_style_even_when_it_names_no_list() {
    let doc = Document::parse(&make_docx(
        &[
            para("Opening item", &num(1, Some(0))),
            para("Probe item", &styled("LevelTwo", &num(1, None))),
            para("Style alone", &styled("LevelTwo", "")),
            para("Opening after", &num(1, Some(0))),
        ]
        .concat(),
    ))
    .unwrap();
    assert_eq!(
        lists(&doc),
        vec![item(1, 0, "1."), item(1, 2, "i."), None, item(1, 0, "2.")],
        "a style writing only w:ilvl numbers nothing itself"
    );
}

#[test]
fn the_direct_list_wins_but_the_style_level_applies() {
    let doc = Document::parse(&make_docx(
        &[
            para("Opening item", &num(1, Some(0))),
            para("Probe item", &styled("OwnList", &num(1, None))),
            para("Style alone", &styled("OwnList", "")),
            para("Opening after", &num(1, Some(0))),
        ]
        .concat(),
    ))
    .unwrap();
    assert_eq!(
        lists(&doc),
        vec![
            item(1, 0, "1."),
            item(1, 1, "a)"),
            item(2, 1, "1]"),
            item(1, 0, "2."),
        ]
    );
}

#[test]
fn the_level_is_the_nearest_one_along_the_based_on_chain() {
    let doc = Document::parse(&make_docx(
        &[
            para("Opening item", &num(1, Some(0))),
            para(
                "Child names a list",
                &styled("ChildListNoLevel", &num(1, None)),
            ),
            para("Child has no numPr", &styled("ChildNoNumPr", &num(1, None))),
            para(
                "Child level under a parent list",
                &styled("ChildLevelOne", &num(1, None)),
            ),
        ]
        .concat(),
    ))
    .unwrap();
    assert_eq!(
        lists(&doc),
        vec![
            item(1, 0, "1."),
            item(1, 3, "A>"),
            item(1, 3, "B>"),
            item(1, 0, "2."),
        ],
        "the last item is Word's exception: a level in a style naming no list, \
         under a style naming one, is ignored"
    );
}

#[test]
fn an_untouched_list_without_a_level_is_saved_as_written() {
    let doc = no_style_doc();
    let saved = save(&doc);
    let xml = document_xml(&saved);
    assert_eq!(
        saved_ppr(&xml, "Probe item"),
        r#"<w:pPr><w:numPr><w:numId w:val="1"/></w:numPr></w:pPr>"#
    );
    let reopened = Document::parse(&saved).unwrap();
    assert_eq!(lists(&reopened), lists(&doc));
}

fn tracked_doc() -> Document {
    Document::parse(&make_docx(
        &[
            para("Opening item", &num(1, Some(0))),
            para(
                "Lima",
                &format!("{}{}", num(1, Some(3)), ppr_change(&num(1, None))),
            ),
            para("Mike", &ppr_change(&num(1, None))),
            para(
                "November",
                &styled(
                    "LevelTwo",
                    &format!(
                        "{}{}",
                        num(1, Some(3)),
                        ppr_change(&styled("LevelTwo", &num(1, None)))
                    ),
                ),
            ),
            para("Oscar", &format!("{}{}", num(1, None), ppr_change(""))),
            para("Opening after", &num(1, Some(0))),
        ]
        .concat(),
    ))
    .expect("a previous list without a level opens")
}

#[test]
fn reject_all_restores_the_previous_list_at_the_level_word_resolves() {
    let rejected = tracked_doc()
        .project(Resolution::RejectAll)
        .expect("reject all");
    assert_eq!(
        lists(&rejected),
        vec![
            item(1, 0, "1."),
            item(1, 0, "2."),
            item(1, 0, "3."),
            item(1, 2, "i."),
            None,
            item(1, 0, "4."),
        ]
    );
    // Word's Reject All writes the resolved level into the live numPr.
    let xml = document_xml(&save(&rejected));
    assert!(saved_ppr(&xml, "Lima").contains(&num(1, Some(0))), "{xml}");
    assert!(saved_ppr(&xml, "Mike").contains(&num(1, Some(0))), "{xml}");
    assert!(
        saved_ppr(&xml, "November").contains(&num(1, Some(2))),
        "{xml}"
    );
    assert!(!saved_ppr(&xml, "Oscar").contains("numPr"), "{xml}");
}

#[test]
fn accept_all_keeps_the_live_numbering() {
    let accepted = tracked_doc()
        .project(Resolution::AcceptAll)
        .expect("accept all");
    assert_eq!(
        lists(&accepted),
        vec![
            item(1, 0, "1."),
            item(1, 3, "A>"),
            None,
            item(1, 3, "B>"),
            item(1, 0, "2."),
            item(1, 0, "3."),
        ]
    );
}

#[test]
fn an_untouched_previous_list_without_a_level_is_saved_as_written() {
    let xml = document_xml(&save(&tracked_doc()));
    let lima = saved_ppr(&xml, "Lima");
    assert!(
        lima.contains(r#"<w:pPr><w:numPr><w:numId w:val="1"/></w:numPr></w:pPr></w:pPrChange>"#),
        "{lima}"
    );
    assert!(
        saved_ppr(&xml, "Oscar").starts_with(r#"<w:pPr><w:numPr><w:numId w:val="1"/></w:numPr>"#),
        "{xml}"
    );
}

#[test]
fn indent_writes_the_new_level_and_keeps_the_previous_numpr_as_written() {
    let doc = no_style_doc();
    let edited = doc
        .apply(&numbering_step(&block_id(&doc, 2), NumberingChange::Indent))
        .expect("indent a list item without a level");
    let xml = document_xml(&save(&edited));
    assert_eq!(
        saved_ppr(&xml, "Probe item"),
        format!(
            r#"<w:pPr>{}<w:pPrChange w:id="1" w:author="Editor" w:date="2026-06-05T00:00:00Z"><w:pPr>{}</w:pPr></w:pPrChange></w:pPr>"#,
            num(1, Some(1)),
            num(1, None)
        )
    );
    let accepted = edited.project(Resolution::AcceptAll).unwrap();
    assert_eq!(lists(&accepted)[2], item(1, 1, "b)"));
    let rejected = edited.project(Resolution::RejectAll).unwrap();
    assert_eq!(lists(&rejected)[2], item(1, 0, "2."));
}

#[test]
fn remove_numbering_writes_a_suppression() {
    let doc = no_style_doc();
    let edited = doc
        .apply(&numbering_step(&block_id(&doc, 2), NumberingChange::Remove))
        .expect("remove a list without a level");
    let accepted = edited.project(Resolution::AcceptAll).unwrap();
    assert_eq!(lists(&accepted)[2], None);
    let xml = document_xml(&save(&accepted));
    assert!(
        saved_ppr(&xml, "Probe item").contains(r#"<w:numId w:val="0"/>"#),
        "{xml}"
    );
}

#[test]
fn a_new_style_keeps_the_item_at_its_level_and_writes_it() {
    let doc = no_style_doc();
    let edited = doc
        .apply(&transaction(EditStep::ApplyStyle {
            block_id: NodeId::from(block_id(&doc, 2).as_str()),
            semantic_hash: None,
            style_id: "LevelTwo".to_string(),
            rationale: None,
        }))
        .expect("apply a style to a list item without a level");
    let saved = save(&edited.project(Resolution::AcceptAll).unwrap());
    let xml = document_xml(&saved);
    assert!(
        saved_ppr(&xml, "Probe item").contains(&num(1, Some(0))),
        "the item stays at level 0 under a style whose level is 2: {xml}"
    );
    assert_eq!(
        lists(&Document::parse(&saved).unwrap())[2],
        item(1, 0, "2.")
    );
}

#[test]
fn a_new_based_on_keeps_items_at_their_level_and_writes_it() {
    use stemma::edit::{StyleDefinition, StyleParaProps, StyleRunProps, StyleType};
    let doc = Document::parse(&make_docx(
        &[
            para("Opening item", &num(1, Some(0))),
            para("Probe item", &styled("Plain", &num(1, None))),
        ]
        .concat(),
    ))
    .unwrap();
    assert_eq!(lists(&doc)[1], item(1, 0, "2."));
    let edited = doc
        .apply(&transaction(EditStep::ModifyStyle {
            style_id: "Plain".to_string(),
            def: StyleDefinition {
                style_id: "Plain".to_string(),
                style_type: StyleType::Para,
                based_on: Some("LevelTwo".to_string()),
                name: "Plain".to_string(),
                run_props: StyleRunProps::default(),
                para_props: StyleParaProps::default(),
            },
            rationale: None,
        }))
        .expect("rebase the style of a list item without a level");
    let saved = save(&edited);
    let xml = document_xml(&saved);
    assert!(
        saved_ppr(&xml, "Probe item").contains(&num(1, Some(0))),
        "{xml}"
    );
    assert_eq!(
        lists(&Document::parse(&saved).unwrap())[1],
        item(1, 0, "2.")
    );
}
