//! The audit sees untracked changes that live outside a body paragraph's own
//! text: in a header, footer or note; behind a relationship; in the
//! numbering part.
//!
//! Each case is a before/after pair with no tracked change, differing in one
//! thing a reader of the document would see. Under either equality the audit
//! must report it. The cases were found by altering real documents one way at
//! a time and counting what the audit let through:
//!
//! - text inside a hyperlink or a text box in a header or a footnote, and
//!   formatting in a header. A story whose blocks differ at all was treated
//!   as accounted for by the text diff, which does not see these;
//! - an image replaced by another under the same relationship id, in the
//!   body or in a header. The reference is unchanged; the picture is not;
//! - the marker text of a list level changed in the numbering part. The
//!   paragraph's reference to the list is unchanged; what it resolves to is
//!   not.

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
    r#"xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture" "#,
    r#"xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape""#,
);
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const WML: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml";
const BODY: &str = r#"<w:p><w:r><w:t>The harbour office opens at seven.</w:t></w:r></w:p>"#;

/// A document with optional header, footnote and numbering parts.
#[derive(Clone, Default)]
struct Parts {
    body: Option<String>,
    /// Content of `w:hdr`.
    header: Option<String>,
    /// Content of footnote 1.
    footnote: Option<String>,
    /// Content of `w:numbering`.
    numbering: Option<String>,
    /// An image the main part refers to as `rId20`.
    body_image: Option<Vec<u8>>,
    /// An image the header refers to as `rId20`.
    header_image: Option<Vec<u8>>,
}

