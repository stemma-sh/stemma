//! `validate` and the parts the main document relates to.
//!
//! Each rule here was found by changing documents Word opens cleanly and
//! asking Word again:
//!
//! - a related part left without its own content type (its Override
//!   removed) is repaired, for every kind tried: styles, numbering,
//!   settings, font table, web settings, theme, footnotes, endnotes, header,
//!   footer, comments;
//! - a related part that is absent is fine for styles, numbering, settings,
//!   font table, web settings and images, and repaired for theme, header,
//!   footer, footnotes, endnotes and comments;
//! - a footnote, endnote or comment reference with no relationship of that
//!   kind is repaired;
//! - a picture whose `r:embed` names no relationship is fine; a hyperlink
//!   whose `r:id` names none is repaired.

use std::io::{Cursor, Write};

use stemma::validate_docx_report;
use zip::{ZipWriter, write::FileOptions};

const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const WML: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml";

struct Package {
    /// `(part name, content type)` Overrides besides the main document's.
    overrides: Vec<(&'static str, String)>,
    /// `(id, kind, target)` relationships of the main document.
    relationships: Vec<(&'static str, &'static str, &'static str)>,
    /// `(part name, content)` parts besides the main document.
    parts: Vec<(&'static str, String)>,
    body: String,
}

fn styles_xml() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:styles {W}><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style></w:styles>"#
    )
}

fn footnotes_xml() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:footnotes {W}><w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote><w:footnote w:id="1"><w:p><w:r><w:footnoteRef/></w:r><w:r><w:t xml:space="preserve"> Tide tables are posted weekly.</w:t></w:r></w:p></w:footnote></w:footnotes>"#
    )
}

const PLAIN_BODY: &str = r#"<w:p><w:r><w:t>The harbour office opens at seven.</w:t></w:r></w:p>"#;
const FOOTNOTE_BODY: &str = r#"<w:p><w:r><w:t>The harbour office opens at seven.</w:t></w:r><w:r><w:footnoteReference w:id="1"/></w:r></w:p>"#;

/// A document with a styles part and a footnotes part, both declared and
/// related, and one footnote reference.
fn sound() -> Package {
    Package {
        overrides: vec![
            ("/word/styles.xml", format!("{WML}.styles+xml")),
            ("/word/footnotes.xml", format!("{WML}.footnotes+xml")),
        ],
        relationships: vec![
            ("rId1", "styles", "styles.xml"),
            ("rId2", "footnotes", "footnotes.xml"),
        ],
        parts: vec![
            ("word/styles.xml", styles_xml()),
            ("word/footnotes.xml", footnotes_xml()),
        ],
        body: FOOTNOTE_BODY.to_string(),
    }
}

fn build(package: &Package) -> Vec<u8> {
    let overrides: String = package
        .overrides
        .iter()
        .map(|(part, content_type)| {
            format!(r#"<Override PartName="{part}" ContentType="{content_type}"/>"#)
        })
        .collect();
    let content_types = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/word/document.xml" ContentType="{WML}.document.main+xml"/>{overrides}</Types>"#
    );
    let relationships: String = package
        .relationships
        .iter()
        .map(|(id, kind, target)| {
            let mode = if target.starts_with("https:") {
                r#" TargetMode="External""#
            } else {
                ""
            };
            format!(r#"<Relationship Id="{id}" Type="{REL}/{kind}" Target="{target}"{mode}/>"#)
        })
        .collect();
    let document_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{relationships}</Relationships>"#
    );
    let root_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL}/officeDocument" Target="word/document.xml"/></Relationships>"#
    );
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document {W} xmlns:r="{REL}" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"><w:body>{}<w:sectPr><w:pgSz w:w="12240" w:h="15840"/></w:sectPr></w:body></w:document>"#,
        package.body
    );
    let mut buf = Vec::new();
    {
        let mut zip = ZipWriter::new(Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        let mut write = |name: &str, text: &str| {
            zip.start_file(name, opts).expect("start_file");
            zip.write_all(text.as_bytes()).expect("write part");
        };
        write("[Content_Types].xml", &content_types);
        write("_rels/.rels", &root_rels);
        write("word/_rels/document.xml.rels", &document_rels);
        write("word/document.xml", &document);
        for (name, text) in &package.parts {
            write(name, text);
        }
        zip.finish().expect("finish zip");
    }
    buf
}

fn messages(package: &Package) -> Vec<String> {
    validate_docx_report(&build(package))
        .expect("validate")
        .issues
        .into_iter()
        .map(|issue| issue.message)
        .collect()
}

