//! Spellings of one document that another program's save produces, read as
//! the same document under appearance equality; and, beside each, the
//! neighbouring real change, which is still reported.
//!
//! Each spelling below is one a program wrote when it saved a document it
//! did not otherwise change there:
//!
//! - attributes of a held element written in another order;
//! - a content control's numeric id renumbered;
//! - the held properties of a tracked formatting change in another
//!   attribute order;
//! - a bookmark start without an end closed, and bookmarks repeating a name
//!   that is already taken;
//! - a reference to a paragraph style the document does not define dropped;
//! - a run with properties and no content dropped;
//! - footnotes renumbered;
//! - the base64 of a VML shape's graphic data broken with spaces instead of
//!   line feeds;
//! - the properties of a list level written in another order;
//! - an empty run-properties element dropped.

use std::io::{Cursor, Write};

use stemma::api::audit_with_equality;
use stemma::audit::{AuditReport, UntouchedEquality};
use zip::{ZipWriter, write::FileOptions};

const HEAD: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#;
const NAMESPACES: &str = concat!(
    r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" "#,
    r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" "#,
    r#"xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" "#,
    r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" "#,
    r#"xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape" "#,
    r#"xmlns:v="urn:schemas-microsoft-com:vml" "#,
    r#"xmlns:o="urn:schemas-microsoft-com:office:office""#,
);
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const WML: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml";
const NORMAL_STYLE: &str = r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>"#;

/// A document: its body, and the optional parts a case needs.
#[derive(Clone, Default)]
struct Parts {
    body: String,
    /// Paragraph styles beside `Normal`, as `w:style` elements.
    styles: String,
    /// Footnotes beside the separators: (id, content of `w:footnote`).
    footnotes: Vec<(u32, String)>,
    /// Content of `w:numbering`.
    numbering: Option<String>,
    /// A chart part the main part refers to as `rId30`: the id under which
    /// the chart refers to its embedded data, the data's bytes, and whether
    /// the chart part starts with a byte order mark.
    chart: Option<(String, Vec<u8>, bool)>,
}

fn build(parts: &Parts) -> Vec<u8> {
    let mut overrides = format!(
        r#"<Override PartName="/word/document.xml" ContentType="{WML}.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="{WML}.styles+xml"/>"#
    );
    let mut relationships = format!(
        r#"<Relationship Id="rId2" Type="{REL}/styles" Target="styles.xml"/><Relationship Id="rId9" Type="{REL}/hyperlink" Target="https://example.com/timetable" TargetMode="External"/>"#
    );
    let mut files: Vec<(String, Vec<u8>)> = vec![(
        "word/styles.xml".to_string(),
        format!(
            r#"{HEAD}<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{NORMAL_STYLE}{}</w:styles>"#,
            parts.styles
        )
        .into_bytes(),
    )];
    if !parts.footnotes.is_empty() {
        overrides.push_str(&format!(
            r#"<Override PartName="/word/footnotes.xml" ContentType="{WML}.footnotes+xml"/>"#
        ));
        relationships.push_str(&format!(
            r#"<Relationship Id="rId6" Type="{REL}/footnotes" Target="footnotes.xml"/>"#
        ));
        let notes: String = parts
            .footnotes
            .iter()
            .map(|(id, content)| format!(r#"<w:footnote w:id="{id}">{content}</w:footnote>"#))
            .collect();
        files.push((
            "word/footnotes.xml".to_string(),
            format!(
                r#"{HEAD}<w:footnotes {NAMESPACES}><w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote>{notes}</w:footnotes>"#
            )
            .into_bytes(),
        ));
    }
    if let Some(numbering) = &parts.numbering {
        overrides.push_str(&format!(
            r#"<Override PartName="/word/numbering.xml" ContentType="{WML}.numbering+xml"/>"#
        ));
        relationships.push_str(&format!(
            r#"<Relationship Id="rId7" Type="{REL}/numbering" Target="numbering.xml"/>"#
        ));
        files.push((
            "word/numbering.xml".to_string(),
            format!("{HEAD}<w:numbering {NAMESPACES}>{numbering}</w:numbering>").into_bytes(),
        ));
    }
    let relationship_part = |entries: &str| {
        format!(
            r#"{HEAD}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{entries}</Relationships>"#
        )
        .into_bytes()
    };
    if let Some((data_id, data, byte_order_mark)) = &parts.chart {
        overrides.push_str(
            r#"<Override PartName="/word/charts/chart1.xml" ContentType="application/vnd.openxmlformats-officedocument.drawingml.chart+xml"/>"#,
        );
        relationships.push_str(&format!(
            r#"<Relationship Id="rId30" Type="{REL}/chart" Target="charts/chart1.xml"/>"#
        ));
        files.push((
            "word/charts/chart1.xml".to_string(),
            format!(
                r#"{}{HEAD}<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:r="{REL}"><c:chart><c:plotArea><c:layout/></c:plotArea></c:chart><c:externalData r:id="{data_id}"/></c:chartSpace>"#,
                if *byte_order_mark { "\u{feff}" } else { "" }
            )
            .into_bytes(),
        ));
        files.push((
            "word/charts/_rels/chart1.xml.rels".to_string(),
            relationship_part(&format!(
                r#"<Relationship Id="{data_id}" Type="{REL}/package" Target="../embeddings/tides.xlsx"/>"#
            )),
        ));
        files.push(("word/embeddings/tides.xlsx".to_string(), data.clone()));
    }
    files.push((
        "[Content_Types].xml".to_string(),
        format!(
            r#"{HEAD}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="xlsx" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"/>{overrides}</Types>"#
        )
        .into_bytes(),
    ));
    files.push((
        "_rels/.rels".to_string(),
        relationship_part(&format!(
            r#"<Relationship Id="rId1" Type="{REL}/officeDocument" Target="word/document.xml"/>"#
        )),
    ));
    files.push((
        "word/_rels/document.xml.rels".to_string(),
        relationship_part(&relationships),
    ));
    files.push((
        "word/document.xml".to_string(),
        format!(
            "{HEAD}<w:document {NAMESPACES}><w:body>{}<w:sectPr/></w:body></w:document>",
            parts.body
        )
        .into_bytes(),
    ));
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, content) in files {
        zip.start_file(name, FileOptions::default()).expect("start");
        zip.write_all(&content).expect("write");
    }
    zip.finish().expect("finish").into_inner()
}

fn body(body: &str) -> Parts {
    Parts {
        body: body.to_string(),
        ..Parts::default()
    }
}

fn audit(before: &Parts, after: &Parts, equality: UntouchedEquality) -> AuditReport {
    audit_with_equality(&build(before), &build(after), equality).expect("the pair audits")
}

fn is_clean(report: &AuditReport) -> bool {
    report.direct_changes.is_empty() && report.untouched.violations.is_empty()
}

/// Appearance equality reads the pair as one document.
fn assert_same_appearance(before: &Parts, after: &Parts) {
    let report = audit(before, after, UntouchedEquality::Appearance);
    assert!(
        is_clean(&report),
        "nothing reads or looks different: {:?} {:?}",
        report.direct_changes,
        report.untouched.violations
    );
}

/// The pair's markup differs: fidelity equality reports it, appearance
/// equality does not.
fn assert_bookkeeping_only(before: &Parts, after: &Parts) {
    assert!(
        !is_clean(&audit(before, after, UntouchedEquality::Fidelity)),
        "the markup differs, so fidelity equality reports it"
    );
    assert_same_appearance(before, after);
}

/// A change a reader sees: reported under both equalities.
fn assert_reported_by_both(before: &Parts, after: &Parts) {
    for equality in [UntouchedEquality::Fidelity, UntouchedEquality::Appearance] {
        assert!(
            !is_clean(&audit(before, after, equality)),
            "{equality:?} reports the change"
        );
    }
}

/// A hyperlink holding a line break in a run of the given fonts: a run the
/// model holds as its XML.
fn link_with_break(fonts: &str) -> Parts {
    body(&format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Timetable: </w:t></w:r><w:hyperlink r:id="rId9"><w:r><w:rPr><w:rFonts {fonts}/><w:u w:val="single"/></w:rPr><w:br/></w:r><w:r><w:t>harbour ferries</w:t></w:r></w:hyperlink></w:p>"#
    ))
}