fn build(parts: &Parts) -> Vec<u8> {
    let mut overrides = format!(
        r#"<Override PartName="/word/document.xml" ContentType="{WML}.document.main+xml"/>"#
    );
    let mut relationships = String::new();
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut section = String::from("<w:sectPr/>");
    let relationship_part = |entries: &str| {
        format!(
            r#"{HEAD}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{entries}</Relationships>"#
        )
        .into_bytes()
    };
    let image_relationship =
        format!(r#"<Relationship Id="rId20" Type="{REL}/image" Target="media/"#);
    if let Some(header) = &parts.header {
        overrides.push_str(&format!(
            r#"<Override PartName="/word/header1.xml" ContentType="{WML}.header+xml"/>"#
        ));
        relationships.push_str(&format!(
            r#"<Relationship Id="rId5" Type="{REL}/header" Target="header1.xml"/>"#
        ));
        section =
            r#"<w:sectPr><w:headerReference w:type="default" r:id="rId5"/></w:sectPr>"#.to_string();
        files.push((
            "word/header1.xml".to_string(),
            format!("{HEAD}<w:hdr {NAMESPACES}>{header}</w:hdr>").into_bytes(),
        ));
        if let Some(image) = &parts.header_image {
            files.push((
                "word/_rels/header1.xml.rels".to_string(),
                relationship_part(&format!(r#"{image_relationship}crest.png"/>"#)),
            ));
            files.push(("word/media/crest.png".to_string(), image.clone()));
        }
    }
    if let Some(footnote) = &parts.footnote {
        overrides.push_str(&format!(
            r#"<Override PartName="/word/footnotes.xml" ContentType="{WML}.footnotes+xml"/>"#
        ));
        relationships.push_str(&format!(
            r#"<Relationship Id="rId6" Type="{REL}/footnotes" Target="footnotes.xml"/>"#
        ));
        files.push((
            "word/footnotes.xml".to_string(),
            format!(
                r#"{HEAD}<w:footnotes {NAMESPACES}><w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote><w:footnote w:id="1">{footnote}</w:footnote></w:footnotes>"#
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
    if let Some(image) = &parts.body_image {
        relationships.push_str(&format!(r#"{image_relationship}chart.png"/>"#));
        files.push(("word/media/chart.png".to_string(), image.clone()));
    }
    let body = parts.body.clone().unwrap_or_else(|| BODY.to_string());
    files.push((
        "[Content_Types].xml".to_string(),
        format!(
            r#"{HEAD}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/>{overrides}</Types>"#
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
        format!("{HEAD}<w:document {NAMESPACES}><w:body>{body}{section}</w:body></w:document>")
            .into_bytes(),
    ));
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, content) in files {
        zip.start_file(name, FileOptions::default()).expect("start");
        zip.write_all(&content).expect("write");
    }
    zip.finish().expect("finish").into_inner()
}

fn is_clean(report: &AuditReport) -> bool {
    report.direct_changes.is_empty() && report.untouched.violations.is_empty()
}

/// The pair differs in something a reader sees: both equalities report it,
/// and each side audited against itself is clean.
fn assert_reported(before: &Parts, after: &Parts) {
    let (before, after) = (build(before), build(after));
    for equality in [UntouchedEquality::Fidelity, UntouchedEquality::Appearance] {
        for same in [&before, &after] {
            assert!(
                is_clean(&audit_with_equality(same, same, equality).expect("audit")),
                "{equality:?}: a document audited against itself is clean"
            );
        }
        assert!(
            !is_clean(&audit_with_equality(&before, &after, equality).expect("audit")),
            "{equality:?} reports the change"
        );
    }
}

fn paragraph(text: &str) -> String {
    format!("<w:p><w:r><w:t>{text}</w:t></w:r></w:p>")
}

fn link(text: &str) -> String {
    format!(
        r#"<w:p><w:r><w:t xml:space="preserve">See </w:t></w:r><w:hyperlink w:anchor="tides"><w:r><w:t>{text}</w:t></w:r></w:hyperlink></w:p>"#
    )
}

fn text_box(text: &str) -> String {
    format!(
        concat!(
            r#"<w:p><w:r><w:drawing><wp:anchor distT="0" distB="0" distL="0" distR="0" simplePos="0" relativeHeight="251658240" behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1">"#,
            r#"<wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="column"><wp:posOffset>0</wp:posOffset></wp:positionH>"#,
            r#"<wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV>"#,
            r#"<wp:extent cx="914400" cy="320040"/><wp:effectExtent l="0" t="0" r="0" b="0"/><wp:wrapNone/>"#,
            r#"<wp:docPr id="1" name="Notice"/><wp:cNvGraphicFramePr/>"#,
            r#"<a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp>"#,
            r#"<wps:cNvSpPr txBox="1"/><wps:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="320040"/></a:xfrm>"#,
            r#"<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></wps:spPr>"#,
            r#"<wps:txbx><w:txbxContent><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:txbxContent></wps:txbx>"#,
            r#"<wps:bodyPr/></wps:wsp></a:graphicData></a:graphic></wp:anchor></w:drawing></w:r></w:p>"#,
        ),
        text = text
    )
}

fn picture() -> String {
    concat!(
        r#"<w:p><w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="914400" cy="320040"/>"#,
        r#"<wp:effectExtent l="0" t="0" r="0" b="0"/><wp:docPr id="1" name="Tide chart"/><wp:cNvGraphicFramePr/>"#,
        r#"<a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic>"#,
        r#"<pic:nvPicPr><pic:cNvPr id="0" name="Tide chart"/><pic:cNvPicPr/></pic:nvPicPr>"#,
        r#"<pic:blipFill><a:blip r:embed="rId20"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>"#,
        r#"<pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="320040"/></a:xfrm>"#,
        r#"<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic>"#,
        r#"</wp:inline></w:drawing></w:r></w:p>"#,
    )
    .to_string()
}

const WITH_NOTE: &str = r#"<w:p><w:r><w:t>The harbour office opens at seven.</w:t></w:r><w:r><w:footnoteReference w:id="1"/></w:r></w:p>"#;

#[test]
fn changed_header_text_is_reported() {
    for (before, after) in [
        (paragraph("Port notice"), paragraph("Port advice")),
        (link("the tide table"), link("the time table")),
        (text_box("harbour office"), text_box("harbour master")),
    ] {
        assert_reported(
            &Parts {
                header: Some(before),
                ..Parts::default()
            },
            &Parts {
                header: Some(after),
                ..Parts::default()
            },
        );
    }
}

#[test]
fn changed_header_formatting_is_reported() {
    assert_reported(
        &Parts {
            header: Some(paragraph("Port notice")),
            ..Parts::default()
        },
        &Parts {
            header: Some(
                r#"<w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Port notice</w:t></w:r></w:p>"#.to_string(),
            ),
            ..Parts::default()
        },
    );
}

#[test]
fn changed_footnote_text_is_reported() {
    for (before, after) in [
        (paragraph("Port notice"), paragraph("Port advice")),
        (link("the tide table"), link("the time table")),
    ] {
        let with_note = |footnote: String| Parts {
            body: Some(WITH_NOTE.to_string()),
            footnote: Some(footnote),
            ..Parts::default()
        };
        assert_reported(&with_note(before), &with_note(after));
    }
}

#[test]
fn an_image_replaced_under_the_same_id_is_reported() {
    let in_body = |image: &[u8]| Parts {
        body: Some(picture()),
        body_image: Some(image.to_vec()),
        ..Parts::default()
    };
    assert_reported(&in_body(b"the first chart"), &in_body(b"another chart"));

    let in_header = |image: &[u8]| Parts {
        header: Some(picture()),
        header_image: Some(image.to_vec()),
        ..Parts::default()
    };
    assert_reported(&in_header(b"the first crest"), &in_header(b"another crest"));
}

#[test]
fn a_changed_list_marker_is_reported() {
    let listed = |marker: &str| {
        Parts {
        body: Some(
            r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Check the mooring lines.</w:t></w:r></w:p>"#
                .to_string(),
        ),
        numbering: Some(format!(
            r#"<w:abstractNum w:abstractNumId="0"><w:multiLevelType w:val="hybridMultilevel"/><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="{marker}"/><w:lvlJc w:val="left"/><w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#
        )),
        ..Parts::default()
    }
    };
    assert_reported(&listed("%1."), &listed("Step %1:"));
}

/// The same list level spelled another way (attributes in another order,
/// another namespace declared on the part, white space between elements) is
/// the same definition: a program that rewrites the numbering part without
/// changing it has changed nothing.
#[test]
fn a_list_level_spelled_another_way_is_not_a_change() {
    let body = r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Check the mooring lines.</w:t></w:r></w:p>"#;
    let listed = |numbering: &str| Parts {
        body: Some(body.to_string()),
        numbering: Some(numbering.to_string()),
        ..Parts::default()
    };
    let before = listed(
        r#"<w:abstractNum w:abstractNumId="0"><w:multiLevelType w:val="hybridMultilevel"/><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="o"/><w:lvlJc w:val="left"/><w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr><w:rPr><w:rFonts w:ascii="Symbol" w:hAnsi="Symbol" w:hint="default"/></w:rPr></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#,
    );
    let after = listed(
        r#"<w:abstractNum w:abstractNumId="0"> <w:multiLevelType w:val="hybridMultilevel" /> <w:lvl w:ilvl="0"> <w:start w:val="1" /> <w:numFmt w:val="bullet" /> <w:lvlText w:val="o" /> <w:lvlJc w:val="left" /> <w:pPr> <w:ind w:hanging="360" w:left="720" /> </w:pPr> <w:rPr> <w:rFonts w:hint="default" w:ascii="Symbol" w:hAnsi="Symbol" /> </w:rPr> </w:lvl> </w:abstractNum> <w:num w:numId="1"> <w:abstractNumId w:val="0" /> </w:num>"#,
    );
    let (before, after) = (build(&before), build(&after));
    for equality in [UntouchedEquality::Fidelity, UntouchedEquality::Appearance] {
        assert!(
            is_clean(&audit_with_equality(&before, &after, equality).expect("audit")),
            "{equality:?}: the list is defined as it was"
        );
    }
}
