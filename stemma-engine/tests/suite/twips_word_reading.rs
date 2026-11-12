//! Twips measures are read as Word reads them (Word 16, verified on the
//! oracle). Every twips attribute (ST_TwipsMeasure / ST_SignedTwipsMeasure,
//! and the number of a CT_TblWidth) goes through one reading:
//!
//! - a decimal truncates toward zero: "720.6" is 720, "720.99" is 720,
//!   "-200.6" and "-720.5" are -200 and -720;
//! - a leading "+" is accepted ("+500" is 500), and a universal measure
//!   converts (".5in" is 720);
//! - an empty value is no value: Word saves the paragraph without it;
//! - a value that is not a number ("abc", "12abc", "1e3") is refused on a
//!   paragraph, tab, frame, section and grid-column measure (Word cannot open
//!   the file), reads as width 0 on `w:tblW` / `w:tcW`, and as no value on
//!   `w:tblInd`, `w:trHeight` and a run's `w:spacing` (Word drops the
//!   element).
//!
//! The model holds the value Word reads; an untouched element keeps the
//! bytes the document wrote, and an edited one writes the value read.

use std::io::Write;

use stemma::ExportOptions;
use stemma::api::Document;
use stemma::domain::{BlockNode, InlineNode, ParagraphNode, TableNode};
use stemma::edit_v4::parse_transaction;