#[test]
fn a_held_link_run_with_its_attributes_in_another_order_is_bookkeeping() {
    assert_bookkeeping_only(
        &link_with_break(r#"w:ascii="Georgia" w:eastAsia="Georgia" w:hAnsi="Georgia""#),
        &link_with_break(r#"w:ascii="Georgia" w:hAnsi="Georgia" w:eastAsia="Georgia""#),
    );
    // Another font in that run is another font.
    assert_reported_by_both(
        &link_with_break(r#"w:ascii="Georgia" w:eastAsia="Georgia" w:hAnsi="Georgia""#),
        &link_with_break(r#"w:ascii="Georgia" w:eastAsia="Verdana" w:hAnsi="Georgia""#),
    );
}

/// A paragraph holding a text box whose content is `content` (paragraphs).
fn text_box(content: &str) -> Parts {
    body(&format!(
        concat!(
            r#"<w:p><w:r><w:drawing><wp:anchor distT="0" distB="0" distL="0" distR="0" simplePos="0" relativeHeight="251658240" behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1">"#,
            r#"<wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="column"><wp:posOffset>0</wp:posOffset></wp:positionH>"#,
            r#"<wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV>"#,
            r#"<wp:extent cx="914400" cy="320040"/><wp:effectExtent l="0" t="0" r="0" b="0"/><wp:wrapNone/>"#,
            r#"<wp:docPr id="1" name="Notice"/><wp:cNvGraphicFramePr/>"#,
            r#"<a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp>"#,
            r#"<wps:cNvSpPr txBox="1"/><wps:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="320040"/></a:xfrm>"#,
            r#"<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></wps:spPr>"#,
            r#"<wps:txbx><w:txbxContent>{content}</w:txbxContent></wps:txbx>"#,
            r#"<wps:bodyPr/></wps:wsp></a:graphicData></a:graphic></wp:anchor></w:drawing></w:r></w:p>"#,
        ),
        content = content
    ))
}

fn control(id: &str, text: &str) -> String {
    format!(
        r#"<w:p><w:sdt><w:sdtPr><w:alias w:val="Berth"/><w:tag w:val="Berth"/><w:id w:val="{id}"/><w:text/></w:sdtPr><w:sdtContent><w:r><w:t>{text}</w:t></w:r></w:sdtContent></w:sdt></w:p>"#
    )
}

#[test]
fn a_renumbered_content_control_id_is_bookkeeping() {
    assert_bookkeeping_only(
        &text_box(&control("-1709555926", "Pier 4")),
        &text_box(&control("1", "Pier 4")),
    );
    // What the control holds is content.
    assert_reported_by_both(
        &text_box(&control("-1709555926", "Pier 4")),
        &text_box(&control("1", "Pier 5")),
    );
}

#[test]
fn an_empty_run_properties_element_is_bookkeeping() {
    let paragraph = |properties: &str| {
        text_box(&format!(
            r#"<w:p><w:pPr><w:jc w:val="right"/>{properties}</w:pPr><w:r><w:t>Departures</w:t></w:r></w:p>"#
        ))
    };
    assert_bookkeeping_only(&paragraph("<w:rPr/>"), &paragraph(""));
    // A paragraph mark that was bold no longer is.
    assert_reported_by_both(&paragraph("<w:rPr><w:b/></w:rPr>"), &paragraph(""));
}

/// A run whose tracked formatting change holds previous fonts the model has
/// no typed slot for.
fn tracked_format_change(previous_fonts: &str) -> Parts {
    body(&format!(
        r#"<w:p><w:r><w:rPr><w:b/><w:rPrChange w:id="5" w:author="Ines" w:date="2024-03-01T09:00:00Z"><w:rPr><w:rFonts {previous_fonts}/></w:rPr></w:rPrChange></w:rPr><w:t>The ferry leaves at noon.</w:t></w:r></w:p>"#
    ))
}

#[test]
fn held_previous_properties_in_another_attribute_order_are_bookkeeping() {
    assert_bookkeeping_only(
        &tracked_format_change(
            r#"w:asciiTheme="majorHAnsi" w:hAnsiTheme="majorHAnsi" w:cs="Gill Sans""#,
        ),
        &tracked_format_change(
            r#"w:cs="Gill Sans" w:asciiTheme="majorHAnsi" w:hAnsiTheme="majorHAnsi""#,
        ),
    );
    // Other previous fonts are another revision's content.
    assert_reported_by_both(
        &tracked_format_change(
            r#"w:asciiTheme="majorHAnsi" w:hAnsiTheme="majorHAnsi" w:cs="Gill Sans""#,
        ),
        &tracked_format_change(
            r#"w:asciiTheme="majorHAnsi" w:hAnsiTheme="majorHAnsi" w:cs="Optima""#,
        ),
    );
}

#[test]
fn bookmarks_that_mark_nothing_are_bookkeeping() {
    // Two starts without an end, and a third bookmark of the same name that
    // is closed. Only the third marks anything (§17.13.6.2).
    let before = body(concat!(
        r#"<w:p><w:r><w:t xml:space="preserve">Berth </w:t></w:r>"#,
        r#"<w:bookmarkStart w:id="0" w:name="berth"/><w:bookmarkStart w:id="1" w:name="berth"/>"#,
        r#"<w:bookmarkStart w:id="2" w:name="berth"/><w:bookmarkEnd w:id="2"/>"#,
        r#"<w:r><w:t>four is closed.</w:t></w:r></w:p>"#,
    ));
    // A program closed the open starts where they stood. The first of the
    // three now marks the place; the later two repeat a taken name.
    let closed = body(concat!(
        r#"<w:p><w:r><w:t xml:space="preserve">Berth </w:t></w:r>"#,
        r#"<w:bookmarkStart w:id="0" w:name="berth"/><w:bookmarkEnd w:id="0"/>"#,
        r#"<w:bookmarkStart w:id="1" w:name="berth"/><w:bookmarkEnd w:id="1"/>"#,
        r#"<w:bookmarkStart w:id="2" w:name="berth"/><w:bookmarkEnd w:id="2"/>"#,
        r#"<w:r><w:t>four is closed.</w:t></w:r></w:p>"#,
    ));
    assert_bookkeeping_only(&before, &closed);
    // The bookmark now marks another place.
    let moved = body(concat!(
        r#"<w:p><w:bookmarkStart w:id="2" w:name="berth"/><w:bookmarkEnd w:id="2"/>"#,
        r#"<w:r><w:t xml:space="preserve">Berth </w:t></w:r>"#,
        r#"<w:r><w:t>four is closed.</w:t></w:r></w:p>"#,
    ));
    assert_reported_by_both(&before, &moved);
    // Or another name.
    let renamed = body(concat!(
        r#"<w:p><w:r><w:t xml:space="preserve">Berth </w:t></w:r>"#,
        r#"<w:bookmarkStart w:id="2" w:name="quay"/><w:bookmarkEnd w:id="2"/>"#,
        r#"<w:r><w:t>four is closed.</w:t></w:r></w:p>"#,
    ));
    assert_reported_by_both(&before, &renamed);
}

const HEADING_STYLE: &str = concat!(
    r#"<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/>"#,
    r#"<w:pPr><w:outlineLvl w:val="0"/></w:pPr><w:rPr><w:sz w:val="32"/></w:rPr></w:style>"#,
);

#[test]
fn a_reference_to_an_undefined_style_is_no_style() {
    // §17.3.1.27: a reference to a style that does not exist applies no
    // paragraph style.
    let styled = body(
        r#"<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Winter timetable</w:t></w:r></w:p>"#,
    );
    let unstyled = body(r#"<w:p><w:r><w:t>Winter timetable</w:t></w:r></w:p>"#);
    assert_bookkeeping_only(&styled, &unstyled);
    // Where the document defines the style, dropping it is a change.
    let defined = |parts: &Parts| Parts {
        styles: HEADING_STYLE.to_string(),
        ..parts.clone()
    };
    assert_reported_by_both(&defined(&styled), &defined(&unstyled));
}

#[test]
fn a_run_with_no_content_is_bookkeeping() {
    let empty_run = body(
        r#"<w:p><w:r><w:t xml:space="preserve">Gate </w:t></w:r><w:r><w:rPr><w:b/></w:rPr></w:r><w:r><w:t>seven</w:t></w:r></w:p>"#,
    );
    let without = body(
        r#"<w:p><w:r><w:t xml:space="preserve">Gate </w:t></w:r><w:r><w:t>seven</w:t></w:r></w:p>"#,
    );
    assert_bookkeeping_only(&empty_run, &without);
    // A run holding a space is text.
    let spaced = body(
        r#"<w:p><w:r><w:t>Gate</w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve"> </w:t></w:r><w:r><w:t>seven</w:t></w:r></w:p>"#,
    );
    let joined = body(r#"<w:p><w:r><w:t>Gate</w:t></w:r><w:r><w:t>seven</w:t></w:r></w:p>"#);
    assert_reported_by_both(&spaced, &joined);
}

fn with_notes(first: (u32, &str), second: (u32, &str)) -> Parts {
    let reference = |id: u32| {
        format!(
            r#"<w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:footnoteReference w:id="{id}"/></w:r>"#
        )
    };
    let note = |text: &str| {
        format!(
            r#"<w:p><w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:footnoteRef/></w:r><w:r><w:t xml:space="preserve"> {text}</w:t></w:r></w:p>"#
        )
    };
    Parts {
        body: format!(
            r#"<w:p><w:r><w:t>The ferry runs daily.</w:t></w:r>{}</w:p><w:p><w:r><w:t>The bus does not.</w:t></w:r>{}</w:p>"#,
            reference(first.0),
            reference(second.0)
        ),
        footnotes: vec![(first.0, note(first.1)), (second.0, note(second.1))],
        ..Parts::default()
    }
}

#[test]
fn renumbered_footnotes_are_bookkeeping() {
    let before = with_notes((1, "Except in storms."), (2, "Since spring."));
    assert_bookkeeping_only(
        &before,
        &with_notes((7, "Except in storms."), (8, "Since spring.")),
    );
    // Renumbered, and one note's text changed.
    assert_reported_by_both(
        &before,
        &with_notes((7, "Except in storms."), (8, "Since autumn.")),
    );
    // The same ids, the notes swapped between the references.
    assert_reported_by_both(
        &before,
        &with_notes((1, "Since spring."), (2, "Except in storms.")),
    );
}

fn vml_shape(graphic_data: &str) -> Parts {
    body(&format!(
        r#"<w:p><w:r><w:pict><v:line id="Rule" o:spid="_x0000_s1026" from="0,0" to="400pt,0" o:gfxdata="{graphic_data}" strokeweight="1pt"/></w:pict></w:r></w:p>"#
    ))
}

#[test]
fn white_space_in_base64_graphic_data_is_bookkeeping() {
    assert_bookkeeping_only(
        &vml_shape("UEsDBBQABgAIAAAAIQC2gziS&#xA;/gAAAOEBAAATAAAAW0Nv"),
        &vml_shape("UEsDBBQABgAIAAAAIQC2gziS /gAAAOEBAAATAAAAW0Nv"),
    );
    // Other data is other data.
    assert_reported_by_both(
        &vml_shape("UEsDBBQABgAIAAAAIQC2gziS&#xA;/gAAAOEBAAATAAAAW0Nv"),
        &vml_shape("UEsDBBQABgAIAAAAIQC2gziS&#xA;/gAAAOEBAAATAAAAW0Nw"),
    );
}

fn listed(level_properties: &str) -> Parts {
    Parts {
        body: r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="4"/></w:numPr></w:pPr><w:r><w:t>Check the moorings.</w:t></w:r></w:p>"#.to_string(),
        numbering: Some(format!(
            r#"<w:abstractNum w:abstractNumId="4"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/><w:lvlJc w:val="left"/><w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr><w:rPr>{level_properties}</w:rPr></w:lvl></w:abstractNum><w:num w:numId="4"><w:abstractNumId w:val="4"/></w:num>"#
        )),
        ..Parts::default()
    }
}

#[test]
fn list_level_properties_in_another_order_number_the_same() {
    // The digest of a level is taken under either equality; the order of a
    // property element's children carries nothing under either.
    let before = listed(r#"<w:sz w:val="18"/><w:i/><w:color w:val="000000"/>"#);
    let after = listed(r#"<w:i/><w:color w:val="000000"/><w:sz w:val="18"/>"#);
    for equality in [UntouchedEquality::Fidelity, UntouchedEquality::Appearance] {
        let report = audit(&before, &after, equality);
        assert!(
            is_clean(&report),
            "{equality:?}: the list is numbered the same: {:?}",
            report.untouched.violations
        );
    }
    // A marker of another size is a change.
    assert_reported_by_both(
        &before,
        &listed(r#"<w:i/><w:color w:val="000000"/><w:sz w:val="20"/>"#),
    );
}

/// A paragraph inserted whole by Ines: its text and its mark. `opening` is
/// written before the insertion, `closing` inside it after the text.
fn inserted_paragraph(opening: &str, closing: &str, date: &str) -> Parts {
    body(&format!(
        concat!(
            r#"<w:p><w:r><w:t>The harbour office opens at seven.</w:t></w:r></w:p>"#,
            r#"<w:p><w:pPr><w:rPr><w:ins w:id="7" w:author="Ines" w:date="{date}"/></w:rPr></w:pPr>{opening}"#,
            r#"<w:ins w:id="8" w:author="Ines" w:date="{date}"><w:r><w:t>Ferrys</w:t></w:r>{closing}"#,
            r#"<w:r><w:t xml:space="preserve"> leave from pier four.</w:t></w:r></w:ins></w:p>"#,
        ),
        opening = opening,
        closing = closing,
        date = date
    ))
}

/// The proofing mark that opens at an inserted paragraph's first word is
/// written outside the insertion. It is not content: without it the
/// paragraph is the same inserted paragraph, and its insertion the same
/// pending revision.
#[test]
fn proofing_marks_around_an_inserted_paragraph_are_bookkeeping() {
    use stemma::audit::RevisionDisposition;
    let marked = inserted_paragraph(
        r#"<w:proofErr w:type="spellStart"/>"#,
        r#"<w:proofErr w:type="spellEnd"/>"#,
        "2024-03-01T09:00:00Z",
    );
    let unmarked = inserted_paragraph("", "", "2024-03-01T09:00:00Z");
    for (before, after) in [(&marked, &unmarked), (&unmarked, &marked)] {
        assert_same_appearance(before, after);
        let report = audit(before, after, UntouchedEquality::Appearance);
        assert!(
            report.new_revisions.is_empty(),
            "{:?}",
            report.new_revisions
        );
        assert!(
            report
                .preexisting_revisions
                .iter()
                .all(|row| row.disposition == RevisionDisposition::Untouched),
            "{:?}",
            report.preexisting_revisions
        );
    }
    // The same paragraph inserted at another time is another revision.
    let report = audit(
        &marked,
        &inserted_paragraph("", "", "2024-03-02T09:00:00Z"),
        UntouchedEquality::Appearance,
    );
    assert!(
        report
            .preexisting_revisions
            .iter()
            .any(|row| row.disposition != RevisionDisposition::Untouched),
        "the insertion of the first of March is gone: {:?}",
        report.preexisting_revisions
    );
}

/// A contents entry: a link whose page number is a field. `split` writes
/// each piece of the field in a run of its own, with an empty run among
/// them; otherwise one run holds them all.
fn contents_entry(split: bool, bookmark: &str) -> Parts {
    let properties = r#"<w:rPr><w:webHidden/></w:rPr>"#;
    let pieces = [
        "<w:tab/>".to_string(),
        r#"<w:fldChar w:fldCharType="begin"/>"#.to_string(),
        format!(r#"<w:instrText xml:space="preserve"> PAGEREF {bookmark} \h </w:instrText>"#),
        r#"<w:fldChar w:fldCharType="separate"/>"#.to_string(),
        "<w:t>4</w:t>".to_string(),
        r#"<w:fldChar w:fldCharType="end"/>"#.to_string(),
    ];
    let field = if split {
        let mut runs: Vec<String> = pieces
            .iter()
            .map(|piece| format!("<w:r>{properties}{piece}</w:r>"))
            .collect();
        runs.insert(3, format!("<w:r>{properties}</w:r>"));
        runs.concat()
    } else {
        format!("<w:r>{properties}{}</w:r>", pieces.concat())
    };
    body(&format!(
        r#"<w:p><w:hyperlink w:anchor="_Toc0001" w:history="1"><w:r><w:t>Mooring fees</w:t></w:r>{field}</w:hyperlink></w:p>"#
    ))
}

#[test]
fn a_link_runs_pieces_in_one_run_or_several_are_bookkeeping() {
    assert_bookkeeping_only(
        &contents_entry(true, "_Toc0001"),
        &contents_entry(false, "_Toc0001"),
    );
    // A field that refers elsewhere is another field.
    assert_reported_by_both(
        &contents_entry(true, "_Toc0001"),
        &contents_entry(false, "_Toc0002"),
    );
}

#[test]
fn runs_in_a_text_box_read_as_the_body_reads_runs() {
    let one_run = text_box(
        r#"<w:p><w:r><w:rPr><w:i/></w:rPr><w:t>Tide tables at the office</w:t></w:r></w:p>"#,
    );
    // Split by proofing marks and an empty run, in the same properties.
    let split = text_box(concat!(
        r#"<w:p><w:r><w:rPr><w:i/></w:rPr><w:t xml:space="preserve">Tide </w:t></w:r>"#,
        r#"<w:proofErr w:type="spellStart"/><w:r><w:rPr><w:i/></w:rPr><w:t>tables</w:t></w:r>"#,
        r#"<w:proofErr w:type="spellEnd"/><w:r><w:rPr><w:b/></w:rPr></w:r>"#,
        r#"<w:r><w:rPr><w:i/></w:rPr><w:t xml:space="preserve"> at the office</w:t></w:r></w:p>"#,
    ));
    assert_bookkeeping_only(&split, &one_run);
    // One word in other properties is a formatting change.
    let bold_word = text_box(concat!(
        r#"<w:p><w:r><w:rPr><w:i/></w:rPr><w:t xml:space="preserve">Tide </w:t></w:r>"#,
        r#"<w:r><w:rPr><w:b/></w:rPr><w:t>tables</w:t></w:r>"#,
        r#"<w:r><w:rPr><w:i/></w:rPr><w:t xml:space="preserve"> at the office</w:t></w:r></w:p>"#,
    ));
    assert_reported_by_both(&bold_word, &one_run);
    // A field written one piece per run, or all in one run.
    let field = |instruction: &str, split: bool| {
        let pieces = [
            r#"<w:fldChar w:fldCharType="begin"/>"#.to_string(),
            format!(r#"<w:instrText xml:space="preserve"> {instruction} </w:instrText>"#),
            r#"<w:fldChar w:fldCharType="separate"/>"#.to_string(),
            "<w:t>3</w:t>".to_string(),
            r#"<w:fldChar w:fldCharType="end"/>"#.to_string(),
        ];
        let runs = if split {
            pieces
                .iter()
                .map(|piece| format!("<w:r><w:rPr><w:i/></w:rPr>{piece}</w:r>"))
                .collect()
        } else {
            format!("<w:r><w:rPr><w:i/></w:rPr>{}</w:r>", pieces.concat())
        };
        text_box(&format!(
            r#"<w:p><w:r><w:t xml:space="preserve">Page </w:t></w:r>{runs}</w:p>"#
        ))
    };
    assert_bookkeeping_only(&field("PAGE", true), &field("PAGE", false));
    assert_reported_by_both(&field("PAGE", true), &field("NUMPAGES", false));
}

fn with_chart(data_id: &str, data: &[u8], byte_order_mark: bool) -> Parts {
    Parts {
        body: concat!(
            r#"<w:p><w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="914400" cy="640080"/>"#,
            r#"<wp:effectExtent l="0" t="0" r="0" b="0"/><wp:docPr id="1" name="Tides"/><wp:cNvGraphicFramePr/>"#,
            r#"<a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart">"#,
            r#"<c:chart xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" r:id="rId30"/>"#,
            r#"</a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#,
        )
        .to_string(),
        chart: Some((data_id.to_string(), data.to_vec(), byte_order_mark)),
        ..Parts::default()
    }
}

/// Under either equality: a relationship id inside a part names nothing but
/// its target, as the part's reference to it is read by that target.
#[test]
fn a_part_whose_own_relationships_were_renumbered_is_the_same_part() {
    let data: &[u8] = b"the same workbook bytes on both sides";
    for equality in [UntouchedEquality::Fidelity, UntouchedEquality::Appearance] {
        // Renumbered, and written with a byte order mark.
        let report = audit(
            &with_chart("rId1", data, false),
            &with_chart("R7", data, true),
            equality,
        );
        assert!(
            is_clean(&report),
            "{equality:?}: the chart is the same chart: {:?}",
            report.untouched.violations
        );
    }
    // The chart's data replaced under the same ids is another chart.
    assert_reported_by_both(
        &with_chart("rId1", data, false),
        &with_chart("rId1", b"another workbook", true),
    );
}

/// A text box holding one bulleted paragraph (list 4), and a numbering part
/// whose list 4 marks with `marker` and whose unused list 5 with `unused`.
fn bulleted_text_box(marker: &str, unused: &str) -> Parts {
    let level = |id: u32, text: &str| {
        format!(
            r#"<w:abstractNum w:abstractNumId="{id}"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="{text}"/><w:lvlJc w:val="left"/><w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr></w:lvl></w:abstractNum>"#
        )
    };
    Parts {
        numbering: Some(format!(
            r#"{}{}<w:num w:numId="4"><w:abstractNumId w:val="9"/></w:num><w:num w:numId="5"><w:abstractNumId w:val="10"/></w:num>"#,
            level(9, marker),
            level(10, unused)
        )),
        ..text_box(
            r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="4"/></w:numPr></w:pPr><w:r><w:t>Life jackets under every seat</w:t></w:r></w:p>"#,
        )
    }
}

/// A paragraph in a text box is numbered through the numbering part like a
/// body paragraph: its marker changed there is a change a reader sees.
#[test]
fn a_list_marker_changed_for_a_text_box_paragraph_is_reported() {
    assert_reported_by_both(
        &bulleted_text_box("\u{2022}", "\u{2013}"),
        &bulleted_text_box("#\u{2022}", "\u{2013}"),
    );
    // A list no paragraph uses numbers nothing.
    for equality in [UntouchedEquality::Fidelity, UntouchedEquality::Appearance] {
        let report = audit(
            &bulleted_text_box("\u{2022}", "\u{2013}"),
            &bulleted_text_box("\u{2022}", "#\u{2013}"),
            equality,
        );
        assert!(
            is_clean(&report),
            "{equality:?}: no paragraph uses list 5: {:?}",
            report.untouched.violations
        );
    }
}

/// Word reads a line feed or a carriage return written in a `w:t` as one
/// space each (observed: it saves `one\ntwo` back as `one two`), whether
/// written as such or as `&#13;` / `&#10;`.
#[test]
fn a_line_feed_or_carriage_return_in_text_is_the_space_it_reads_as() {
    let space = body(r#"<w:p><w:r><w:t xml:space="preserve">Pier four </w:t></w:r></w:p>"#);
    for written in [
        "<w:p><w:r><w:t xml:space=\"preserve\">Pier four\r</w:t></w:r></w:p>",
        "<w:p><w:r><w:t xml:space=\"preserve\">Pier four\n</w:t></w:r></w:p>",
        r#"<w:p><w:r><w:t xml:space="preserve">Pier four&#13;</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t xml:space="preserve">Pier four&#10;</w:t></w:r></w:p>"#,
    ] {
        assert_same_text_for_both(&body(written), &space);
    }
}

#[test]
fn a_line_feed_in_text_is_not_a_line_break() {
    let line_feed = body("<w:p><w:r><w:t xml:space=\"preserve\">one\ntwo</w:t></w:r></w:p>");
    let space = body(r#"<w:p><w:r><w:t xml:space="preserve">one two</w:t></w:r></w:p>"#);
    assert_same_text_for_both(&line_feed, &space);
    // A line break is another reading.
    let line_break = body(r#"<w:p><w:r><w:t>one</w:t><w:br/><w:t>two</w:t></w:r></w:p>"#);
    assert_reported_by_both(&line_feed, &line_break);
}

/// The same text under both equalities: nothing reported.
fn assert_same_text_for_both(before: &Parts, after: &Parts) {
    for equality in [UntouchedEquality::Fidelity, UntouchedEquality::Appearance] {
        let report = audit(before, after, equality);
        assert!(
            is_clean(&report),
            "{equality:?}: the same text: {:?} {:?}",
            report.direct_changes,
            report.untouched.violations
        );
    }
}

/// A paragraph inside a block content control whose properties carry the
/// given attributes.
fn block_control(alias: &str, text: &str) -> Parts {
    body(&format!(
        r#"<w:sdt><w:sdtPr><w:alias {alias}/><w:id w:val="7"/></w:sdtPr><w:sdtContent><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:sdtContent></w:sdt><w:p><w:r><w:t>After the control.</w:t></w:r></w:p>"#
    ))
}

#[test]
fn a_block_controls_properties_in_another_attribute_order_are_bookkeeping() {
    let before = block_control(r#"w:val="Berth" w:hidden="0""#, "Pier four");
    let after = block_control(r#"w:hidden="0" w:val="Berth""#, "Pier four");
    assert_same_appearance(&before, &after);
    // What the control holds is content.
    assert_reported_by_both(
        &before,
        &block_control(r#"w:hidden="0" w:val="Berth""#, "Pier five"),
    );
}

/// A picture whose drawing element is written in the default namespace or
/// under the usual prefix.
fn picture(default_namespace: bool, width: &str) -> Parts {
    let inner = format!(
        concat!(
            r#"<wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="{w}" cy="320040"/>"#,
            r#"<wp:effectExtent l="0" t="0" r="0" b="0"/><wp:docPr id="1" name="Chart"/><wp:cNvGraphicFramePr/>"#,
            r#"<a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingShape"><wps:wsp>"#,
            r#"<wps:cNvSpPr/><wps:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="{w}" cy="320040"/></a:xfrm>"#,
            r#"<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></wps:spPr><wps:bodyPr/></wps:wsp></a:graphicData></a:graphic></wp:inline>"#,
        ),
        w = width
    );
    let drawing = if default_namespace {
        format!(
            r#"<drawing xmlns="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{inner}</drawing>"#
        )
    } else {
        format!("<w:drawing>{inner}</w:drawing>")
    };
    body(&format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Tide chart: </w:t></w:r><w:r>{drawing}</w:r></w:p>"#
    ))
}

#[test]
fn the_prefix_a_held_element_is_written_with_is_bookkeeping() {
    assert_bookkeeping_only(&picture(true, "914400"), &picture(false, "914400"));
    // Another width is another picture.
    assert_reported_by_both(&picture(true, "914400"), &picture(false, "1828800"));
}

fn indented_link(indented: bool, text: &str) -> Parts {
    let run = if indented {
        format!(
            "<w:r>\n        <w:rPr>\n          <w:rStyle w:val=\"Hyperlink\"/>\n          <w:sz w:val=\"16\"/>\n        </w:rPr>\n        <w:t>{text}</w:t>\n      </w:r>"
        )
    } else {
        format!(
            r#"<w:r><w:rPr><w:rStyle w:val="Hyperlink"/><w:sz w:val="16"/></w:rPr><w:t>{text}</w:t></w:r>"#
        )
    };
    body(&format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Timetable: </w:t></w:r><w:hyperlink r:id="rId9">{run}</w:hyperlink></w:p>"#
    ))
}

#[test]
fn white_space_laid_out_inside_a_link_run_is_bookkeeping() {
    assert_bookkeeping_only(
        &indented_link(true, "harbour ferries"),
        &indented_link(false, "harbour ferries"),
    );
    assert_reported_by_both(
        &indented_link(true, "harbour ferries"),
        &indented_link(false, "harbour buses"),
    );
}

fn twice_marked(second_id: &str, first_name: &str) -> Parts {
    body(&format!(
        concat!(
            r#"<w:p><w:bookmarkStart w:id="0" w:name="{first}"/><w:bookmarkEnd w:id="0"/><w:r><w:t>Tide tables</w:t></w:r></w:p>"#,
            r#"<w:p><w:bookmarkStart w:id="{second}" w:name="tides"/><w:bookmarkEnd w:id="{second}"/><w:r><w:t>Mooring fees</w:t></w:r></w:p>"#,
        ),
        first = first_name,
        second = second_id
    ))
}

/// Two bookmarks of one name: only the first marks a place (§17.13.6.2),
/// whether the second repeats its id or has its own.
#[test]
fn a_repeated_bookmark_renumbered_is_bookkeeping() {
    assert_same_appearance(&twice_marked("0", "tides"), &twice_marked("1", "tides"));
    assert_reported_by_both(&twice_marked("0", "tides"), &twice_marked("1", "moorings"));
}

fn link_attributes(reversed: bool, frame: &str) -> Parts {
    let attributes = if reversed {
        format!(r#"w:history="1" w:tgtFrame="{frame}""#)
    } else {
        format!(r#"w:tgtFrame="{frame}" w:history="1""#)
    };
    body(&format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Timetable: </w:t></w:r><w:hyperlink r:id="rId9" {attributes}><w:r><w:t>harbour ferries</w:t></w:r></w:hyperlink></w:p>"#
    ))
}

#[test]
fn a_links_attributes_in_another_order_are_bookkeeping() {
    assert_same_appearance(
        &link_attributes(false, "_blank"),
        &link_attributes(true, "_blank"),
    );
    // Opening the link elsewhere is another link.
    assert_reported_by_both(
        &link_attributes(false, "_blank"),
        &link_attributes(true, "_top"),
    );
}

fn vml_shape_id(id: &str, colour: &str) -> Parts {
    body(&format!(
        r#"<w:p><w:r><w:pict><v:rect id="{id}" style="width:40pt;height:20pt" fillcolor="{colour}"/></w:pict></w:r></w:p>"#
    ))
}

#[test]
fn a_renamed_vml_shape_id_is_bookkeeping() {
    assert_same_appearance(
        &vml_shape_id("Box 3", "#00FF00"),
        &vml_shape_id("Box 3_1", "#00FF00"),
    );
    assert_reported_by_both(
        &vml_shape_id("Box 3", "#00FF00"),
        &vml_shape_id("Box 3_1", "#FF0000"),
    );
}

fn equation(runs: &[(&str, &str)]) -> Parts {
    let runs: String = runs
        .iter()
        .map(|(font, text)| {
            format!(r#"<m:r><w:rPr><w:rFonts w:ascii="{font}" w:hAnsi="{font}"/></w:rPr><m:t>{text}</m:t></m:r>"#)
        })
        .collect();
    body(&format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Area: </w:t></w:r><m:oMath xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math">{runs}</m:oMath></w:p>"#
    ))
}

#[test]
fn an_equations_text_in_one_math_run_or_several_is_bookkeeping() {
    assert_same_appearance(
        &equation(&[("Cambria Math", "4"), ("Cambria Math", "πr")]),
        &equation(&[("Cambria Math", "4πr")]),
    );
    // A symbol in another font is another look.
    assert_reported_by_both(
        &equation(&[("Cambria Math", "4"), ("Tahoma", "πr")]),
        &equation(&[("Cambria Math", "4πr")]),
    );
}

fn gridded(history: &str, width: &str) -> Parts {
    body(&format!(
        concat!(
            r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="{width}"/>{history}</w:tblGrid>"#,
            r#"<w:tr><w:tc><w:tcPr><w:tcW w:w="{width}" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>Berth four</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/>"#,
        ),
        history = history,
        width = width
    ))
}

/// A grid history no pending property change owns is discarded by Word on
/// Accept and on Reject alike (§17.13.5.33 gives it no author or date).
#[test]
fn a_grid_history_nothing_owns_is_bookkeeping() {
    let history = r#"<w:tblGridChange w:id="7"><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid></w:tblGridChange>"#;
    assert_same_appearance(&gridded(history, "4000"), &gridded("", "4000"));
    // The grid itself is still compared.
    assert_reported_by_both(&gridded(history, "4000"), &gridded("", "5000"));
}

fn linked(text: &str) -> Parts {
    body(&format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Timetable: </w:t></w:r><w:hyperlink w:anchor="ferries"><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:hyperlink></w:p>"#
    ))
}

/// A link's text is read as a paragraph's: a line feed in it is a space.
#[test]
fn a_line_feed_in_a_links_text_is_the_space_it_reads_as() {
    assert_same_text_for_both(&linked("harbour\nferries"), &linked("harbour ferries"));
    assert_reported_by_both(&linked("harbour\nferries"), &linked("harbour buses"));
}

fn boxed(text: &str) -> Parts {
    body(&format!(
        r#"<w:p><w:r><w:pict><v:rect id="Box 3" style="width:90pt;height:40pt"><v:textbox><w:txbxContent><w:p><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p></w:txbxContent></v:textbox></v:rect></w:pict></w:r></w:p>"#
    ))
}

/// Text held inside an object is read as a paragraph's: a line feed in it is
/// a space.
#[test]
fn a_line_feed_in_held_text_is_the_space_it_reads_as() {
    assert_same_text_for_both(&boxed("Berth\nfour"), &boxed("Berth four"));
    assert_reported_by_both(&boxed("Berth\nfour"), &boxed("Berth five"));
}
