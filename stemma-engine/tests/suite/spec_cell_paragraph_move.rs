//! `move` of paragraphs into, out of, and between table cells, as Word
//! records them.
//!
//! Word (16, oracle) records a cut and paste with Track Changes on as a move
//! only when the cut lies outside a table cell: a paragraph moved from the body
//! into a cell is a `w:moveFrom` in the body and a `w:moveTo` in the cell (its
//! mark, its runs, and a move range named like the source's). Accept All gives
//! the target, Reject All the base, either half resolves the whole move, and
//! the pairing holds after reopen. The same laws hold here, before and after
//! save and reopen and through the package-level resolution, and the
//! moved-from text is plain `w:t` (I-TC-004).
//!
//! A cut from inside a cell (within it, to another cell, to the body) Word
//! records as a tracked deletion plus a tracked insertion, two revisions, and
//! never cuts the cell's end mark: moving the cell's last paragraph leaves an
//! empty paragraph there. The verb writes exactly that. Refused: a cell's whole
//! content (`move_row` moves a row), a destination after a cell's last
//! paragraph, and a table into a cell.

use std::collections::HashSet;
use std::io::Write;

use stemma::api::Document;
use stemma::domain::{BlockNode, InlineNode, ParagraphNode, TrackingStatus};
use stemma::normalize::{normalize_docx, reject_all_docx};
use stemma::{ExportOptions, Resolution, ResolveSelectionAction, RevisionKind};

