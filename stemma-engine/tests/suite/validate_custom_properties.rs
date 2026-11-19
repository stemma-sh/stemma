//! `validate` and the custom document properties part.
//!
//! A custom property belongs to the property set its `fmtid` names. Checked
//! against Word: it repairs a document whose `docProps/custom.xml` holds, in
//! one set, two properties with the same name (also when the names differ
//! only in case) or the same `pid`, and one holding a `pid` below 2 in any
//! set. The same name or `pid` in two different sets is fine, and so are
//! gaps between pids. Generators that stamp the same property several times
//! produce exactly the repaired shape.

use std::io::{Cursor, Write};

use stemma::validate_docx_report;
use zip::{ZipWriter, write::FileOptions};

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/docProps/custom.xml" ContentType="application/vnd.openxmlformats-officedocument.custom-properties+xml"/></Types>"#;
const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/custom-properties" Target="docProps/custom.xml"/></Relationships>"#;
const DOCUMENT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
const DOCUMENT: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>The ferry timetable changes on Monday.</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="12240" w:h="15840"/></w:sectPr></w:body></w:document>"#;

const USER_SET: &str = "{D5CDD505-2E9C-101B-9397-08002B2CF9AE}";
const OTHER_SET: &str = "{5C58129F-E5B8-477A-9B38-B3E54BFA04C8}";

/// `(pid, name, value element)` per property, all in the user-defined set.
fn docx(properties: &[(i32, &str, &str)]) -> Vec<u8> {
    let in_sets: Vec<_> = properties
        .iter()
        .map(|&(pid, name, value)| (USER_SET, pid, name, value))
        .collect();
    docx_with_sets(&in_sets)
}

/// `(fmtid, pid, name, value element)` per property.
fn docx_with_sets(properties: &[(&str, i32, &str, &str)]) -> Vec<u8> {
    let listed: String = properties
        .iter()
        .map(|(set, pid, name, value)| {
            format!(r#"<property fmtid="{set}" pid="{pid}" name="{name}">{value}</property>"#)
        })
        .collect();
    let custom = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/custom-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes">{listed}</Properties>"#
    );
    let mut buf = Vec::new();
    {
        let mut zip = ZipWriter::new(Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        for (part, text) in [
            ("[Content_Types].xml", CONTENT_TYPES),
            ("_rels/.rels", ROOT_RELS),
            ("word/_rels/document.xml.rels", DOCUMENT_RELS),
            ("word/document.xml", DOCUMENT),
            ("docProps/custom.xml", custom.as_str()),
        ] {
            zip.start_file(part, opts).expect("start_file");
            zip.write_all(text.as_bytes()).expect("write part");
        }
        zip.finish().expect("finish zip");
    }
    buf
}

fn messages(bytes: &[u8]) -> Vec<String> {
    validate_docx_report(bytes)
        .expect("validate")
        .issues
        .into_iter()
        .map(|issue| issue.message)
        .collect()
}

const SEVEN: &str = "<vt:i4>7</vt:i4>";
const HIGH: &str = "<vt:lpwstr>high</vt:lpwstr>";

#[test]
fn distinct_names_and_ids_are_sound() {
    assert_eq!(
        messages(&docx(&[(2, "Berth", SEVEN), (3, "Tide", HIGH)])),
        Vec::<String>::new()
    );
}

#[test]
fn a_gap_between_ids_is_sound() {
    assert_eq!(
        messages(&docx(&[(2, "Berth", SEVEN), (9, "Tide", HIGH)])),
        Vec::<String>::new()
    );
}

#[test]
fn a_name_used_twice_is_reported() {
    assert_eq!(
        messages(&docx(&[(2, "Berth", SEVEN), (3, "Berth", HIGH)])),
        vec![
            r#"custom document property name "Berth" is used more than once; Word repairs such a file"#
                .to_string()
        ]
    );
}

#[test]
fn names_that_differ_only_in_case_are_the_same_name() {
    assert_eq!(
        messages(&docx(&[(2, "Berth", SEVEN), (3, "berth", HIGH)])),
        vec![
            r#"custom document property name "berth" is used more than once; Word repairs such a file"#
                .to_string()
        ]
    );
}

#[test]
fn an_id_used_twice_is_reported() {
    assert_eq!(
        messages(&docx(&[(2, "Berth", SEVEN), (2, "Tide", HIGH)])),
        vec![
            "custom document property id 2 is used more than once; Word repairs such a file"
                .to_string()
        ]
    );
}

#[test]
fn an_id_below_two_is_reported() {
    for pid in [0, 1] {
        assert_eq!(
            messages(&docx(&[(pid, "Berth", SEVEN)])),
            vec![format!(
                "custom document property id {pid} is below 2; Word repairs such a file"
            )]
        );
    }
}

#[test]
fn the_same_name_and_id_in_another_property_set_are_sound() {
    assert_eq!(
        messages(&docx_with_sets(&[
            (USER_SET, 2, "Berth", SEVEN),
            (OTHER_SET, 2, "Berth", HIGH)
        ])),
        Vec::<String>::new()
    );
}

#[test]
fn property_sets_are_compared_ignoring_case() {
    let lowercase = USER_SET.to_ascii_lowercase();
    assert_eq!(
        messages(&docx_with_sets(&[
            (USER_SET, 2, "Berth", SEVEN),
            (lowercase.as_str(), 2, "Tide", HIGH)
        ])),
        vec![
            "custom document property id 2 is used more than once; Word repairs such a file"
                .to_string()
        ]
    );
}

#[test]
fn an_id_below_two_in_another_property_set_is_reported() {
    assert_eq!(
        messages(&docx_with_sets(&[
            (USER_SET, 2, "Berth", SEVEN),
            (OTHER_SET, 1, "Tide", HIGH)
        ])),
        vec!["custom document property id 1 is below 2; Word repairs such a file".to_string()]
    );
}
