//! What text takes from other parts of the package, changed there alone:
//! the theme's colours, fonts and shape formats where the document uses
//! them, list levels reached through a numbering style or through a
//! paragraph style, a style's tab stops under a tab, formatting of runs
//! held in a field or a note reference, whether a comment is resolved, a
//! data store a content control shows, an embedded font. Each such change
//! is reported; beside each, the neighbouring change that a reader cannot
//! see still passes.

use std::io::{Cursor, Write};

use stemma::api::audit_with_equality;
use stemma::audit::{AuditReport, UntouchedEquality};
use zip::{ZipWriter, write::FileOptions};

const HEAD: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#;
const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const WML: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml";
const NAMESPACES: &str = concat!(
    r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" "#,
    r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" "#,
    r#"xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" "#,
    r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" "#,
    r#"xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape" "#,
    r#"xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" "#,
    r#"xmlns:w15="http://schemas.microsoft.com/office/word/2012/wordml""#,
);
const NORMAL: &str = r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>"#;
const STORE_ID: &str = "{A1B2C3D4-0000-4000-8000-000000000001}";

/// A document: its body and the parts a case needs.
#[derive(Clone)]
struct Doc {
    body: String,
    styles: String,
    /// The theme's colour scheme, font scheme and format scheme.
    accent1: String,
    accent2: String,
    minor_font: String,
    major_font: String,
    fill: String,
    numbering: Option<String>,
    /// Content of `w:fonts`, and the embedded font parts it names.
    fonts: Option<String>,
    comments: Option<String>,
    comments_extended: Option<String>,
    footnotes: Option<String>,
    /// A custom XML data store with the store id [`STORE_ID`].
    store: Option<String>,
    /// A default header: the relationship target its part is named by,
    /// and its content.
    header: Option<(String, String)>,
    /// What the footnote separator draws.
    separator: String,
}

fn doc(body: &str) -> Doc {
    Doc {
        body: body.to_string(),
        styles: NORMAL.to_string(),
        accent1: "4F81BD".to_string(),
        accent2: "C0504D".to_string(),
        minor_font: "Calibri".to_string(),
        major_font: "Cambria".to_string(),
        fill: "FFFFFF".to_string(),
        numbering: None,
        fonts: None,
        comments: None,
        comments_extended: None,
        footnotes: None,
        store: None,
        header: None,
        separator: "<w:separator/>".to_string(),
    }
}

