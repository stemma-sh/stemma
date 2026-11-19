//! Shapes Word opens cleanly that the reader must read as Word does, and
//! faults Word repairs or refuses that `validate` must report. Each case was
//! checked against Word with small documents that differ in one piece (and,
//! for a reading, Word's plain resave):
//!
//! - a second font table relationship whose type only ends in `fontTable`
//!   (`.../wordprocessingml/2006/fontTable`): clean; two standard ones:
//!   repaired;
//! - Word's discarded items named with hexadecimal digits
//!   (`[trash]/000a.dat`): clean;
//! - a negative cell width (`w:tcW w:w="-2300"`, `pct` or `dxa`): clean,
//!   saved as written;
//! - a style's tab stop `<w:tab w:val="" w:pos=""/>`: clean, saved as a left
//!   stop at 0;
//! - `w:rPrChange` directly in a run: clean, and no revision (Accept All,
//!   Reject All and a plain save drop it);
//! - a negative list level: clean, saved as the unsigned byte (`-1` as 255);
//! - a `w:ind` value past 32 signed bits: clean, saved without it;
//! - a `w:br` type outside ST_BrType (`line`): clean, saved as `<w:br/>`;
//! - a relationship target written with `\`: read as `/`;
//! - several body-level `w:sectPr`: one final `w:sectPr`, header and footer
//!   references from the first, other properties folded in order;
//! - paragraphs inside a `w:t`: shown as paragraphs;
//! - faults: an Override part name without its leading `/`, a picture's
//!   graphic data naming another kind, a font without a name, a custom XML
//!   part that is not well-formed, core properties that are not well-formed
//!   or hold a date that is not W3CDTF, enumerated values outside their
//!   type, a misspelt WordprocessingML namespace, a run inside a `w:t`.

use std::io::{Cursor, Read, Write};

use stemma::ExportOptions;
use stemma::api::Document;
use stemma::validate_docx_report;
use zip::{ZipArchive, ZipWriter, write::FileOptions};

const HEAD: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#;
const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const WML: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.";
const PARAGRAPH: &str = r#"<w:p><w:r><w:t>The quay opens at eight.</w:t></w:r></w:p>"#;

/// A package built from its pieces, so one test can change one of them.
#[derive(Clone)]
struct Package {
    /// Extra `Default` / `Override` entries of `[Content_Types].xml`.
    content_types: String,
    /// The main part's own Override (replaced to test its part name).
    main_override: String,
    /// Extra `Relationship` entries of `_rels/.rels`.
    root_relationships: String,
    /// `Relationship` entries of `word/_rels/document.xml.rels`.
    document_relationships: String,
    /// The namespace bound to the `w` prefix on the main part's root.
    w_namespace: String,
    /// The body, final `w:sectPr` included.
    body: String,
    /// Further parts: (name, bytes).
    parts: Vec<(String, Vec<u8>)>,
}

