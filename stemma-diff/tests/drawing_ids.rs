//! A drawing's id (`wp:docPr/@id`, ISO 29500-1 §20.4.2.5) "shall be unique"
//! within the document. A redline holds both inputs' drawings, and two
//! documents number their drawings independently: a deleted picture and the
//! picture that replaces it, or a kept picture and a new one, can share an
//! id. The redline must still have unique drawing ids, and Accept All must
//! still give the target and Reject All the base.

use std::io::{Read, Write};

use stemma::api::Document;
use stemma::{ExportOptions, Resolution};

const RED_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0,
    0, 0, 144, 119, 83, 222, 0, 0, 0, 12, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 0, 0, 3, 1,
    1, 0, 201, 254, 146, 239, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];
const BLUE_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0,
    0, 0, 144, 119, 83, 222, 0, 0, 0, 12, 73, 68, 65, 84, 120, 156, 99, 96, 96, 248, 15, 0, 1, 3,
    1, 0, 8, 137, 194, 236, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

const NAMESPACES: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture""#;

/// An inline picture of the image bound as `rid`, with drawing id `id`.
fn picture(rid: &str, id: u32) -> String {
    format!(
        r#"<w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="95250" cy="95250"/><wp:docPr id="{id}" name="Picture {id}"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic><pic:nvPicPr><pic:cNvPr id="0" name="pixel.png"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="{rid}"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="95250" cy="95250"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>"#
    )
}

fn text(words: &str) -> String {
    format!(r#"<w:r><w:t xml:space="preserve">{words}</w:t></w:r>"#)
}

/// A package whose body is `paragraphs` (each a run list), with two images
/// bound: `rIdRed` and `rIdBlue`.
fn docx(paragraphs: &[String]) -> Vec<u8> {
    let body: String = paragraphs
        .iter()
        .map(|p| format!("<w:p>{p}</w:p>"))
        .collect();
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document {NAMESPACES}><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdRed" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/red.png"/><Relationship Id="rIdBlue" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/blue.png"/></Relationships>"#;
    let mut bytes = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
        let options = zip::write::FileOptions::default();
        for (name, part) in [
            ("[Content_Types].xml", content_types.as_bytes()),
            ("_rels/.rels", rels.as_bytes()),
            ("word/_rels/document.xml.rels", doc_rels.as_bytes()),
            ("word/document.xml", document.as_bytes()),
            ("word/media/red.png", RED_PNG),
            ("word/media/blue.png", BLUE_PNG),
        ] {
            zip.start_file(name, options).expect("start part");
            zip.write_all(part).expect("write part");
        }
        zip.finish().expect("finish package");
    }
    bytes
}

/// Every `wp:docPr` id in the saved document part, in order.
fn drawing_ids(document: &Document) -> Vec<String> {
    let bytes = document
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("open package");
    let mut xml = String::new();
    archive
        .by_name("word/document.xml")
        .expect("document part")
        .read_to_string(&mut xml)
        .expect("read document part");
    xml.split("<wp:docPr ")
        .skip(1)
        .map(|element| {
            let start = element.find("id=\"").expect("docPr id") + 4;
            let end = start + element[start..].find('"').expect("closing quote");
            element[start..end].to_string()
        })
        .collect()
}

fn assert_readings(redline: &Document, base: &Document, target: &Document, case: &str) {
    for (label, resolution, input) in [
        ("Accept All", Resolution::AcceptAll, target),
        ("Reject All", Resolution::RejectAll, base),
    ] {
        let reading = redline.project(resolution).expect("project");
        let remaining =
            stemma_diff::diff_as_detailed(&reading, input, "check").expect("re-compare");
        assert_eq!(
            remaining.semantic_change_count, 0,
            "{case}: {label} differs from the input it gives back"
        );
    }
}

#[test]
fn a_redline_keeps_drawing_ids_unique() {
    let cases = [
        (
            "a picture replaced by another with the same id",
            vec![format!("{}{}", text("Site plan: "), picture("rIdRed", 1))],
            vec![format!("{}{}", text("Site plan: "), picture("rIdBlue", 1))],
        ),
        (
            "a new picture with a kept picture's id",
            vec![format!("{}{}", text("Site plan: "), picture("rIdRed", 1))],
            vec![
                format!("{}{}", text("Site plan: "), picture("rIdRed", 1)),
                format!("{}{}", text("Elevation: "), picture("rIdBlue", 1)),
            ],
        ),
    ];
    for (case, base, target) in cases {
        let base = Document::parse(&docx(&base)).expect("parse base");
        let target = Document::parse(&docx(&target)).expect("parse target");
        let redline = stemma_diff::diff_as(&base, &target, "compare").expect("compare");
        let reopened =
            Document::parse(&redline.serialize(&ExportOptions::default()).expect("save"))
                .expect("reopen");
        for document in [&redline, &reopened] {
            let ids = drawing_ids(document);
            assert_eq!(
                ids.len(),
                2,
                "{case}: both pictures are in the redline: {ids:?}"
            );
            let mut unique = ids.clone();
            unique.sort();
            unique.dedup();
            assert_eq!(
                unique.len(),
                ids.len(),
                "{case}: drawing ids repeat: {ids:?}"
            );
            assert_readings(document, &base, &target, case);
        }
    }
}
