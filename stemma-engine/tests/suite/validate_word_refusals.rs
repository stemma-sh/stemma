//! `validate` and five shapes Word repairs or cannot open, plus a negative
//! paragraph spacing Word opens cleanly.
//!
//! Each rule was checked against Word with small documents that differ in
//! one piece:
//!
//! - a building block (glossary part) whose `w:category/w:gallery` value is
//!   not in ST_DocPartGallery (`quickParts`, `bogus`, `Placeholder`), or has
//!   no value: Word repairs. `placeholder`, `docParts`, `custQuickParts`, no
//!   `w:gallery` at all, or a glossary part without its content type (Word
//!   does not read it): clean;
//! - a `w:numPicBullet` with no child, an empty `w:pict` or an empty
//!   `w:drawing`: Word repairs, whether or not a level uses it. No bullet, or
//!   a VML picture bullet with or without image data: clean;
//! - a `w:contentPart` in a run: Word repairs, whatever it names (an ink
//!   part, an image, an OLE part). The same paragraph without it: clean;
//! - a `w:object` naming its part with `w:objectEmbed`: Word cannot open the
//!   file, whether the part is a valid compound file or not. The same object
//!   with `o:OLEObject` naming the part, or with only its shape: clean;
//! - the package thumbnail relationship targeting the main document part:
//!   Word repairs. Targeting a JPEG image: clean;
//! - `w:spacing/@w:before` and `@w:after` negative, in the document defaults
//!   or in a paragraph: clean, and Word keeps the value as written.

use std::io::{Cursor, Read, Write};

use stemma::ExportOptions;
use stemma::api::Document;
use stemma::validate_docx_report;
use zip::{ZipArchive, ZipWriter, write::FileOptions};

const HEAD: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#;
const NS: &str = concat!(
    r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" "#,
    r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" "#,
    r#"xmlns:v="urn:schemas-microsoft-com:vml" xmlns:o="urn:schemas-microsoft-com:office:office""#,
);
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/";
const WML: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.";
const PARAGRAPH: &str = r#"<w:p><w:r><w:t>The lighthouse keeper logs the tide.</w:t></w:r></w:p>"#;

/// A package built from its pieces, so one test can change one of them.
#[derive(Clone, Default)]
struct Package {
    /// Extra `Default` / `Override` entries of `[Content_Types].xml`.
    content_types: String,
    /// Extra `Relationship` entries of `_rels/.rels`.
    root_relationships: String,
    /// `Relationship` entries of `word/_rels/document.xml.rels`.
    document_relationships: String,
    body: String,
    /// Further parts: (name, bytes).
    parts: Vec<(String, Vec<u8>)>,
}

fn sound() -> Package {
    Package {
        body: PARAGRAPH.to_string(),
        ..Package::default()
    }
}

