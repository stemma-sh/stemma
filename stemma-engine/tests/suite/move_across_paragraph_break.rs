//! A move of text that crosses a paragraph break, in the shape Word writes.
//!
//! Word (checked: a cut and paste of "tail one.¶head two" in Word with
//! tracking on) writes one move: `w:moveFromRangeStart` in the first source
//! paragraph and `w:moveFromRangeEnd` in the next, the moved break as
//! `w:moveFrom` on the first source paragraph's mark, and at the destination
//! the paragraph the paste split with `w:moveTo` on its new mark. Word lists
//! one moved-from/moved-to pair, and resolving it alone reads like resolving
//! everything.

use std::collections::HashSet;
use std::io::Write;

use stemma::api::Document;
use stemma::{ExportOptions, Resolution, ResolveSelectionAction, RevisionKind};

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

const MOVED_ACROSS_A_BREAK: &str = concat!(
    r#"<w:p><w:pPr><w:rPr><w:moveFrom w:id="0" w:author="Mover" w:date="2026-01-01T00:00:00Z"/></w:rPr></w:pPr>"#,
    r#"<w:r><w:t xml:space="preserve">Stay one. </w:t></w:r>"#,
    r#"<w:moveFromRangeStart w:id="1" w:author="Mover" w:date="2026-01-01T00:00:00Z" w:name="move1"/>"#,
    r#"<w:moveFrom w:id="2" w:author="Mover" w:date="2026-01-01T00:00:00Z"><w:r><w:t>going east.</w:t></w:r></w:moveFrom></w:p>"#,
    r#"<w:p><w:moveFrom w:id="3" w:author="Mover" w:date="2026-01-01T00:00:00Z"><w:r><w:t xml:space="preserve">going west </w:t></w:r></w:moveFrom>"#,
    r#"<w:moveFromRangeEnd w:id="1"/><w:r><w:t>stay two.</w:t></w:r></w:p>"#,
    r#"<w:p><w:pPr><w:rPr><w:moveTo w:id="4" w:author="Mover" w:date="2026-01-01T00:00:00Z"/></w:rPr></w:pPr>"#,
    r#"<w:r><w:t xml:space="preserve">Stay three. </w:t></w:r>"#,
    r#"<w:moveToRangeStart w:id="5" w:author="Mover" w:date="2026-01-01T00:00:00Z" w:name="move1"/>"#,
    r#"<w:moveTo w:id="6" w:author="Mover" w:date="2026-01-01T00:00:00Z"><w:r><w:t>going east.</w:t></w:r></w:moveTo></w:p>"#,
    r#"<w:p><w:moveTo w:id="7" w:author="Mover" w:date="2026-01-01T00:00:00Z"><w:r><w:t xml:space="preserve">going west </w:t></w:r></w:moveTo>"#,
    r#"<w:moveToRangeEnd w:id="5"/><w:r><w:t>stay four.</w:t></w:r></w:p>"#,
);

fn texts(doc: &Document) -> Vec<String> {
    doc.read()
        .blocks
        .iter()
        .map(|block| block.text.clone())
        .collect()
}

const ACCEPTED: [&str; 3] = [
    "Stay one. stay two.",
    "Stay three. going east.",
    "going west stay four.",
];
const REJECTED: [&str; 3] = [
    "Stay one. going east.",
    "going west stay two.",
    "Stay three. stay four.",
];

#[test]
fn a_move_across_a_paragraph_break_is_one_move_on_every_path() {
    let original = Document::parse(&docx(MOVED_ACROSS_A_BREAK)).expect("parse");
    let reopened =
        Document::parse(&original.serialize(&ExportOptions::default()).unwrap()).unwrap();
    for (label, doc) in [("imported", &original), ("reopened", &reopened)] {
        let revisions = stemma::enumerate_revisions(&doc.snapshot().canonical);
        assert_eq!(
            revisions.iter().map(|r| r.kind).collect::<Vec<_>>(),
            vec![RevisionKind::Move],
            "{label}: one move: {revisions:?}"
        );
        let id = revisions[0].revision_id;
        assert_eq!(
            texts(&doc.project(Resolution::AcceptAll).unwrap()),
            ACCEPTED,
            "{label}"
        );
        assert_eq!(
            texts(&doc.project(Resolution::RejectAll).unwrap()),
            REJECTED,
            "{label}"
        );
        for (action, expected) in [
            (ResolveSelectionAction::Accept, ACCEPTED),
            (ResolveSelectionAction::Reject, REJECTED),
        ] {
            let resolved = doc
                .project(Resolution::Selective {
                    ids: HashSet::from([id]),
                    action,
                })
                .expect("resolve the move");
            assert_eq!(
                texts(&resolved),
                expected,
                "{label}: {action:?} the move by id"
            );
            assert!(stemma::enumerate_revisions(&resolved.snapshot().canonical).is_empty());
        }
    }
}