fn docx(body: &str) -> Vec<u8> {
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let parts = [
        (
            "[Content_Types].xml",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#.to_string(),
        ),
        (
            "_rels/.rels",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#.to_string(),
        ),
        ("word/document.xml", document),
    ];
    let mut bytes = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
        let options: zip::write::FileOptions = zip::write::FileOptions::default();
        for (name, data) in &parts {
            zip.start_file(*name, options).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    bytes
}

fn p(text: &str) -> String {
    format!("<w:p><w:r><w:t>{text}</w:t></w:r></w:p>")
}

/// Body "Harbor echo"; a two-row table whose first cell holds "Pine alpha",
/// "Pine bravo", "Pine charlie" and whose second holds "Oak delta"; body
/// "Harbor foxtrot", "Harbor golf", "Harbor hotel".
fn base() -> Document {
    let cell = |paragraphs: &[&str]| {
        format!(
            r#"<w:tr><w:tc><w:tcPr><w:tcW w:w="4000" w:type="dxa"/></w:tcPr>{}</w:tc></w:tr>"#,
            paragraphs.iter().map(|text| p(text)).collect::<String>()
        )
    };
    let body = p("Harbor echo")
        + r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid>"#
        + &cell(&["Pine alpha", "Pine bravo", "Pine charlie"])
        + &cell(&["Oak delta"])
        + "</w:tbl>"
        + &p("Harbor foxtrot")
        + &p("Harbor golf")
        + &p("Harbor hotel");
    Document::parse(&docx(&body)).expect("parse")
}

const BASE: &[&str] = &[
    "Harbor echo",
    "[Pine alpha / Pine bravo / Pine charlie | Oak delta]",
    "Harbor foxtrot",
    "Harbor golf",
    "Harbor hotel",
];

fn paragraph_text(paragraph: &ParagraphNode) -> String {
    paragraph
        .segments
        .iter()
        .flat_map(|segment| &segment.inlines)
        .filter_map(|inline| match inline {
            InlineNode::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect()
}

/// The layout of a document with no pending change: body paragraphs by
/// text, a table as `[cell | cell]` with each cell's paragraphs joined by
/// ` / `.
fn layout(document: &Document) -> Vec<String> {
    fn render(block: &BlockNode) -> String {
        match block {
            BlockNode::Paragraph(paragraph) => paragraph_text(paragraph),
            BlockNode::Table(table) => format!(
                "[{}]",
                table
                    .rows
                    .iter()
                    .flat_map(|row| &row.cells)
                    .map(|cell| cell
                        .blocks
                        .iter()
                        .map(render)
                        .collect::<Vec<_>>()
                        .join(" / "))
                    .collect::<Vec<_>>()
                    .join(" | ")
            ),
            BlockNode::OpaqueBlock(_) => "<opaque>".to_string(),
        }
    }
    assert!(
        document.revisions().is_empty(),
        "a resolved layout has no pending change: {:?}",
        document.revisions()
    );
    document
        .snapshot()
        .canonical
        .blocks
        .iter()
        .map(|tracked| render(&tracked.block))
        .collect()
}

fn find(document: &Document, text: &str) -> String {
    fn walk(blocks: &[BlockNode], text: &str) -> Option<String> {
        blocks.iter().find_map(|block| match block {
            BlockNode::Paragraph(p) if paragraph_text(p) == text => Some(p.id.0.to_string()),
            BlockNode::Table(table) => table
                .rows
                .iter()
                .flat_map(|row| &row.cells)
                .find_map(|cell| walk(&cell.blocks, text)),
            _ => None,
        })
    }
    let blocks: Vec<BlockNode> = document
        .snapshot()
        .canonical
        .blocks
        .iter()
        .map(|tracked| tracked.block.clone())
        .collect();
    walk(&blocks, text).unwrap_or_else(|| panic!("no paragraph reads {text:?}"))
}

fn move_op(document: &Document, from: &str, to: &str, anchor: &str, position: &str) -> String {
    let (from, to, anchor) = (
        find(document, from),
        find(document, to),
        find(document, anchor),
    );
    serde_json::json!({
        "ops": [{"op": "move", "target": {"from": from, "to": to},
                 "destination": {"anchor": anchor, "position": position}}],
        "revision": {"author": "Mover"}
    })
    .to_string()
}

fn try_move(
    document: &Document,
    from: &str,
    to: &str,
    anchor: &str,
    position: &str,
) -> Result<Document, stemma::RuntimeError> {
    let transaction =
        stemma::edit_v4::parse_transaction(&move_op(document, from, to, anchor, position))
            .unwrap()
            .into_edit_transaction()
            .unwrap();
    document.apply(&transaction)
}

fn save(document: &Document) -> Vec<u8> {
    document.serialize(&ExportOptions::default()).expect("save")
}

fn reopen(document: &Document) -> Document {
    Document::parse(&save(document)).expect("reopen")
}

fn document_xml(bytes: &[u8]) -> String {
    String::from_utf8_lossy(
        stemma::docx::DocxArchive::read(bytes)
            .unwrap()
            .get("word/document.xml")
            .unwrap(),
    )
    .to_string()
}

/// The one pending revision, which must be the move.
fn the_move(document: &Document, label: &str) -> u32 {
    let revisions = document.revisions();
    assert_eq!(
        revisions.len(),
        1,
        "{label}: the move is one revision: {revisions:?}"
    );
    assert_eq!(revisions[0].kind, RevisionKind::Move, "{label}");
    revisions[0].revision_id
}

fn select(document: &Document, id: u32, action: ResolveSelectionAction) -> Document {
    document
        .project(Resolution::Selective {
            ids: HashSet::from([id]),
            action,
        })
        .unwrap_or_else(|e| panic!("selective {action:?}: {e}"))
}

/// A move range ends after its last paragraph, never inside it before the
/// paragraph's end: Word (16) reads a mark outside the named range as a
/// revision of its own, not part of the move.
fn assert_ranges_cover_their_marks(xml: &str, label: &str) {
    for piece in xml.split("RangeEnd ").skip(1) {
        let after = piece.split_once("/>").map_or("", |(_, rest)| rest);
        assert!(
            !after.starts_with("</w:p>"),
            "{label}: a move range ends inside its last paragraph, leaving the mark out: \
             {}",
            &piece[..piece.len().min(200)]
        );
    }
}

/// The move laws, on the redline and on its save and reopen: Accept All gives
/// `target`, Reject All the base, and accepting or rejecting the move's one
/// id resolves both halves the same way; each result survives its own save
/// and reopen. The written move keeps moved-from text plain (I-TC-004).
fn assert_move_laws(redline: &Document, target: &[&str], label: &str) {
    let xml = document_xml(&save(redline));
    assert!(
        xml.contains("<w:moveFrom ") && xml.contains("<w:moveTo "),
        "{label}: written as a move: {xml}"
    );
    assert!(
        !xml.contains("<w:ins ") && !xml.contains("<w:del "),
        "{label}: no part of the move is a plain insertion or deletion: {xml}"
    );
    for piece in xml.split("<w:moveFrom ").skip(1) {
        let inside = piece.split("</w:moveFrom>").next().unwrap();
        assert!(
            !inside.contains("<w:delText"),
            "{label}: w:delText inside w:moveFrom: {inside}"
        );
    }
    assert!(stemma::api::validate(&save(redline)).ok, "{label}");
    assert_ranges_cover_their_marks(&xml, label);
    // A paragraph's properties come first (CT_P, §17.3.1.22): a range start
    // inside a paragraph follows its pPr.
    for chunk in xml.split("</w:p>") {
        let paragraph = chunk
            .rfind("<w:p>")
            .or_else(|| chunk.rfind("<w:p "))
            .map_or("", |at| &chunk[at..]);
        if let (Some(range), Some(properties)) =
            (paragraph.find("RangeStart "), paragraph.find("<w:pPr>"))
        {
            assert!(
                properties < range,
                "{label}: a move range starts before its paragraph's properties: {paragraph}"
            );
        }
    }

    // The package-level resolution agrees.
    let archive = stemma::docx::DocxArchive::read(&save(redline)).unwrap();
    for (resolved, expected, how) in [
        (normalize_docx(&archive), target, "package accept all"),
        (reject_all_docx(&archive), BASE, "package reject all"),
    ] {
        let (resolved, _) = resolved.unwrap_or_else(|e| panic!("{label}: {how}: {e:?}"));
        let document = Document::parse(&resolved.write().unwrap()).unwrap();
        assert_eq!(layout(&document), expected, "{label}: {how}");
    }

    let reopened = reopen(redline);
    for (document, when) in [(redline, "in memory"), (&reopened, "reopened")] {
        let label = format!("{label}, {when}");
        let id = the_move(document, &label);
        for (resolved, expected, how) in [
            (
                document.project(Resolution::AcceptAll).unwrap(),
                target,
                "accept all",
            ),
            (
                document.project(Resolution::RejectAll).unwrap(),
                BASE,
                "reject all",
            ),
            (
                select(document, id, ResolveSelectionAction::Accept),
                target,
                "accept the move",
            ),
            (
                select(document, id, ResolveSelectionAction::Reject),
                BASE,
                "reject the move",
            ),
        ] {
            assert_eq!(layout(&resolved), expected, "{label}: {how}");
            assert_eq!(
                layout(&reopen(&resolved)),
                expected,
                "{label}: {how}, reopened"
            );
        }
    }
}

#[test]
fn a_body_paragraph_moved_into_a_cell_is_one_move() {
    // Word's own recording of this cut and paste: moveFrom in the body,
    // moveTo in the cell, one range name.
    let redline = try_move(
        &base(),
        "Harbor foxtrot",
        "Harbor foxtrot",
        "Pine alpha",
        "before",
    )
    .expect("move into a cell");
    let xml = document_xml(&save(&redline));
    let cell = xml
        .split("<w:tc>")
        .nth(1)
        .unwrap()
        .split("</w:tc>")
        .next()
        .unwrap();
    assert!(
        cell.contains("<w:moveToRangeStart ")
            && cell.contains("<w:moveToRangeEnd ")
            && cell.contains("<w:rPr><w:moveTo "),
        "the cell holds the moveTo half with its mark: {cell}"
    );
    let name = |tag: &str| {
        xml.split(tag)
            .nth(1)
            .unwrap()
            .split("w:name=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap()
            .to_string()
    };
    assert_eq!(name("<w:moveFromRangeStart "), name("<w:moveToRangeStart "));
    assert_move_laws(
        &redline,
        &[
            "Harbor echo",
            "[Harbor foxtrot / Pine alpha / Pine bravo / Pine charlie | Oak delta]",
            "Harbor golf",
            "Harbor hotel",
        ],
        "body into a cell",
    );
}

#[test]
fn body_paragraphs_moved_into_a_cell_are_one_move() {
    let redline = try_move(
        &base(),
        "Harbor foxtrot",
        "Harbor golf",
        "Oak delta",
        "before",
    )
    .expect("move into a cell");
    assert_move_laws(
        &redline,
        &[
            "Harbor echo",
            "[Pine alpha / Pine bravo / Pine charlie | Harbor foxtrot / Harbor golf / Oak delta]",
            "Harbor hotel",
        ],
        "two body paragraphs into a cell",
    );
    let after = try_move(&base(), "Harbor golf", "Harbor golf", "Pine alpha", "after")
        .expect("move into the middle of a cell");
    assert_move_laws(
        &after,
        &[
            "Harbor echo",
            "[Pine alpha / Harbor golf / Pine bravo / Pine charlie | Oak delta]",
            "Harbor foxtrot",
            "Harbor hotel",
        ],
        "after a cell paragraph",
    );
}

/// The laws of a move Word records as a deletion plus an insertion: no move
/// markup, two revisions (one deletion, one insertion), Accept All gives
/// `target` and Reject All the base, in memory, reopened and through the
/// package; resolving either revision alone and then the other in the same
/// direction reaches the same result.
fn assert_edit_laws(redline: &Document, target: &[&str], label: &str) {
    let bytes = save(redline);
    let xml = document_xml(&bytes);
    assert!(
        !xml.contains("<w:moveFrom") && !xml.contains("<w:moveTo"),
        "{label}: Word records no move from inside a cell: {xml}"
    );
    assert!(stemma::api::validate(&bytes).ok, "{label}");
    let archive = stemma::docx::DocxArchive::read(&bytes).unwrap();
    for (resolved, expected, how) in [
        (normalize_docx(&archive), target, "package accept all"),
        (reject_all_docx(&archive), BASE, "package reject all"),
    ] {
        let (resolved, _) = resolved.unwrap_or_else(|e| panic!("{label}: {how}: {e:?}"));
        let document = Document::parse(&resolved.write().unwrap()).unwrap();
        assert_eq!(layout(&document), expected, "{label}: {how}");
    }
    let reopened = reopen(redline);
    for (document, when) in [(redline, "in memory"), (&reopened, "reopened")] {
        let label = format!("{label}, {when}");
        // Records are per carrier; a revision is one identity.
        let revisions = document.revisions();
        let mut ids: Vec<u32> = Vec::new();
        let mut kinds: Vec<RevisionKind> = Vec::new();
        for record in &revisions {
            if !ids.contains(&record.revision_id) {
                ids.push(record.revision_id);
                kinds.push(record.kind);
            }
            assert_eq!(
                record.kind,
                kinds[ids.iter().position(|id| *id == record.revision_id).unwrap()],
                "{label}: one revision is one kind: {revisions:?}"
            );
        }
        kinds.sort_by_key(|kind| format!("{kind:?}"));
        assert_eq!(
            kinds,
            [RevisionKind::Delete, RevisionKind::Insert],
            "{label}: one deletion and one insertion: {revisions:?}"
        );
        for (action, expected, how) in [
            (ResolveSelectionAction::Accept, target, "accept"),
            (ResolveSelectionAction::Reject, BASE, "reject"),
        ] {
            let whole = match action {
                ResolveSelectionAction::Accept => Resolution::AcceptAll,
                ResolveSelectionAction::Reject => Resolution::RejectAll,
            };
            assert_eq!(
                layout(&document.project(whole).unwrap()),
                expected,
                "{label}: {how} all"
            );
            for order in [[ids[0], ids[1]], [ids[1], ids[0]]] {
                let first = reopen(&select(document, order[0], action));
                let both = select(&first, order[1], action);
                assert_eq!(layout(&both), expected, "{label}: {how} one at a time");
                assert_eq!(layout(&reopen(&both)), expected, "{label}: {how}, reopened");
            }
        }
    }
}

#[test]
fn a_cell_paragraph_moved_into_the_body_is_a_deletion_and_an_insertion() {
    let redline = try_move(&base(), "Pine bravo", "Pine bravo", "Harbor golf", "before")
        .expect("move out of a cell");
    assert_edit_laws(
        &redline,
        &[
            "Harbor echo",
            "[Pine alpha / Pine charlie | Oak delta]",
            "Harbor foxtrot",
            "Pine bravo",
            "Harbor golf",
            "Harbor hotel",
        ],
        "cell into the body",
    );
}

#[test]
fn cell_paragraphs_moved_into_another_cell_are_a_deletion_and_an_insertion() {
    let redline = try_move(&base(), "Pine alpha", "Pine bravo", "Oak delta", "before")
        .expect("move between cells");
    assert_edit_laws(
        &redline,
        &[
            "Harbor echo",
            "[Pine charlie | Pine alpha / Pine bravo / Oak delta]",
            "Harbor foxtrot",
            "Harbor golf",
            "Harbor hotel",
        ],
        "cell into another cell",
    );
}

#[test]
fn a_cell_paragraph_moved_within_its_cell_is_a_deletion_and_an_insertion() {
    let redline = try_move(&base(), "Pine bravo", "Pine bravo", "Pine alpha", "before")
        .expect("move within a cell");
    assert_edit_laws(
        &redline,
        &[
            "Harbor echo",
            "[Pine bravo / Pine alpha / Pine charlie | Oak delta]",
            "Harbor foxtrot",
            "Harbor golf",
            "Harbor hotel",
        ],
        "within one cell",
    );
}

/// Word never cuts a cell's end mark: moving the cell's last paragraph takes
/// its text and leaves an empty paragraph in the cell (Word, Accept All).
#[test]
fn a_cells_last_paragraph_moves_its_text_and_leaves_an_empty_paragraph() {
    let redline = try_move(
        &base(),
        "Pine charlie",
        "Pine charlie",
        "Pine alpha",
        "before",
    )
    .expect("move a cell's last paragraph");
    assert_edit_laws(
        &redline,
        &[
            "Harbor echo",
            "[Pine charlie / Pine alpha / Pine bravo /  | Oak delta]",
            "Harbor foxtrot",
            "Harbor golf",
            "Harbor hotel",
        ],
        "a cell's last paragraph",
    );
    let range = try_move(
        &base(),
        "Pine bravo",
        "Pine charlie",
        "Harbor golf",
        "before",
    )
    .expect("move a range ending in a cell's last paragraph");
    assert_edit_laws(
        &range,
        &[
            "Harbor echo",
            "[Pine alpha /  | Oak delta]",
            "Harbor foxtrot",
            "Pine bravo",
            "Pine charlie",
            "Harbor golf",
            "Harbor hotel",
        ],
        "a range ending in a cell's last paragraph",
    );
}

fn assert_refused(result: Result<Document, stemma::RuntimeError>, needle: &str, label: &str) {
    let Err(error) = result else {
        panic!("{label}: refused");
    };
    assert_eq!(
        error.code,
        stemma::ErrorCode::UnsupportedEdit,
        "{label}: {error}"
    );
    assert!(error.message.contains(needle), "{label}: {error}");
}

#[test]
fn moves_word_does_not_carry_are_refused_with_the_reason() {
    let doc = base();
    assert_refused(
        try_move(&doc, "Oak delta", "Oak delta", "Harbor golf", "before"),
        "move_row",
        "a cell's whole content (one paragraph)",
    );
    assert_refused(
        try_move(&doc, "Pine alpha", "Pine charlie", "Oak delta", "before"),
        "move_row",
        "a cell's whole content",
    );
    assert_refused(
        try_move(&doc, "Harbor golf", "Harbor golf", "Pine charlie", "after"),
        "end mark",
        "after a cell's last paragraph",
    );
    let table_id = doc.snapshot().canonical.blocks[1].block.clone();
    let BlockNode::Table(table) = table_id else {
        panic!("the second block is the table")
    };
    let json = serde_json::json!({
        "ops": [{"op": "move", "target": {"from": find(&doc, "Harbor echo"), "to": table.id.0.to_string()},
                 "destination": {"anchor": find(&doc, "Oak delta"), "position": "before"}}],
        "revision": {"author": "Mover"}
    })
    .to_string();
    assert_refused(
        doc.apply(
            &stemma::edit_v4::parse_transaction(&json)
                .unwrap()
                .into_edit_transaction()
                .unwrap(),
        ),
        "only paragraphs move into a table cell",
        "a table into a cell",
    );
    assert!(
        try_move(&doc, "Pine alpha", "Pine alpha", "Pine bravo", "before").is_err(),
        "a move onto itself"
    );
    // A cell paragraph with a pending change of its own does not move.
    let pending = Document::parse(&docx(&format!(
        r#"{}<w:tbl><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc>{}<w:p><w:ins w:id="1" w:author="Other" w:date="2026-01-01T00:00:00Z"><w:r><w:t>Pine bravo</w:t></w:r></w:ins></w:p>{}</w:tc></w:tr></w:tbl>{}"#,
        p("Harbor echo"),
        p("Pine alpha"),
        p("Pine charlie"),
        p("Harbor golf")
    )))
    .unwrap();
    assert!(
        try_move(
            &pending,
            "Pine bravo",
            "Pine bravo",
            "Harbor golf",
            "before"
        )
        .is_err(),
        "a paragraph holding another change is not moved"
    );
}

/// A move Word wrote from the body into a cell (its exact markup, with
/// invented text) reads as one move and keeps its markup through a save
/// after an unrelated resolution.
#[test]
fn a_word_move_into_a_cell_reads_as_one_move_and_round_trips() {
    let m = r#"w:author="Mover" w:date="2026-01-01T00:00:00Z""#;
    let cell = format!(
        concat!(
            r#"<w:p><w:pPr><w:rPr><w:moveTo w:id="0" {m}/></w:rPr></w:pPr><w:moveToRangeStart w:id="1" {m} w:name="move1"/><w:moveTo w:id="2" {m}><w:r><w:t>Harbor foxtrot</w:t></w:r></w:moveTo></w:p>"#,
            r#"<w:p><w:pPr><w:rPr><w:moveTo w:id="3" {m}/></w:rPr></w:pPr><w:moveTo w:id="4" {m}><w:r><w:t>Harbor golf</w:t></w:r></w:moveTo></w:p><w:moveToRangeEnd w:id="1"/>"#,
            r#"<w:p><w:r><w:t>Oak delta</w:t></w:r></w:p>"#,
        ),
        m = m
    );
    let source = format!(
        concat!(
            r#"<w:p><w:pPr><w:rPr><w:moveFrom w:id="5" {m}/></w:rPr></w:pPr><w:moveFromRangeStart w:id="6" {m} w:name="move1"/><w:moveFrom w:id="7" {m}><w:r><w:t>Harbor foxtrot</w:t></w:r></w:moveFrom></w:p>"#,
            r#"<w:p><w:pPr><w:rPr><w:moveFrom w:id="8" {m}/></w:rPr></w:pPr><w:moveFrom w:id="9" {m}><w:r><w:t>Harbor golf</w:t></w:r></w:moveFrom></w:p><w:moveFromRangeEnd w:id="6"/>"#,
        ),
        m = m
    );
    let body = format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Harbor echo</w:t></w:r><w:ins w:id="20" w:author="Other" w:date="2026-01-02T00:00:00Z"><w:r><w:t xml:space="preserve"> aside</w:t></w:r></w:ins></w:p><w:tbl><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc>{}</w:tc></w:tr><w:tr><w:tc>{cell}</w:tc></w:tr></w:tbl>{source}{}"#,
        p("Pine alpha"),
        p("Harbor hotel"),
    );
    let doc = Document::parse(&docx(&body)).unwrap();
    let kinds: Vec<RevisionKind> = doc.revisions().iter().map(|r| r.kind).collect();
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| **kind == RevisionKind::Move)
            .count(),
        1,
        "{kinds:?}"
    );
    let other: HashSet<u32> = doc
        .revisions()
        .iter()
        .filter(|r| r.author.as_deref() == Some("Other"))
        .map(|r| r.revision_id)
        .collect();
    let written = doc
        .project(Resolution::Selective {
            ids: other,
            action: ResolveSelectionAction::Reject,
        })
        .unwrap();
    // Word's placement holds on a save of the untouched document and after
    // an unrelated resolution: each range ends after its last paragraph, so
    // both marks stay in the move.
    assert_ranges_cover_their_marks(&document_xml(&save(&doc)), "saved untouched");
    let xml = document_xml(&save(&written));
    assert_ranges_cover_their_marks(&xml, "saved after a resolution");
    let cell = xml
        .split("<w:tc>")
        .nth(2)
        .unwrap()
        .split("</w:tc>")
        .next()
        .unwrap();
    assert!(
        cell.contains("<w:moveToRangeStart ")
            && cell.contains("<w:moveToRangeEnd ")
            && cell.matches("<w:rPr><w:moveTo ").count() == 2
            && !cell.contains("<w:ins "),
        "the cell keeps the moveTo half: {cell}"
    );
    let reopened = reopen(&written);
    let id = the_move(&reopened, "reopened Word move");
    assert_eq!(
        layout(&select(&reopened, id, ResolveSelectionAction::Accept)),
        [
            "Harbor echo",
            "[Pine alpha | Harbor foxtrot / Harbor golf / Oak delta]",
            "Harbor hotel"
        ]
    );
    assert_eq!(
        layout(&select(&reopened, id, ResolveSelectionAction::Reject)),
        [
            "Harbor echo",
            "[Pine alpha | Oak delta]",
            "Harbor foxtrot",
            "Harbor golf",
            "Harbor hotel"
        ]
    );
    // The mark statuses are the move's: nothing else is pending.
    for tracked in &reopened.snapshot().canonical.blocks {
        if let BlockNode::Paragraph(p) = &tracked.block {
            assert!(
                !matches!(
                    p.para_mark_status,
                    Some(TrackingStatus::InsertedThenDeleted(_))
                ),
                "{p:?}"
            );
        }
    }
}

/// A move Word wrote in the body (moved whole paragraphs: the range starts
/// inside the first paragraph after its properties and ends after the last
/// paragraph's `</w:p>`) keeps that placement through a save, untouched and
/// after an unrelated edit, and stays one move.
#[test]
fn a_word_body_move_keeps_its_range_ends_after_the_paragraphs() {
    let m = r#"w:author="Mover" w:date="2026-01-01T00:00:00Z""#;
    let body = format!(
        concat!(
            r#"<w:p><w:r><w:t xml:space="preserve">Harbor echo</w:t></w:r><w:ins w:id="20" w:author="Other" w:date="2026-01-02T00:00:00Z"><w:r><w:t xml:space="preserve"> aside</w:t></w:r></w:ins></w:p>"#,
            r#"<w:p><w:pPr><w:rPr><w:moveTo w:id="0" {m}/></w:rPr></w:pPr><w:moveToRangeStart w:id="1" {m} w:name="move1"/><w:moveTo w:id="2" {m}><w:r><w:t>Harbor foxtrot</w:t></w:r></w:moveTo></w:p>"#,
            r#"<w:p><w:pPr><w:rPr><w:moveTo w:id="3" {m}/></w:rPr></w:pPr><w:moveTo w:id="4" {m}><w:r><w:t>Harbor golf</w:t></w:r></w:moveTo></w:p><w:moveToRangeEnd w:id="1"/>"#,
            r#"<w:p><w:r><w:t>Harbor middle</w:t></w:r></w:p>"#,
            r#"<w:p><w:pPr><w:rPr><w:moveFrom w:id="5" {m}/></w:rPr></w:pPr><w:moveFromRangeStart w:id="6" {m} w:name="move1"/><w:moveFrom w:id="7" {m}><w:r><w:t>Harbor foxtrot</w:t></w:r></w:moveFrom></w:p>"#,
            r#"<w:p><w:pPr><w:rPr><w:moveFrom w:id="8" {m}/></w:rPr></w:pPr><w:moveFrom w:id="9" {m}><w:r><w:t>Harbor golf</w:t></w:r></w:moveFrom></w:p><w:moveFromRangeEnd w:id="6"/>"#,
            r#"<w:p><w:r><w:t>Harbor hotel</w:t></w:r></w:p>"#,
        ),
        m = m
    );
    let doc = Document::parse(&docx(&body)).unwrap();
    let untouched = document_xml(&save(&doc));
    assert_ranges_cover_their_marks(&untouched, "body move saved untouched");
    assert!(
        untouched.contains("</w:p><w:moveFromRangeEnd "),
        "{untouched}"
    );
    assert!(
        untouched.contains("</w:p><w:moveToRangeEnd "),
        "{untouched}"
    );
    let other: HashSet<u32> = doc
        .revisions()
        .iter()
        .filter(|r| r.author.as_deref() == Some("Other"))
        .map(|r| r.revision_id)
        .collect();
    let written = doc
        .project(Resolution::Selective {
            ids: other,
            action: ResolveSelectionAction::Reject,
        })
        .unwrap();
    let xml = document_xml(&save(&written));
    assert_ranges_cover_their_marks(&xml, "body move saved after a resolution");
    let reopened = reopen(&written);
    let id = the_move(&reopened, "reopened body move");
    assert_eq!(
        layout(&select(&reopened, id, ResolveSelectionAction::Accept)),
        [
            "Harbor echo",
            "Harbor foxtrot",
            "Harbor golf",
            "Harbor middle",
            "Harbor hotel"
        ]
    );
    assert_eq!(
        layout(&select(&reopened, id, ResolveSelectionAction::Reject)),
        [
            "Harbor echo",
            "Harbor middle",
            "Harbor foxtrot",
            "Harbor golf",
            "Harbor hotel"
        ]
    );
}
