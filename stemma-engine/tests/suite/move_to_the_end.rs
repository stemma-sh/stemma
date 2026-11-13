//! A paragraph moved to the end of the document (Word's own shape and
//! readings, checked in Word 16 on a file Word wrote).
//!
//! Word cannot paste after the final mark, so the mover presses Enter at the
//! end of the last paragraph and pastes there: the old last paragraph's mark
//! becomes the mover's inserted mark, the moved paragraph follows with its
//! moveTo pilcrow, and the range ends are written at body level right after
//! each moved paragraph. Here another author then typed a new last paragraph.
//! Word's readings, resolving all and one author at a time:
//!
//! | resolution                         | paragraphs                                     |
//! |------------------------------------|------------------------------------------------|
//! | Accept All                         | Opening / Middle / Closing / Moving / Post     |
//! | Reject All                         | Opening / Moving / Middle / Closing            |
//! | reject Other                       | (the move pending) ... / Moving / (empty)      |
//! | reject Other, then accept Mover    | Opening / Middle / Closing / Moving / (empty)  |
//! | reject Other, then reject Mover    | Opening / Moving / Middle / Closing            |
//! | accept Other, then reject Mover    | Opening / Moving / Middle / ClosingPost        |
//! | accept Mover                       | Opening / Middle / Closing / Moving / Post(ins)|
//! | reject Mover                       | Opening / Moving / Middle / ClosingPost(ins)   |

use std::collections::HashSet;
use std::io::Write;

use stemma::api::Document;
use stemma::{ExportOptions, Resolution, ResolveSelectionAction};