fn sound() -> Package {
    Package {
        content_types: String::new(),
        main_override: format!(
            r#"<Override PartName="/word/document.xml" ContentType="{WML}document.main+xml"/>"#
        ),
        root_relationships: String::new(),
        document_relationships: String::new(),
        w_namespace: W_NS.to_string(),
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
    let kind = if kind.starts_with("http") {
        kind.to_string()
    } else {
        format!("{R_NS}/{kind}")
    };
    format!(r#"<Relationship Id="{id}" Type="{kind}" Target="{target}"/>"#)
}

fn relationships(entries: &str) -> String {
    format!(
        r#"{HEAD}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{entries}</Relationships>"#
    )
}

fn override_for(part: &str, content_type: &str) -> String {
    format!(r#"<Override PartName="/{part}" ContentType="{content_type}"/>"#)
}

fn part(name: &str, content: &str) -> (String, Vec<u8>) {
    (name.to_string(), content.as_bytes().to_vec())
}

fn build(package: &Package) -> Vec<u8> {
    let mut files: Vec<(String, Vec<u8>)> = vec![
        part(
            "[Content_Types].xml",
            &format!(
                concat!(
                    r#"{}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">"#,
                    r#"<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>"#,
                    r#"<Default Extension="xml" ContentType="application/xml"/>{}{}</Types>"#
                ),
                HEAD, package.main_override, package.content_types
            ),
        ),
        part(
            "_rels/.rels",
            &relationships(&format!(
                "{}{}",
                relationship("rId1", "officeDocument", "word/document.xml"),
                package.root_relationships
            )),
        ),
        part(
            "word/_rels/document.xml.rels",
            &relationships(&package.document_relationships),
        ),
        part(
            "word/document.xml",
            &format!(
                r#"{HEAD}<w:document xmlns:w="{}" xmlns:r="{R_NS}"><w:body>{}</w:body></w:document>"#,
                package.w_namespace, package.body
            ),
        ),
    ];
    files.extend(package.parts.iter().cloned());
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, content) in files {
        zip.start_file(name, FileOptions::default()).expect("start");
        zip.write_all(&content).expect("write");
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

fn parse(package: &Package) -> Document {
    match Document::parse(&build(package)) {
        Ok(document) => document,
        Err(error) => panic!("Word opens it cleanly, so it must load: {error}"),
    }
}

fn part_text(docx: &[u8], name: &str) -> String {
    let mut archive = ZipArchive::new(Cursor::new(docx)).expect("zip");
    let mut text = String::new();
    archive
        .by_name(name)
        .expect("part")
        .read_to_string(&mut text)
        .expect("read");
    text
}

/// Replace the text of block `target` (whose text is `expect`) as a tracked
/// change, so the save rebuilds the document rather than copying it.
fn edit(document: &Document, target: &str, expect: &str, text: &str) -> Document {
    let json = serde_json::json!({
        "ops": [{"op": "replace", "target": target, "expect": expect,
            "content": {"type": "paragraph", "content": [{"type": "text", "text": text}]}}],
        "revision": {"author": "Ines", "date": "2026-11-16T09:00:00Z"},
    })
    .to_string();
    document
        .apply(
            &stemma::edit_v4::parse_transaction(&json)
                .expect("transaction")
                .into_edit_transaction()
                .expect("edit"),
        )
        .expect("the replacement applies")
}

/// The text of each body paragraph, in order.
fn paragraphs(document: &Document) -> Vec<String> {
    document
        .to_text()
        .trim_end()
        .split("\n\n")
        .map(str::to_string)
        .collect()
}

fn saved(document: &Document) -> Vec<u8> {
    document.serialize(&ExportOptions::default()).expect("save")
}

// --- relationships and package items -------------------------------------------

fn font_tables(second_type: &str) -> Package {
    let fonts = format!(r#"{HEAD}<w:fonts xmlns:w="{W_NS}"><w:font w:name="Calibri"/></w:fonts>"#);
    Package {
        content_types: format!(
            "{}{}",
            override_for("word/fontTable.xml", &format!("{WML}fontTable+xml")),
            override_for("word/fontTable0.xml", &format!("{WML}fontTable+xml"))
        ),
        document_relationships: format!(
            "{}{}",
            relationship("rIdF1", "fontTable", "fontTable.xml"),
            relationship("rIdF2", second_type, "fontTable0.xml")
        ),
        parts: vec![
            part("word/fontTable.xml", &fonts),
            part("word/fontTable0.xml", &fonts),
        ],
        ..sound()
    }
}

#[test]
fn only_standard_font_table_relationships_count_as_duplicates() {
    assert_one_issue(
        &font_tables("fontTable"),
        &["2 fontTable relationships", "Word repairs"],
    );
    assert_sound(&font_tables(
        "http://schemas.openxmlformats.org/wordprocessingml/2006/fontTable",
    ));
}

#[test]
fn many_discarded_items_with_hexadecimal_names_are_sound() {
    // More items than the old limit of 1,000, named as Word names them.
    let mut marker = vec![0xFF; 4];
    marker.extend_from_slice(&[0; 12]);
    let parts = (0..3715)
        .map(|index| (format!("[trash]/{index:04x}.dat"), marker.clone()))
        .collect();
    assert_sound(&Package { parts, ..sound() });
}

#[test]
fn a_relationship_target_with_backslashes_resolves_as_with_slashes() {
    let custom = |with_part: bool| Package {
        document_relationships: relationship("rIdX", "customXml", r"..\customXml\item1.xml"),
        parts: if with_part {
            vec![part(
                "customXml/item1.xml",
                &format!(r#"{HEAD}<tides xmlns="urn:example:tides"><high>06:10</high></tides>"#),
            )]
        } else {
            Vec::new()
        },
        ..sound()
    };
    assert_sound(&custom(true));
    assert_one_issue(&custom(false), &["does not exist"]);
}

#[test]
fn an_override_part_name_without_its_slash_is_reported() {
    assert_one_issue(
        &Package {
            main_override: format!(
                r#"<Override PartName="word/document.xml" ContentType="{WML}document.main+xml"/>"#
            ),
            ..sound()
        },
        &["\"word/document.xml\"", "Word cannot open"],
    );
    let found = messages(&Package {
        content_types: format!(
            r#"<Override PartName="word/styles.xml" ContentType="{WML}styles+xml"/>"#
        ),
        document_relationships: relationship("rIdS", "styles", "styles.xml"),
        parts: vec![part(
            "word/styles.xml",
            &format!(r#"{HEAD}<w:styles xmlns:w="{W_NS}"/>"#),
        )],
        ..sound()
    });
    assert!(
        found
            .iter()
            .any(|m| m.contains("\"word/styles.xml\"") && m.contains("Word repairs")),
        "{found:?}"
    );
}

#[test]
fn a_font_without_a_name_is_reported() {
    let fonts = format!(r#"{HEAD}<w:fonts xmlns:w="{W_NS}"><w:font/></w:fonts>"#);
    assert_one_issue(
        &Package {
            content_types: override_for("word/fontTable.xml", &format!("{WML}fontTable+xml")),
            document_relationships: relationship("rIdF1", "fontTable", "fontTable.xml"),
            parts: vec![part("word/fontTable.xml", &fonts)],
            ..sound()
        },
        &["w:font without w:name", "Word repairs"],
    );
}

#[test]
fn a_custom_xml_part_that_is_not_well_formed_is_reported() {
    assert_one_issue(
        &Package {
            document_relationships: relationship("rIdX", "customXml", "../customXml/item1.xml"),
            parts: vec![part(
                "customXml/item1.xml",
                &format!(r#"{HEAD}<tides xmlns="urn:example:tides"><high>06"#),
            )],
            ..sound()
        },
        &["custom XML part", "not well-formed", "Word repairs"],
    );
}

fn core_properties(content: &str) -> Package {
    Package {
        content_types: override_for(
            "docProps/core.xml",
            "application/vnd.openxmlformats-package.core-properties+xml",
        ),
        root_relationships: relationship(
            "rId2",
            "http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties",
            "docProps/core.xml",
        ),
        parts: vec![part("docProps/core.xml", content)],
        ..sound()
    }
}

fn core_with(dates: &str) -> String {
    format!(
        concat!(
            r#"{}<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" "#,
            r#"xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" "#,
            r#"xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><dc:title>Quay notice</dc:title>{}</cp:coreProperties>"#
        ),
        HEAD, dates
    )
}

#[test]
fn core_property_dates_are_w3cdtf() {
    for value in [
        "2024-01-02T15:04:00Z",
        "2024-01-02",
        "2024",
        "2024-01-02T15:04:05.25+01:00",
        "2008-04-16T01:11:07.625",
    ] {
        assert_sound(&core_properties(&core_with(&format!(
            r#"<dcterms:created xsi:type="dcterms:W3CDTF">{value}</dcterms:created>"#
        ))));
    }
    for element in ["created", "modified"] {
        assert_one_issue(
            &core_properties(&core_with(&format!(
                r#"<dcterms:{element} xsi:type="dcterms:W3CDTF">02/01/2024 3:04 PM</dcterms:{element}>"#
            ))),
            &[element, "not a W3CDTF date", "Word repairs"],
        );
    }
}

#[test]
fn core_properties_that_are_not_well_formed_are_reported() {
    let truncated = core_with("").replace("</cp:coreProperties>", "");
    assert_one_issue(
        &core_properties(&truncated),
        &["core properties", "not well-formed", "Word repairs"],
    );
}

#[test]
fn a_misspelt_wordprocessing_namespace_is_reported() {
    assert_one_issue(
        &Package {
            w_namespace: W_NS.replace("main", "mian"),
            ..sound()
        },
        &["not in the WordprocessingML namespace", "Word cannot open"],
    );
}

// --- story content faults ---------------------------------------------------

#[test]
fn enumerated_values_outside_their_type_are_reported() {
    for (properties, outcome) in [
        (
            r#"<w:pPr><w:jc w:val="justify"/></w:pPr>"#,
            "Word cannot open",
        ),
        (
            r#"<w:pPr><w:jc w:val="bogus"/></w:pPr>"#,
            "Word cannot open",
        ),
        (
            r#"<w:pPr><w:outlineLvl w:val="x"/></w:pPr>"#,
            "Word cannot open",
        ),
        (
            r#"<w:pPr><w:outlineLvl w:val="10"/></w:pPr>"#,
            "Word repairs",
        ),
        (
            r#"<w:pPr><w:rPr><w:highlight w:val="bogus"/></w:rPr></w:pPr>"#,
            "Word repairs",
        ),
        (
            r#"<w:pPr><w:rPr><w:vertAlign w:val="bogus"/></w:rPr></w:pPr>"#,
            "Word repairs",
        ),
    ] {
        assert_one_issue(
            &with_body(&format!(
                "<w:p>{properties}<w:r><w:t>Quay.</w:t></w:r></w:p>"
            )),
            &["I-ATTR-002", outcome],
        );
    }
    for properties in [
        r#"<w:pPr><w:jc w:val="both"/></w:pPr>"#,
        r#"<w:pPr><w:jc w:val="start"/><w:outlineLvl w:val="9"/></w:pPr>"#,
    ] {
        assert_sound(&with_body(&format!(
            "<w:p>{properties}<w:r><w:t>Quay.</w:t></w:r></w:p>"
        )));
    }
}

#[test]
fn a_picture_whose_graphic_data_names_another_kind_is_reported() {
    let picture = |uri: &str| {
        with_body(&format!(
            concat!(
                r#"<w:p><w:r><w:drawing><wp:inline xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing">"#,
                r#"<wp:extent cx="152400" cy="152400"/><wp:docPr id="1" name="Picture 1"/>"#,
                r#"<a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:graphicData uri="{}">"#,
                r#"<pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:nvPicPr><pic:cNvPr id="0" name="buoy.png"/><pic:cNvPicPr/></pic:nvPicPr>"#,
                r#"<pic:blipFill><a:blip/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>"#,
                r#"<pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="152400" cy="152400"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr>"#,
                r#"</pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#
            ),
            uri
        ))
    };
    assert_sound(&picture(
        "http://schemas.openxmlformats.org/drawingml/2006/picture",
    ));
    for uri in [
        "http://schemas.openxmlformats.org/drawingml/2006/pictures",
        "http://schemas.openxmlformats.org/drawingml/2006/chart",
    ] {
        assert_one_issue(&picture(uri), &["I-DRAW-003", uri]);
    }
}

#[test]
fn a_shape_group_whose_graphic_data_names_pictures_is_reported() {
    let group = |uri: &str| {
        with_body(&format!(
            concat!(
                r#"<w:p><w:r><w:drawing><wp:inline xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing">"#,
                r#"<wp:extent cx="152400" cy="152400"/><wp:docPr id="1" name="Group 1"/>"#,
                r#"<a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:graphicData uri="{}">"#,
                r#"<wpg:wgp xmlns:wpg="http://schemas.microsoft.com/office/word/2010/wordprocessingGroup"><wpg:cNvGrpSpPr/>"#,
                r#"<wpg:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="152400" cy="152400"/><a:chOff x="0" y="0"/><a:chExt cx="152400" cy="152400"/></a:xfrm></wpg:grpSpPr>"#,
                r#"</wpg:wgp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#
            ),
            uri
        ))
    };
    let draw_003 = |package: &Package| {
        messages(package)
            .into_iter()
            .filter(|m| m.contains("I-DRAW-003"))
            .count()
    };
    assert_eq!(
        draw_003(&group(
            "http://schemas.microsoft.com/office/word/2010/wordprocessingGroup"
        )),
        0
    );
    assert_eq!(
        draw_003(&group(
            "http://schemas.openxmlformats.org/drawingml/2006/pictures"
        )),
        1
    );
}

#[test]
fn runs_and_text_after_paragraphs_inside_text_are_reported() {
    for body in [
        "<w:p><w:r><w:t>Outer <w:r><w:t>inner</w:t></w:r> text.</w:t></w:r></w:p>",
        "<w:p><w:r><w:t>Before <w:p><w:r><w:t>inner</w:t></w:r></w:p> after.</w:t></w:r></w:p>",
        "<w:p><w:r><w:t><w:p><w:r><w:t>inner</w:t></w:r></w:p></w:t></w:r><w:r><w:t>Next.</w:t></w:r></w:p>",
    ] {
        let found = messages(&with_body(body));
        assert!(
            found
                .iter()
                .any(|m| m.contains("I-RUN-003") && m.contains("Word cannot open")),
            "{body}: {found:?}"
        );
    }
}

// --- readings -----------------------------------------------------------------

#[test]
fn a_negative_cell_width_is_read_and_kept() {
    for kind in ["pct", "dxa"] {
        let package = with_body(&format!(
            concat!(
                r#"<w:tbl><w:tblPr><w:tblW w:w="5000" w:type="pct"/></w:tblPr><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid>"#,
                r#"<w:tr><w:tc><w:tcPr><w:tcW w:w="-2300" w:type="{}"/></w:tcPr><w:p><w:r><w:t>Berth</w:t></w:r></w:p></w:tc></w:tr></w:tbl>{}"#
            ),
            kind, PARAGRAPH
        ));
        assert_sound(&package);
        let document = parse(&package);
        let edited = edit(
            &document,
            "p_2",
            "The quay opens at eight.",
            "The quay opens at nine.",
        );
        let xml = part_text(&saved(&edited), "word/document.xml");
        assert!(xml.contains(r#"w:w="-2300""#), "{kind}: {xml}");
    }
}

#[test]
fn a_style_tab_stop_with_empty_values_is_a_left_stop_at_zero() {
    let styles = format!(
        concat!(
            r#"{}<w:styles xmlns:w="{}"><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>"#,
            r#"<w:style w:type="paragraph" w:styleId="Listing"><w:name w:val="Listing"/><w:pPr><w:tabs><w:tab w:val="" w:pos=""/></w:tabs></w:pPr></w:style></w:styles>"#
        ),
        HEAD, W_NS
    );
    let package = Package {
        content_types: override_for("word/styles.xml", &format!("{WML}styles+xml")),
        document_relationships: relationship("rIdS", "styles", "styles.xml"),
        parts: vec![part("word/styles.xml", &styles)],
        ..with_body(
            r#"<w:p><w:pPr><w:pStyle w:val="Listing"/></w:pPr><w:r><w:t>Quay</w:t></w:r></w:p>"#,
        )
    };
    assert_sound(&package);
    parse(&package);
}

#[test]
fn a_change_record_directly_in_a_run_is_no_revision() {
    let package = with_body(
        r#"<w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Quay.</w:t><w:rPrChange w:id="7" w:author="Ines" w:date="2024-03-02T09:10:00Z"><w:rPr/></w:rPrChange></w:r></w:p>"#,
    );
    assert_sound(&package);
    let document = parse(&package);
    assert!(document.revisions().is_empty());
    assert_eq!(document.to_text().trim(), "Quay.");
}

#[test]
fn a_negative_list_level_is_read_as_its_unsigned_byte() {
    let numbering = format!(
        concat!(
            r#"{}<w:numbering xmlns:w="{}"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/>"#,
            r#"<w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#
        ),
        HEAD, W_NS
    );
    for level in ["-1", "-2"] {
        let package = Package {
            content_types: override_for("word/numbering.xml", &format!("{WML}numbering+xml")),
            document_relationships: relationship("rIdN", "numbering", "numbering.xml"),
            parts: vec![part("word/numbering.xml", &numbering)],
            ..with_body(&format!(
                r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="{level}"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Item.</w:t></w:r></w:p>{PARAGRAPH}"#
            ))
        };
        assert_sound(&package);
        parse(&package);
    }
}

#[test]
fn an_indent_past_32_bits_is_no_indent() {
    let package = with_body(
        r#"<w:p><w:pPr><w:ind w:left="4294967296" w:right="360"/></w:pPr><w:r><w:t>Quay.</w:t></w:r></w:p>"#,
    );
    assert_sound(&package);
    let document = parse(&package);
    let edited = edit(&document, "p_1", "Quay.", "Quay seven.");
    let xml = part_text(&saved(&edited), "word/document.xml");
    assert!(!xml.contains("4294967296"), "{xml}");
    assert!(xml.contains(r#"w:right="360""#), "{xml}");
}

#[test]
fn a_break_of_unknown_type_is_a_line_break() {
    let package = with_body(&format!(
        r#"<w:p><w:r><w:t>Quay</w:t><w:br w:type="line"/><w:t>seven.</w:t></w:r></w:p>{PARAGRAPH}"#
    ));
    assert_sound(&package);
    let document = parse(&package);
    let edited = edit(
        &document,
        "p_2",
        "The quay opens at eight.",
        "The quay opens at nine.",
    );
    let reopened = Document::parse(&saved(&edited)).expect("the edited document reopens");
    assert_eq!(paragraphs(&reopened)[0], "Quayseven.");
}

#[test]
fn a_break_inside_text_is_a_break_and_a_tab_is_nothing() {
    // Word saves `<w:t>Quay<w:br/>seven.</w:t>` as the text, a break and the
    // text, and drops a `w:tab` held inside a `w:t`.
    let package = with_body(&format!(
        r#"<w:p><w:r><w:t>Quay<w:br/>seven.</w:t></w:r></w:p><w:p><w:r><w:t>Pier<w:tab/>two.</w:t></w:r></w:p>{PARAGRAPH}"#
    ));
    assert_sound(&package);
    let document = parse(&package);
    assert_eq!(paragraphs(&document)[1], "Piertwo.");
    let edited = edit(
        &document,
        "p_3",
        "The quay opens at eight.",
        "The quay opens at nine.",
    );
    let reopened = Document::parse(&saved(&edited)).expect("the edited document reopens");
    assert_eq!(paragraphs(&reopened)[1], "Piertwo.");
    let edited = edit(&document, "p_2", "Piertwo.", "Pier three.");
    Document::parse(&saved(&edited)).expect("an edit next to a dropped tab reopens");
}

// --- section properties --------------------------------------------------------

#[test]
fn body_section_properties_fold_into_one_final_section() {
    let header = format!(
        r#"{HEAD}<w:hdr xmlns:w="{W_NS}"><w:p><w:r><w:t>Harbour master</w:t></w:r></w:p></w:hdr>"#
    );
    let package = Package {
        content_types: override_for("word/header1.xml", &format!("{WML}header+xml")),
        document_relationships: relationship("rIdH", "header", "header1.xml"),
        parts: vec![part("word/header1.xml", &header)],
        body: concat!(
            r#"<w:sectPr><w:headerReference w:type="default" r:id="rIdH"/><w:pgSz w:w="15840" w:h="12240" w:orient="landscape"/><w:titlePg/></w:sectPr>"#,
            r#"<w:p><w:r><w:t>Alpha.</w:t></w:r></w:p><w:sectPr></w:sectPr><w:p><w:r><w:t>Beta.</w:t></w:r></w:p>"#,
            r#"<w:sectPr><w:headerReference w:type="first" r:id="rIdH"/><w:pgSz w:w="11906" w:h="16838"/></w:sectPr>"#
        )
        .to_string(),
        ..sound()
    };
    assert_sound(&package);
    let document = parse(&package);
    assert_eq!(paragraphs(&document), ["Alpha.", "Beta."]);
    let edited = edit(&document, "p_1", "Alpha.", "Alpha one.");
    let xml = part_text(&saved(&edited), "word/document.xml");
    assert_eq!(xml.matches("<w:sectPr").count(), 1, "{xml}");
    let section = &xml[xml.find("<w:sectPr").expect("one section")..];
    assert!(section.contains("</w:body>"), "the section is last: {xml}");
    for expected in [
        r#"w:type="default""#,
        r#"w:w="11906""#,
        r#"w:orient="landscape""#,
        "<w:titlePg",
    ] {
        assert!(section.contains(expected), "{expected}: {section}");
    }
    assert!(!section.contains(r#"w:type="first""#), "{section}");
}

// --- paragraphs inside text ----------------------------------------------------

#[test]
fn paragraphs_inside_text_are_read_as_paragraphs() {
    let package = with_body(concat!(
        r#"<w:p><w:pPr><w:jc w:val="right"/></w:pPr><w:r><w:rPr><w:i/></w:rPr><w:t>"#,
        r#"<w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:t>Tide one.</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Tide two.</w:t></w:r></w:p>"#,
        r#"</w:t></w:r><w:bookmarkStart w:id="3" w:name="tides"/><w:bookmarkEnd w:id="3"/></w:p>"#,
        r#"<w:p><w:r><w:t>Closing.</w:t></w:r></w:p>"#
    ));
    assert_sound(&package);
    let document = parse(&package);
    // Word's reading: the two inner paragraphs, then the outer paragraph
    // left empty (holding the bookmark), then the next paragraph.
    assert_eq!(
        paragraphs(&document),
        ["Tide one.", "Tide two.", "", "Closing."]
    );
    let edited = edit(&document, "p_2", "Tide two.", "Tide three.");
    let reopened = Document::parse(&saved(&edited)).expect("reopen");
    assert!(
        reopened
            .read_accepted()
            .expect("accept")
            .to_text()
            .contains("Tide three."),
        "{}",
        reopened.to_text()
    );
}

// =============================================================================
// Further shapes, each checked against Word the same way (one document with
// and without the one change).
// =============================================================================

fn run(text: &str) -> String {
    format!(r#"<w:r><w:t xml:space="preserve">{text}</w:t></w:r>"#)
}

fn control(properties: &str, content: &str) -> String {
    format!(
        r#"<w:sdt><w:sdtPr><w:id w:val="4021"/>{properties}</w:sdtPr><w:sdtContent>{content}</w:sdtContent></w:sdt>"#
    )
}

fn issue_count(package: &Package, rule: &str) -> usize {
    messages(package)
        .iter()
        .filter(|m| m.contains(rule))
        .count()
}

const FIELD: &str = concat!(
    r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> PAGE </w:instrText></w:r>"#,
    r#"<w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>1</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r>"#
);
const START: &str = r#"<w:bookmarkStart w:id="5" w:name="berth"/>"#;
const END: &str = r#"<w:bookmarkEnd w:id="5"/>"#;

#[test]
fn a_plain_text_control_holding_a_field_or_a_bookmark_end_is_reported() {
    for body in [
        format!("<w:p>{}{}</w:p>", run("Quay "), control("<w:text/>", FIELD)),
        format!(
            "<w:p>{}{}</w:p>",
            run("Quay "),
            control(
                "<w:text/>",
                r#"<w:fldSimple w:instr=" PAGE "><w:r><w:t>1</w:t></w:r></w:fldSimple>"#
            )
        ),
        format!(
            "<w:p>{START}{}{}</w:p>",
            run("Quay "),
            control("<w:text/>", &format!("{}{END}", run("seven")))
        ),
        format!(
            "{}{}",
            PARAGRAPH,
            control(
                "<w:text/>",
                &format!("<w:p>{START}{}{END}</w:p>", run("Quay"))
            )
        ),
    ] {
        assert_eq!(issue_count(&with_body(&body), "I-SDT-004"), 1, "{body}");
    }
}

#[test]
fn plain_text_controls_word_accepts_are_sound() {
    for body in [
        format!("<w:p>{}{}</w:p>", run("Quay "), control("", FIELD)),
        format!(
            "<w:p>{}{}</w:p>",
            run("Quay "),
            control("<w:text/>", &format!("{START}{}{END}", run("seven")))
        ),
        format!(
            "<w:p>{}{}{END}</w:p>",
            run("Quay "),
            control("<w:text/>", &format!("{START}{}", run("seven")))
        ),
        format!(
            "<w:p>{}</w:p>",
            control(
                r#"<w:text w:multiLine="1"/>"#,
                &format!("{}<w:r><w:br/></w:r>{}", run("Quay"), run("seven"))
            )
        ),
        format!(
            "{PARAGRAPH}{}",
            control("<w:text/>", &format!("<w:p>{}</w:p>", run("Quay")))
        ),
        // Block level: markers before the text, or a start alone.
        format!(
            "{PARAGRAPH}{}",
            control(
                "<w:text/>",
                &format!("<w:p>{START}{END}{}</w:p>", run("Quay"))
            )
        ),
        format!(
            "{PARAGRAPH}{}<w:p>{END}{}</w:p>",
            control("<w:text/>", &format!("<w:p>{}{START}</w:p>", run("Quay"))),
            run("Next")
        ),
    ] {
        assert_sound(&with_body(&body));
    }
}

const ITEMS: &str = r#"<w:listItem w:displayText="Yes" w:value="Yes"/><w:listItem w:displayText="No" w:value="No"/>"#;
const PLACEHOLDER: &str = r#"<w:placeholder><w:docPart w:val="A4AFE79343DC459DB206A078F3CDFB6B"/></w:placeholder><w:showingPlcHdr/>"#;

#[test]
fn a_control_showing_its_placeholder_may_name_two_kinds_or_hold_paragraphs() {
    let picture_list = format!("<w:picture/><w:dropDownList>{ITEMS}</w:dropDownList>");
    assert_eq!(
        issue_count(
            &with_body(&format!(
                "<w:p>{}</w:p>",
                control(&picture_list, &run("Yes"))
            )),
            "I-SDT-003"
        ),
        1
    );
    assert_sound(&with_body(&format!(
        "<w:p>{}</w:p>",
        control(&format!("{PLACEHOLDER}{picture_list}"), &run("Choose"))
    )));
    let list = format!("<w:dropDownList>{ITEMS}</w:dropDownList>");
    assert_eq!(
        issue_count(
            &with_body(&control(&list, &format!("{PARAGRAPH}{PARAGRAPH}"))),
            "I-SDT-001"
        ),
        1
    );
    assert_sound(&with_body(&control(
        &format!("{PLACEHOLDER}{list}"),
        &format!("{PARAGRAPH}{PARAGRAPH}"),
    )));
}

fn commented(comment_id: &str, reference_id: &str) -> Package {
    let comments = format!(
        r#"{HEAD}<w:comments xmlns:w="{W_NS}"><w:comment w:id="{comment_id}" w:author="Ines" w:date="2024-03-02T09:10:00Z"><w:p>{}</w:p></w:comment></w:comments>"#,
        run("Check the tide.")
    );
    Package {
        content_types: override_for("word/comments.xml", &format!("{WML}comments+xml")),
        document_relationships: relationship("rIdC", "comments", "comments.xml"),
        parts: vec![part("word/comments.xml", &comments)],
        ..with_body(&format!(
            r#"<w:p><w:commentRangeStart w:id="{reference_id}"/>{}<w:commentRangeEnd w:id="{reference_id}"/><w:r><w:commentReference w:id="{reference_id}"/></w:r></w:p>"#,
            run("Quay")
        ))
    }
}

#[test]
fn comment_references_are_judged_only_when_comment_ids_are_numbers() {
    assert_sound(&commented("0", "0"));
    assert_sound(&commented("c1", "0"));
    assert_sound(&commented("c1", "c1"));
    assert_one_issue(&commented("7", "5"), &["commentReference"]);
}

#[test]
fn the_main_root_may_have_any_name_in_the_wordprocessing_namespace() {
    let renamed = |name: &str| {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        let original = build(&sound());
        let mut archive = ZipArchive::new(Cursor::new(original)).expect("zip");
        for index in 0..archive.len() {
            let mut file = archive.by_index(index).expect("entry");
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).expect("read");
            if file.name() == "word/document.xml" {
                bytes = String::from_utf8(bytes)
                    .expect("utf-8")
                    .replace("w:document", &format!("w:{name}"))
                    .into_bytes();
            }
            zip.start_file(file.name(), FileOptions::default())
                .expect("start");
            zip.write_all(&bytes).expect("write");
        }
        zip.finish().expect("finish").into_inner()
    };
    for name in ["wordDocument", "harbourDocument"] {
        let bytes = renamed(name);
        assert!(validate_docx_report(&bytes).expect("validate").ok, "{name}");
        let Ok(document) = Document::parse(&bytes) else {
            panic!("{name}: Word reads the body, so it must load");
        };
        assert_eq!(paragraphs(&document), ["The quay opens at eight."]);
    }
}

#[test]
fn an_empty_custom_xml_item_is_sound_but_whitespace_is_not() {
    let custom = |content: &str| Package {
        document_relationships: relationship("rIdX", "customXml", "../customXml/item1.xml"),
        parts: vec![part("customXml/item1.xml", content)],
        ..sound()
    };
    assert_sound(&custom(""));
    assert_one_issue(&custom("  \r\n"), &["custom XML part", "not well-formed"]);
}

#[test]
fn missing_core_people_and_embedded_package_parts_are_sound() {
    assert_sound(&Package {
        root_relationships: relationship(
            "rId2",
            "http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties",
            "docProps/core.xml",
        ),
        content_types: override_for(
            "docProps/core.xml",
            "application/vnd.openxmlformats-package.core-properties+xml",
        ),
        ..sound()
    });
    assert_sound(&Package {
        document_relationships: format!(
            "{}{}",
            relationship(
                "rIdP",
                "http://schemas.microsoft.com/office/2011/relationships/people",
                "people.xml"
            ),
            relationship("rIdE", "package", "embeddings/Sheet1.xlsx")
        ),
        ..sound()
    });
}

#[test]
fn an_external_target_on_a_part_word_reads_is_reported() {
    for kind in ["styles", "numbering", "fontTable", "settings"] {
        assert_one_issue(
            &Package {
                document_relationships: format!(
                    r#"<Relationship Id="rIdX" Type="{R_NS}/{kind}" Target="https://example.com/{kind}.xml" TargetMode="External"/>"#
                ),
                ..sound()
            },
            &[kind, "External", "Word repairs"],
        );
    }
}

fn with_styles_part(content: &str) -> Package {
    Package {
        content_types: override_for("word/styles.xml", &format!("{WML}styles+xml")),
        document_relationships: relationship("rIdS", "styles", "styles.xml"),
        parts: vec![part("word/styles.xml", content)],
        ..sound()
    }
}

#[test]
fn an_xml_version_other_than_1_0_is_reported() {
    let styles = format!(r#"{HEAD}<w:styles xmlns:w="{W_NS}"/>"#);
    assert_sound(&with_styles_part(&styles));
    assert_one_issue(
        &with_styles_part(&styles.replace(r#"version="1.0""#, r#"version="1.1""#)),
        &["styles part", "1.1", "Word repairs"],
    );
}

fn with_settings(content: &str) -> Package {
    Package {
        content_types: override_for("word/settings.xml", &format!("{WML}settings+xml")),
        document_relationships: relationship("rIdSt", "settings", "settings.xml"),
        parts: vec![part(
            "word/settings.xml",
            &format!(r#"{HEAD}<w:settings xmlns:w="{W_NS}">{content}</w:settings>"#),
        )],
        ..sound()
    }
}

#[test]
fn settings_revision_ids_and_future_elements_are_judged() {
    assert_sound(&with_settings(
        r#"<w:rsids><w:rsidRoot w:val="00AB12CD"/><w:rsid w:val="00AB12CD"/></w:rsids>"#,
    ));
    assert_one_issue(
        &with_settings(r#"<w:rsids><w:rsid w:val="ZZ12XY34"/></w:rsids>"#),
        &["not hexadecimal", "Word repairs"],
    );
    assert_one_issue(
        &with_settings(
            r#"<w99:futureOption xmlns:w99="http://schemas.microsoft.com/office/word/2099/wordml" w99:val="1"/>"#,
        ),
        &["2099/wordml", "Word repairs"],
    );
    assert_sound(&with_body(
        r#"<w:p w:rsidR="ZZ12XY34"><w:r><w:t>Quay</w:t></w:r></w:p>"#,
    ));
}

#[test]
fn an_element_in_an_unknown_namespace_is_reported_unless_ignorable() {
    for body in [
        r#"<w:p><tg:marker xmlns:tg="urn:example:tagger"/><w:r><w:t>Quay</w:t></w:r></w:p>"#,
        r#"<w:p><Insert Id="7" xmlns="urn:example:builder"/><w:r><w:t>Quay</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:rPr><tg:marker xmlns:tg="urn:example:tagger"/></w:rPr><w:t>Quay</w:t></w:r></w:p>"#,
    ] {
        assert_eq!(issue_count(&with_body(body), "I-NS-003"), 1, "{body}");
    }
    assert_sound(&with_body(
        r#"<w:p><tg:marker xmlns:tg="urn:example:tagger" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" mc:Ignorable="tg"/><w:r><w:t>Quay</w:t></w:r></w:p>"#,
    ));
}

#[test]
fn package_part_faults_are_reported() {
    let theme_colors: String = [
        "dk1", "lt1", "dk2", "lt2", "accent1", "accent2", "accent3", "accent4", "accent5",
        "accent6", "hlink",
    ]
    .iter()
    .map(|name| format!(r#"<a:{name}><a:srgbClr val="000000"/></a:{name}>"#))
    .collect();
    let theme = format!(
        r#"{HEAD}<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="Harbour"><a:themeElements><a:clrScheme name="Harbour">{theme_colors}</a:clrScheme></a:themeElements></a:theme>"#
    );
    assert_one_issue(
        &Package {
            content_types: override_for(
                "word/theme/theme1.xml",
                "application/vnd.openxmlformats-officedocument.theme+xml",
            ),
            document_relationships: relationship("rIdT", "theme", "theme/theme1.xml"),
            parts: vec![part("word/theme/theme1.xml", &theme)],
            ..sound()
        },
        &["a:folHlink", "Word repairs"],
    );

    let app = |inner: &str| Package {
        content_types: override_for(
            "docProps/app.xml",
            "application/vnd.openxmlformats-officedocument.extended-properties+xml",
        ),
        root_relationships: relationship("rId3", "extended-properties", "docProps/app.xml"),
        parts: vec![part(
            "docProps/app.xml",
            &format!(
                r#"{HEAD}<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties">{inner}</Properties>"#
            ),
        )],
        ..sound()
    };
    assert_sound(&app("<Application>Harbour</Application><Pages>-2</Pages>"));
    assert_one_issue(&app("<Pages>many</Pages>"), &["Pages", "not a number"]);
    assert_one_issue(
        &app("<Harbour>7</Harbour>"),
        &["Harbour", "does not define"],
    );

    let core = |dc: &str| Package {
        content_types: override_for(
            "docProps/core.xml",
            "application/vnd.openxmlformats-package.core-properties+xml",
        ),
        root_relationships: relationship(
            "rId2",
            "http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties",
            "docProps/core.xml",
        ),
        parts: vec![part(
            "docProps/core.xml",
            &format!(
                r#"{HEAD}<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="{dc}"><dc:title>Quay notice</dc:title></cp:coreProperties>"#
            ),
        )],
        ..sound()
    };
    assert_sound(&core("http://purl.org/dc/elements/1.1/"));
    assert_one_issue(
        &core("http://purl.org/dc/elements/1.1"),
        &["dc:title", "Word repairs"],
    );
}

#[test]
fn story_structure_faults_are_reported() {
    let anchor = |position: &str, extent: &str| {
        format!(
            concat!(
                r#"<w:p><w:r><w:drawing><wp:anchor xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" distT="0" distB="0" distL="0" distR="0" simplePos="0" relativeHeight="1" behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1">"#,
                r#"<wp:simplePos x="0" y="0"/>{}<wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV>"#,
                r#"<wp:extent {}/><wp:wrapNone/><wp:docPr id="1" name="Picture 1"/>"#,
                r#"<a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture">"#,
                r#"<pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:nvPicPr><pic:cNvPr id="0" name="buoy.png"/><pic:cNvPicPr/></pic:nvPicPr>"#,
                r#"<pic:blipFill><a:blip/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>"#,
                r#"<pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="152400" cy="152400"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr>"#,
                r#"</pic:pic></a:graphicData></a:graphic></wp:anchor></w:drawing></w:r></w:p>"#
            ),
            position, extent
        )
    };
    let column = |offset: &str| {
        format!(
            r#"<wp:positionH relativeFrom="column"><wp:posOffset>{offset}</wp:posOffset></wp:positionH>"#
        )
    };
    let fine = r#"cx="152400" cy="152400""#;
    assert_eq!(
        issue_count(&with_body(&anchor(&column("-9144"), fine)), "I-STRUCT-001"),
        0
    );
    for body in [
        anchor(&column("0"), r#"cx="152400" cy="-152400""#),
        anchor(&column("0"), r#"cx="152400.5" cy="152400""#),
        anchor(&column("12.75"), fine),
        anchor(
            r#"<wp:positionH relativeFrom="body"><wp:posOffset>0</wp:posOffset></wp:positionH>"#,
            fine,
        ),
        anchor(r#"<wp:positionH relativeFrom="column"><wp:align>middle</wp:align></wp:positionH>"#, fine),
        format!(
            "<w:p>{}</w:p>",
            control(
                r#"<w:dropDownList><w:listItem w:displayText="Yes"/></w:dropDownList>"#,
                &run("Yes")
            )
        ),
        concat!(
            r#"<w:p><w:r><mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006">"#,
            r#"<mc:Fallback><w:t>old</w:t></mc:Fallback><mc:Choice Requires="w14"><w:t>new</w:t></mc:Choice>"#,
            r#"</mc:AlternateContent></w:r></w:p>"#
        )
        .to_string(),
        r#"<w:p><w:bookmarkStart w:id="1"/><w:r><w:t>Quay</w:t></w:r><w:bookmarkEnd w:id="1"/></w:p>"#.to_string(),
        format!(
            "<w:p>{}{}</w:p>",
            control("", &format!("<w:p>{}</w:p>", run("Quay"))),
            run("seven")
        ),
        r#"<w:p><m:oMath xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math"><m:sSup><m:e><m:t>x</m:t></m:e><m:sup><m:r><m:t>2</m:t></m:r></m:sup></m:sSup></m:oMath></w:p>"#.to_string(),
    ] {
        assert!(issue_count(&with_body(&body), "I-STRUCT-001") >= 1, "{body}");
    }
}

#[test]
fn an_unnamed_bookmark_start_without_an_end_and_an_inline_control_ending_its_paragraph_are_sound() {
    assert_sound(&with_body(
        r#"<w:p><w:bookmarkStart w:displacedByCustomXml="next" w:id="0"/><w:r><w:t>Quay</w:t></w:r></w:p>"#,
    ));
    assert_sound(&with_body(&format!(
        "<w:p>{}{}</w:p>",
        run("Quay "),
        control("", &format!("<w:p>{}</w:p>", run("seven")))
    )));
}

#[test]
fn a_section_type_outside_its_type_is_not_judged() {
    // `bogus` cannot open but `page` opens cleanly: the values Word takes are
    // not established.
    assert_sound(&Package {
        body: format!(r#"{PARAGRAPH}<w:sectPr><w:type w:val="page"/></w:sectPr>"#),
        ..sound()
    });
}

#[test]
fn values_outside_their_type_follow_word() {
    for (properties, outcome) in [
        (
            r#"<w:pPr><w:textAlignment w:val="bogus"/></w:pPr>"#,
            "Word cannot open",
        ),
        (
            r#"<w:pPr><w:textDirection w:val="vertical"/></w:pPr>"#,
            "Word cannot open",
        ),
        (
            r#"<w:pPr><w:rPr><w:effect w:val="bogus"/></w:rPr></w:pPr>"#,
            "Word repairs",
        ),
        (
            r#"<w:pPr><w:rPr><w:em w:val="bogus"/></w:rPr></w:pPr>"#,
            "Word repairs",
        ),
    ] {
        assert_one_issue(
            &with_body(&format!(
                "<w:p>{properties}<w:r><w:t>Quay.</w:t></w:r></w:p>"
            )),
            &["I-ATTR-002", outcome],
        );
    }
    for section in [
        r#"<w:sectPr><w:cols w:num="two"/></w:sectPr>"#,
        r#"<w:sectPr><w:vAlign w:val="bogus"/></w:sectPr>"#,
        r#"<w:sectPr><w:textDirection w:val="vertical"/></w:sectPr>"#,
    ] {
        assert_one_issue(
            &Package {
                body: format!("{PARAGRAPH}{section}"),
                ..sound()
            },
            &["I-ATTR-002", "Word cannot open"],
        );
    }
}

#[test]
fn readings_of_values_word_ignores_or_wraps() {
    for body in [
        r#"<w:p><w:r><w:rPr><w:u w:val="solid"/></w:rPr><w:t>Quay.</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:rPr><w:sz w:val="99999999999"/></w:rPr><w:t>Quay.</w:t></w:r></w:p>"#,
        r#"<w:p><w:pPr><w:spacing w:line="360" w:lineRule="bogus"/></w:pPr><w:r><w:t>Quay.</w:t></w:r></w:p>"#,
        r#"<w:p><w:pPr><w:pBdr><w:top w:val="bogus" w:sz="4" w:space="1" w:color="auto"/></w:pBdr></w:pPr><w:r><w:t>Quay.</w:t></w:r></w:p>"#,
        r#"<w:p><w:pPr><w:tabs><w:tab w:val="bogus" w:pos="720"/></w:tabs></w:pPr><w:r><w:t>Quay.</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:rPr><w:kern w:val="big"/></w:rPr><w:t>Quay.</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:rPr><w:shd w:val="bogus" w:color="auto" w:fill="FFFF00"/></w:rPr><w:t>Quay.</w:t></w:r></w:p>"#,
        concat!(
            r#"<w:tbl><w:tblPr><w:tblLayout w:type="bogus"/></w:tblPr><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid>"#,
            r#"<w:tr><w:trPr><w:trHeight w:val="400" w:hRule="bogus"/></w:trPr><w:tc><w:p><w:r><w:t>Quay.</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#
        ),
        concat!(
            r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="-500"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="2400" w:type="px"/></w:tcPr>"#,
            r#"<w:p><w:r><w:t>Quay.</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#
        ),
    ] {
        let package = with_body(&format!("{body}{PARAGRAPH}"));
        assert_sound(&package);
        let document = parse(&package);
        let edited = edit(
            &document,
            &format!("p_{}", paragraphs(&document).len()),
            "The quay opens at eight.",
            "The quay opens at nine.",
        );
        Document::parse(&saved(&edited)).expect("the edited document reopens");
    }
}

#[test]
fn numbering_markers_beside_paragraph_changes_are_read_as_word_reads_them() {
    let numbering = format!(
        concat!(
            r#"{}<w:numbering xmlns:w="{}"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/>"#,
            r#"<w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#
        ),
        HEAD, W_NS
    );
    let marker = r#"<w:ins w:id="7" w:author="Ines" w:date="2024-03-02T09:10:00Z"/>"#;
    let change = |previous: &str| {
        format!(
            r#"<w:pPrChange w:id="8" w:author="Ines" w:date="2024-03-03T09:00:00Z"><w:pPr>{previous}</w:pPr></w:pPrChange>"#
        )
    };
    for properties in [
        format!(
            r#"<w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/>{marker}</w:numPr><w:jc w:val="both"/>{}"#,
            change(r#"<w:numPr><w:numId w:val="1"/></w:numPr>"#)
        ),
        format!(
            r#"<w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/>{marker}</w:numPr><w:jc w:val="both"/>{}"#,
            change(r#"<w:jc w:val="left"/>"#)
        ),
        format!(
            r#"<w:numPr>{marker}</w:numPr><w:jc w:val="both"/>{}"#,
            change(r#"<w:jc w:val="left"/>"#)
        ),
        format!(r#"<w:numPr>{marker}</w:numPr>"#),
    ] {
        let package = Package {
            content_types: override_for("word/numbering.xml", &format!("{WML}numbering+xml")),
            document_relationships: relationship("rIdN", "numbering", "numbering.xml"),
            parts: vec![part("word/numbering.xml", &numbering)],
            ..with_body(&format!(
                "<w:p><w:pPr>{properties}</w:pPr><w:r><w:t>Item one.</w:t></w:r></w:p>"
            ))
        };
        assert_sound(&package);
        let document = parse(&package);
        // Word keeps the numbering both ways; only the paragraph change is
        // resolved.
        for resolved in [
            document.read_accepted().expect("accept"),
            document.read_rejected().expect("reject"),
        ] {
            assert_eq!(paragraphs(&resolved), ["Item one."]);
        }
    }
}

#[test]
fn list_level_and_note_type_faults_are_reported() {
    let numbering = |level: &str| Package {
        content_types: override_for("word/numbering.xml", &format!("{WML}numbering+xml")),
        document_relationships: relationship("rIdN", "numbering", "numbering.xml"),
        parts: vec![part(
            "word/numbering.xml",
            &format!(
                r#"{HEAD}<w:numbering xmlns:w="{W_NS}"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0">{level}<w:lvlText w:val="%1."/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#
            ),
        )],
        ..sound()
    };
    assert_sound(&numbering(
        r#"<w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlJc w:val="bogus"/>"#,
    ));
    assert_one_issue(
        &numbering(r#"<w:start w:val="1"/><w:numFmt w:val="arabic"/>"#),
        &["numFmt", "arabic"],
    );
    assert_one_issue(
        &numbering(r#"<w:start w:val="one"/><w:numFmt w:val="decimal"/>"#),
        &["start", "one"],
    );

    let notes = |kind: &str, named: bool| {
        let settings = if named {
            r#"<w:footnotePr><w:footnote w:id="-1"/><w:footnote w:id="0"/></w:footnotePr>"#
        } else {
            ""
        };
        let footnotes = format!(
            concat!(
                r#"{}<w:footnotes xmlns:w="{}"><w:footnote w:type="{}" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote>"#,
                r#"<w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote>"#,
                r#"<w:footnote w:id="1"><w:p><w:r><w:t>Note.</w:t></w:r></w:p></w:footnote></w:footnotes>"#
            ),
            HEAD, W_NS, kind
        );
        Package {
            content_types: format!(
                "{}{}",
                override_for("word/footnotes.xml", &format!("{WML}footnotes+xml")),
                override_for("word/settings.xml", &format!("{WML}settings+xml"))
            ),
            document_relationships: format!(
                "{}{}",
                relationship("rIdFn", "footnotes", "footnotes.xml"),
                relationship("rIdSt", "settings", "settings.xml")
            ),
            parts: vec![
                part("word/footnotes.xml", &footnotes),
                part(
                    "word/settings.xml",
                    &format!(r#"{HEAD}<w:settings xmlns:w="{W_NS}">{settings}</w:settings>"#),
                ),
            ],
            ..with_body(
                r#"<w:p><w:r><w:t>Pier</w:t></w:r><w:r><w:footnoteReference w:id="1"/></w:r></w:p>"#,
            )
        }
    };
    assert_sound(&notes("separator", true));
    assert_sound(&notes("sep", false));
    assert_one_issue(&notes("sep", true), &["\"sep\"", "Word repairs"]);
}

#[test]
fn a_custom_property_set_that_is_not_a_guid_is_reported() {
    let custom = |fmtid: &str| Package {
        content_types: override_for(
            "docProps/custom.xml",
            "application/vnd.openxmlformats-officedocument.custom-properties+xml",
        ),
        root_relationships: relationship("rId4", "custom-properties", "docProps/custom.xml"),
        parts: vec![part(
            "docProps/custom.xml",
            &format!(
                r#"{HEAD}<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/custom-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><property fmtid="{fmtid}" pid="2" name="Berth"><vt:lpwstr>7</vt:lpwstr></property></Properties>"#
            ),
        )],
        ..sound()
    };
    assert_sound(&custom("{D5CDD505-2E9C-101B-9397-08002B2CF9AE}"));
    assert_one_issue(&custom("D5CDD505"), &["fmtid", "not a GUID"]);
}

/// The package of `sound()` with one item's bytes replaced.
fn with_item(name: &str, bytes: Vec<u8>) -> Vec<u8> {
    let original = build(&sound());
    let mut archive = ZipArchive::new(Cursor::new(original)).expect("zip");
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).expect("entry");
        let mut content = Vec::new();
        file.read_to_end(&mut content).expect("read");
        let entry_name = file.name().to_string();
        zip.start_file(entry_name.clone(), FileOptions::default())
            .expect("start");
        if entry_name == name {
            zip.write_all(&bytes).expect("write");
        } else {
            zip.write_all(&content).expect("write");
        }
    }
    zip.finish().expect("finish").into_inner()
}

#[test]
fn a_wrong_crc_on_intact_data_is_sound() {
    // Word does not check the CRC: the main part's stored CRC-32 changed in
    // both headers, its data intact, opens without repair.
    let mut bytes = build(&sound());
    let name = b"word/document.xml";
    let positions: Vec<usize> = bytes
        .windows(name.len())
        .enumerate()
        .filter(|(_, window)| *window == name)
        .map(|(index, _)| index)
        .collect();
    for position in positions {
        let (header, crc_offset) = if bytes[position - 30..position - 26] == *b"PK\x03\x04" {
            (position - 30, 14)
        } else if bytes[position - 46..position - 42] == *b"PK\x01\x02" {
            (position - 46, 16)
        } else {
            continue;
        };
        for byte in &mut bytes[header + crc_offset..header + crc_offset + 4] {
            *byte ^= 0xA5;
        }
    }
    assert!(validate_docx_report(&bytes).expect("validate").ok);
    let Ok(document) = Document::parse(&bytes) else {
        panic!("Word opens it cleanly, so it must load");
    };
    assert_eq!(paragraphs(&document), ["The quay opens at eight."]);
}

#[test]
fn utf16_content_types_are_read() {
    let mut archive = ZipArchive::new(Cursor::new(build(&sound()))).expect("zip");
    let mut text = String::new();
    archive
        .by_name("[Content_Types].xml")
        .expect("content types")
        .read_to_string(&mut text)
        .expect("read");
    let utf16 = text.replace(r#"encoding="UTF-8""#, r#"encoding="UTF-16""#);
    let mut bytes = vec![0xFF, 0xFE];
    for unit in utf16.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    let package = with_item("[Content_Types].xml", bytes);
    assert!(validate_docx_report(&package).expect("validate").ok);
    let Ok(document) = Document::parse(&package) else {
        panic!("Word opens it cleanly, so it must load");
    };
    assert_eq!(paragraphs(&document), ["The quay opens at eight."]);
}

#[test]
fn narrower_cases_word_opens_cleanly_are_sound() {
    // Math text inside a tracked change inside a math run.
    assert_sound(&with_body(concat!(
        r#"<w:p><m:oMath xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math"><m:r>"#,
        r#"<w:ins w:id="3" w:author="Ines" w:date="2024-03-02T09:10:00Z"><m:t>x</m:t></w:ins></m:r></m:oMath></w:p>"#
    )));
    // A plain-text control around a field, showing its placeholder.
    assert_sound(&with_body(&format!(
        "<w:p>{}{}</w:p>",
        run("Quay "),
        control(&format!("{PLACEHOLDER}<w:text/>"), FIELD)
    )));
    // An unnamed bookmark start displaced by custom XML.
    assert_sound(&with_body(
        r#"<w:p><w:bookmarkStart w:displacedByCustomXml="next" w:id="0"/><w:r><w:t>Quay</w:t></w:r><w:bookmarkEnd w:displacedByCustomXml="prev" w:id="0"/></w:p>"#,
    ));
    // A total editing time that is not a whole number.
    assert_sound(&Package {
        content_types: override_for(
            "docProps/app.xml",
            "application/vnd.openxmlformats-officedocument.extended-properties+xml",
        ),
        root_relationships: relationship("rId3", "extended-properties", "docProps/app.xml"),
        parts: vec![part(
            "docProps/app.xml",
            &format!(
                r#"{HEAD}<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties"><TotalTime>7.5</TotalTime><Pages>2</Pages></Properties>"#
            ),
        )],
        ..sound()
    });
}