fn build(d: &Doc) -> Vec<u8> {
    let mut overrides = format!(
        concat!(
            r#"<Override PartName="/word/document.xml" ContentType="{wml}.document.main+xml"/>"#,
            r#"<Override PartName="/word/styles.xml" ContentType="{wml}.styles+xml"/>"#,
            r#"<Override PartName="/word/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/>"#,
        ),
        wml = WML
    );
    let mut relationships = format!(
        concat!(
            r#"<Relationship Id="rId2" Type="{rel}/styles" Target="styles.xml"/>"#,
            r#"<Relationship Id="rId4" Type="{rel}/theme" Target="theme/theme1.xml"/>"#,
        ),
        rel = REL
    );
    let theme = format!(
        concat!(
            r#"<a:theme xmlns:a="{a}" name="Harbour"><a:themeElements><a:clrScheme name="Harbour">"#,
            r#"<a:dk1><a:sysClr val="windowText" lastClr="000000"/></a:dk1><a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1>"#,
            r#"<a:dk2><a:srgbClr val="1F497D"/></a:dk2><a:lt2><a:srgbClr val="EEECE1"/></a:lt2>"#,
            r#"<a:accent1><a:srgbClr val="{a1}"/></a:accent1><a:accent2><a:srgbClr val="{a2}"/></a:accent2>"#,
            r#"<a:accent3><a:srgbClr val="9BBB59"/></a:accent3><a:accent4><a:srgbClr val="8064A2"/></a:accent4>"#,
            r#"<a:accent5><a:srgbClr val="4BACC6"/></a:accent5><a:accent6><a:srgbClr val="F79646"/></a:accent6>"#,
            r#"<a:hlink><a:srgbClr val="0000FF"/></a:hlink><a:folHlink><a:srgbClr val="800080"/></a:folHlink></a:clrScheme>"#,
            r#"<a:fontScheme name="Harbour"><a:majorFont><a:latin typeface="{major}"/><a:ea typeface=""/><a:cs typeface=""/></a:majorFont>"#,
            r#"<a:minorFont><a:latin typeface="{minor}"/><a:ea typeface=""/><a:cs typeface=""/></a:minorFont></a:fontScheme>"#,
            r#"<a:fmtScheme name="Harbour"><a:fillStyleLst><a:solidFill><a:srgbClr val="{fill}"/></a:solidFill></a:fillStyleLst>"#,
            r#"<a:lnStyleLst><a:ln w="9525"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln></a:lnStyleLst>"#,
            r#"<a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst>"#,
            r#"<a:bgFillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:bgFillStyleLst></a:fmtScheme>"#,
            r#"</a:themeElements></a:theme>"#,
        ),
        a = A,
        a1 = d.accent1,
        a2 = d.accent2,
        major = d.major_font,
        minor = d.minor_font,
        fill = d.fill
    );
    let mut files: Vec<(String, Vec<u8>)> = vec![
        (
            "word/styles.xml".to_string(),
            format!(r#"{HEAD}<w:styles xmlns:w="{W}">{}</w:styles>"#, d.styles).into_bytes(),
        ),
        (
            "word/theme/theme1.xml".to_string(),
            format!("{HEAD}{theme}").into_bytes(),
        ),
    ];
    let mut part = |path: &str, rel_id: &str, rel_type: &str, content_type: &str, xml: String| {
        overrides.push_str(&format!(
            r#"<Override PartName="/word/{path}" ContentType="{content_type}"/>"#
        ));
        relationships.push_str(&format!(
            r#"<Relationship Id="{rel_id}" Type="{rel_type}" Target="{path}"/>"#
        ));
        files.push((format!("word/{path}"), format!("{HEAD}{xml}").into_bytes()));
    };
    if let Some(numbering) = &d.numbering {
        part(
            "numbering.xml",
            "rId5",
            &format!("{REL}/numbering"),
            &format!("{WML}.numbering+xml"),
            format!("<w:numbering {NAMESPACES}>{numbering}</w:numbering>"),
        );
    }
    if let Some(fonts) = &d.fonts {
        part(
            "fontTable.xml",
            "rId6",
            &format!("{REL}/fontTable"),
            &format!("{WML}.fontTable+xml"),
            format!("<w:fonts {NAMESPACES}>{fonts}</w:fonts>"),
        );
    }
    if let Some(comments) = &d.comments {
        part(
            "comments.xml",
            "rId7",
            &format!("{REL}/comments"),
            &format!("{WML}.comments+xml"),
            format!("<w:comments {NAMESPACES}>{comments}</w:comments>"),
        );
    }
    if let Some(extended) = &d.comments_extended {
        part(
            "commentsExtended.xml",
            "rId8",
            "http://schemas.microsoft.com/office/2011/relationships/commentsExtended",
            &format!("{WML}.commentsExtended+xml"),
            format!("<w15:commentsEx {NAMESPACES}>{extended}</w15:commentsEx>"),
        );
    }
    if let Some(footnotes) = &d.footnotes {
        part(
            "footnotes.xml",
            "rId9",
            &format!("{REL}/footnotes"),
            &format!("{WML}.footnotes+xml"),
            format!(
                r#"<w:footnotes {NAMESPACES}><w:footnote w:type="separator" w:id="-1"><w:p><w:r>{separator}</w:r></w:p></w:footnote><w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote>{footnotes}</w:footnotes>"#,
                separator = d.separator
            ),
        );
    }
    let relationship_part = |entries: &str| {
        format!(
            r#"{HEAD}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{entries}</Relationships>"#
        )
        .into_bytes()
    };
    if let Some(store) = &d.store {
        relationships.push_str(&format!(
            r#"<Relationship Id="rId10" Type="{REL}/customXml" Target="../customXml/item1.xml"/>"#
        ));
        overrides.push_str(
            r#"<Override PartName="/customXml/itemProps1.xml" ContentType="application/vnd.openxmlformats-officedocument.customXmlProperties+xml"/>"#,
        );
        files.push((
            "customXml/item1.xml".to_string(),
            format!("{HEAD}{store}").into_bytes(),
        ));
        files.push((
            "customXml/itemProps1.xml".to_string(),
            format!(
                r#"{HEAD}<ds:datastoreItem ds:itemID="{STORE_ID}" xmlns:ds="http://schemas.openxmlformats.org/officeDocument/2006/customXml"><ds:schemaRefs/></ds:datastoreItem>"#
            )
            .into_bytes(),
        ));
        files.push((
            "customXml/_rels/item1.xml.rels".to_string(),
            relationship_part(&format!(
                r#"<Relationship Id="rId1" Type="{REL}/customXmlProps" Target="itemProps1.xml"/>"#
            )),
        ));
    }
    let mut section = String::from("<w:sectPr/>");
    if let Some((target, header)) = &d.header {
        overrides.push_str(&format!(
            r#"<Override PartName="/word/header1.xml" ContentType="{WML}.header+xml"/>"#
        ));
        relationships.push_str(&format!(
            r#"<Relationship Id="rId11" Type="{REL}/header" Target="{target}"/>"#
        ));
        files.push((
            "word/header1.xml".to_string(),
            format!("{HEAD}<w:hdr {NAMESPACES}>{header}</w:hdr>").into_bytes(),
        ));
        section = r#"<w:sectPr><w:headerReference w:type="default" r:id="rId11"/></w:sectPr>"#
            .to_string();
    }
    files.push((
        "[Content_Types].xml".to_string(),
        format!(
            r#"{HEAD}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="odttf" ContentType="application/vnd.openxmlformats-officedocument.obfuscatedFont"/>{overrides}</Types>"#
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
            "{HEAD}<w:document {NAMESPACES}><w:body>{}{section}</w:body></w:document>",
            d.body
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

fn audit(before: &Doc, after: &Doc, equality: UntouchedEquality) -> AuditReport {
    audit_with_equality(&build(before), &build(after), equality).expect("the pair audits")
}

fn is_clean(report: &AuditReport) -> bool {
    report.direct_changes.is_empty() && report.untouched.violations.is_empty()
}

/// A change a reader sees: reported under both equalities; each side
/// audited against itself is clean.
fn assert_reported(before: &Doc, after: &Doc) {
    for equality in [UntouchedEquality::Fidelity, UntouchedEquality::Appearance] {
        for same in [before, after] {
            let report = audit(same, same, equality);
            assert!(
                is_clean(&report),
                "{equality:?}: a document audited against itself is clean: {:?} {:?}",
                report.direct_changes,
                report.untouched.violations
            );
        }
        assert!(
            !is_clean(&audit(before, after, equality)),
            "{equality:?} reports the change"
        );
    }
}

/// Nothing a reader sees changed: clean under both equalities.
fn assert_clean(before: &Doc, after: &Doc) {
    for equality in [UntouchedEquality::Fidelity, UntouchedEquality::Appearance] {
        let report = audit(before, after, equality);
        assert!(
            is_clean(&report),
            "{equality:?}: nothing reads or looks different: {:?} {:?}",
            report.direct_changes,
            report.untouched.violations
        );
    }
}

const PLAIN: &str = r#"<w:p><w:r><w:t>The harbour office opens at seven.</w:t></w:r></w:p>"#;

#[test]
fn a_theme_colour_a_shading_or_a_border_uses_is_compared() {
    let shaded = doc(concat!(
        r#"<w:p><w:pPr><w:pBdr><w:top w:val="single" w:sz="4" w:space="1" w:color="C0504D" w:themeColor="accent2"/></w:pBdr>"#,
        r#"<w:shd w:val="clear" w:color="auto" w:fill="4F81BD" w:themeFill="accent1"/></w:pPr>"#,
        r#"<w:r><w:t>Tide warning</w:t></w:r></w:p>"#,
    ));
    assert_reported(
        &shaded,
        &Doc {
            accent1: "FF00FF".to_string(),
            ..shaded.clone()
        },
    );
    assert_reported(
        &shaded,
        &Doc {
            accent2: "00FF00".to_string(),
            ..shaded.clone()
        },
    );
    // A colour nothing in the document names.
    let plain = doc(PLAIN);
    assert_clean(
        &plain,
        &Doc {
            accent1: "FF00FF".to_string(),
            ..plain.clone()
        },
    );
}

fn with_default_font(theme_font: &str) -> Doc {
    Doc {
        styles: format!(
            r#"<w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri" w:asciiTheme="{theme_font}" w:hAnsiTheme="{theme_font}"/></w:rPr></w:rPrDefault></w:docDefaults>{NORMAL}"#
        ),
        ..doc(PLAIN)
    }
}

#[test]
fn a_theme_font_the_document_defaults_name_is_compared() {
    let before = with_default_font("minorHAnsi");
    assert_reported(
        &before,
        &Doc {
            minor_font: "Courier New".to_string(),
            ..before.clone()
        },
    );
    // The heading font, which nothing names.
    assert_clean(
        &before,
        &Doc {
            major_font: "Courier New".to_string(),
            ..before.clone()
        },
    );
}

fn shape() -> Doc {
    doc(concat!(
        r#"<w:p><w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="914400" cy="457200"/>"#,
        r#"<wp:effectExtent l="0" t="0" r="0" b="0"/><wp:docPr id="1" name="Buoy"/><wp:cNvGraphicFramePr/>"#,
        r#"<a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp>"#,
        r#"<wps:cNvSpPr/><wps:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="457200"/></a:xfrm><a:prstGeom prst="ellipse"><a:avLst/></a:prstGeom></wps:spPr>"#,
        r#"<wps:style><a:lnRef idx="1"><a:schemeClr val="accent1"/></a:lnRef><a:fillRef idx="1"><a:schemeClr val="accent1"/></a:fillRef>"#,
        r#"<a:effectRef idx="0"><a:schemeClr val="accent1"/></a:effectRef><a:fontRef idx="minor"><a:schemeClr val="lt1"/></a:fontRef></wps:style>"#,
        r#"<wps:bodyPr/></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#,
    ))
}

#[test]
fn the_format_scheme_a_shape_style_refers_to_is_compared() {
    let before = shape();
    assert_reported(
        &before,
        &Doc {
            fill: "FF00FF".to_string(),
            ..before.clone()
        },
    );
    // Without a shape, the format scheme shows nowhere.
    let plain = doc(PLAIN);
    assert_clean(
        &plain,
        &Doc {
            fill: "FF00FF".to_string(),
            ..plain.clone()
        },
    );
}

/// A list whose levels come through a numbering style: list 4's abstract
/// definition links to the style, whose own abstract definition holds the
/// levels.
fn styled_list(marker_colour: &str) -> Doc {
    Doc {
        styles: format!(
            r#"{NORMAL}<w:style w:type="numbering" w:styleId="TideList"><w:name w:val="Tide List"/><w:pPr><w:numPr><w:numId w:val="5"/></w:numPr></w:pPr></w:style>"#
        ),
        numbering: Some(format!(
            concat!(
                r#"<w:abstractNum w:abstractNumId="1"><w:styleLink w:val="TideList"/><w:lvl w:ilvl="0"><w:start w:val="1"/>"#,
                r#"<w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/><w:lvlJc w:val="left"/><w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr>"#,
                r#"<w:rPr><w:color w:val="{colour}"/></w:rPr></w:lvl></w:abstractNum>"#,
                r#"<w:abstractNum w:abstractNumId="2"><w:numStyleLink w:val="TideList"/></w:abstractNum>"#,
                r#"<w:num w:numId="4"><w:abstractNumId w:val="2"/></w:num><w:num w:numId="5"><w:abstractNumId w:val="1"/></w:num>"#,
            ),
            colour = marker_colour
        )),
        ..doc(
            r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="4"/></w:numPr></w:pPr><w:r><w:t>Check the moorings.</w:t></w:r></w:p>"#,
        )
    }
}

#[test]
fn a_list_level_reached_through_a_numbering_style_is_compared() {
    assert_reported(&styled_list("000000"), &styled_list("C00000"));
}

/// A heading style numbered by list 3, through the style's own `w:numPr`
/// and through the list level that names the style.
fn numbered_heading(style_numbering: bool, used: bool) -> Doc {
    let numbering = if style_numbering {
        r#"<w:pPr><w:numPr><w:numId w:val="3"/></w:numPr></w:pPr>"#
    } else {
        ""
    };
    let body = if used {
        r#"<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Winter timetable</w:t></w:r></w:p>"#
    } else {
        PLAIN
    };
    Doc {
        styles: format!(
            r#"{NORMAL}<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/>{numbering}</w:style>"#
        ),
        numbering: Some(concat!(
            r#"<w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/>"#,
            r#"<w:pStyle w:val="Heading1"/><w:lvlText w:val="%1."/><w:lvlJc w:val="left"/></w:lvl></w:abstractNum>"#,
            r#"<w:num w:numId="3"><w:abstractNumId w:val="1"/></w:num>"#,
        )
        .to_string()),
        ..doc(body)
    }
}

#[test]
fn numbering_dropped_from_a_paragraph_style_is_compared() {
    assert_reported(
        &numbered_heading(true, true),
        &numbered_heading(false, true),
    );
    // The same style when no paragraph uses it.
    assert_clean(
        &numbered_heading(true, false),
        &numbered_heading(false, false),
    );
}

fn tabbed(stop: &str, tab: bool) -> Doc {
    let text = if tab {
        r#"<w:r><w:t>Pier</w:t></w:r><w:r><w:tab/><w:t>4</w:t></w:r>"#
    } else {
        r#"<w:r><w:t>Pier 4</w:t></w:r>"#
    };
    Doc {
        styles: format!(
            r#"{NORMAL}<w:style w:type="paragraph" w:styleId="Listing"><w:name w:val="Listing"/><w:pPr><w:tabs><w:tab w:val="left" w:pos="{stop}"/></w:tabs></w:pPr></w:style>"#
        ),
        ..doc(&format!(
            r#"<w:p><w:pPr><w:pStyle w:val="Listing"/></w:pPr>{text}</w:p>"#
        ))
    }
}

#[test]
fn a_styles_tab_stop_moved_under_a_tab_is_compared() {
    assert_reported(&tabbed("567", true), &tabbed("2007", true));
    // Without a tab, a tab stop places nothing.
    assert_clean(&tabbed("567", false), &tabbed("2007", false));
}

fn field_in_style(colour: &str, runs: &str) -> Doc {
    Doc {
        styles: format!(
            r#"{NORMAL}<w:style w:type="paragraph" w:styleId="Running"><w:name w:val="Running"/><w:rPr><w:color w:val="{colour}"/></w:rPr></w:style>"#
        ),
        ..doc(&format!(
            r#"<w:p><w:pPr><w:pStyle w:val="Running"/></w:pPr><w:fldSimple w:instr=" TITLE ">{runs}</w:fldSimple></w:p>"#
        ))
    }
}

#[test]
fn formatting_the_runs_of_a_simple_field_take_from_styles_is_compared() {
    let one = r#"<w:r><w:t>Harbour Rules</w:t></w:r>"#;
    assert_reported(
        &field_in_style("000000", one),
        &field_in_style("C00000", one),
    );
    // The result written in one run or two: another markup, which the
    // fidelity comparison reports, and the same text in the same look.
    let two = r#"<w:r><w:t xml:space="preserve">Harbour </w:t></w:r><w:r><w:t>Rules</w:t></w:r>"#;
    let report = audit(
        &field_in_style("000000", one),
        &field_in_style("000000", two),
        UntouchedEquality::Appearance,
    );
    assert!(is_clean(&report), "{:?}", report.untouched.violations);
}

fn noted(reference_style: &str) -> Doc {
    Doc {
        styles: format!(
            r#"{NORMAL}<w:style w:type="character" w:styleId="NoteMark"><w:name w:val="Note Mark"/><w:rPr>{reference_style}</w:rPr></w:style>"#
        ),
        footnotes: Some(
            r#"<w:footnote w:id="1"><w:p><w:r><w:rPr><w:rStyle w:val="NoteMark"/></w:rPr><w:footnoteRef/></w:r><w:r><w:t xml:space="preserve"> Except in storms.</w:t></w:r></w:p></w:footnote>"#
                .to_string(),
        ),
        ..doc(
            r#"<w:p><w:r><w:t>The ferry runs daily.</w:t></w:r><w:r><w:rPr><w:rStyle w:val="NoteMark"/></w:rPr><w:footnoteReference w:id="1"/></w:r></w:p>"#,
        )
    }
}

#[test]
fn the_style_of_a_note_reference_is_compared() {
    assert_reported(&noted(r#"<w:vertAlign w:val="superscript"/>"#), &noted(""));
}

fn commented(done: &str, reversed: bool) -> Doc {
    let record = if reversed {
        format!(r#"<w15:commentEx w15:done="{done}" w15:paraId="0A0B0C0D"/>"#)
    } else {
        format!(r#"<w15:commentEx w15:paraId="0A0B0C0D" w15:done="{done}"/>"#)
    };
    Doc {
        comments: Some(
            r#"<w:comment w:id="0" w:author="Ines Ramos" w:date="2024-03-01T09:00:00Z" w:initials="IR"><w:p w14:paraId="0A0B0C0D"><w:r><w:t>Check the berth number.</w:t></w:r></w:p></w:comment>"#
                .to_string(),
        ),
        comments_extended: Some(record),
        ..doc(concat!(
            r#"<w:p><w:commentRangeStart w:id="0"/><w:r><w:t>The ferry leaves from pier four.</w:t></w:r>"#,
            r#"<w:commentRangeEnd w:id="0"/><w:r><w:commentReference w:id="0"/></w:r></w:p>"#,
        ))
    }
}

#[test]
fn a_comment_marked_resolved_is_reported() {
    assert_reported(&commented("0", false), &commented("1", false));
    assert_clean(&commented("0", false), &commented("0", true));
}

fn bound(store_title: &str, store_note: &str) -> Doc {
    Doc {
        store: Some(format!(
            r#"<harbour xmlns="urn:example:harbour"><title>{store_title}</title><note>{store_note}</note></harbour>"#
        )),
        ..doc(&format!(
            concat!(
                r#"<w:p><w:sdt><w:sdtPr><w:dataBinding w:prefixMappings="xmlns:ns0='urn:example:harbour'" "#,
                r#"w:xpath="/ns0:harbour[1]/ns0:title[1]" w:storeItemID="{id}"/><w:text/></w:sdtPr>"#,
                r#"<w:sdtContent><w:r><w:t>Harbour Rules</w:t></w:r></w:sdtContent></w:sdt></w:p>"#,
            ),
            id = STORE_ID
        ))
    }
}

#[test]
fn a_store_value_a_content_control_shows_is_compared() {
    // Word fills a bound control from its store on open.
    assert_reported(
        &bound("Harbour Rules", "draft"),
        &bound("Harbour Bylaws", "draft"),
    );
    // A node no control is bound to.
    assert_clean(
        &bound("Harbour Rules", "draft"),
        &bound("Harbour Rules", "final"),
    );
}

fn embedding(faces: &str) -> Doc {
    Doc {
        fonts: Some(format!(
            r#"<w:font w:name="Calibri"/><w:font w:name="Harbour Sans">{faces}</w:font>"#
        )),
        ..doc(PLAIN)
    }
}

#[test]
fn a_dropped_font_embedding_is_reported() {
    let regular =
        r#"<w:embedRegular r:id="rId1" w:fontKey="{00000000-0000-0000-0000-000000000000}"/>"#;
    assert_reported(&embedding(regular), &embedding(""));
    // A font embedded that was not.
    assert_clean(&embedding(""), &embedding(regular));
}

fn headed(target: &str, text: &str) -> Doc {
    Doc {
        header: Some((
            target.to_string(),
            format!("<w:p><w:r><w:t>{text}</w:t></w:r></w:p>"),
        )),
        ..doc(PLAIN)
    }
}

#[test]
fn a_header_part_named_absolute_or_relative_is_the_same_part() {
    for equality in [UntouchedEquality::Fidelity, UntouchedEquality::Appearance] {
        let report = audit(
            &headed("/word/header1.xml", "Harbour Rules"),
            &headed("header1.xml", "Harbour Rules"),
            equality,
        );
        assert!(
            is_clean(&report),
            "{equality:?}: the same header: {:?}",
            report.direct_changes
        );
    }
    // The header's text is the header's.
    assert_reported(
        &headed("/word/header1.xml", "Harbour Rules"),
        &headed("header1.xml", "Harbour Bylaws"),
    );
}

/// A paragraph with a pending insertion before or after the word "daily".
fn pending_insertion(after_the_word: bool) -> Doc {
    let insertion = r#"<w:ins w:id="5" w:author="Ines" w:date="2024-03-01T09:00:00Z"><w:r><w:t xml:space="preserve">twice </w:t></w:r></w:ins>"#;
    let word = r#"<w:r><w:t xml:space="preserve">daily </w:t></w:r>"#;
    let (first, second) = if after_the_word {
        (word, insertion)
    } else {
        (insertion, word)
    };
    doc(&format!(
        r#"<w:p><w:r><w:t xml:space="preserve">The ferry runs </w:t></w:r>{first}{second}<w:r><w:t>from pier four.</w:t></w:r></w:p>"#
    ))
}

/// A pending revision is where it stands: moved, the document reads
/// differently once it is accepted, though the revision itself (author,
/// date, text) and the rejected reading are unchanged.
#[test]
fn a_pending_revision_moved_elsewhere_in_its_paragraph_is_reported() {
    assert_reported(&pending_insertion(false), &pending_insertion(true));
}

fn styled_table(style_xml: &str, look: &str, style_ref: &str) -> Doc {
    Doc {
        styles: format!("{NORMAL}{style_xml}"),
        ..doc(&format!(
            concat!(
                r#"<w:tbl><w:tblPr>{style_ref}<w:tblW w:w="0" w:type="auto"/>{look}</w:tblPr><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid>"#,
                r#"<w:tr><w:tc><w:tcPr><w:tcW w:w="4000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>Berth</w:t></w:r></w:p></w:tc></w:tr>"#,
                r#"<w:tr><w:tc><w:tcPr><w:tcW w:w="4000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>Pier four</w:t></w:r></w:p></w:tc></w:tr>"#,
                r#"</w:tbl><w:p/>"#,
            ),
            style_ref = style_ref,
            look = look
        ))
    }
}

fn harbour_grid(first_row: &str) -> String {
    format!(
        r#"<w:style w:type="table" w:default="1" w:styleId="HarbourGrid"><w:name w:val="Harbour Grid"/><w:tblPr><w:tblBorders><w:top w:val="single" w:sz="4" w:space="0" w:color="auto"/></w:tblBorders></w:tblPr><w:tblStylePr w:type="firstRow"><w:rPr>{first_row}</w:rPr></w:tblStylePr></w:style>"#
    )
}

#[test]
fn what_a_table_takes_from_its_table_style_is_compared() {
    // The document's default table style, which a table without
    // `w:tblStyle` takes.
    let reference = "";
    let first_row_on = r#"<w:tblLook w:val="0020" w:firstRow="1" w:lastRow="0" w:firstColumn="0" w:lastColumn="0" w:noHBand="1" w:noVBand="1"/>"#;
    let first_row_off = r#"<w:tblLook w:val="0000" w:firstRow="0" w:lastRow="0" w:firstColumn="0" w:lastColumn="0" w:noHBand="1" w:noVBand="1"/>"#;
    // The header row's formatting, which the table's look turns on.
    assert_reported(
        &styled_table(&harbour_grid("<w:b/>"), first_row_on, reference),
        &styled_table(
            &harbour_grid(r#"<w:color w:val="C00000"/>"#),
            first_row_on,
            reference,
        ),
    );
    // With the header row turned off in the look, it formats nothing.
    assert_clean(
        &styled_table(&harbour_grid("<w:b/>"), first_row_off, reference),
        &styled_table(
            &harbour_grid(r#"<w:color w:val="C00000"/>"#),
            first_row_off,
            reference,
        ),
    );
}

#[test]
fn the_default_table_style_formats_a_table_without_a_style() {
    let default_style = |size: &str| {
        format!(
            r#"<w:style w:type="table" w:default="1" w:styleId="PlainTable"><w:name w:val="Normal Table"/><w:rPr><w:sz w:val="{size}"/></w:rPr><w:tblPr><w:tblCellMar><w:left w:w="108" w:type="dxa"/></w:tblCellMar></w:tblPr></w:style>"#
        )
    };
    assert_reported(
        &styled_table(&default_style("22"), "", ""),
        &styled_table(&default_style("32"), "", ""),
    );
    // Word ignores what a style with the id TableNormal sets.
    let ignored = |size: &str| default_style(size).replace("PlainTable", "TableNormal");
    assert_clean(
        &styled_table(&ignored("22"), "", ""),
        &styled_table(&ignored("32"), "", ""),
    );
}

fn right_to_left(bidi: &str) -> Doc {
    Doc {
        styles: format!(
            r#"{NORMAL}<w:style w:type="paragraph" w:styleId="Notice"><w:name w:val="Notice"/><w:pPr>{bidi}<w:keepNext/></w:pPr></w:style>"#
        ),
        ..doc(
            r#"<w:p><w:pPr><w:pStyle w:val="Notice"/></w:pPr><w:r><w:t>Tide warning</w:t></w:r></w:p>"#,
        )
    }
}

#[test]
fn paragraph_properties_a_style_gives_are_compared() {
    assert_reported(&right_to_left(""), &right_to_left("<w:bidi/>"));
    // The same properties written in another order.
    let reordered = Doc {
        styles: format!(
            r#"{NORMAL}<w:style w:type="paragraph" w:styleId="Notice"><w:name w:val="Notice"/><w:pPr><w:keepNext/><w:bidi/></w:pPr></w:style>"#
        ),
        ..right_to_left("<w:bidi/>")
    };
    assert_clean(&right_to_left("<w:bidi/>"), &reordered);
}

fn running_head(style_name: &str) -> Doc {
    Doc {
        styles: format!(
            r#"{NORMAL}<w:style w:type="paragraph" w:styleId="ActName"><w:name w:val="{style_name}"/></w:style>"#
        ),
        header: Some((
            "header1.xml".to_string(),
            r#"<w:p><w:fldSimple w:instr=" STYLEREF &quot;Name of Act&quot; "><w:r><w:t>Harbour Act</w:t></w:r></w:fldSimple></w:p>"#.to_string(),
        )),
        ..doc(r#"<w:p><w:pPr><w:pStyle w:val="ActName"/></w:pPr><w:r><w:t>Harbour Act</w:t></w:r></w:p>"#)
    }
}

/// Word recomputes a `STYLEREF` field in a header as it lays the page out:
/// a style renamed away from the name the field gives breaks it.
#[test]
fn a_style_a_styleref_field_names_renamed_is_reported() {
    assert_reported(&running_head("Name of Act"), &running_head("Name of Act 2"));
    // Its name written in another case still names it.
    assert_clean(&running_head("Name of Act"), &running_head("name of act"));
}

fn with_separator(separator: &str) -> Doc {
    Doc {
        separator: separator.to_string(),
        ..noted(r#"<w:vertAlign w:val="superscript"/>"#)
    }
}

#[test]
fn a_changed_footnote_separator_is_reported() {
    assert_reported(
        &with_separator("<w:separator/>"),
        &with_separator("<w:continuationSeparator/>"),
    );
    assert_clean(
        &with_separator("<w:separator/>"),
        &with_separator("<w:separator></w:separator>"),
    );
}

fn reply(parent: bool) -> Doc {
    let link = if parent {
        r#" w15:paraIdParent="0A0B0C0D""#
    } else {
        ""
    };
    Doc {
        comments: Some(concat!(
            r#"<w:comment w:id="0" w:author="Ines Ramos" w:date="2024-03-01T09:00:00Z" w:initials="IR"><w:p w14:paraId="0A0B0C0D"><w:r><w:t>Check the berth number.</w:t></w:r></w:p></w:comment>"#,
            r#"<w:comment w:id="1" w:author="Ola Berg" w:date="2024-03-02T09:00:00Z" w:initials="OB"><w:p w14:paraId="0E0F0A0B"><w:r><w:t>It is four.</w:t></w:r></w:p></w:comment>"#,
        ).to_string()),
        comments_extended: Some(format!(
            r#"<w15:commentEx w15:paraId="0A0B0C0D" w15:done="0"/><w15:commentEx w15:paraId="0E0F0A0B"{link} w15:done="0"/>"#
        )),
        ..doc(concat!(
            r#"<w:p><w:commentRangeStart w:id="0"/><w:commentRangeStart w:id="1"/><w:r><w:t>The ferry leaves from pier four.</w:t></w:r>"#,
            r#"<w:commentRangeEnd w:id="0"/><w:commentRangeEnd w:id="1"/><w:r><w:commentReference w:id="0"/></w:r><w:r><w:commentReference w:id="1"/></w:r></w:p>"#,
        ))
    }
}

#[test]
fn a_reply_detached_from_its_comment_is_reported() {
    assert_reported(&reply(true), &reply(false));
}

fn tracked_comment(author: &str) -> Doc {
    Doc {
        comments: Some(format!(
            r#"<w:comment w:id="0" w:author="{author}" w:date="2024-03-01T09:00:00Z" w:initials="IR"><w:p><w:r><w:t>Check the berth number.</w:t></w:r></w:p></w:comment>"#
        )),
        ..doc(concat!(
            r#"<w:p><w:r><w:t xml:space="preserve">The ferry leaves </w:t></w:r><w:commentRangeStart w:id="0"/>"#,
            r#"<w:ins w:id="5" w:author="Ines" w:date="2024-03-01T09:00:00Z"><w:r><w:t>from pier four</w:t></w:r></w:ins>"#,
            r#"<w:commentRangeEnd w:id="0"/><w:ins w:id="6" w:author="Ines" w:date="2024-03-01T09:00:00Z"><w:r><w:commentReference w:id="0"/></w:r></w:ins>"#,
            r#"<w:r><w:t>.</w:t></w:r></w:p>"#,
        ))
    }
}

/// A comment whose anchor is itself a pending insertion is shown until the
/// insertion is rejected; who wrote it is compared all the same.
#[test]
fn a_comment_anchored_in_a_pending_insertion_is_compared() {
    assert_reported(
        &tracked_comment("Ines Ramos"),
        &tracked_comment("Somebody Else"),
    );
}

/// A heading typed with its number and a tab, in a style whose tab stop
/// has the given leader.
fn typed_heading(leader: &str) -> Doc {
    Doc {
        styles: format!(
            r#"{NORMAL}<w:style w:type="paragraph" w:styleId="Clause"><w:name w:val="Clause"/><w:pPr><w:tabs><w:tab w:val="left" w:pos="879"{leader}/></w:tabs><w:ind w:left="879" w:hanging="879"/></w:pPr></w:style>"#
        ),
        ..doc(
            r#"<w:p><w:pPr><w:pStyle w:val="Clause"/></w:pPr><w:r><w:t>4.1</w:t></w:r><w:r><w:tab/><w:t>Mooring Fees</w:t></w:r></w:p>"#,
        )
    }
}

#[test]
fn a_tab_leader_after_a_typed_number_is_compared() {
    assert_reported(&typed_heading(""), &typed_heading(r#" w:leader="dot""#));
}
