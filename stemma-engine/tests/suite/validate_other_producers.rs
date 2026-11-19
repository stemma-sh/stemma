//! `validate` and the faults found in documents written by other programs.
//!
//! Each rule was checked against Word with small documents that differ in
//! one piece:
//!
//! - a `w:listItem` of a combo box or drop-down list with `w:displayText=""`:
//!   Word cannot open the file. With no `w:displayText`, with an empty
//!   `w:value`, or with two items sharing a value or a display text: clean;
//! - `[Content_Types].xml` with two `Default` entries for one extension,
//!   identical, differing in case (`png`, `PNG`) or in content type: Word
//!   repairs. One entry, in either case: clean;
//! - a custom-properties part left to the generic `application/xml` default:
//!   Word repairs;
//! - `m:oMath` or `m:oMathPara` directly in a table cell, before or after the
//!   cell's paragraph: Word cannot open the file. Inside the cell's
//!   paragraph: clean;
//! - a list level whose `w:lvlPicBulletId` names no `w:numPicBullet`: Word
//!   repairs, whether or not a paragraph uses the list;
//! - a picture effect layer (`a14:imgLayer`, inside the blip's extension
//!   list) whose `r:embed` names no relationship: clean;
//! - a compatibility `Choice` written under another prefix for the
//!   markup-compatibility namespace, its `Requires` prefix declared on the
//!   part's root: clean.

use std::io::{Cursor, Write};

use stemma::api::Document;
use stemma::validate_docx_report;
use zip::{ZipWriter, write::FileOptions};

const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;
const HEAD: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#;
const DEFAULTS: &str = concat!(
    r#"<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>"#,
    r#"<Default Extension="xml" ContentType="application/xml"/>"#,
);
const MAIN_OVERRIDE: &str = r#"<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>"#;
const MAIN_RELATIONSHIP: &str = r#"<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>"#;
const PARAGRAPH: &str = r#"<w:p><w:r><w:t>The ferry leaves at noon.</w:t></w:r></w:p>"#;

/// A package built from its pieces, so one test can change one of them.
struct Package {
    /// `Default` and `Override` entries of `[Content_Types].xml`.
    content_types: String,
    /// `Relationship` entries of `_rels/.rels`.
    root_relationships: String,
    /// `Relationship` entries of `word/_rels/document.xml.rels`.
    document_relationships: String,
    /// Attributes added to `w:document` (namespace declarations).
    root_attributes: String,
    body: String,
    /// Further parts: (name, content).
    parts: Vec<(String, String)>,
}

fn sound() -> Package {
    Package {
        content_types: format!("{DEFAULTS}{MAIN_OVERRIDE}"),
        root_relationships: MAIN_RELATIONSHIP.to_string(),
        document_relationships: String::new(),
        root_attributes: String::new(),
        body: PARAGRAPH.to_string(),
        parts: Vec::new(),
    }
}

