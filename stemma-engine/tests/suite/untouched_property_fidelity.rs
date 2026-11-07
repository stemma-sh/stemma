//! Properties of content an edit does not touch survive the save that the
//! edit triggers.
//!
//! An edit anywhere in the body rewrites the whole of `word/document.xml`
//! from the model, so every property of an untouched paragraph, run, table or
//! section is written back from what the import read. A property the import
//! misreads, or drops, changes content nobody edited. Each test here builds a
//! minimal package, edits an unrelated first paragraph, and checks the
//! untouched property against its ECMA-376 meaning.

use stemma::ExportOptions;
use stemma::api::Document;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

/// A package whose body is a first paragraph "Edit me here." followed by
/// `body`, with an optional `word/styles.xml` body.
fn docx(body: &str, styles: Option<&str>) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}"><w:body><w:p><w:r><w:t>Edit me here.</w:t></w:r></w:p>{body}</w:body></w:document>"#
    );
    let styles_xml = styles.map(|styles| {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="{W_NS}">{styles}</w:styles>"#
        )
    });
    let styles_override = if styles.is_some() {
        r#"<Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>"#
    } else {
        ""
    };
    let content_types = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>{styles_override}</Types>"#
    );
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let document_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;
    let mut parts: Vec<(&str, &str)> = vec![
        ("[Content_Types].xml", content_types.as_str()),
        ("_rels/.rels", rels),
        ("word/document.xml", document_xml.as_str()),
    ];
    if let Some(styles_xml) = styles_xml.as_deref() {
        parts.push(("word/_rels/document.xml.rels", document_rels));
        parts.push(("word/styles.xml", styles_xml));
    }
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions = zip::write::FileOptions::default();
        for (name, data) in parts {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

/// `word/document.xml` of `bytes` after a tracked edit to the first
/// paragraph only.
fn saved_after_unrelated_edit(bytes: &[u8]) -> String {
    let doc = Document::parse(bytes).expect("parse");
    let paragraph = doc.read().blocks[0].id.to_string();
    let json = serde_json::json!({"ops":[{"op":"replace","target":paragraph,
        "expect":"Edit me here.","content":{"type":"paragraph",
            "content":[{"type":"text","text":"Edit me there."}]}}],
        "revision":{"author":"Clerk"}})
    .to_string();
    let edited = doc
        .apply(
            &stemma::edit_v4::parse_transaction(&json)
                .unwrap()
                .into_edit_transaction()
                .unwrap(),
        )
        .expect("the first paragraph is edited");
    let saved = edited.serialize(&ExportOptions::default()).expect("save");
    String::from_utf8(
        stemma::docx::DocxArchive::read(&saved)
            .unwrap()
            .get("word/document.xml")
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

/// The ST_OnOff state of the first `<w:{name} .../>` in `xml`: `None` when
/// absent, otherwise whether it reads as on (§17.17.4: an omitted w:val is
/// "true"; "0", "false" and "off" are off).
fn on_off_element(xml: &str, name: &str) -> Option<bool> {
    let open = format!("<w:{name}");
    let start = xml
        .match_indices(&open)
        .map(|(at, _)| at)
        .find(|&at| matches!(xml.as_bytes()[at + open.len()], b' ' | b'/' | b'>'))?;
    let element = &xml[start..start + xml[start..].find('>').unwrap()];
    Some(
        !["\"0\"", "\"false\"", "\"off\""]
            .iter()
            .any(|off| element.contains(&format!("w:val={off}"))),
    )
}

const ONE_CELL_ROW: &str = r#"<w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>First</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Second</w:t></w:r></w:p></w:tc></w:tr>"#;

// --- w:bidiVisual (§17.4.1, ST_OnOff) ---------------------------------------

#[test]
fn a_table_with_bidi_visual_off_stays_left_to_right() {
    for off in ["0", "false", "off"] {
        let saved = saved_after_unrelated_edit(&docx(
            &format!(
                r#"<w:tbl><w:tblPr><w:bidiVisual w:val="{off}"/></w:tblPr>{ONE_CELL_ROW}</w:tbl><w:p/>"#
            ),
            None,
        ));
        assert_ne!(
            on_off_element(&saved, "bidiVisual"),
            Some(true),
            "bidiVisual w:val={off:?} is off (§17.17.4); the untouched table \
             must not be written right-to-left: {saved}"
        );
    }
}

#[test]
fn a_table_with_bidi_visual_on_stays_right_to_left() {
    for on in [
        r#"<w:bidiVisual/>"#,
        r#"<w:bidiVisual w:val="1"/>"#,
        r#"<w:bidiVisual w:val="true"/>"#,
    ] {
        let saved = saved_after_unrelated_edit(&docx(
            &format!(r#"<w:tbl><w:tblPr>{on}</w:tblPr>{ONE_CELL_ROW}</w:tbl><w:p/>"#),
            None,
        ));
        assert_eq!(
            on_off_element(&saved, "bidiVisual"),
            Some(true),
            "{on} is on; the untouched table stays right-to-left: {saved}"
        );
    }
}

#[test]
fn a_table_on_off_value_no_schema_form_admits_is_refused() {
    for (property, xml) in [
        (
            "bidiVisual",
            format!(r#"<w:tbl><w:tblPr><w:bidiVisual w:val="maybe"/></w:tblPr>{ONE_CELL_ROW}</w:tbl><w:p/>"#),
        ),
        (
            "noWrap",
            r#"<w:tbl><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:noWrap w:val="maybe"/></w:tcPr><w:p/></w:tc></w:tr></w:tbl><w:p/>"#.to_string(),
        ),
    ] {
        let Err(err) = Document::parse(&docx(&xml, None)) else {
            panic!("{property} w:val=\"maybe\" is no ST_OnOff value; it must not be guessed");
        };
        let message = err.to_string();
        assert!(
            message.contains(property) && message.contains("maybe"),
            "the refusal names the property and value: {message}"
        );
    }
}

/// The element `<w:{name}>` or `<w:{name} ...>` in `xml` that encloses the
/// first occurrence of `text`.
fn enclosing<'a>(xml: &'a str, name: &str, text: &str) -> &'a str {
    let at = xml
        .find(text)
        .unwrap_or_else(|| panic!("{text:?} is saved: {xml}"));
    let start = [format!("<w:{name}>"), format!("<w:{name} ")]
        .iter()
        .filter_map(|open| xml[..at].rfind(open.as_str()))
        .max()
        .unwrap_or_else(|| panic!("{text:?} sits in a w:{name}: {xml}"));
    let close = format!("</w:{name}>");
    &xml[start..at + xml[at..].find(&close).unwrap() + close.len()]
}

// --- w:vertAlign baseline over a style's superscript (§17.3.2.42, §17.7.2) --

const SUPERSCRIPT_PARAGRAPH_STYLE: &str = r#"<w:style w:type="paragraph" w:styleId="Sup"><w:name w:val="Sup"/><w:rPr><w:vertAlign w:val="superscript"/></w:rPr></w:style>"#;

#[test]
fn a_run_set_to_baseline_in_a_superscript_paragraph_style_stays_on_the_baseline() {
    let saved = saved_after_unrelated_edit(&docx(
        r#"<w:p><w:pPr><w:pStyle w:val="Sup"/></w:pPr><w:r><w:rPr><w:vertAlign w:val="baseline"/></w:rPr><w:t>On the baseline.</w:t></w:r></w:p>"#,
        Some(SUPERSCRIPT_PARAGRAPH_STYLE),
    ));
    let run = enclosing(&saved, "r", "On the baseline.");
    assert!(
        run.contains(r#"<w:vertAlign w:val="baseline""#),
        "direct run formatting overrides the paragraph style's superscript \
         (§17.7.2); the untouched run keeps its authored baseline: {run}"
    );
}

#[test]
fn a_paragraph_mark_set_to_baseline_in_a_superscript_paragraph_style_stays_on_the_baseline() {
    let saved = saved_after_unrelated_edit(&docx(
        r#"<w:p><w:pPr><w:pStyle w:val="Sup"/><w:rPr><w:vertAlign w:val="baseline"/></w:rPr></w:pPr><w:r><w:t>Mark on the baseline.</w:t></w:r></w:p>"#,
        Some(SUPERSCRIPT_PARAGRAPH_STYLE),
    ));
    let paragraph = enclosing(&saved, "p", "Mark on the baseline.");
    let mark = enclosing(paragraph, "pPr", "<w:pStyle");
    assert!(
        mark.contains(r#"<w:vertAlign w:val="baseline""#),
        "the paragraph mark's direct baseline overrides the paragraph style's \
         superscript (§17.7.2) and is kept: {paragraph}"
    );
}

// --- Section integers (§17.6, ST_DecimalNumber) ----------------------------

/// The saved `<w:{name} .../>` element of the body's section.
fn saved_section_element(sect_pr: &str, name: &str) -> String {
    let saved = saved_after_unrelated_edit(&docx(&format!("<w:p/>{sect_pr}"), None));
    let sect = &saved[saved
        .rfind("<w:sectPr")
        .expect("the body keeps its section")..];
    let start = sect
        .find(&format!("<w:{name} "))
        .unwrap_or_else(|| panic!("the section keeps w:{name}: {sect}"));
    sect[start..start + sect[start..].find("/>").unwrap()].to_string()
}

#[test]
fn a_negative_document_grid_character_space_is_kept() {
    // §17.6.5: charSpace is ST_DecimalNumber, signed; Word writes negative
    // values to tighten the character pitch of a linesAndChars grid.
    let doc_grid = saved_section_element(
        r#"<w:sectPr><w:pgSz w:w="11906" w:h="16838"/><w:docGrid w:type="linesAndChars" w:linePitch="312" w:charSpace="-3426"/></w:sectPr>"#,
        "docGrid",
    );
    assert!(
        doc_grid.contains(r#"w:charSpace="-3426""#) && doc_grid.contains(r#"w:linePitch="312""#),
        "the grid's character space and line pitch are kept: {doc_grid}"
    );
}

#[test]
fn section_numbers_in_a_decimal_spelling_are_kept_as_integers() {
    // ST_DecimalNumber is an integer; "5.0" is the tolerated float spelling
    // the import reads everywhere else, truncated, never dropped.
    let line_numbering = saved_section_element(
        r#"<w:sectPr><w:lnNumType w:countBy="5.0" w:start="1.0"/></w:sectPr>"#,
        "lnNumType",
    );
    assert!(
        line_numbering.contains(r#"w:countBy="5""#) && line_numbering.contains(r#"w:start="1""#),
        "{line_numbering}"
    );
    let page_numbers = saved_section_element(
        r#"<w:sectPr><w:pgNumType w:start="3.0"/></w:sectPr>"#,
        "pgNumType",
    );
    assert!(page_numbers.contains(r#"w:start="3""#), "{page_numbers}");
}

#[test]
fn a_section_number_no_schema_form_admits_is_refused_with_its_attribute() {
    for (element, attribute, sect_pr) in [
        (
            "docGrid",
            "charSpace",
            r#"<w:sectPr><w:docGrid w:charSpace="tight"/></w:sectPr>"#,
        ),
        (
            "docGrid",
            "linePitch",
            r#"<w:sectPr><w:docGrid w:linePitch="auto"/></w:sectPr>"#,
        ),
        (
            "lnNumType",
            "countBy",
            r#"<w:sectPr><w:lnNumType w:countBy="five"/></w:sectPr>"#,
        ),
        (
            "pgNumType",
            "start",
            r#"<w:sectPr><w:pgNumType w:start="i"/></w:sectPr>"#,
        ),
        (
            "cols",
            "num",
            r#"<w:sectPr><w:cols w:num="two"/></w:sectPr>"#,
        ),
        (
            "top",
            "sz",
            r#"<w:sectPr><w:pgBorders><w:top w:val="single" w:sz="thin"/></w:pgBorders></w:sectPr>"#,
        ),
    ] {
        let Err(err) = Document::parse(&docx(&format!("<w:p/>{sect_pr}"), None)) else {
            panic!("w:{element}/@w:{attribute} in {sect_pr} must be refused, not dropped");
        };
        let message = err.to_string();
        assert!(
            message.contains(element) && message.contains(attribute),
            "the refusal names the element and attribute: {message}"
        );
    }
}

// --- Run sizes (§17.3.2.38 sz, ST_HpsMeasure) -------------------------------

#[test]
fn a_decimal_half_point_size_is_kept_as_an_integer() {
    // ST_HpsMeasure is an unsigned integer of half-points or a positive
    // universal measure; "21.0" is the tolerated decimal spelling the import
    // reads everywhere else, truncated, never dropped to the default size.
    let saved = saved_after_unrelated_edit(&docx(
        r#"<w:p><w:pPr><w:rPr><w:sz w:val="36.0"/></w:rPr></w:pPr><w:r><w:rPr><w:sz w:val="21.0"/><w:szCs w:val="21.0"/></w:rPr><w:t>Ten and a half points.</w:t></w:r></w:p>"#,
        None,
    ));
    let run = enclosing(&saved, "r", "Ten and a half points.");
    assert!(
        run.contains(r#"<w:sz w:val="21""#) && run.contains(r#"<w:szCs w:val="21""#),
        "the run keeps its 10.5pt size: {run}"
    );
    let paragraph = enclosing(&saved, "p", "Ten and a half points.");
    let mark = enclosing(paragraph, "pPr", "<w:sz");
    assert!(
        mark.contains(r#"<w:sz w:val="36""#),
        "the paragraph mark keeps its 18pt size: {paragraph}"
    );
}

#[test]
fn a_universal_measure_size_is_read_in_half_points() {
    let saved = saved_after_unrelated_edit(&docx(
        r#"<w:p><w:r><w:rPr><w:sz w:val="10.5pt"/></w:rPr><w:t>Measured size.</w:t></w:r></w:p>"#,
        None,
    ));
    let run = enclosing(&saved, "r", "Measured size.");
    assert!(
        run.contains(r#"<w:sz w:val="21""#),
        "10.5pt is 21 half-points: {run}"
    );
}

#[test]
fn a_run_size_no_schema_form_admits_is_refused_with_its_element() {
    for (element, rpr) in [
        ("sz", r#"<w:sz w:val="large"/>"#),
        ("szCs", r#"<w:szCs w:val="-4"/>"#),
        ("spacing", r#"<w:spacing w:val="wide"/>"#),
        ("position", r#"<w:position w:val="up"/>"#),
    ] {
        let body = format!(r#"<w:p><w:r><w:rPr>{rpr}</w:rPr><w:t>Sized.</w:t></w:r></w:p>"#);
        let Err(err) = Document::parse(&docx(&body, None)) else {
            panic!("{rpr} admits no schema form; it must be refused, not dropped");
        };
        let message = err.to_string();
        assert!(
            message.contains(element) && message.contains("val"),
            "the refusal names the element and attribute: {message}"
        );
    }
}

// --- Run border (§17.3.2.4 bdr, CT_Border) ----------------------------------

#[test]
fn a_run_border_keeps_exactly_the_attributes_it_authored() {
    // CT_Border's sz, space and color are optional; an absent one takes the
    // schema's meaning (no colour is "auto"), which an invented value such as
    // color="" (not an ST_HexColor) or sz="0" does not preserve.
    let saved = saved_after_unrelated_edit(&docx(
        r#"<w:p><w:r><w:rPr><w:bdr w:val="single"/></w:rPr><w:t>Bordered.</w:t></w:r><w:r><w:rPr><w:bdr w:val="double" w:sz="4" w:space="1" w:color="FF0000"/></w:rPr><w:t>Fully bordered.</w:t></w:r></w:p>"#,
        None,
    ));
    let bare = enclosing(&saved, "r", "Bordered.");
    assert!(
        bare.contains(r#"<w:bdr w:val="single""#)
            && !bare.contains("w:sz=")
            && !bare.contains("w:space=")
            && !bare.contains("w:color="),
        "the border keeps only its style: {bare}"
    );
    let full = enclosing(&saved, "r", "Fully bordered.");
    for attribute in [
        r#"w:val="double""#,
        r#"w:sz="4""#,
        r#"w:space="1""#,
        r#"w:color="FF0000""#,
    ] {
        assert!(full.contains(attribute), "{attribute} is kept: {full}");
    }
}

#[test]
fn a_run_border_width_no_schema_form_admits_is_refused() {
    let Err(err) = Document::parse(&docx(
        r#"<w:p><w:r><w:rPr><w:bdr w:val="single" w:sz="thick"/></w:rPr><w:t>Bordered.</w:t></w:r></w:p>"#,
        None,
    )) else {
        panic!("w:bdr/@w:sz=\"thick\" admits no schema form; it must be refused, not dropped");
    };
    let message = err.to_string();
    assert!(
        message.contains("bdr") && message.contains("sz") && message.contains("thick"),
        "the refusal names the element, attribute and value: {message}"
    );
}
