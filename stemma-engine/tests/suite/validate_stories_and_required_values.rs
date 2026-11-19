//! `validate` and the load path on shapes checked against Word one piece at a
//! time: required property values, the stories beside the body (comments,
//! notes, headers, footers) and their relationships, simple fields, picture
//! content controls, elements Word does not know, a tracked change inside a
//! run, and relationships whose target part is gone.
//!
//! Word's verdicts, each against the same document without the change:
//!
//! - without `w:val`, Word cannot open the file for `w:pStyle`, `w:jc`,
//!   `w:outlineLvl`, `w:numId`, `w:ilvl`, `w:rStyle` and `w:tblStyle`, and
//!   repairs it for `w:highlight` and `w:vertAlign`; `w:sz`, `w:color`,
//!   `w:lang` and a tab stop's `w:tab` open cleanly (a tab stop without
//!   `w:val` is saved by Word as a left stop);
//! - a run directly in a comment, footnote or endnote: repaired; directly in
//!   a header or footer: cannot open;
//! - a hyperlink whose relationship is missing from its story's
//!   relationships part, in each of those stories: repaired;
//! - the separator notes removed while the settings name them: repaired;
//!   with the settings naming none: clean;
//! - `w:attachedTemplate` naming a relationship the settings part lacks:
//!   repaired;
//! - `w:fldSimple` without `w:instr`: cannot open; with an empty one: clean;
//! - a picture content control naming any second kind (`w14:checkbox`
//!   included): repaired; two kinds without a picture: clean;
//! - an empty element Word does not know, in a run (or elsewhere): clean;
//!   Word drops it. One holding text: clean, and Word shows the text;
//! - `w:del` or `w:ins` inside a run: clean; Word reads it as that change on
//!   a run with the outer run's properties;
//! - an embedded object's part or the glossary part gone: clean; a header,
//!   footer, notes or comments part gone: repaired when something refers to
//!   it (a body reference, or for notes the settings naming separator
//!   notes), clean otherwise; a custom XML or theme part gone: repaired;
//! - under a `.docx` name, a main part declared as a template or a
//!   macro-enabled document or template: cannot open;
//! - an altChunk part declared as a WordprocessingML document (package or
//!   main-document type) holding bare document XML: cannot open. A real
//!   package under either type, an HTML or plain-text chunk: clean; the
//!   chunk's part missing: repaired.

use std::io::{Cursor, Write};

use stemma::api::Document;
use stemma::validate_docx_report;
use zip::{ZipWriter, write::FileOptions};

const HEAD: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#;
const NS: &str = concat!(
    r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" "#,
    r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" "#,
    r#"xmlns:v="urn:schemas-microsoft-com:vml" xmlns:o="urn:schemas-microsoft-com:office:office""#,
);
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/";
const WML: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.";
const PARAGRAPH: &str = r#"<w:p><w:r><w:t>The ferry leaves the quay at six.</w:t></w:r></w:p>"#;
const CHANGE: &str = r#"w:id="5" w:author="Ingrid" w:date="2024-03-02T09:10:00Z""#;

/// A package built from its pieces, so one test can change one of them.
#[derive(Clone)]
struct Package {
    /// Extra `Default` / `Override` entries of `[Content_Types].xml`.
    content_types: String,
    /// `Relationship` entries of `word/_rels/document.xml.rels`.
    document_relationships: String,
    /// The body content, final `w:sectPr` included.
    body: String,
    /// Further parts: (name, content).
    parts: Vec<(String, String)>,
}

fn sound() -> Package {
    Package {
        content_types: String::new(),
        document_relationships: String::new(),
        body: format!("{PARAGRAPH}<w:sectPr/>"),
        parts: Vec::new(),
    }
}

fn with_body(body: &str) -> Package {
    Package {
        body: format!("{body}<w:sectPr/>"),
        ..sound()
    }
}