#[test]
fn the_unchanged_document_is_sound() {
    assert_eq!(messages(&sound()), Vec::<String>::new());
}

#[test]
fn a_styles_part_left_to_the_generic_xml_type_is_reported() {
    let mut package = sound();
    package
        .overrides
        .retain(|(part, _)| *part != "/word/styles.xml");
    assert_eq!(
        messages(&package),
        vec![format!(
            r#"styles part "word/styles.xml" is declared with content type "application/xml", not "{WML}.styles+xml"; Word repairs such a file"#
        )]
    );
}

#[test]
fn a_footnotes_part_left_to_the_generic_xml_type_is_reported() {
    let mut package = sound();
    package
        .overrides
        .retain(|(part, _)| *part != "/word/footnotes.xml");
    let found = messages(&package);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].starts_with(r#"footnotes part "word/footnotes.xml" is declared with"#));
}

#[test]
fn an_absent_styles_part_is_sound() {
    let mut package = sound();
    package.parts.retain(|(name, _)| *name != "word/styles.xml");
    package
        .overrides
        .retain(|(part, _)| *part != "/word/styles.xml");
    assert_eq!(messages(&package), Vec::<String>::new());
}

#[test]
fn an_absent_footnotes_part_is_reported() {
    let mut package = sound();
    package
        .parts
        .retain(|(name, _)| *name != "word/footnotes.xml");
    package
        .overrides
        .retain(|(part, _)| *part != "/word/footnotes.xml");
    let found = messages(&package);
    assert!(
        found
            .iter()
            .any(|message| message.contains("word/footnotes.xml")
                && message.contains("does not exist in the package")),
        "{found:?}"
    );
}

#[test]
fn a_footnote_reference_without_a_footnotes_relationship_is_reported() {
    let mut package = sound();
    package
        .relationships
        .retain(|(_, kind, _)| *kind != "footnotes");
    assert_eq!(
        messages(&package),
        vec![
            "the document holds a w:footnoteReference but has no footnotes relationship; Word repairs such a file"
                .to_string()
        ]
    );
}

#[test]
fn an_unused_footnotes_part_without_its_relationship_is_sound() {
    let mut package = sound();
    package
        .relationships
        .retain(|(_, kind, _)| *kind != "footnotes");
    package.body = PLAIN_BODY.to_string();
    assert_eq!(messages(&package), Vec::<String>::new());
}

const PICTURE: &str = r#"<w:p><w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="914400" cy="914400"/><wp:docPr id="1" name="Harbour plan"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic><pic:nvPicPr><pic:cNvPr id="1" name="Harbour plan"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="rId9"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="914400"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#;

#[test]
fn a_picture_whose_image_relationship_is_missing_is_sound() {
    let mut package = sound();
    package.body = format!("{FOOTNOTE_BODY}{PICTURE}");
    assert_eq!(messages(&package), Vec::<String>::new());
}

#[test]
fn a_picture_whose_image_part_is_absent_is_sound() {
    let mut package = sound();
    package.body = format!("{FOOTNOTE_BODY}{PICTURE}");
    package
        .relationships
        .push(("rId9", "image", "media/plan.png"));
    assert_eq!(messages(&package), Vec::<String>::new());
}