fn build(package: &Package) -> Vec<u8> {
    let relationships = |entries: &str| {
        format!(
            r#"{HEAD}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{entries}</Relationships>"#
        )
    };
    let mut files = vec![
        (
            "[Content_Types].xml".to_string(),
            format!(
                r#"{HEAD}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">{}</Types>"#,
                package.content_types
            ),
        ),
        (
            "_rels/.rels".to_string(),
            relationships(&package.root_relationships),
        ),
        (
            "word/_rels/document.xml.rels".to_string(),
            relationships(&package.document_relationships),
        ),
        (
            "word/document.xml".to_string(),
            format!(
                r#"{HEAD}<w:document {W} {}><w:body>{}<w:sectPr/></w:body></w:document>"#,
                package.root_attributes, package.body
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

#[test]
fn the_unchanged_document_is_sound() {
    assert_sound(&sound());
}

// --- content-control list items ---------------------------------------------

fn list_control(kind: &str, items: &[&str]) -> Package {
    let items: String = items
        .iter()
        .map(|attributes| format!("<w:listItem {attributes}/>"))
        .collect();
    Package {
        body: format!(
            r#"<w:p><w:sdt><w:sdtPr><w:id w:val="77"/><w:{kind}>{items}</w:{kind}></w:sdtPr><w:sdtContent><w:r><w:t>Dredging</w:t></w:r></w:sdtContent></w:sdt></w:p>"#
        ),
        ..sound()
    }
}

const ITEM: &str = r#"w:displayText="Dredging" w:value="dredging""#;

#[test]
fn a_list_item_with_an_empty_display_text_is_reported() {
    for kind in ["comboBox", "dropDownList"] {
        for empty in [
            r#"w:displayText="" w:value="none""#,
            r#"w:displayText="" w:value="""#,
        ] {
            assert_one_issue(
                &list_control(kind, &[empty, ITEM]),
                &["I-SDT-002", "Word cannot open such a file"],
            );
        }
    }
}

#[test]
fn list_items_word_accepts_are_sound() {
    for kind in ["comboBox", "dropDownList"] {
        for items in [
            vec![ITEM],
            vec![r#"w:value="none""#, ITEM],
            vec![r#"w:displayText="None" w:value="""#, ITEM],
            vec![ITEM, r#"w:displayText="Other" w:value="dredging""#],
            vec![ITEM, r#"w:displayText="Dredging" w:value="other""#],
        ] {
            assert_sound(&list_control(kind, &items));
        }
    }
}

// --- content-type defaults ----------------------------------------------------

fn with_defaults(extra: &str) -> Package {
    Package {
        content_types: format!("{extra}{DEFAULTS}{MAIN_OVERRIDE}"),
        ..sound()
    }
}

const PNG: &str = r#"<Default Extension="png" ContentType="image/png"/>"#;

#[test]
fn two_defaults_for_one_extension_are_reported() {
    for extra in [
        format!("{PNG}{PNG}"),
        format!(r#"{PNG}<Default Extension="PNG" ContentType="image/png"/>"#),
        format!(r#"{PNG}<Default Extension="PNG" ContentType="image/x-png"/>"#),
        r#"<Default Extension="XML" ContentType="application/xml"/>"#.to_string(),
    ] {
        assert_one_issue(
            &with_defaults(&extra),
            &["more than once", "Word repairs such a file"],
        );
    }
}

#[test]
fn one_default_for_an_extension_in_either_case_is_sound() {
    assert_sound(&with_defaults(PNG));
    assert_sound(&with_defaults(
        r#"<Default Extension="PNG" ContentType="image/png"/>"#,
    ));
}

// --- custom properties ------------------------------------------------------------

const CUSTOM_TYPE: &str = "application/vnd.openxmlformats-officedocument.custom-properties+xml";

fn with_custom_properties(declared: bool) -> Package {
    let package = sound();
    Package {
        content_types: if declared {
            format!(
                r#"{}<Override PartName="/docProps/custom.xml" ContentType="{CUSTOM_TYPE}"/>"#,
                package.content_types
            )
        } else {
            package.content_types
        },
        root_relationships: format!(
            r#"{MAIN_RELATIONSHIP}<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/custom-properties" Target="docProps/custom.xml"/>"#
        ),
        parts: vec![(
            "docProps/custom.xml".to_string(),
            format!(
                r#"{HEAD}<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/custom-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><property fmtid="{{D5CDD505-2E9C-101B-9397-08002B2CF9AE}}" pid="2" name="Berth"><vt:lpwstr>seven</vt:lpwstr></property></Properties>"#
            ),
        )],
        ..package
    }
}

#[test]
fn a_custom_properties_part_left_to_the_generic_xml_type_is_reported() {
    assert_sound(&with_custom_properties(true));
    assert_one_issue(
        &with_custom_properties(false),
        &[
            "custom properties part",
            "application/xml",
            "Word repairs such a file",
        ],
    );
}

// --- math outside a paragraph -------------------------------------------------------

const MATH_NS: &str = r#"xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math""#;
const MATH: &str = "<m:oMath><m:r><m:t>x</m:t></m:r></m:oMath>";

fn with_cell(cell: &str) -> Package {
    Package {
        root_attributes: MATH_NS.to_string(),
        body: format!(
            r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="4000" w:type="dxa"/></w:tcPr>{cell}</w:tc></w:tr></w:tbl>{PARAGRAPH}"#
        ),
        ..sound()
    }
}

#[test]
fn math_directly_in_a_table_cell_is_reported() {
    let math_paragraph = format!("<m:oMathPara>{MATH}</m:oMathPara>");
    for cell in [
        format!("{math_paragraph}{PARAGRAPH}"),
        format!("{PARAGRAPH}{math_paragraph}"),
        format!("{MATH}{PARAGRAPH}"),
    ] {
        assert_one_issue(
            &with_cell(&cell),
            &["outside any paragraph", "Word cannot open the file"],
        );
    }
}

#[test]
fn math_inside_a_cell_paragraph_is_sound() {
    assert_sound(&with_cell(&format!(
        "<w:p><m:oMathPara>{MATH}</m:oMathPara></w:p>"
    )));
    assert_sound(&with_cell(&format!("<w:p>{MATH}</w:p>")));
}

// --- picture bullets ------------------------------------------------------------------

fn with_list_level(level_extra: &str, used: bool) -> Package {
    let package = sound();
    Package {
        content_types: format!(
            r#"{}<Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/>"#,
            package.content_types
        ),
        document_relationships: r#"<Relationship Id="rId7" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/>"#.to_string(),
        body: if used {
            r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Check the mooring lines.</w:t></w:r></w:p>"#.to_string()
        } else {
            PARAGRAPH.to_string()
        },
        parts: vec![(
            "word/numbering.xml".to_string(),
            format!(
                r#"{HEAD}<w:numbering {W}><w:abstractNum w:abstractNumId="0"><w:multiLevelType w:val="hybridMultilevel"/><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="bullet"/>{level_extra}<w:lvlText w:val="o"/><w:lvlJc w:val="left"/><w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#
            ),
        )],
        ..package
    }
}

#[test]
fn a_list_level_naming_a_picture_bullet_that_does_not_exist_is_reported() {
    assert_sound(&with_list_level("", true));
    for used in [true, false] {
        assert_one_issue(
            &with_list_level(r#"<w:lvlPicBulletId w:val="0"/>"#, used),
            &["picture bullet \"0\"", "Word repairs such a file"],
        );
    }
}

// --- a picture layer with no image behind it ------------------------------------------

#[test]
fn a_picture_effect_layer_whose_image_relationship_is_missing_is_sound() {
    let namespaces = concat!(
        r#"xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" "#,
        r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" "#,
        r#"xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture" "#,
        r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#,
    );
    let picture = concat!(
        r#"<w:p><w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0">"#,
        r#"<wp:extent cx="914400" cy="320040"/><wp:effectExtent l="0" t="0" r="0" b="0"/>"#,
        r#"<wp:docPr id="1" name="Tide table"/><wp:cNvGraphicFramePr/>"#,
        r#"<a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic>"#,
        r#"<pic:nvPicPr><pic:cNvPr id="0" name="Tide table"/><pic:cNvPicPr/></pic:nvPicPr>"#,
        r#"<pic:blipFill><a:blip r:embed="rId9"><a:extLst><a:ext uri="{BEBA8EAE-BF5A-486C-A8C5-ECC9F3942E4B}">"#,
        r#"<a14:imgProps xmlns:a14="http://schemas.microsoft.com/office/drawing/2010/main"><a14:imgLayer r:embed="rId8">"#,
        r#"<a14:imgEffect><a14:saturation sat="33000"/></a14:imgEffect></a14:imgLayer></a14:imgProps></a:ext></a:extLst></a:blip>"#,
        r#"<a:stretch><a:fillRect/></a:stretch></pic:blipFill>"#,
        r#"<pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="320040"/></a:xfrm>"#,
        r#"<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic>"#,
        r#"</wp:inline></w:drawing></w:r></w:p>"#,
    );
    assert_sound(&Package {
        root_attributes: namespaces.to_string(),
        body: picture.to_string(),
        ..sound()
    });
}

// --- a compatibility choice under another prefix ----------------------------------------

#[test]
fn a_choice_under_another_compatibility_prefix_loads() {
    let namespaces = concat!(
        r#"xmlns:ve="http://schemas.openxmlformats.org/markup-compatibility/2006" "#,
        r#"xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape""#,
    );
    let body = concat!(
        r#"<w:p><w:r><w:t xml:space="preserve">Harbour </w:t></w:r>"#,
        r#"<ve:AlternateContent><ve:Choice Requires="wps"><w:r><w:t>master</w:t></w:r></ve:Choice>"#,
        r#"<ve:Fallback><w:r><w:t>office</w:t></w:r></ve:Fallback></ve:AlternateContent></w:p>"#,
    );
    let package = Package {
        root_attributes: namespaces.to_string(),
        body: body.to_string(),
        ..sound()
    };
    assert_sound(&package);
    let document = Document::parse(&build(&package)).expect("the document loads");
    assert_eq!(document.to_text(), "Harbour master");
}

// --- a comments part under a numbered name ------------------------------------------

/// A producer may name its comments part `comments1.xml`; the relationship
/// says what it is. Word opens such a document, and a reference to a comment
/// defined there is a defined reference.
#[test]
fn a_comment_defined_in_a_numbered_comments_part_is_sound() {
    let package = sound();
    let with_comment = |part: &str, defined_id: &str| {
        Package {
        content_types: format!(
            r#"{}<Override PartName="/word/{part}" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/>"#,
            package.content_types
        ),
        document_relationships: format!(
            r#"<Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="{part}"/>"#
        ),
        body: concat!(
            r#"<w:p><w:commentRangeStart w:id="1"/><w:r><w:t>The ferry leaves at noon.</w:t></w:r>"#,
            r#"<w:commentRangeEnd w:id="1"/><w:r><w:commentReference w:id="1"/></w:r></w:p>"#,
        )
        .to_string(),
        parts: vec![(
            format!("word/{part}"),
            format!(
                r#"{HEAD}<w:comments {W}><w:comment w:id="{defined_id}" w:author="Ingrid" w:initials="I"><w:p><w:r><w:t>Check the timetable.</w:t></w:r></w:p></w:comment></w:comments>"#
            ),
        )],
        ..sound()
    }
    };
    assert_sound(&with_comment("comments.xml", "1"));
    assert_sound(&with_comment("comments1.xml", "1"));
    assert_one_issue(
        &with_comment("comments1.xml", "2"),
        &["commentReference w:id='1'", "not defined"],
    );

    // An unreferenced `comments.xml` left beside the part the relationship
    // names does not stand in for it.
    let mut beside_an_orphan = with_comment("comments2.xml", "1");
    beside_an_orphan.parts.push((
        "word/comments.xml".to_string(),
        format!(r#"{HEAD}<w:comments {W}/>"#),
    ));
    beside_an_orphan.content_types.push_str(
        r#"<Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/>"#,
    );
    assert_sound(&beside_an_orphan);
}

// --- one entry per part, one relationship per singleton part -----------------------------

const STYLES_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml";

fn with_styles(overrides: usize, relationships: usize) -> Package {
    let package = sound();
    let entry = format!(r#"<Override PartName="/word/styles.xml" ContentType="{STYLES_TYPE}"/>"#);
    Package {
        content_types: format!("{}{}", package.content_types, entry.repeat(overrides)),
        document_relationships: (0..relationships)
            .map(|index| {
                format!(
                    r#"<Relationship Id="rId{}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>"#,
                    index + 3
                )
            })
            .collect(),
        parts: vec![(
            "word/styles.xml".to_string(),
            format!(
                r#"{HEAD}<w:styles {W}><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style></w:styles>"#
            ),
        )],
        ..package
    }
}

#[test]
fn a_part_declared_twice_is_reported() {
    assert_sound(&with_styles(1, 1));
    assert_one_issue(
        &with_styles(2, 1),
        &[
            "Override",
            "/word/styles.xml",
            "more than once",
            "Word repairs such a file",
        ],
    );
}

#[test]
fn two_relationships_to_the_styles_part_are_reported() {
    assert_one_issue(
        &with_styles(1, 2),
        &["2 styles relationships", "Word repairs"],
    );
}

#[test]
fn two_relationships_to_one_image_are_sound() {
    let package = sound();
    assert_sound(&Package {
        content_types: format!(
            r#"<Default Extension="png" ContentType="image/png"/>{}"#,
            package.content_types
        ),
        document_relationships: concat!(
            r#"<Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/chart.png"/>"#,
            r#"<Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/chart.png"/>"#,
        )
        .to_string(),
        parts: vec![("word/media/chart.png".to_string(), "not a real picture".to_string())],
        ..package
    });
}