use crate::common::read_zip_entry;

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn package(body: &str, styles: Option<&str>, numbering: Option<&str>) -> Vec<u8> {
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="{W}"><w:body>{body}</w:body></w:document>"#
    );
    let mut overrides = String::from(
        r#"<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>"#,
    );
    let mut relationships = String::new();
    let mut parts = vec![("word/document.xml".to_string(), document)];
    if let Some(styles) = styles {
        overrides.push_str(r#"<Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>"#);
        relationships.push_str(r#"<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>"#);
        parts.push((
            "word/styles.xml".to_string(),
            format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:styles xmlns:w="{W}">{styles}</w:styles>"#),
        ));
    }
    if let Some(numbering) = numbering {
        overrides.push_str(r#"<Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/>"#);
        relationships.push_str(r#"<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/>"#);
        parts.push((
            "word/numbering.xml".to_string(),
            format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:numbering xmlns:w="{W}">{numbering}</w:numbering>"#),
        ));
    }
    parts.push((
        "[Content_Types].xml".to_string(),
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/>{overrides}</Types>"#
        ),
    ));
    parts.push((
        "_rels/.rels".to_string(),
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#.to_string(),
    ));
    parts.push((
        "word/_rels/document.xml.rels".to_string(),
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{relationships}</Relationships>"#
        ),
    ));
    let mut bytes = Vec::new();
    {
        use zip::write::FileOptions;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
        let options: FileOptions = FileOptions::default();
        for (name, data) in &parts {
            zip.start_file(name.as_str(), options).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    bytes
}

fn open(body: &str) -> Document {
    Document::parse(&package(body, None, None)).expect("Word opens it")
}

/// The import error for a document Word refuses to open.
fn refused(body: &str) -> String {
    match Document::parse(&package(body, None, None)) {
        Ok(_) => panic!("Word refuses this document, so must the import: {body}"),
        Err(error) => error.message,
    }
}

fn paragraph(doc: &Document, index: usize) -> &ParagraphNode {
    match &doc.snapshot().canonical.blocks[index].block {
        BlockNode::Paragraph(paragraph) => paragraph,
        other => panic!("block {index} is not a paragraph: {other:?}"),
    }
}

fn table(doc: &Document) -> &TableNode {
    doc.snapshot()
        .canonical
        .blocks
        .iter()
        .find_map(|block| match &block.block {
            BlockNode::Table(table) => Some(table),
            _ => None,
        })
        .expect("a table")
}

fn para(ppr: &str, text: &str) -> String {
    format!(r#"<w:p><w:pPr>{ppr}</w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>"#)
}

fn document_xml(doc: &Document) -> String {
    let bytes = doc.serialize(&ExportOptions::default()).expect("save");
    read_zip_entry(&bytes, "word/document.xml").expect("document.xml")
}

/// Replace the whole text of the paragraph `old` with `new`, as a tracked edit.
fn edit_text(doc: &Document, old: &str, new: &str) -> Document {
    let block = doc
        .read()
        .blocks
        .iter()
        .find(|block| block.text == old)
        .expect("the paragraph to edit")
        .id
        .to_string();
    let transaction = parse_transaction(&format!(
        r#"{{"ops":[{{"op":"replace","target":"{block}","expect":"{old}","content":{{"type":"paragraph","content":[{{"type":"text","text":"{new}"}}]}}}}],"revision":{{"author":"Editor"}}}}"#
    ))
    .expect("schema")
    .into_edit_transaction()
    .expect("adapter");
    doc.apply(&transaction).expect("edit")
}

#[test]
fn paragraph_measures_truncate_toward_zero() {
    let body = [
        para(
            r#"<w:spacing w:before="240.6" w:after="120.7" w:line="360.8" w:lineRule="auto"/><w:ind w:left="720.6" w:right="360.9" w:hanging="300.7"/>"#,
            "Indented words.",
        ),
        para(
            r#"<w:ind w:left="-200.6" w:firstLine="400.8"/>"#,
            "Outdented words.",
        ),
        para(
            r#"<w:tabs><w:tab w:val="left" w:pos="2880.7"/><w:tab w:val="right" w:pos="5760.4"/></w:tabs>"#,
            "Tabbed words.",
        ),
    ]
    .concat();
    let doc = open(&body);

    let first = paragraph(&doc, 0);
    let indent = first.authored_indent.as_ref().expect("an indent");
    assert_eq!(
        (indent.left, indent.right, indent.effective_first_line_twips),
        (Some(720), Some(360), Some(-300))
    );
    let spacing = first.authored_spacing.as_ref().expect("spacing");
    assert_eq!(
        (spacing.before, spacing.after, spacing.line),
        (Some(240), Some(120), Some(360))
    );

    let second = paragraph(&doc, 1)
        .authored_indent
        .as_ref()
        .expect("an indent");
    assert_eq!(
        (second.left, second.effective_first_line_twips),
        (Some(-200), Some(400))
    );

    let positions: Vec<i32> = paragraph(&doc, 2)
        .tab_stops
        .iter()
        .map(|stop| stop.position)
        .collect();
    assert_eq!(positions, [2880, 5760]);
}

#[test]
fn a_twips_number_is_read_by_word_spellings() {
    let left = |value: &str| {
        let doc = open(&para(&format!(r#"<w:ind w:left="{value}"/>"#), "Words."));
        paragraph(&doc, 0)
            .authored_indent
            .as_ref()
            .and_then(|indent| indent.left)
    };
    assert_eq!(left("+500"), Some(500));
    assert_eq!(left(".5in"), Some(720));
    assert_eq!(left("720.5"), Some(720));
    assert_eq!(left("-720.5"), Some(-720));
    assert_eq!(left("720.99"), Some(720));
    // Word saves the paragraph without the attribute.
    assert_eq!(left(""), None);
}

#[test]
fn a_non_number_on_a_paragraph_measure_is_refused() {
    for value in ["abc", "12abc", "1e3"] {
        let message = refused(&para(&format!(r#"<w:ind w:left="{value}"/>"#), "Words."));
        assert!(
            message.contains("w:ind/@w:left") && message.contains(value),
            "{value}: {message}"
        );
    }
    let message = refused(&para(r#"<w:spacing w:before="abc"/>"#, "Words."));
    assert!(message.contains("w:spacing/@w:before"), "{message}");
    let message = refused(&para(
        r#"<w:tabs><w:tab w:val="left" w:pos="abc"/></w:tabs>"#,
        "Words.",
    ));
    assert!(message.contains("w:tab/@w:pos"), "{message}");
}

const FRAME: &str = r#"<w:framePr w:w="2000.6" w:h="800.7" w:hSpace="180.9" w:vSpace="90.6" w:wrap="around" w:vAnchor="text" w:hAnchor="page" w:x="1500.8" w:y="300.6"/>"#;

#[test]
fn frame_measures_truncate_and_survive_an_edit() {
    let body = [para(FRAME, "Framed words."), para("", "Body words.")].concat();
    let doc = open(&body);
    let frame = paragraph(&doc, 0).frame_pr.as_ref().expect("a frame");
    assert_eq!(
        (
            frame.width,
            frame.height,
            frame.h_space,
            frame.v_space,
            frame.x,
            frame.y
        ),
        (
            Some(2000),
            Some(800),
            Some(180),
            Some(90),
            Some(1500),
            Some(300)
        )
    );

    // An untouched frame keeps its bytes.
    assert!(document_xml(&doc).contains(r#"w:w="2000.6""#));

    // An edited one writes the value read: the frame keeps its size and
    // position.
    let xml = document_xml(&edit_text(&doc, "Framed words.", "Framed text."));
    let frame = xml
        .split("<w:framePr")
        .nth(1)
        .and_then(|rest| rest.split("/>").next())
        .expect("the edited paragraph keeps its frame");
    for attribute in [
        r#"w:w="2000""#,
        r#"w:h="800""#,
        r#"w:hSpace="180""#,
        r#"w:vSpace="90""#,
        r#"w:x="1500""#,
        r#"w:y="300""#,
    ] {
        assert!(frame.contains(attribute), "{attribute} in {frame}");
    }
}

#[test]
fn a_non_number_on_a_frame_measure_is_refused() {
    let message = refused(&para(
        r#"<w:framePr w:w="abc" w:hAnchor="page" w:x="1500"/>"#,
        "Framed words.",
    ));
    assert!(message.contains("w:framePr/@w:w"), "{message}");
}

fn table_body(tbl_pr: &str, grid_col: &str, tr_pr: &str, tc_w: &str) -> String {
    format!(
        concat!(
            r#"<w:tbl><w:tblPr>{tbl_pr}</w:tblPr><w:tblGrid><w:gridCol w:w="{grid_col}"/><w:gridCol w:w="2500"/></w:tblGrid>"#,
            r#"<w:tr><w:trPr>{tr_pr}</w:trPr><w:tc><w:tcPr><w:tcW w:w="{tc_w}" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>Cell one</w:t></w:r></w:p></w:tc>"#,
            r#"<w:tc><w:tcPr><w:tcW w:w="2500" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>Cell two</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/>"#
        ),
        tbl_pr = tbl_pr,
        grid_col = grid_col,
        tr_pr = tr_pr,
        tc_w = tc_w,
    )
}

#[test]
fn table_measures_truncate_toward_zero() {
    let doc = open(&table_body(
        concat!(
            r#"<w:tblpPr w:leftFromText="180.9" w:rightFromText="180.6" w:topFromText="90.6" w:bottomFromText="90.7" w:vertAnchor="text" w:horzAnchor="page" w:tblpX="1500.8" w:tblpY="-300.6"/>"#,
            r#"<w:tblW w:w="5000.8" w:type="dxa"/><w:tblInd w:w="100.6" w:type="dxa"/><w:tblCellSpacing w:w="20.7" w:type="dxa"/>"#,
            r#"<w:tblCellMar><w:left w:w="110.7" w:type="dxa"/></w:tblCellMar>"#
        ),
        "2500.6",
        r#"<w:trHeight w:val="400.7"/>"#,
        "2500.6",
    ));
    let table = table(&doc);
    let formatting = &table.formatting;
    assert_eq!(formatting.width.as_ref().map(|width| width.w), Some(5000));
    assert_eq!(formatting.indent, Some(100));
    assert_eq!(formatting.cell_spacing, Some(20));
    assert_eq!(
        formatting
            .default_cell_margins
            .as_ref()
            .and_then(|margins| margins.left),
        Some(110)
    );
    assert_eq!(formatting.grid_cols, [2500, 2500]);
    let positioning = formatting.positioning.as_ref().expect("floating");
    assert_eq!(
        (
            positioning.left_from_text,
            positioning.right_from_text,
            positioning.top_from_text,
            positioning.bottom_from_text,
            positioning.tblp_x,
            positioning.tblp_y
        ),
        (
            Some(180),
            Some(180),
            Some(90),
            Some(90),
            Some(1500),
            Some(-300)
        )
    );
    let row = &table.rows[0];
    assert_eq!(row.height, Some(400));
    assert_eq!(
        row.cells[0].formatting.width.as_ref().map(|width| width.w),
        Some(2500)
    );
}

#[test]
fn a_non_number_on_a_table_measure_reads_as_word_reads_it() {
    // tblW and tcW read as 0; tblInd and trHeight as no value.
    let doc = open(&table_body(
        r#"<w:tblW w:w="abc" w:type="dxa"/><w:tblInd w:w="abc" w:type="dxa"/>"#,
        "2500",
        r#"<w:trHeight w:val="abc"/>"#,
        "abc",
    ));
    let table = table(&doc);
    assert_eq!(
        table.formatting.width.as_ref().map(|width| width.w),
        Some(0)
    );
    assert_eq!(table.formatting.indent, None);
    assert_eq!(table.rows[0].height, None);
    assert_eq!(
        table.rows[0].cells[0]
            .formatting
            .width
            .as_ref()
            .map(|width| width.w),
        Some(0)
    );
    // Each default reading is named in an import diagnostic.
    assert_eq!(
        warnings(&doc),
        [
            r#"w:tblW/@w:w = "abc" read as a width of 0"#,
            r#"w:tblInd/@w:w = "abc" read as no value"#,
            r#"w:trHeight/@w:val = "abc" read as no row height"#,
            r#"w:tcW/@w:w = "abc" read as a width of 0"#,
        ]
    );

    // A grid column Word can only repair is refused.
    let message = refused(&table_body("", "abc", "", "2500"));
    assert!(message.contains("gridCol"), "{message}");
    // A floating position, like a frame's, is refused.
    let message = refused(&table_body(
        r#"<w:tblpPr w:vertAnchor="text" w:tblpX="abc"/>"#,
        "2500",
        "",
        "2500",
    ));
    assert!(message.contains("w:tblpPr/@w:tblpX"), "{message}");
}

#[test]
fn section_measures_truncate_and_a_non_number_is_refused() {
    let section = |pg_sz_w: &str, top: &str| {
        format!(
            r#"<w:p><w:r><w:t>Words.</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="{pg_sz_w}" w:h="15840.6"/><w:pgMar w:top="{top}" w:right="1440.7" w:bottom="1440.8" w:left="1440.9" w:header="720.6" w:footer="720.7" w:gutter="0"/></w:sectPr>"#
        )
    };
    let doc = open(&section("12240.7", "1440.6"));
    let properties = doc
        .snapshot()
        .canonical
        .body_section_properties
        .as_ref()
        .expect("a section");
    assert_eq!(
        (properties.page_width, properties.page_height),
        (Some(12240), Some(15840))
    );
    assert_eq!(
        (
            properties.margin_top,
            properties.margin_right,
            properties.margin_bottom,
            properties.margin_left
        ),
        (Some(1440), Some(1440), Some(1440), Some(1440))
    );
    assert_eq!(
        (properties.header_distance, properties.footer_distance),
        (Some(720), Some(720))
    );
    let message = refused(&section("12240", "abc"));
    assert!(message.contains("w:pgMar/@w:top"), "{message}");
    let message = refused(&section("abc", "1440"));
    assert!(message.contains("w:pgSz/@w:w"), "{message}");
}

fn run_spacing(doc: &Document) -> Vec<Option<i32>> {
    paragraph(doc, 0)
        .segments
        .iter()
        .flat_map(|segment| &segment.inlines)
        .filter_map(|inline| match inline {
            InlineNode::Text(text) => Some(text.style_props.char_spacing),
            _ => None,
        })
        .collect()
}

#[test]
fn run_spacing_truncates_and_a_non_number_is_no_spacing() {
    let run = |value: &str, text: &str| {
        format!(
            r#"<w:r><w:rPr><w:spacing w:val="{value}"/></w:rPr><w:t xml:space="preserve">{text} </w:t></w:r>"#
        )
    };
    let body = format!(
        "<w:p>{}{}{}</w:p>",
        run("20.7", "Alpha"),
        run("-20.7", "Beta"),
        run("abc", "Gamma")
    );
    let doc = open(&body);
    assert_eq!(run_spacing(&doc), [Some(20), Some(-20), None]);
    // Untouched runs keep the bytes the document wrote.
    let xml = document_xml(&doc);
    for value in ["20.7", "-20.7", "abc"] {
        assert!(
            xml.contains(&format!(r#"<w:spacing w:val="{value}""#)),
            "{value}"
        );
    }
    assert_eq!(
        warnings(&doc),
        [r#"w:spacing/@w:val = "abc" read as no character spacing"#]
    );
}

/// The import diagnostics naming a twips default, as "<attribute> read as
/// <reading>".
fn warnings(doc: &Document) -> Vec<String> {
    doc.diagnostics()
        .iter()
        .filter_map(|diagnostic| {
            let (attribute, rest) = diagnostic
                .message
                .split_once(" is not a number; read as ")?;
            let reading = rest.split(", as Word reads it").next()?;
            Some(format!("{attribute} read as {reading}"))
        })
        .collect()
}

#[test]
fn style_measures_are_read_as_paragraph_measures() {
    let styles = concat!(
        r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>"#,
        r#"<w:style w:type="paragraph" w:styleId="Indented"><w:name w:val="Indented"/><w:basedOn w:val="Normal"/>"#,
        r#"<w:pPr><w:spacing w:before="240.6" w:after="120.7" w:line="360.8" w:lineRule="auto"/><w:ind w:left="720.6" w:right="360.9" w:hanging="300.7"/></w:pPr>"#,
        r#"<w:rPr><w:spacing w:val="20.7"/></w:rPr></w:style>"#,
    );
    let body = r#"<w:p><w:pPr><w:pStyle w:val="Indented"/></w:pPr><w:r><w:t>Styled words.</w:t></w:r></w:p>"#;
    let doc = Document::parse(&package(body, Some(styles), None)).expect("Word opens it");
    let paragraph = paragraph(&doc, 0);
    let indent = paragraph.indent.as_ref().expect("the style's indent");
    assert_eq!(
        (indent.left, indent.right, indent.effective_first_line_twips),
        (Some(720), Some(360), Some(-300))
    );
    let spacing = paragraph.spacing.as_ref().expect("the style's spacing");
    assert_eq!(
        (spacing.before, spacing.after, spacing.line),
        (Some(240), Some(120), Some(360))
    );

    let bad = styles.replace(r#"w:left="720.6""#, r#"w:left="abc""#);
    let message = match Document::parse(&package(body, Some(&bad), None)) {
        Ok(_) => panic!("a non-number style indent is refused like a paragraph's"),
        Err(error) => error.message,
    };
    assert!(
        message.contains("Indented") && message.contains("abc"),
        "{message}"
    );
}

#[test]
fn numbering_level_indents_are_read_as_paragraph_indents() {
    let numbering = |left: &str| {
        format!(
            concat!(
                r#"<w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/>"#,
                r#"<w:pPr><w:ind w:left="{left}" w:hanging="360.7"/></w:pPr></w:lvl></w:abstractNum>"#,
                r#"<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#
            ),
            left = left
        )
    };
    let body = r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Listed words.</w:t></w:r></w:p>"#;
    let doc = Document::parse(&package(body, None, Some(&numbering("720.6")))).expect("opens");
    let indent = paragraph(&doc, 0)
        .indent
        .as_ref()
        .expect("the level's indent");
    assert_eq!(
        (indent.left, indent.effective_first_line_twips),
        (Some(720), Some(-360))
    );

    let message = match Document::parse(&package(body, None, Some(&numbering("abc")))) {
        Ok(_) => panic!("a non-number level indent is refused like a paragraph's"),
        Err(error) => error.message,
    };
    assert!(message.contains("abc"), "{message}");
}