#[test]
fn a_hyperlink_whose_relationship_is_missing_is_reported() {
    let mut package = sound();
    package.body = format!(
        r#"{FOOTNOTE_BODY}<w:p><w:hyperlink r:id="rId9"><w:r><w:t>Timetable</w:t></w:r></w:hyperlink></w:p>"#
    );
    let found = messages(&package);
    assert!(
        found
            .iter()
            .any(|message| message.contains(r#"relationship reference "rId9" not found"#)),
        "{found:?}"
    );
}

#[test]
fn a_footnote_reference_to_a_note_the_part_does_not_define_is_reported() {
    let mut package = sound();
    package.body = FOOTNOTE_BODY.replace(r#"w:id="1""#, r#"w:id="2""#);
    assert_eq!(
        messages(&package),
        vec![
            r#"w:footnoteReference names footnote "2", which word/footnotes.xml does not define; Word repairs such a file"#
                .to_string()
        ]
    );
}

/// Settings that name the separator footnotes, as Word writes them.
fn settings_naming_separators() -> (&'static str, String) {
    (
        "word/settings.xml",
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:settings {W}><w:footnotePr><w:footnote w:id="-1"/><w:footnote w:id="0"/></w:footnotePr></w:settings>"#
        ),
    )
}

fn with_settings(mut package: Package) -> Package {
    package.parts.push(settings_naming_separators());
    package
        .overrides
        .push(("/word/settings.xml", format!("{WML}.settings+xml")));
    package
        .relationships
        .push(("rId3", "settings", "settings.xml"));
    package
}

#[test]
fn settings_naming_separator_footnotes_with_the_footnotes_part_are_sound() {
    assert_eq!(messages(&with_settings(sound())), Vec::<String>::new());
}

/// No footnote reference in the body: the settings alone need the part.
#[test]
fn settings_naming_separator_footnotes_without_a_footnotes_relationship_are_reported() {
    let mut package = with_settings(sound());
    package
        .relationships
        .retain(|(_, kind, _)| *kind != "footnotes");
    package.body = PLAIN_BODY.to_string();
    assert_eq!(
        messages(&package),
        vec![
            "the settings name separator footnotes (w:footnotePr/w:footnote) but the document has no footnotes relationship; Word repairs such a file"
                .to_string()
        ]
    );
}

/// One required child removed from an otherwise complete picture.
#[test]
fn a_picture_missing_a_required_child_is_reported() {
    for (removed, start, end) in [
        ("wp:extent", "<wp:extent", "/>"),
        ("wp:docPr", "<wp:docPr", "/>"),
        ("pic:nvPicPr", "<pic:nvPicPr>", "</pic:nvPicPr>"),
        ("pic:blipFill", "<pic:blipFill>", "</pic:blipFill>"),
        ("pic:spPr", "<pic:spPr>", "</pic:spPr>"),
    ] {
        let from = PICTURE.find(start).expect("child present");
        let to = from + PICTURE[from..].find(end).expect("child end") + end.len();
        let picture = format!("{}{}", &PICTURE[..from], &PICTURE[to..]);
        let mut package = sound();
        package.body = format!("{FOOTNOTE_BODY}{picture}");
        let found = messages(&package);
        assert!(
            found.len() == 1
                && found[0].contains("I-DRAW-001")
                && found[0].contains(&format!("no {removed} child")),
            "{removed}: {found:?}"
        );
    }
}

/// Word opens a picture with no `a:stretch` in its fill cleanly.
#[test]
fn a_picture_without_a_stretch_is_sound() {
    let mut package = sound();
    package.body = format!(
        "{FOOTNOTE_BODY}{}",
        PICTURE.replace("<a:stretch><a:fillRect/></a:stretch>", "")
    );
    assert_eq!(messages(&package), Vec::<String>::new());
}

/// Checked against Word: it cannot open a document whose referenced header
/// part is empty, whitespace only, or cut off mid-element.
#[test]
fn a_header_part_that_is_not_well_formed_is_reported() {
    let whole = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:hdr {W}><w:p><w:r><w:t>Harbour notices</w:t></w:r></w:p></w:hdr>"#
    );
    for (name, header) in [
        ("whole", whole.clone()),
        ("empty", String::new()),
        ("whitespace", "  \n".to_string()),
        ("cut off", whole[..whole.len() / 2].to_string()),
    ] {
        let mut package = sound();
        package.parts.push(("word/header1.xml", header));
        package
            .overrides
            .push(("/word/header1.xml", format!("{WML}.header+xml")));
        package
            .relationships
            .push(("rId7", "header", "header1.xml"));
        package.body = format!(
            r#"{FOOTNOTE_BODY}<w:p><w:pPr><w:sectPr><w:headerReference w:type="default" r:id="rId7"/><w:pgSz w:w="12240" w:h="15840"/></w:sectPr></w:pPr></w:p>{PLAIN_BODY}"#
        );
        let found = messages(&package);
        if name == "whole" {
            assert_eq!(found, Vec::<String>::new(), "{name}");
        } else {
            assert!(
                found.iter().any(|message| message
                    .starts_with(r#"header part "word/header1.xml" is not well-formed XML"#)),
                "{name}: {found:?}"
            );
        }
    }
}

/// A complete picture whose `r:link` names no relationship: Word opens it
/// without repair, as it does for `r:embed`.
#[test]
fn a_picture_whose_linked_image_relationship_is_missing_is_sound() {
    let mut package = sound();
    package.body = format!(
        "{FOOTNOTE_BODY}{}",
        PICTURE.replace(r#"r:embed="rId9""#, r#"r:link="rId9""#)
    );
    assert_eq!(messages(&package), Vec::<String>::new());
}