fn docx(body: &str) -> Vec<u8> {
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let document_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    let mut bytes = Vec::new();
    {
        use zip::write::FileOptions;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
        let options: FileOptions = FileOptions::default();
        for (name, data) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", root_rels),
            ("word/_rels/document.xml.rels", document_rels),
            ("word/document.xml", document.as_str()),
        ] {
            zip.start_file(name, options).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    bytes
}

const MOVED_TO_THE_END: &str = concat!(
    r#"<w:p><w:r><w:t>Opening.</w:t></w:r></w:p>"#,
    r#"<w:p><w:pPr><w:rPr><w:moveFrom w:id="1" w:author="Mover" w:date="2026-01-01T00:00:00Z"/></w:rPr></w:pPr>"#,
    r#"<w:moveFromRangeStart w:id="2" w:author="Mover" w:date="2026-01-01T00:00:00Z" w:name="move1"/>"#,
    r#"<w:moveFrom w:id="3" w:author="Mover" w:date="2026-01-01T00:00:00Z"><w:r><w:t>Travelling.</w:t></w:r></w:moveFrom></w:p>"#,
    r#"<w:moveFromRangeEnd w:id="2"/>"#,
    r#"<w:p><w:r><w:t>Middle.</w:t></w:r></w:p>"#,
    r#"<w:p><w:pPr><w:rPr><w:ins w:id="4" w:author="Mover" w:date="2026-01-01T00:00:00Z"/></w:rPr></w:pPr><w:r><w:t>Closing.</w:t></w:r></w:p>"#,
    r#"<w:p><w:pPr><w:rPr><w:moveTo w:id="5" w:author="Mover" w:date="2026-01-01T00:00:00Z"/></w:rPr></w:pPr>"#,
    r#"<w:moveToRangeStart w:id="6" w:author="Mover" w:date="2026-01-01T00:00:00Z" w:name="move1"/>"#,
    r#"<w:moveTo w:id="7" w:author="Mover" w:date="2026-01-01T00:00:00Z"><w:r><w:t>Travelling.</w:t></w:r></w:moveTo></w:p>"#,
    r#"<w:moveToRangeEnd w:id="6"/>"#,
    r#"<w:p><w:ins w:id="8" w:author="Other" w:date="2026-01-02T00:00:00Z"><w:r><w:t>Postscript.</w:t></w:r></w:ins></w:p>"#,
);

fn texts(doc: &Document) -> Vec<String> {
    doc.read()
        .blocks
        .iter()
        .map(|block| block.text.clone())
        .collect()
}

fn ids(doc: &Document, author: &str) -> HashSet<u32> {
    stemma::enumerate_revisions(&doc.snapshot().canonical)
        .into_iter()
        .filter(|r| r.author.as_deref() == Some(author))
        .map(|r| r.revision_id)
        .collect()
}

fn resolve(doc: &Document, author: &str, action: ResolveSelectionAction) -> Document {
    doc.project(Resolution::Selective {
        ids: ids(doc, author),
        action,
    })
    .unwrap_or_else(|e| panic!("{action:?} {author}: {e}"))
}

fn reopen(doc: &Document) -> Document {
    Document::parse(&doc.serialize(&ExportOptions::default()).unwrap()).expect("reopen")
}

#[test]
fn a_paragraph_moved_to_the_end_resolves_in_every_order_as_word_does() {
    use ResolveSelectionAction::{Accept, Reject};
    let doc = Document::parse(&docx(MOVED_TO_THE_END)).expect("parse");
    let mut kinds: Vec<(String, &str)> = stemma::enumerate_revisions(&doc.snapshot().canonical)
        .into_iter()
        .map(|r| (r.author.unwrap_or_default(), r.kind.as_str()))
        .collect();
    kinds.sort();
    assert_eq!(
        kinds,
        vec![
            ("Mover".to_string(), "insert"),
            ("Mover".to_string(), "move"),
            ("Other".to_string(), "insert"),
        ],
        "Word lists the move, the mover's inserted mark and the new paragraph"
    );
    let moved = ["Opening.", "Middle.", "Closing.", "Travelling."];
    let base = ["Opening.", "Travelling.", "Middle.", "Closing."];
    let with = |paragraphs: &[&str], last: &str| -> Vec<String> {
        paragraphs
            .iter()
            .map(|p| p.to_string())
            .chain(std::iter::once(last.to_string()))
            .collect()
    };

    assert_eq!(
        texts(&doc.project(Resolution::AcceptAll).unwrap()),
        with(&moved, "Postscript.")
    );
    assert_eq!(texts(&doc.project(Resolution::RejectAll).unwrap()), base);

    let other_rejected = resolve(&doc, "Other", Reject);
    let reopened = reopen(&other_rejected);
    for (label, state) in [("in memory", &other_rejected), ("reopened", &reopened)] {
        let left: HashSet<&str> = stemma::enumerate_revisions(&state.snapshot().canonical)
            .into_iter()
            .map(|r| r.kind.as_str())
            .collect();
        assert_eq!(left, HashSet::from(["move", "insert"]), "{label}");
        assert_eq!(
            texts(&resolve(state, "Mover", Accept)),
            with(&moved, ""),
            "{label}: Word keeps the empty last paragraph"
        );
        assert_eq!(texts(&resolve(state, "Mover", Reject)), base, "{label}");
    }
    // The pending move keeps Word's spelling after the projection: moved
    // pilcrows as moveFrom/moveTo and both ranges closed.
    let bytes = other_rejected.serialize(&ExportOptions::default()).unwrap();
    let xml = String::from_utf8(
        stemma::docx::DocxArchive::read(&bytes)
            .unwrap()
            .get("word/document.xml")
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    for marker in ["<w:moveFromRangeEnd", "<w:moveToRangeEnd"] {
        assert!(xml.contains(marker), "{marker} kept: {xml}");
    }
    assert_eq!(
        xml.matches("<w:rPr><w:moveFrom ").count() + xml.matches("<w:rPr><w:moveTo ").count(),
        2,
        "the moved pilcrows stay moveFrom/moveTo: {xml}"
    );

    let joined = ["Opening.", "Travelling.", "Middle.", "Closing.Postscript."];
    assert_eq!(
        texts(&resolve(&resolve(&doc, "Other", Accept), "Mover", Reject)),
        joined
    );
    assert_eq!(
        texts(&resolve(&doc, "Mover", Accept)),
        with(&moved, "Postscript.")
    );
    assert_eq!(texts(&resolve(&doc, "Mover", Reject)), joined);
}
