//! What a reader sees that the block does not hold: formatting that
//! reaches text through the theme or the styles part, the document's
//! settings and background, who wrote a comment, the formatting of a typed
//! list label. Each untracked change there is reported; beside each, the
//! neighbouring spelling that changes nothing still passes.

use std::io::{Cursor, Write};

use stemma::api::audit_with_equality;
use stemma::audit::{AuditReport, UntouchedEquality};
use zip::{ZipWriter, write::FileOptions};

const HEAD: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#;
const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const WML: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml";

/// A document: its body and the parts a case needs.
#[derive(Clone)]
struct Parts {
    body: String,
    /// Written between `<w:document>` and `<w:body>` (a background).
    before_body: String,
    /// Content of `w:styles`.
    styles: String,
    /// Content of `w:settings`.
    settings: String,
    /// Content of `a:clrScheme`.
    colours: String,
    /// Content of `w:comments`, when the document has comments.
    comments: Option<String>,
}

const NORMAL: &str = r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>"#;
const LINK_STYLE: &str = r#"<w:style w:type="character" w:styleId="Hyperlink"><w:name w:val="Hyperlink"/><w:rPr><w:color w:val="0563C1"/><w:u w:val="single"/></w:rPr></w:style>"#;
const SETTINGS: &str = r#"<w:zoom w:percent="100"/><w:defaultTabStop w:val="720"/><w:characterSpacingControl w:val="doNotCompress"/>"#;

fn colours(accent1: &str, accent2: &str) -> String {
    format!(
        concat!(
            r#"<a:dk1><a:sysClr val="windowText" lastClr="000000"/></a:dk1><a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1>"#,
            r#"<a:dk2><a:srgbClr val="1F497D"/></a:dk2><a:lt2><a:srgbClr val="EEECE1"/></a:lt2>"#,
            r#"<a:accent1><a:srgbClr val="{a1}"/></a:accent1><a:accent2><a:srgbClr val="{a2}"/></a:accent2>"#,
            r#"<a:accent3><a:srgbClr val="9BBB59"/></a:accent3><a:accent4><a:srgbClr val="8064A2"/></a:accent4>"#,
            r#"<a:accent5><a:srgbClr val="4BACC6"/></a:accent5><a:accent6><a:srgbClr val="F79646"/></a:accent6>"#,
            r#"<a:hlink><a:srgbClr val="0000FF"/></a:hlink><a:folHlink><a:srgbClr val="800080"/></a:folHlink>"#,
        ),
        a1 = accent1,
        a2 = accent2
    )
}

fn document(body: &str) -> Parts {
    Parts {
        body: body.to_string(),
        before_body: String::new(),
        styles: format!("{NORMAL}{LINK_STYLE}"),
        settings: SETTINGS.to_string(),
        colours: colours("4F81BD", "C0504D"),
        comments: None,
    }
}

