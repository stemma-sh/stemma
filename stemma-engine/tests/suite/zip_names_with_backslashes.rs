//! A package whose ZIP entries are named with Windows path separators
//! (`word\document.xml`), as some archivers write them.
//!
//! Observed in Word: it opens such a package without repair, reading each
//! entry as the item its name stands for. So the package is sound, it loads,
//! and a saved copy names its items with `/`.

use std::io::{Cursor, Write};

use stemma::ExportOptions;
use stemma::api::{Document, validate};
use zip::{ZipArchive, ZipWriter, write::FileOptions};

fn package(separator: &str) -> Vec<u8> {
    let files = [
        (
            "[Content_Types].xml".to_string(),
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#,
        ),
        (
            format!("_rels{separator}.rels"),
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#,
        ),
        (
            format!("word{separator}document.xml"),
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>The tide table is posted at the pier.</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#,
        ),
    ];
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, content) in files {
        zip.start_file(name, FileOptions::default()).unwrap();
        zip.write_all(content.as_bytes()).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

#[test]
fn a_package_named_with_backslashes_is_sound_and_reads_the_same() {
    let plain = package("/");
    let windows = package("\\");
    assert_ne!(
        plain, windows,
        "the two packages name their entries differently"
    );
    let report = validate(&windows);
    assert!(report.ok, "{:?}", report.issues);
    let read = Document::parse(&windows).expect("the package loads");
    assert_eq!(
        read.to_text(),
        Document::parse(&plain).expect("parse").to_text()
    );
}

#[test]
fn a_saved_copy_names_its_items_with_forward_slashes() {
    let doc = Document::parse(&package("\\")).expect("the package loads");
    let saved = doc.serialize(&ExportOptions::default()).expect("serialize");
    let archive = ZipArchive::new(Cursor::new(saved)).unwrap();
    let names: Vec<&str> = archive.file_names().collect();
    assert!(names.contains(&"word/document.xml"), "{names:?}");
    assert!(names.iter().all(|name| !name.contains('\\')), "{names:?}");
}