fn relationship(id: &str, kind: &str, target: &str) -> String {
    format!(r#"<Relationship Id="{id}" Type="{kind}" Target="{target}"/>"#)
}

fn relationships(entries: &str) -> String {
    format!(
        r#"{HEAD}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{entries}</Relationships>"#
    )
}

fn build(package: &Package) -> Vec<u8> {
    let mut files: Vec<(String, Vec<u8>)> = vec![
        (
            "[Content_Types].xml".to_string(),
            format!(
                concat!(
                    r#"{}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">"#,
                    r#"<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>"#,
                    r#"<Default Extension="xml" ContentType="application/xml"/>"#,
                    r#"<Override PartName="/word/document.xml" ContentType="{}document.main+xml"/>{}</Types>"#
                ),
                HEAD, WML, package.content_types
            )
            .into_bytes(),
        ),
        (
            "_rels/.rels".to_string(),
            relationships(&format!(
                "{}{}",
                relationship("rId1", &format!("{REL}officeDocument"), "word/document.xml"),
                package.root_relationships
            ))
            .into_bytes(),
        ),
        (
            "word/_rels/document.xml.rels".to_string(),
            relationships(&package.document_relationships).into_bytes(),
        ),
        (
            "word/document.xml".to_string(),
            format!(
                r#"{HEAD}<w:document {NS}><w:body>{}<w:sectPr/></w:body></w:document>"#,
                package.body
            )
            .into_bytes(),
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

#[test]
fn the_unchanged_document_is_sound() {
    assert_sound(&sound());
}

// --- building-block gallery -----------------------------------------------------

/// A glossary part with one building block; `category` is the content of its
/// `w:category`.
fn glossary(category: &str, declared: bool) -> Package {
    let part = format!(
        concat!(
            r#"{}<w:glossaryDocument {}><w:docParts><w:docPart><w:docPartPr>"#,
            r#"<w:name w:val="Tide table"/><w:category><w:name w:val="General"/>{}</w:category>"#,
            r#"<w:types><w:type w:val="bbPlcHdr"/></w:types><w:guid w:val="{{5D0C3A2E-1111-4222-8333-944455556666}}"/>"#,
            r#"</w:docPartPr><w:docPartBody><w:p><w:r><w:t>High water</w:t></w:r></w:p></w:docPartBody>"#,
            r#"</w:docPart></w:docParts></w:glossaryDocument>"#
        ),
        HEAD, NS, category
    );
    Package {
        content_types: if declared {
            format!(
                r#"<Override PartName="/word/glossary/document.xml" ContentType="{WML}document.glossary+xml"/>"#
            )
        } else {
            String::new()
        },
        document_relationships: relationship(
            "rId7",
            &format!("{REL}glossaryDocument"),
            "glossary/document.xml",
        ),
        parts: vec![("word/glossary/document.xml".to_string(), part.into_bytes())],
        ..sound()
    }
}

#[test]
fn a_gallery_word_does_not_know_is_reported() {
    for value in ["quickParts", "bogus", "Placeholder"] {
        assert_one_issue(
            &glossary(&format!(r#"<w:gallery w:val="{value}"/>"#), true),
            &["w:gallery", value, "Word repairs"],
        );
    }
    assert_one_issue(
        &glossary("<w:gallery/>", true),
        &["w:gallery has no w:val", "Word repairs"],
    );
}

#[test]
fn galleries_word_knows_are_sound() {
    for value in ["placeholder", "docParts", "custQuickParts"] {
        assert_sound(&glossary(&format!(r#"<w:gallery w:val="{value}"/>"#), true));
    }
    assert_sound(&glossary("", true));
}

#[test]
fn a_glossary_part_word_does_not_read_is_not_judged() {
    assert_sound(&glossary(r#"<w:gallery w:val="quickParts"/>"#, false));
}

// --- picture bullets --------------------------------------------------------------

const LISTED: &str = r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="3"/></w:numPr></w:pPr><w:r><w:t>Buoy</w:t></w:r></w:p>"#;
const VML_BULLET: &str = concat!(
    r#"<w:pict><v:shape id="_x0000_i1025" style="width:9pt;height:9pt" o:bullet="t">"#,
    r#"<v:imagedata r:id="rId1" o:title="dot"/></v:shape></w:pict>"#
);

/// A numbering part whose first child is `bullet`; the list level names
/// picture bullet 0 when `used`.
fn numbering(bullet: &str, used: bool) -> Package {
    let level_picture = if used {
        r#"<w:lvlPicBulletId w:val="0"/>"#
    } else {
        ""
    };
    let part = format!(
        concat!(
            r#"{}<w:numbering {}>{}<w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/>"#,
            r#"<w:numFmt w:val="bullet"/><w:lvlText w:val="o"/>{}</w:lvl></w:abstractNum>"#,
            r#"<w:num w:numId="3"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#
        ),
        HEAD, NS, bullet, level_picture
    );
    Package {
        content_types: format!(
            r#"<Default Extension="png" ContentType="image/png"/><Override PartName="/word/numbering.xml" ContentType="{WML}numbering+xml"/>"#
        ),
        document_relationships: relationship("rId5", &format!("{REL}numbering"), "numbering.xml"),
        body: LISTED.to_string(),
        parts: vec![
            ("word/numbering.xml".to_string(), part.into_bytes()),
            (
                "word/_rels/numbering.xml.rels".to_string(),
                relationships(&relationship(
                    "rId1",
                    &format!("{REL}image"),
                    "media/image1.png",
                ))
                .into_bytes(),
            ),
            ("word/media/image1.png".to_string(), b"\x89PNG".to_vec()),
        ],
        ..sound()
    }
}

fn picture_bullet(inner: &str) -> String {
    format!(r#"<w:numPicBullet w:numPicBulletId="0">{inner}</w:numPicBullet>"#)
}

#[test]
fn a_picture_bullet_with_no_picture_is_reported() {
    for inner in ["", "<w:pict/>", "<w:drawing/>"] {
        for used in [false, true] {
            assert_one_issue(
                &numbering(&picture_bullet(inner), used),
                &["w:numPicBullet", "holds no picture", "Word repairs"],
            );
        }
    }
}

#[test]
fn picture_bullets_word_accepts_are_sound() {
    assert_sound(&numbering("", false));
    assert_sound(&numbering(&picture_bullet(VML_BULLET), true));
    assert_sound(&numbering(
        &picture_bullet(
            r#"<w:pict><v:shape id="_x0000_i1025" style="width:9pt;height:9pt" o:bullet="t"/></w:pict>"#,
        ),
        false,
    ));
}

// --- w:contentPart -----------------------------------------------------------------

fn content_part(with_reference: bool) -> Package {
    let reference = if with_reference {
        r#"<w:r><w:contentPart r:id="rId8"/></w:r>"#
    } else {
        ""
    };
    Package {
        content_types: r#"<Override PartName="/word/ink/ink1.xml" ContentType="application/inkml+xml"/>"#
            .to_string(),
        document_relationships: relationship("rId8", &format!("{REL}customXml"), "ink/ink1.xml"),
        body: format!(r#"<w:p><w:r><w:t xml:space="preserve">Signed </w:t></w:r>{reference}</w:p>"#),
        parts: vec![(
            "word/ink/ink1.xml".to_string(),
            br#"<inkml:ink xmlns:inkml="http://www.w3.org/2003/InkML"><inkml:trace>0 0, 9 9</inkml:trace></inkml:ink>"#
                .to_vec(),
        )],
        ..sound()
    }
}

#[test]
fn a_content_part_in_a_run_is_reported() {
    assert_one_issue(
        &content_part(true),
        &["I-RUN-002", "w:contentPart", "Word repairs"],
    );
}

#[test]
fn the_same_paragraph_without_a_content_part_is_sound() {
    assert_sound(&content_part(false));
}

// --- embedded objects --------------------------------------------------------------

/// The 8-byte signature of an OLE compound file, padded: the validator does
/// not read the embedded part, so its full structure is not needed here.
fn compound_file_bytes() -> Vec<u8> {
    let mut bytes = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    bytes.resize(512, 0);
    bytes
}

fn embedded_object(link: &str) -> Package {
    Package {
        content_types:
            r#"<Default Extension="bin" ContentType="application/vnd.openxmlformats-officedocument.oleObject"/>"#
                .to_string(),
        document_relationships: relationship(
            "rId3",
            &format!("{REL}oleObject"),
            "embeddings/oleObject1.bin",
        ),
        body: format!(
            r#"<w:p><w:r><w:object w:dxaOrig="3360" w:dyaOrig="2520"><v:shape id="_x0000_i1026" style="width:168pt;height:126pt" o:ole=""/>{link}</w:object></w:r></w:p>"#
        ),
        parts: vec![(
            "word/embeddings/oleObject1.bin".to_string(),
            compound_file_bytes(),
        )],
        ..sound()
    }
}

#[test]
fn an_object_written_with_object_embed_is_reported() {
    assert_one_issue(
        &embedded_object(
            r#"<w:objectEmbed w:drawAspect="content" r:id="rId3" w:progId="Equation.3" w:shapeId="_x0000_i1026"/>"#,
        ),
        &["I-OBJ-001", "w:objectEmbed", "Word cannot open"],
    );
    assert_one_issue(
        &embedded_object(r#"<w:objectEmbed r:id="rId3"/>"#),
        &["I-OBJ-001"],
    );
}

#[test]
fn an_object_written_as_word_writes_it_is_sound() {
    assert_sound(&embedded_object(
        r#"<o:OLEObject Type="Embed" ProgID="Equation.3" ShapeID="_x0000_i1026" DrawAspect="Content" ObjectID="_1000000001" r:id="rId3"/>"#,
    ));
    assert_sound(&embedded_object(""));
}

// --- package thumbnail -------------------------------------------------------------

const THUMBNAIL: &str =
    "http://schemas.openxmlformats.org/package/2006/relationships/metadata/thumbnail";

#[test]
fn a_thumbnail_that_is_the_main_document_is_reported() {
    for target in ["word/document.xml", "/word/document.xml"] {
        assert_one_issue(
            &Package {
                root_relationships: relationship("rId2", THUMBNAIL, target),
                ..sound()
            },
            &["thumbnail", "main document part", "Word repairs"],
        );
    }
}

#[test]
fn a_jpeg_thumbnail_is_sound() {
    assert_sound(&Package {
        content_types: r#"<Default Extension="jpeg" ContentType="image/jpeg"/>"#.to_string(),
        root_relationships: relationship("rId2", THUMBNAIL, "docProps/thumbnail.jpeg"),
        parts: vec![(
            "docProps/thumbnail.jpeg".to_string(),
            vec![0xFF, 0xD8, 0xFF, 0xD9],
        )],
        ..sound()
    });
}

// --- negative paragraph spacing ------------------------------------------------

fn with_styles(default_spacing: &str, body: &str) -> Package {
    let styles = format!(
        concat!(
            r#"{}<w:styles {}><w:docDefaults><w:pPrDefault><w:pPr>{}</w:pPr></w:pPrDefault></w:docDefaults>"#,
            r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style></w:styles>"#
        ),
        HEAD, NS, default_spacing
    );
    Package {
        content_types: format!(
            r#"<Override PartName="/word/styles.xml" ContentType="{WML}styles+xml"/>"#
        ),
        document_relationships: relationship("rId4", &format!("{REL}styles"), "styles.xml"),
        body: body.to_string(),
        parts: vec![("word/styles.xml".to_string(), styles.into_bytes())],
        ..sound()
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

#[test]
fn negative_paragraph_spacing_is_read_and_kept() {
    let negative = r#"<w:spacing w:before="-1" w:after="-240"/>"#;
    for (package, part) in [
        (with_styles(negative, PARAGRAPH), "word/styles.xml"),
        (
            with_styles(
                "",
                &format!(
                    r#"<w:p><w:pPr>{negative}</w:pPr><w:r><w:t>The lighthouse keeper logs the tide.</w:t></w:r></w:p>"#
                ),
            ),
            "word/document.xml",
        ),
    ] {
        assert_sound(&package);
        let document = Document::parse(&build(&package)).expect("Word opens it cleanly");
        let saved = document.serialize(&ExportOptions::default()).expect("save");
        let xml = part_text(&saved, part);
        assert!(
            xml.contains(r#"w:before="-1""#) && xml.contains(r#"w:after="-240""#),
            "Word keeps the values as written; {part}: {xml}"
        );
    }
}