fn build(parts: &Parts) -> Vec<u8> {
    let mut overrides = format!(
        concat!(
            r#"<Override PartName="/word/document.xml" ContentType="{wml}.document.main+xml"/>"#,
            r#"<Override PartName="/word/styles.xml" ContentType="{wml}.styles+xml"/>"#,
            r#"<Override PartName="/word/settings.xml" ContentType="{wml}.settings+xml"/>"#,
            r#"<Override PartName="/word/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/>"#,
        ),
        wml = WML
    );
    let mut relationships = format!(
        concat!(
            r#"<Relationship Id="rId2" Type="{rel}/styles" Target="styles.xml"/>"#,
            r#"<Relationship Id="rId3" Type="{rel}/settings" Target="settings.xml"/>"#,
            r#"<Relationship Id="rId4" Type="{rel}/theme" Target="theme/theme1.xml"/>"#,
            r#"<Relationship Id="rId9" Type="{rel}/hyperlink" Target="https://example.com/timetable" TargetMode="External"/>"#,
        ),
        rel = REL
    );
    let mut files: Vec<(String, Vec<u8>)> = vec![
        (
            "word/styles.xml".to_string(),
            format!(r#"{HEAD}<w:styles xmlns:w="{W}">{}</w:styles>"#, parts.styles).into_bytes(),
        ),
        (
            "word/settings.xml".to_string(),
            format!(r#"{HEAD}<w:settings xmlns:w="{W}">{}</w:settings>"#, parts.settings)
                .into_bytes(),
        ),
        (
            "word/theme/theme1.xml".to_string(),
            format!(
                r#"{HEAD}<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="Harbour"><a:themeElements><a:clrScheme name="Harbour">{}</a:clrScheme><a:fontScheme name="Harbour"><a:majorFont><a:latin typeface="Cambria"/><a:ea typeface=""/><a:cs typeface=""/></a:majorFont><a:minorFont><a:latin typeface="Calibri"/><a:ea typeface=""/><a:cs typeface=""/></a:minorFont></a:fontScheme></a:themeElements></a:theme>"#,
                parts.colours
            )
            .into_bytes(),
        ),
    ];
    if let Some(comments) = &parts.comments {
        overrides.push_str(&format!(
            r#"<Override PartName="/word/comments.xml" ContentType="{WML}.comments+xml"/>"#
        ));
        relationships.push_str(&format!(
            r#"<Relationship Id="rId5" Type="{REL}/comments" Target="comments.xml"/>"#
        ));
        files.push((
            "word/comments.xml".to_string(),
            format!(r#"{HEAD}<w:comments xmlns:w="{W}">{comments}</w:comments>"#).into_bytes(),
        ));
    }
    let relationship_part = |entries: &str| {
        format!(
            r#"{HEAD}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{entries}</Relationships>"#
        )
        .into_bytes()
    };
    files.push((
        "[Content_Types].xml".to_string(),
        format!(
            r#"{HEAD}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/>{overrides}</Types>"#
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
            r#"{HEAD}<w:document xmlns:w="{W}" xmlns:r="{REL}">{}<w:body>{}<w:sectPr/></w:body></w:document>"#,
            parts.before_body, parts.body
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

fn audit(before: &Parts, after: &Parts, equality: UntouchedEquality) -> AuditReport {
    audit_with_equality(&build(before), &build(after), equality).expect("the pair audits")
}

fn is_clean(report: &AuditReport) -> bool {
    report.direct_changes.is_empty() && report.untouched.violations.is_empty()
}

/// A change a reader sees: reported under both equalities, and each side
/// audited against itself is clean.
fn assert_reported(before: &Parts, after: &Parts) {
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
fn assert_clean(before: &Parts, after: &Parts) {
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

const ACCENT_RUN: &str = r#"<w:p><w:r><w:rPr><w:color w:val="4F81BD" w:themeColor="accent1"/></w:rPr><w:t>High water at noon</w:t></w:r></w:p>"#;

#[test]
fn a_theme_colour_a_run_uses_changed_in_the_theme_is_reported() {
    let before = document(ACCENT_RUN);
    let recoloured = Parts {
        colours: colours("FF00FF", "C0504D"),
        ..before.clone()
    };
    assert_reported(&before, &recoloured);
    // A theme colour no run uses changes nothing a reader sees.
    let unused = Parts {
        colours: colours("4F81BD", "00FF00"),
        ..before.clone()
    };
    assert_clean(&before, &unused);
}

const PLAIN: &str = r#"<w:p><w:r><w:t>The harbour office opens at seven.</w:t></w:r></w:p>"#;

#[test]
fn a_page_background_added_is_reported() {
    let before = document(PLAIN);
    let coloured = Parts {
        before_body: r#"<w:background w:color="FFFF99"/>"#.to_string(),
        settings: format!("{SETTINGS}<w:displayBackgroundShape/>"),
        ..before.clone()
    };
    assert_reported(&before, &coloured);
    // The background colour alone, shown or not, is the document's.
    let colour_only = Parts {
        before_body: r#"<w:background w:color="FFFF99"/>"#.to_string(),
        ..before.clone()
    };
    assert_reported(&before, &colour_only);
}

fn with_settings(settings: &str) -> Parts {
    Parts {
        settings: settings.to_string(),
        ..document(PLAIN)
    }
}

#[test]
fn a_changed_setting_that_lays_the_text_out_is_reported() {
    let before = with_settings(SETTINGS);
    assert_reported(
        &before,
        &with_settings(&SETTINGS.replace(r#"w:val="720""#, r#"w:val="2160""#)),
    );
    assert_reported(
        &before,
        &with_settings(&format!("{SETTINGS}<w:autoHyphenation/>")),
    );
    assert_reported(
        &before,
        &with_settings(&format!("{SETTINGS}<w:evenAndOddHeaders/>")),
    );
}

#[test]
fn settings_that_record_editing_viewing_or_saving_are_bookkeeping() {
    let before = with_settings(SETTINGS);
    let after = with_settings(concat!(
        r#"<w:zoom w:percent="140"/><w:proofState w:spelling="clean" w:grammar="clean"/>"#,
        r#"<w:trackRevisions/><w:defaultTabStop w:val="720"/>"#,
        r#"<w:characterSpacingControl w:val="doNotCompress"/>"#,
        r#"<w:rsids><w:rsidRoot w:val="00A1B2C3"/><w:rsid w:val="00A1B2C3"/></w:rsids>"#,
    ));
    assert_clean(&before, &after);
}

#[test]
fn a_setting_written_as_its_default_is_the_setting_left_out() {
    let before = with_settings(r#"<w:defaultTabStop w:val="720"/>"#);
    assert_clean(
        &before,
        &with_settings(concat!(
            r#"<w:defaultTabStop w:val="720"/><w:evenAndOddHeaders w:val="false"/>"#,
            r#"<w:characterSpacingControl w:val="doNotCompress"/>"#,
        )),
    );
    // Default tab stops every 720 twentieths of a point are what a
    // document without the setting has (§17.15.1.25).
    assert_clean(
        &with_settings(r#"<w:zoom w:percent="100"/>"#),
        &with_settings(r#"<w:zoom w:percent="100"/><w:defaultTabStop w:val="720"/>"#),
    );
    assert_reported(
        &with_settings(r#"<w:zoom w:percent="100"/>"#),
        &with_settings(r#"<w:zoom w:percent="100"/><w:defaultTabStop w:val="360"/>"#),
    );
    // Compatibility mode 12 is the mode of a document that names none
    // (MS-DOCX §2.3.5); the language of Latin text selects no theme font.
    assert_clean(
        &before,
        &with_settings(concat!(
            r#"<w:themeFontLang w:val="en-GB"/><w:defaultTabStop w:val="720"/>"#,
            r#"<w:compat><w:compatSetting w:name="compatibilityMode" w:uri="http://schemas.microsoft.com/office/word" w:val="12"/></w:compat>"#,
        )),
    );
    // Another compatibility mode lays the document out by other rules.
    assert_reported(
        &before,
        &with_settings(concat!(
            r#"<w:defaultTabStop w:val="720"/>"#,
            r#"<w:compat><w:compatSetting w:name="compatibilityMode" w:uri="http://schemas.microsoft.com/office/word" w:val="15"/></w:compat>"#,
        )),
    );
    // `on` and a bare element are one spelling of a setting that is on.
    assert_clean(
        &with_settings(r#"<w:defaultTabStop w:val="720"/><w:autoHyphenation w:val="on"/>"#),
        &with_settings(r#"<w:autoHyphenation/><w:defaultTabStop w:val="720"/>"#),
    );
}

/// A paragraph of the given style holding a link whose runs are in the
/// hyperlink character style.
fn link_paragraph(style: &str) -> String {
    format!(
        r#"<w:p><w:pPr><w:pStyle w:val="{style}"/></w:pPr><w:r><w:t xml:space="preserve">Timetable: </w:t></w:r><w:hyperlink r:id="rId9"><w:r><w:rPr><w:rStyle w:val="Hyperlink"/></w:rPr><w:t>harbour ferries</w:t></w:r></w:hyperlink></w:p>"#
    )
}

#[test]
fn formatting_a_link_takes_from_its_styles_is_compared() {
    let body = link_paragraph("Normal");
    let before = document(&body);
    // The character style's colour.
    let recoloured = Parts {
        styles: format!(
            "{NORMAL}{}",
            LINK_STYLE.replace(r#"w:val="0563C1""#, r#"w:val="FF0000""#)
        ),
        ..before.clone()
    };
    assert_reported(&before, &recoloured);
    // The paragraph style's size.
    let sized = Parts {
        styles: format!(
            r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:rPr><w:sz w:val="40"/></w:rPr></w:style>{LINK_STYLE}"#
        ),
        ..before.clone()
    };
    assert_reported(&before, &sized);
    // The document defaults' size.
    let defaults = Parts {
        styles: format!(
            r#"<w:docDefaults><w:rPrDefault><w:rPr><w:sz w:val="40"/></w:rPr></w:rPrDefault></w:docDefaults>{NORMAL}{LINK_STYLE}"#
        ),
        ..before.clone()
    };
    assert_reported(&before, &defaults);
    // A character style the link does not use.
    let unused = Parts {
        styles: format!(
            r#"{NORMAL}{LINK_STYLE}<w:style w:type="character" w:styleId="Strong"><w:name w:val="Strong"/><w:rPr><w:b/></w:rPr></w:style>"#
        ),
        ..before.clone()
    };
    assert_clean(&before, &unused);
}

fn linked_heading(heading_colour: &str) -> String {
    format!(
        concat!(
            "{normal}",
            r#"<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:link w:val="Heading1Char"/>"#,
            r#"<w:rPr><w:color w:val="{colour}"/><w:sz w:val="32"/></w:rPr></w:style>"#,
            r#"<w:style w:type="character" w:styleId="Heading1Char"><w:name w:val="Heading 1 Char"/><w:link w:val="Heading1"/>"#,
            r#"<w:rPr><w:color w:val="17365D"/><w:sz w:val="32"/></w:rPr></w:style>"#,
        ),
        normal = NORMAL,
        colour = heading_colour
    )
}

const HEADING: &str = r#"<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Winter timetable</w:t></w:r></w:p>"#;

#[test]
fn a_linked_paragraph_styles_own_run_properties_are_compared() {
    let before = Parts {
        styles: linked_heading("17365D"),
        ..document(HEADING)
    };
    let recoloured = Parts {
        styles: linked_heading("FF0000"),
        ..before.clone()
    };
    assert_reported(&before, &recoloured);
    // A linked paragraph style no paragraph uses.
    let unused = Parts {
        body: PLAIN.to_string(),
        ..before.clone()
    };
    assert_clean(
        &unused,
        &Parts {
            body: PLAIN.to_string(),
            ..recoloured
        },
    );
}

fn commented(author: &str, attributes_reversed: bool) -> Parts {
    let comment = if attributes_reversed {
        format!(
            r#"<w:comment w:initials="IR" w:date="2024-03-01T09:00:00Z" w:author="{author}" w:id="0"><w:p><w:r><w:t>Check the berth number.</w:t></w:r></w:p></w:comment>"#
        )
    } else {
        format!(
            r#"<w:comment w:id="0" w:author="{author}" w:date="2024-03-01T09:00:00Z" w:initials="IR"><w:p><w:r><w:t>Check the berth number.</w:t></w:r></w:p></w:comment>"#
        )
    };
    Parts {
        comments: Some(comment),
        ..document(concat!(
            r#"<w:p><w:commentRangeStart w:id="0"/><w:r><w:t>The ferry leaves from pier four.</w:t></w:r>"#,
            r#"<w:commentRangeEnd w:id="0"/><w:r><w:commentReference w:id="0"/></w:r></w:p>"#,
        ))
    }
}

#[test]
fn a_comment_attributed_to_someone_else_is_reported() {
    assert_reported(
        &commented("Ines Ramos", false),
        &commented("Somebody Else", false),
    );
    // The same comment written with its attributes in another order.
    assert_clean(
        &commented("Ines Ramos", false),
        &commented("Ines Ramos", true),
    );
}

/// A heading typed with its number, the label written in the given runs
/// (run properties, text).
fn typed_heading(label: &[(&str, &str)]) -> Parts {
    let label: String = label
        .iter()
        .map(|(properties, text)| {
            format!("<w:r><w:rPr>{properties}</w:rPr><w:t>{text}</w:t></w:r>")
        })
        .collect();
    document(&format!(
        r#"<w:p>{label}<w:r><w:tab/><w:t>Mooring Fees</w:t></w:r></w:p>"#
    ))
}

#[test]
fn the_formatting_of_a_typed_labels_runs_is_compared() {
    let before = typed_heading(&[("<w:b/>", "4"), ("", ".")]);
    assert_reported(
        &before,
        &typed_heading(&[("<w:b/>", "4"), (r#"<w:color w:val="FF0000"/>"#, ".")]),
    );
    // The label in one formatting, written in one run or two.
    assert_clean(
        &typed_heading(&[("<w:b/>", "4"), ("<w:b/>", ".")]),
        &typed_heading(&[("<w:b/>", "4.")]),
    );
    // The separator in a run of its own, or in the run of the label's last
    // character, in the same formatting.
    let separator = |joined: bool| {
        let label = if joined {
            r#"<w:r><w:rPr><w:b/></w:rPr><w:t>4</w:t></w:r><w:r><w:t>.</w:t><w:tab/><w:t>Mooring Fees</w:t></w:r>"#
        } else {
            r#"<w:r><w:rPr><w:b/></w:rPr><w:t>4</w:t></w:r><w:r><w:t>.</w:t></w:r><w:r><w:tab/><w:t>Mooring Fees</w:t></w:r>"#
        };
        document(&format!("<w:p>{label}</w:p>"))
    };
    assert_clean(&separator(false), &separator(true));
}