fn relationship(id: &str, kind: &str, target: &str) -> String {
    format!(r#"<Relationship Id="{id}" Type="{REL}{kind}" Target="{target}"/>"#)
}

fn relationships(entries: &str) -> String {
    format!(
        r#"{HEAD}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{entries}</Relationships>"#
    )
}

fn override_for(part: &str, content_type: &str) -> String {
    format!(r#"<Override PartName="/{part}" ContentType="{content_type}"/>"#)
}

fn build(package: &Package) -> Vec<u8> {
    let mut files = vec![
        (
            "[Content_Types].xml".to_string(),
            format!(
                concat!(
                    r#"{}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">"#,
                    r#"<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>"#,
                    r#"<Default Extension="xml" ContentType="application/xml"/>"#,
                    r#"<Default Extension="bin" ContentType="application/vnd.openxmlformats-officedocument.oleObject"/>"#,
                    r#"<Override PartName="/word/document.xml" ContentType="{}document.main+xml"/>{}</Types>"#
                ),
                HEAD, WML, package.content_types
            ),
        ),
        (
            "_rels/.rels".to_string(),
            relationships(&relationship("rId1", "officeDocument", "word/document.xml")),
        ),
        (
            "word/_rels/document.xml.rels".to_string(),
            relationships(&package.document_relationships),
        ),
        (
            "word/document.xml".to_string(),
            format!(
                r#"{HEAD}<w:document {NS}><w:body>{}</w:body></w:document>"#,
                package.body
            ),
        ),
    ];
    files.extend(package.parts.iter().cloned());
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, content) in files {
        zip.start_file(name, FileOptions::default()).expect("start");
        zip.write_all(content.as_bytes()).expect("write");
    }
    zip.finish().expect("finish").into_inner()
}

fn messages(package: &Package) -> Vec<String> {
    validate_docx_report(&build(package))
        .expect("validate")
        .issues
        .into_iter()
        .map(|issue| issue.message)
        .collect()
}

fn assert_sound(package: &Package) {
    assert_eq!(messages(package), Vec::<String>::new());
}

/// Exactly one issue, and it holds every one of `fragments`.
fn assert_one_issue(package: &Package, fragments: &[&str]) {
    let found = messages(package);
    assert_eq!(found.len(), 1, "{found:?}");
    for fragment in fragments {
        assert!(found[0].contains(fragment), "{fragment:?} not in {found:?}");
    }
}

fn text(document: &Document) -> String {
    document.to_text().trim().to_string()
}

// --- required property values -----------------------------------------------

const VALUE_PROPERTIES: [(&str, &str); 9] = [
    ("pStyle", r#"<w:pStyle w:val="Quay"/>"#),
    ("jc", r#"<w:jc w:val="center"/>"#),
    ("outlineLvl", r#"<w:outlineLvl w:val="1"/>"#),
    ("numId", r#"<w:numId w:val="1"/>"#),
    ("ilvl", r#"<w:ilvl w:val="0"/>"#),
    ("rStyle", r#"<w:rStyle w:val="Berth"/>"#),
    ("highlight", r#"<w:highlight w:val="yellow"/>"#),
    ("vertAlign", r#"<w:vertAlign w:val="superscript"/>"#),
    ("tblStyle", r#"<w:tblStyle w:val="Grid"/>"#),
];

/// A paragraph and a table carrying every property of [`VALUE_PROPERTIES`]
/// and the value-optional ones; `drop` names the one whose `w:val` is removed.
fn properties(drop: Option<&str>) -> Package {
    let property = |name: &str| {
        let (_, xml) = VALUE_PROPERTIES
            .iter()
            .chain(&[
                ("sz", r#"<w:sz w:val="24"/>"#),
                ("color", r#"<w:color w:val="FF0000"/>"#),
                ("lang", r#"<w:lang w:val="en-GB"/>"#),
                ("tab", r#"<w:tab w:val="left" w:pos="720"/>"#),
            ])
            .find(|(candidate, _)| *candidate == name)
            .expect("listed property");
        if drop == Some(name) {
            let start = xml.find(" w:val=").expect("has a value");
            let end = xml[start + 8..].find('"').expect("closing quote") + start + 9;
            format!("{}{}", &xml[..start], &xml[end..])
        } else {
            xml.to_string()
        }
    };
    let body = format!(
        concat!(
            "<w:p><w:pPr>{}{}{}<w:numPr>{}{}</w:numPr><w:tabs>{}</w:tabs></w:pPr>",
            "<w:r><w:rPr>{}{}{}{}{}{}</w:rPr><w:t>The ferry leaves the quay at six.</w:t></w:r></w:p>",
            r#"<w:tbl><w:tblPr>{}<w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid>"#,
            r#"<w:tr><w:tc><w:tcPr><w:tcW w:w="4000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>Berth</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
            "{}"
        ),
        property("pStyle"),
        property("jc"),
        property("outlineLvl"),
        property("ilvl"),
        property("numId"),
        property("tab"),
        property("rStyle"),
        property("sz"),
        property("color"),
        property("highlight"),
        property("vertAlign"),
        property("lang"),
        property("tblStyle"),
        PARAGRAPH
    );
    let numbering = format!(
        concat!(
            r#"{}<w:numbering {}><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/>"#,
            r#"<w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum>"#,
            r#"<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#
        ),
        HEAD, NS
    );
    Package {
        content_types: override_for("word/numbering.xml", &format!("{WML}numbering+xml")),
        document_relationships: relationship("rId5", "numbering", "numbering.xml"),
        parts: vec![("word/numbering.xml".to_string(), numbering)],
        ..with_body(&body)
    }
}

#[test]
fn every_property_with_its_value_is_sound() {
    assert_sound(&properties(None));
}

#[test]
fn a_property_word_needs_the_value_of_is_reported_without_it() {
    for (name, _) in VALUE_PROPERTIES {
        let outcome = if matches!(name, "highlight" | "vertAlign") {
            "Word repairs"
        } else {
            "Word cannot open"
        };
        assert_one_issue(
            &properties(Some(name)),
            &["I-ATTR-001", &format!("w:{name} "), "has no w:val", outcome],
        );
    }
}

#[test]
fn properties_word_opens_without_a_value_are_sound() {
    for name in ["sz", "color", "lang", "tab"] {
        assert_sound(&properties(Some(name)));
    }
}

#[test]
fn a_tab_stop_without_alignment_is_read_as_a_left_stop() {
    let document = Document::parse(&build(&properties(Some("tab"))))
        .expect("Word opens a tab stop without w:val cleanly");
    assert_eq!(
        text(&document).lines().next(),
        Some("The ferry leaves the quay at six.")
    );
}

// --- stories beside the body ----------------------------------------------------

const STORY_PARAGRAPH: &str = r#"<w:p><w:r><w:t>Note on the tide.</w:t></w:r></w:p>"#;
const STORY_RUN: &str = r#"<w:r><w:t>Note on the tide.</w:t></w:r>"#;
const LINKED: &str =
    r#"<w:p><w:hyperlink r:id="rIdLink"><w:r><w:t>tide table</w:t></w:r></w:hyperlink></w:p>"#;
const SETTINGS_NAME_SEPARATORS: &str = concat!(
    r#"<w:footnotePr><w:footnote w:id="-1"/><w:footnote w:id="0"/></w:footnotePr>"#,
    r#"<w:endnotePr><w:endnote w:id="-1"/><w:endnote w:id="0"/></w:endnotePr>"#,
);

const STORIES: [(&str, &str, &str, &str); 5] = [
    // (story name, part, root element, content type suffix)
    ("comment", "word/comments.xml", "comments", "comments+xml"),
    (
        "footnote",
        "word/footnotes.xml",
        "footnotes",
        "footnotes+xml",
    ),
    ("endnote", "word/endnotes.xml", "endnotes", "endnotes+xml"),
    ("header", "word/header1.xml", "hdr", "header+xml"),
    ("footer", "word/footer1.xml", "ftr", "footer+xml"),
];

/// A document with one comment, footnote, endnote, header and footer, each
/// holding `content(story)`. `linked` stories' relationships parts define
/// the hyperlink `rIdLink`. `separators`: the notes parts carry their
/// separator notes. `settings`: the settings part's content.
struct Stories<'a> {
    content: &'a dyn Fn(&str) -> String,
    linked: &'a [&'a str],
    separators: bool,
    settings: &'a str,
    settings_relationships: Option<&'a str>,
    /// A part left out of the package (its relationship and references kept).
    missing: Option<&'a str>,
    /// The body holds the comment, footnote and endnote references.
    referenced: bool,
}

fn default_content(_: &str) -> String {
    STORY_PARAGRAPH.to_string()
}

impl Default for Stories<'_> {
    fn default() -> Self {
        Stories {
            content: &default_content,
            linked: &[],
            separators: true,
            settings: SETTINGS_NAME_SEPARATORS,
            settings_relationships: None,
            missing: None,
            referenced: true,
        }
    }
}

fn stories(spec: &Stories) -> Package {
    let separators = |note: &str| {
        if spec.separators {
            format!(
                concat!(
                    r#"<w:{0} w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:{0}>"#,
                    r#"<w:{0} w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:{0}>"#
                ),
                note
            )
        } else {
            String::new()
        }
    };
    let mut parts = Vec::new();
    let mut content_types = override_for("word/settings.xml", &format!("{WML}settings+xml"));
    for (story, part, root, content_type) in STORIES {
        let inner = (spec.content)(story);
        let body = match story {
            "comment" => format!(
                r#"<w:comment w:id="1" w:author="Ingrid" w:date="2024-03-02T09:10:00Z">{inner}</w:comment>"#
            ),
            "footnote" | "endnote" => {
                format!(
                    r#"{}<w:{story} w:id="1">{inner}</w:{story}>"#,
                    separators(story)
                )
            }
            _ => inner,
        };
        content_types.push_str(&override_for(part, &format!("{WML}{content_type}")));
        if spec.missing != Some(part) {
            parts.push((
                part.to_string(),
                format!("{HEAD}<w:{root} {NS}>{body}</w:{root}>"),
            ));
        }
        if spec.linked.contains(&story) {
            let file = part.trim_start_matches("word/");
            parts.push((
                format!("word/_rels/{file}.rels"),
                relationships(r#"<Relationship Id="rIdLink" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.com/tides" TargetMode="External"/>"#),
            ));
        }
    }
    parts.push((
        "word/settings.xml".to_string(),
        format!("{HEAD}<w:settings {NS}>{}</w:settings>", spec.settings),
    ));
    if let Some(entries) = spec.settings_relationships {
        parts.push((
            "word/_rels/settings.xml.rels".to_string(),
            relationships(entries),
        ));
    }
    let references = if spec.referenced {
        concat!(
            r#"<w:commentRangeStart w:id="1"/><w:r><w:t>Quay</w:t></w:r><w:commentRangeEnd w:id="1"/>"#,
            r#"<w:r><w:commentReference w:id="1"/></w:r><w:r><w:footnoteReference w:id="1"/></w:r>"#,
            r#"<w:r><w:endnoteReference w:id="1"/></w:r>"#
        )
    } else {
        r#"<w:r><w:t>Quay</w:t></w:r>"#
    };
    Package {
        content_types,
        document_relationships: [
            relationship("rIdC", "comments", "comments.xml"),
            relationship("rIdFn", "footnotes", "footnotes.xml"),
            relationship("rIdEn", "endnotes", "endnotes.xml"),
            relationship("rIdH", "header", "header1.xml"),
            relationship("rIdF", "footer", "footer1.xml"),
            relationship("rIdSt", "settings", "settings.xml"),
        ]
        .concat(),
        body: format!(
            concat!(
                "<w:p>{}</w:p>",
                r#"<w:sectPr><w:headerReference w:type="default" r:id="rIdH"/>"#,
                r#"<w:footerReference w:type="default" r:id="rIdF"/></w:sectPr>"#
            ),
            references
        ),
        parts,
    }
}

#[test]
fn the_stories_document_is_sound() {
    assert_sound(&stories(&Stories::default()));
}

#[test]
fn a_run_directly_in_a_story_is_reported() {
    for (story, _, root, _) in STORIES {
        let content = |name: &str| {
            if name == story {
                STORY_RUN.to_string()
            } else {
                STORY_PARAGRAPH.to_string()
            }
        };
        let outcome = if matches!(story, "header" | "footer") {
            "Word cannot open"
        } else {
            "Word repairs"
        };
        let container = if matches!(story, "header" | "footer") {
            root
        } else {
            story
        };
        assert_one_issue(
            &stories(&Stories {
                content: &content,
                ..Stories::default()
            }),
            &["I-STORY-002", &format!("w:{container}"), outcome],
        );
    }
}

#[test]
fn a_hyperlink_its_story_cannot_resolve_is_reported() {
    let all = ["comment", "footnote", "endnote", "header", "footer"];
    let linked = |_: &str| LINKED.to_string();
    assert_sound(&stories(&Stories {
        content: &linked,
        linked: &all,
        ..Stories::default()
    }));
    for (story, part, _, _) in STORIES {
        let others: Vec<&str> = all.iter().copied().filter(|name| *name != story).collect();
        assert_one_issue(
            &stories(&Stories {
                content: &linked,
                linked: &others,
                ..Stories::default()
            }),
            &["\"rIdLink\"", part],
        );
    }
}

#[test]
fn separator_notes_the_settings_name_must_exist() {
    assert_sound(&stories(&Stories {
        separators: false,
        settings: "",
        ..Stories::default()
    }));
    let found = messages(&stories(&Stories {
        separators: false,
        ..Stories::default()
    }));
    assert_eq!(found.len(), 4, "{found:?}");
    assert!(
        found
            .iter()
            .all(|message| message.contains("as a separator") && message.contains("Word repairs")),
        "{found:?}"
    );
}

#[test]
fn an_attached_template_needs_its_relationship() {
    let settings = format!(r#"<w:attachedTemplate r:id="rIdT"/>{SETTINGS_NAME_SEPARATORS}"#);
    assert_sound(&stories(&Stories {
        settings: &settings,
        settings_relationships: Some(
            r#"<Relationship Id="rIdT" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/attachedTemplate" Target="file:///C:/Templates/Harbour.dotx" TargetMode="External"/>"#,
        ),
        ..Stories::default()
    }));
    for relationships in [Some(""), None] {
        assert_one_issue(
            &stories(&Stories {
                settings: &settings,
                settings_relationships: relationships,
                ..Stories::default()
            }),
            &["w:attachedTemplate", "\"rIdT\"", "Word repairs"],
        );
    }
}

// --- relationships whose target part is gone ----------------------------------

#[test]
fn a_story_part_gone_is_reported_only_when_referred_to() {
    for (_, part, _, _) in STORIES {
        // A missing comments part also leaves the comment reference undefined,
        // reported on its own.
        let found = messages(&stories(&Stories {
            missing: Some(part),
            ..Stories::default()
        }));
        assert!(
            found
                .iter()
                .any(|message| message.contains(part) && message.contains("does not exist")),
            "{part}: {found:?}"
        );
    }
    for part in [
        "word/comments.xml",
        "word/footnotes.xml",
        "word/endnotes.xml",
    ] {
        assert_sound(&stories(&Stories {
            missing: Some(part),
            referenced: false,
            settings: "",
            ..Stories::default()
        }));
    }
}

#[test]
fn an_unreferenced_header_gone_is_sound() {
    assert_sound(&Package {
        content_types: override_for("word/header1.xml", &format!("{WML}header+xml")),
        document_relationships: relationship("rId7", "header", "header1.xml"),
        ..sound()
    });
}

#[test]
fn an_embedded_object_or_glossary_part_gone_is_sound() {
    assert_sound(&Package {
        document_relationships: relationship("rId3", "oleObject", "embeddings/oleObject1.bin"),
        ..with_body(concat!(
            r#"<w:p><w:r><w:object w:dxaOrig="1440" w:dyaOrig="1440"><v:shape id="_x0000_i1026" style="width:72pt;height:72pt" o:ole=""/>"#,
            r#"<o:OLEObject Type="Embed" ProgID="Package" ShapeID="_x0000_i1026" DrawAspect="Content" ObjectID="_1000000001" r:id="rId3"/>"#,
            r#"</w:object></w:r></w:p>"#
        ))
    });
    assert_sound(&Package {
        content_types: override_for(
            "word/glossary/document.xml",
            &format!("{WML}document.glossary+xml"),
        ),
        document_relationships: relationship("rId4", "glossaryDocument", "glossary/document.xml"),
        ..sound()
    });
}

#[test]
fn a_custom_xml_or_theme_part_gone_is_reported() {
    for (kind, target) in [
        ("customXml", "../customXml/item1.xml"),
        ("theme", "theme/theme1.xml"),
    ] {
        assert_one_issue(
            &Package {
                document_relationships: relationship("rId9", kind, target),
                ..sound()
            },
            &["\"rId9\"", "does not exist"],
        );
    }
}

// --- simple fields and picture controls ------------------------------------

#[test]
fn a_simple_field_without_an_instruction_is_reported() {
    assert_one_issue(
        &with_body(r#"<w:p><w:fldSimple><w:r><w:t>1</w:t></w:r></w:fldSimple></w:p>"#),
        &["I-FLD-001", "Word cannot open"],
    );
    for instruction in [r#" w:instr=" PAGE ""#, r#" w:instr="""#] {
        assert_sound(&with_body(&format!(
            r#"<w:p><w:fldSimple{instruction}><w:r><w:t>1</w:t></w:r></w:fldSimple></w:p>"#
        )));
    }
}

const CHECKBOX: &str = concat!(
    r#"<w14:checkbox xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml"><w14:checked w14:val="0"/>"#,
    r#"<w14:checkedState w14:val="2612" w14:font="MS Gothic"/><w14:uncheckedState w14:val="2610" w14:font="MS Gothic"/>"#,
    r#"</w14:checkbox>"#
);

fn control(kinds: &str) -> Package {
    with_body(&format!(
        r#"<w:p><w:sdt><w:sdtPr><w:id w:val="4021"/>{kinds}</w:sdtPr><w:sdtContent><w:r><w:t>Berth</w:t></w:r></w:sdtContent></w:sdt></w:p>"#
    ))
}

#[test]
fn a_picture_control_naming_a_second_kind_is_reported() {
    for second in [
        "<w:text/>",
        "<w:richText/>",
        "<w:date/>",
        "<w:dropDownList/>",
        "<w:comboBox/>",
        "<w:picture/>",
        "<w:equation/>",
        "<w:citation/>",
        "<w:group/>",
        "<w:bibliography/>",
        r#"<w:docPartList><w:docPartGallery w:val="Quick Parts"/></w:docPartList>"#,
        CHECKBOX,
    ] {
        for kinds in [
            format!("<w:picture/>{second}"),
            format!("{second}<w:picture/>"),
        ] {
            assert_one_issue(&control(&kinds), &["I-SDT-003", "Word repairs"]);
        }
    }
}

#[test]
fn two_kinds_without_a_picture_are_sound() {
    for kinds in [
        "<w:text/>",
        "<w:picture/>",
        "<w:text/><w:text/>",
        "<w:richText/><w:text/>",
        "<w:date/><w:dropDownList/>",
        "<w:comboBox/><w:dropDownList/>",
        "<w:citation/><w:text/>",
        "<w:group/><w:text/>",
        CHECKBOX,
    ] {
        assert_sound(&control(kinds));
    }
    assert_sound(&control(&format!("{CHECKBOX}<w:text/>")));
}

// --- elements Word does not know ---------------------------------------------

#[test]
fn an_empty_unknown_element_in_a_run_is_read_and_kept() {
    let package = with_body(
        r#"<w:p><w:r><w:t xml:space="preserve">Quay </w:t><w:harbourMark w:val="7"/><w:t>seven.</w:t></w:r></w:p>"#,
    );
    assert_sound(&package);
    let document = Document::parse(&build(&package)).expect("Word opens it cleanly");
    assert_eq!(text(&document), "Quay seven.");
    let saved = document
        .serialize(&stemma::ExportOptions::default())
        .expect("save");
    let xml = String::from_utf8_lossy(&saved).to_string();
    let reopened = Document::parse(&saved).expect("reopen");
    assert_eq!(text(&reopened), "Quay seven.", "{xml}");
}

#[test]
fn an_unknown_element_holding_text_in_a_run_is_refused() {
    // Word shows the text inside; an opaque marker would hide it.
    let package = with_body(
        r#"<w:p><w:r><w:t xml:space="preserve">Quay </w:t><w:harbourMark><w:t>inside </w:t></w:harbourMark><w:t>seven.</w:t></w:r></w:p>"#,
    );
    let Err(error) = Document::parse(&build(&package)) else {
        panic!("an unknown element holding text is not modeled");
    };
    assert!(error.to_string().contains("harbourMark"), "{error}");
}

// --- a tracked change inside a run ---------------------------------------------

#[test]
fn a_deletion_inside_a_run_is_read_as_a_deleted_run() {
    let package = with_body(&format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Quay </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:del {CHANGE}><w:delText xml:space="preserve">old </w:delText></w:del></w:r><w:r><w:t>seven.</w:t></w:r></w:p>"#
    ));
    assert_sound(&package);
    let document = Document::parse(&build(&package)).expect("Word opens it cleanly");
    assert_eq!(
        text(&document.read_accepted().expect("accept")),
        "Quay seven."
    );
    assert_eq!(
        text(&document.read_rejected().expect("reject")),
        "Quay old seven."
    );
}

#[test]
fn an_insertion_inside_a_run_is_read_as_an_inserted_run() {
    let package = with_body(&format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Quay </w:t><w:ins {CHANGE}><w:t xml:space="preserve">new </w:t></w:ins></w:r><w:r><w:t>seven.</w:t></w:r></w:p>"#
    ));
    assert_sound(&package);
    let document = Document::parse(&build(&package)).expect("Word opens it cleanly");
    assert_eq!(
        text(&document.read_accepted().expect("accept")),
        "Quay new seven."
    );
    assert_eq!(
        text(&document.read_rejected().expect("reject")),
        "Quay seven."
    );
}

#[test]
fn content_after_a_change_inside_the_same_run_is_still_refused() {
    // Word reads the following text as part of the change; not modeled.
    let package = with_body(&format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Quay </w:t><w:del {CHANGE}><w:delText xml:space="preserve">old </w:delText></w:del><w:t>seven.</w:t></w:r></w:p>"#
    ));
    assert!(Document::parse(&build(&package)).is_err());
}

// --- imported chunks (w:altChunk) ---------------------------------------------

const DOCX_TYPE: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

/// A host document whose body starts with an altChunk naming `target`,
/// holding `content` (absent when `None`) under `content_type`.
fn alt_chunk(target: &str, content: Option<Vec<u8>>, content_type: &str) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let files: Vec<(String, Vec<u8>)> = vec![
        (
            "[Content_Types].xml".to_string(),
            format!(
                concat!(
                    r#"{}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">"#,
                    r#"<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>"#,
                    r#"<Default Extension="xml" ContentType="application/xml"/>"#,
                    r#"<Override PartName="/word/document.xml" ContentType="{}document.main+xml"/>"#,
                    r#"<Override PartName="/word/{}" ContentType="{}"/></Types>"#
                ),
                HEAD, WML, target, content_type
            )
            .into_bytes(),
        ),
        (
            "_rels/.rels".to_string(),
            relationships(&relationship("rId1", "officeDocument", "word/document.xml")).into_bytes(),
        ),
        (
            "word/_rels/document.xml.rels".to_string(),
            relationships(&relationship("rIdMS", "aFChunk", target)).into_bytes(),
        ),
        (
            "word/document.xml".to_string(),
            format!(
                r#"{HEAD}<w:document {NS}><w:body><w:altChunk r:id="rIdMS"/>{PARAGRAPH}<w:sectPr/></w:body></w:document>"#
            )
            .into_bytes(),
        ),
    ];
    for (name, bytes) in files
        .into_iter()
        .chain(content.map(|bytes| (format!("word/{target}"), bytes)))
    {
        zip.start_file(name, FileOptions::default()).expect("start");
        zip.write_all(&bytes).expect("write");
    }
    zip.finish().expect("finish").into_inner()
}

fn chunk_document() -> Vec<u8> {
    format!(
        r#"{HEAD}<w:document {NS}><w:body><w:p><w:r><w:t>Chunk paragraph.</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#
    )
    .into_bytes()
}

fn bytes_messages(docx: &[u8]) -> Vec<String> {
    validate_docx_report(docx)
        .expect("validate")
        .issues
        .into_iter()
        .map(|issue| issue.message)
        .collect()
}

#[test]
fn a_chunk_declared_as_a_document_but_holding_bare_xml_is_reported() {
    let main_type = format!("{WML}document.main+xml");
    for (target, content_type) in [
        ("chunk1.docx", main_type.as_str()),
        ("chunk1.docx", DOCX_TYPE),
        ("chunk1.xml", main_type.as_str()),
    ] {
        let found = bytes_messages(&alt_chunk(target, Some(chunk_document()), content_type));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].contains("not a package") && found[0].contains("Word cannot open"),
            "{found:?}"
        );
    }
}

#[test]
fn chunks_word_imports_are_sound() {
    let embedded = build(&sound());
    let main_type = format!("{WML}document.main+xml");
    for (target, content, content_type) in [
        ("chunk1.docx", embedded.clone(), DOCX_TYPE),
        ("chunk1.docx", embedded, main_type.as_str()),
        (
            "chunk1.htm",
            b"<html><body><p>Chunk paragraph.</p></body></html>".to_vec(),
            "text/html",
        ),
        ("chunk1.txt", b"Chunk paragraph.".to_vec(), "text/plain"),
    ] {
        assert_eq!(
            bytes_messages(&alt_chunk(target, Some(content), content_type)),
            Vec::<String>::new(),
            "{target} {content_type}"
        );
    }
}

#[test]
fn a_chunk_whose_part_is_missing_is_reported() {
    let found = bytes_messages(&alt_chunk("chunk1.docx", None, DOCX_TYPE));
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("does not exist"), "{found:?}");
}

// --- notes parts named only by the settings -----------------------------------

#[test]
fn a_notes_part_gone_is_reported_when_the_settings_name_its_separators() {
    for part in ["word/footnotes.xml", "word/endnotes.xml"] {
        let found = messages(&stories(&Stories {
            missing: Some(part),
            referenced: false,
            ..Stories::default()
        }));
        assert!(
            found
                .iter()
                .any(|message| message.contains(part) && message.contains("does not exist")),
            "{part}: {found:?}"
        );
    }
}

// --- the main part's content type against the file name ----------------------

fn with_main_type(content_type: &str) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, content) in [
        (
            "[Content_Types].xml".to_string(),
            format!(
                concat!(
                    r#"{}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">"#,
                    r#"<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>"#,
                    r#"<Default Extension="xml" ContentType="application/xml"/>"#,
                    r#"<Override PartName="/word/document.xml" ContentType="{}"/></Types>"#
                ),
                HEAD, content_type
            ),
        ),
        (
            "_rels/.rels".to_string(),
            relationships(&relationship("rId1", "officeDocument", "word/document.xml")),
        ),
        (
            "word/document.xml".to_string(),
            format!(
                r#"{HEAD}<w:document {NS}><w:body>{PARAGRAPH}<w:sectPr/></w:body></w:document>"#
            ),
        ),
    ] {
        zip.start_file(name, FileOptions::default()).expect("start");
        zip.write_all(content.as_bytes()).expect("write");
    }
    zip.finish().expect("finish").into_inner()
}

const OTHER_MAIN_TYPES: [&str; 3] = [
    "application/vnd.openxmlformats-officedocument.wordprocessingml.template.main+xml",
    "application/vnd.ms-word.document.macroEnabled.main+xml",
    "application/vnd.ms-word.template.macroEnabledTemplate.main+xml",
];

#[test]
fn under_a_docx_name_only_the_document_main_type_is_sound() {
    let document = with_main_type(&format!("{WML}document.main+xml"));
    assert!(stemma::api::validate_file(&document, "harbour.docx").ok);
    for content_type in OTHER_MAIN_TYPES {
        let report = stemma::api::validate_file(&with_main_type(content_type), "harbour.DOCX");
        assert!(!report.ok, "{content_type}");
        assert_eq!(report.issues.len(), 1, "{:?}", report.issues);
        assert!(
            report.issues[0].message.contains(".docx name"),
            "{:?}",
            report.issues
        );
    }
}

#[test]
fn other_main_types_are_not_judged_without_a_docx_name() {
    for content_type in OTHER_MAIN_TYPES {
        let bytes = with_main_type(content_type);
        assert!(
            stemma::api::validate_file(&bytes, "harbour.docm").ok,
            "{content_type}"
        );
        assert!(stemma::api::validate(&bytes).ok, "{content_type}");
    }
}
