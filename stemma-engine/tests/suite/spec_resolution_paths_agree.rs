//! Every way of resolving a redline reaches the same document.
//!
//! The four paths: Accept/Reject All on the model (`Document::project`), the
//! byte-level Accept/Reject All (`normalize_docx` / `reject_all_docx`),
//! selective resolution of every revision in one batch, and selective
//! resolution one revision at a time in every order. The domain rule
//! (ECMA-376 §17.13.5, and the selective-resolution contract in
//! docs/guide/revisions.md) is that selectively resolving a set of revisions
//! equals the total resolution restricted to that set, in any order, and that
//! resolving one revision never changes the tracking status of text no
//! revision claims. Each fixture states the expected Accept All and Reject All
//! reading from the spec; the harness checks every path reaches it, in memory
//! and after save and reopen.

use std::collections::HashSet;
use std::io::Write;

use stemma::api::Document;
use stemma::docx::DocxArchive;
use stemma::domain::{BlockNode, InlineNode, TrackedBlock, TrackingStatus};
use stemma::normalize::{normalize_docx, reject_all_docx};
use stemma::{ExportOptions, Resolution, ResolveSelectionAction};

fn docx(body: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = zip::write::FileOptions::default();
        for (name, part) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/document.xml", document_xml.as_str()),
        ] {
            zip.start_file(name, opts).expect("start part");
            zip.write_all(part.as_bytes()).expect("write part");
        }
        zip.finish().expect("finish package");
    }
    buf
}

const DATE: &str = r#"w:date="2026-01-01T00:00:00Z""#;

fn rev(id: u32, author: &str) -> String {
    format!(r#"w:id="{id}" w:author="{author}" {DATE}"#)
}

fn run(text: &str) -> String {
    format!(r#"<w:r><w:t xml:space="preserve">{text}</w:t></w:r>"#)
}

fn del_run(text: &str) -> String {
    format!(r#"<w:r><w:delText xml:space="preserve">{text}</w:delText></w:r>"#)
}

fn para(inner: &str) -> String {
    format!("<w:p>{inner}</w:p>")
}

/// A paragraph whose mark carries `mark` (`w:ins` / `w:del` / `w:moveFrom` /
/// `w:moveTo` with its attributes, e.g. `del w:id=..`).
fn para_marked(mark: &str, inner: &str) -> String {
    format!("<w:p><w:pPr><w:rPr><w:{mark}/></w:rPr></w:pPr>{inner}</w:p>")
}

/// One row, one paragraph run of cells; each cell is given as its paragraphs.
fn table(cells: &[&[String]]) -> String {
    let grid: String = cells.iter().map(|_| r#"<w:gridCol w:w="2000"/>"#).collect();
    let tcs: String = cells
        .iter()
        .map(|paragraphs| format!("<w:tc>{}</w:tc>", paragraphs.concat()))
        .collect();
    format!("<w:tbl><w:tblGrid>{grid}</w:tblGrid><w:tr>{tcs}</w:tr></w:tbl>")
}

/// The body as one line per block. Text a revision still claims is bracketed
/// (`{+ins+}`, `{-del-}`, `{+-ins then del-+}`) and a pending paragraph mark
/// is shown after the text (`¶+`, `¶-`, `¶+-`), so a path that leaves text in
/// the wrong revision, or changes the tracking of untracked text, reads
/// differently. A table reads as `[cell | cell]`, a cell's paragraphs joined
/// by ` / `.
fn reading(document: &Document) -> Vec<String> {
    document
        .snapshot()
        .canonical
        .blocks
        .iter()
        .map(tracked_block_reading)
        .collect()
}

fn tracked_block_reading(tracked: &TrackedBlock) -> String {
    let status = match &tracked.status {
        TrackingStatus::Normal => "",
        TrackingStatus::Inserted(_) => "ins-block ",
        TrackingStatus::Deleted(_) => "del-block ",
        TrackingStatus::InsertedThenDeleted(_) => "ins-del-block ",
    };
    format!("{status}{}", block_reading(&tracked.block))
}

fn block_reading(block: &BlockNode) -> String {
    match block {
        BlockNode::Paragraph(paragraph) => {
            let mut out = String::new();
            for segment in &paragraph.segments {
                let text: String = segment
                    .inlines
                    .iter()
                    .filter_map(|inline| match inline {
                        InlineNode::Text(text) => Some(text.text.as_str()),
                        _ => None,
                    })
                    .collect();
                if text.is_empty() {
                    continue;
                }
                match &segment.status {
                    TrackingStatus::Normal => out.push_str(&text),
                    TrackingStatus::Inserted(_) => out.push_str(&format!("{{+{text}+}}")),
                    TrackingStatus::Deleted(_) => out.push_str(&format!("{{-{text}-}}")),
                    TrackingStatus::InsertedThenDeleted(_) => {
                        out.push_str(&format!("{{+-{text}-+}}"))
                    }
                }
            }
            match &paragraph.para_mark_status {
                None | Some(TrackingStatus::Normal) => {}
                Some(TrackingStatus::Inserted(_)) => out.push_str("¶+"),
                Some(TrackingStatus::Deleted(_)) => out.push_str("¶-"),
                Some(TrackingStatus::InsertedThenDeleted(_)) => out.push_str("¶+-"),
            }
            out
        }
        BlockNode::Table(table) => {
            let rows: Vec<String> = table
                .rows
                .iter()
                .map(|row| {
                    let cells: Vec<String> = row
                        .cells
                        .iter()
                        .map(|cell| {
                            cell.blocks
                                .iter()
                                .map(block_reading)
                                .collect::<Vec<_>>()
                                .join(" / ")
                        })
                        .collect();
                    format!("[{}]", cells.join(" | "))
                })
                .collect();
            rows.join(" ")
        }
        BlockNode::OpaqueBlock(_) => "<opaque>".to_string(),
    }
}

fn reopen(document: &Document, context: &str) -> Document {
    let bytes = document
        .serialize(&ExportOptions::default())
        .unwrap_or_else(|e| panic!("{context}: serialize: {e:?}"));
    Document::parse(&bytes).unwrap_or_else(|e| panic!("{context}: reopen: {e:?}"))
}

/// The reading of `document`, checked to survive save and reopen.
fn persisted_reading(document: &Document, context: &str) -> Vec<String> {
    let in_memory = reading(document);
    let reopened = reading(&reopen(document, context));
    assert_eq!(
        in_memory, reopened,
        "{context}: the resolved document must equal its own save and reopen"
    );
    in_memory
}

fn revision_ids(document: &Document) -> Vec<u32> {
    let mut ids: Vec<u32> = Vec::new();
    for record in stemma::enumerate_revisions(&document.snapshot().canonical) {
        if !ids.contains(&record.revision_id) {
            ids.push(record.revision_id);
        }
    }
    ids
}

fn select(document: &Document, ids: &[u32], action: ResolveSelectionAction) -> Document {
    document
        .project(Resolution::Selective {
            ids: ids.iter().copied().collect::<HashSet<_>>(),
            action,
        })
        .unwrap_or_else(|e| panic!("selective {action:?} of {ids:?}: {e:?}"))
}

fn permutations(items: &[u32]) -> Vec<Vec<u32>> {
    if items.len() <= 1 {
        return vec![items.to_vec()];
    }
    let mut out = Vec::new();
    for (index, first) in items.iter().enumerate() {
        let mut rest = items.to_vec();
        rest.remove(index);
        for mut tail in permutations(&rest) {
            tail.insert(0, *first);
            out.push(tail);
        }
    }
    out
}

fn byte_reading(bytes: &[u8], accept: bool) -> Vec<String> {
    let archive = DocxArchive::read(bytes).expect("read package");
    let (resolved, _) = if accept {
        normalize_docx(&archive).expect("byte accept all")
    } else {
        reject_all_docx(&archive).expect("byte reject all")
    };
    let document = Document::parse(&resolved.write().expect("write package"))
        .expect("reopen byte-path result");
    reading(&document)
}

/// Every resolution path, in both directions, reaches `accepted` / `rejected`.
fn assert_every_path(body: &str, accepted: &[&str], rejected: &[&str]) {
    let bytes = docx(body);
    let original = Document::parse(&bytes).expect("parse fixture");
    let ids = revision_ids(&original);
    assert!(!ids.is_empty(), "the fixture has pending revisions");

    for (action, expected) in [
        (ResolveSelectionAction::Accept, accepted),
        (ResolveSelectionAction::Reject, rejected),
    ] {
        let accept = action == ResolveSelectionAction::Accept;
        let total = original
            .project(if accept {
                Resolution::AcceptAll
            } else {
                Resolution::RejectAll
            })
            .expect("total resolution");
        assert_eq!(
            persisted_reading(&total, &format!("{action:?} All")),
            expected,
            "{action:?} All on the model"
        );
        assert_eq!(
            byte_reading(&bytes, accept),
            expected,
            "{action:?} All on the bytes"
        );
        let batch = select(&original, &ids, action);
        assert_eq!(
            persisted_reading(&batch, &format!("selective {action:?} of {ids:?}")),
            expected,
            "selective {action:?} of every revision at once"
        );
        // One at a time, in every order: once chaining the in-memory results,
        // once saving and reopening after every step.
        for order in permutations(&ids) {
            for reopen_between in [false, true] {
                let mut document = Document::parse(&bytes).expect("parse fixture");
                // The ids still to resolve, in the order of `order`.
                let mut pending: Vec<u32> = order.clone();
                while let Some(id) = (!pending.is_empty()).then(|| pending.remove(0)) {
                    // A step may already have resolved this id as part of
                    // another (a move resolves as one).
                    if !revision_ids(&document).contains(&id) {
                        continue;
                    }
                    document = select(&document, &[id], action);
                    if reopen_between {
                        let step = format!("{action:?} {order:?} at {id}");
                        let before = revision_ids(&document);
                        let reopened = reopen(&document, &step);
                        let after = revision_ids(&reopened);
                        assert_eq!(
                            after.len(),
                            before.len(),
                            "{step}: the pending revisions must survive save and reopen"
                        );
                        // An id is derived from the revision's content, so a
                        // revision whose content an earlier step changed (a
                        // move whose moved text lost an accepted deletion)
                        // reopens under a new id. Follow it by position.
                        for id in &mut pending {
                            if let Some(index) = before.iter().position(|b| b == id) {
                                *id = after[index];
                            }
                        }
                        document = reopened;
                    }
                }
                let context = format!(
                    "selective {action:?} one at a time in the order {order:?} \
                     (reopened between steps: {reopen_between})"
                );
                assert_eq!(
                    persisted_reading(&document, &context),
                    expected,
                    "{context}"
                );
            }
        }
    }
}

// ─── A tracked mark on the last paragraph of a table cell ──────────────────
//
// §17.13.5.15 / §17.13.5.20: removing a paragraph mark joins the paragraph's
// content into the FOLLOWING paragraph. The last paragraph of a cell has no
// following paragraph — its mark ends the cell (CT_Tc §17.4.66 requires one) —
// so resolving that mark away has nothing to join with and the content stays
// where it is. Untracked text is never removed by a resolution.

#[test]
fn deleted_mark_on_a_cells_only_paragraph_keeps_its_untracked_text() {
    let cell = para_marked(&format!("del {}", rev(1, "Bob")), &run("a"));
    let body = table(&[&[cell], &[para(&run("b"))]]) + &para(&run("After."));
    assert_every_path(&body, &["[a | b]", "After."], &["[a | b]", "After."]);
}

#[test]
fn deleted_mark_on_a_cells_last_paragraph_keeps_both_paragraphs() {
    let last = para_marked(&format!("del {}", rev(1, "Bob")), &run("p2"));
    let body = table(&[&[para(&run("p1")), last]]) + &para(&run("After."));
    assert_every_path(&body, &["[p1 / p2]", "After."], &["[p1 / p2]", "After."]);
}

#[test]
fn deleted_mark_on_a_cells_last_paragraph_keeps_untracked_text_beside_a_deletion() {
    let last = para_marked(
        &format!("del {}", rev(1, "Bob")),
        &format!(
            "{}<w:del {}>{}</w:del>",
            run("keep "),
            rev(2, "Bob"),
            del_run("gone")
        ),
    );
    let body = table(&[&[last]]) + &para(&run("After."));
    assert_every_path(&body, &["[keep ]", "After."], &["[keep gone]", "After."]);
}

#[test]
fn inserted_mark_on_a_cells_only_paragraph_keeps_its_untracked_text_on_reject() {
    let cell = para_marked(&format!("ins {}", rev(1, "Bob")), &run("a"));
    let body = table(&[&[cell], &[para(&run("b"))]]) + &para(&run("After."));
    assert_every_path(&body, &["[a | b]", "After."], &["[a | b]", "After."]);
}

// ─── Moves of text within paragraphs ───────────────────────────────────────
//
// §17.13.5.22 / §17.13.5.25: a move is a deletion at its source (moveFrom)
// and an insertion at its destination (moveTo) that resolve together. When
// both halves are runs inside paragraphs that otherwise stay, accepting
// removes the source text and keeps the destination text, rejecting does the
// reverse; the paragraphs themselves are untouched.

/// A run-level move half wrapped in its Word-written range markers.
fn inline_move(kind: &str, range_id: u32, id: u32, text: &str) -> String {
    format!(
        r#"<w:{kind}RangeStart {} w:name="m1"/><w:{kind} {}>{}</w:{kind}><w:{kind}RangeEnd w:id="{range_id}"/>"#,
        rev(range_id, "Mover"),
        rev(id, "Mover"),
        run(text)
    )
}

#[test]
fn text_moved_within_one_paragraph_resolves_in_place() {
    let body = para(&format!(
        "{}{}{}{}{}",
        run("Before. "),
        inline_move("moveFrom", 10, 11, "moved"),
        run(" Middle. "),
        inline_move("moveTo", 12, 13, "moved"),
        run(" After.")
    ));
    assert_every_path(
        &body,
        &["Before.  Middle. moved After."],
        &["Before. moved Middle.  After."],
    );
}

#[test]
fn text_moved_between_paragraphs_keeps_both_paragraphs() {
    let body = para(&format!(
        "{}{}{}",
        run("First "),
        inline_move("moveFrom", 10, 11, "moved"),
        run(" end.")
    )) + &para(&format!(
        "{}{}{}",
        run("Second "),
        inline_move("moveTo", 12, 13, "moved"),
        run(" end.")
    )) + &para(&run("Third."));
    assert_every_path(
        &body,
        &["First  end.", "Second moved end.", "Third."],
        &["First moved end.", "Second  end.", "Third."],
    );
}

#[test]
fn a_whole_paragraph_move_still_resolves_as_one_unit() {
    let source = format!(
        r#"<w:p><w:pPr><w:rPr><w:moveFrom {}/></w:rPr></w:pPr><w:moveFromRangeStart {} w:name="m1"/><w:moveFrom {}>{}</w:moveFrom><w:moveFromRangeEnd w:id="1"/></w:p>"#,
        rev(0, "Mover"),
        rev(1, "Mover"),
        rev(2, "Mover"),
        run("Alpha.")
    );
    let destination = format!(
        r#"<w:p><w:pPr><w:rPr><w:moveTo {}/></w:rPr></w:pPr><w:moveToRangeStart {} w:name="m1"/><w:moveTo {}>{}</w:moveTo><w:moveToRangeEnd w:id="4"/></w:p>"#,
        rev(3, "Mover"),
        rev(4, "Mover"),
        rev(5, "Mover"),
        run("Alpha.")
    );
    let body = source + &para(&run("Middle.")) + &destination + &para(&run("Tail."));
    assert_every_path(
        &body,
        &["Middle.", "Alpha.", "Tail."],
        &["Alpha.", "Middle.", "Tail."],
    );
}

// ─── A mark deletion joining into another author's pending paragraph ───────
//
// §17.13.5.15: accepting a deleted paragraph mark joins the paragraph's runs
// into the following paragraph; the joined runs keep their own status. When
// the following paragraph is still another author's pending proposal (a
// whole inserted or deleted paragraph), the joined untracked text does not
// become part of that proposal: resolving it later removes only its own text.

/// "Keep B." whose mark Bob deleted, then `middle`, then "Z.".
fn mark_deletion_before(middle: &str) -> String {
    para_marked(&format!("del {}", rev(1, "Bob")), &run("Keep B.")) + middle + &para(&run("Z."))
}

#[test]
fn joining_into_another_authors_inserted_paragraph_keeps_the_joined_text_untracked() {
    let inserted = para_marked(
        &format!("ins {}", rev(2, "Carol")),
        &format!("<w:ins {}>{}</w:ins>", rev(3, "Carol"), run("New.")),
    );
    assert_every_path(
        &mark_deletion_before(&inserted),
        &["Keep B.New.", "Z."],
        &["Keep B.", "Z."],
    );
    // The mixed resolutions, in both orders: accepting Bob's join and
    // rejecting Carol's paragraph leaves Bob's text joined to "Z.".
    let original = Document::parse(&docx(&mark_deletion_before(&inserted))).expect("parse");
    let bob = revision_by(&original, "Bob");
    let carol = revision_by(&original, "Carol");
    let accept_then_reject = select(
        &reopen(
            &select(&original, &[bob], ResolveSelectionAction::Accept),
            "accept Bob",
        ),
        &[carol],
        ResolveSelectionAction::Reject,
    );
    let reject_then_accept = select(
        &reopen(
            &select(&original, &[carol], ResolveSelectionAction::Reject),
            "reject Carol",
        ),
        &[bob],
        ResolveSelectionAction::Accept,
    );
    assert_eq!(
        persisted_reading(&accept_then_reject, "accept Bob, reject Carol"),
        ["Keep B.Z."]
    );
    assert_eq!(
        persisted_reading(&reject_then_accept, "reject Carol, accept Bob"),
        ["Keep B.Z."]
    );
    // After only Bob's acceptance, the joined text is untracked and Carol's
    // insertion is exactly her own text and mark.
    assert_eq!(
        persisted_reading(
            &select(&original, &[bob], ResolveSelectionAction::Accept),
            "accept Bob"
        ),
        ["Keep B.{+New.+}¶+", "Z."]
    );
}

#[test]
fn joining_into_another_authors_deleted_paragraph_keeps_the_joined_text_untracked() {
    let deleted = para_marked(
        &format!("del {}", rev(2, "Carol")),
        &format!("<w:del {}>{}</w:del>", rev(3, "Carol"), del_run("Old.")),
    );
    assert_every_path(
        &mark_deletion_before(&deleted),
        &["Keep B.Z."],
        &["Keep B.", "Old.", "Z."],
    );
    let original = Document::parse(&docx(&mark_deletion_before(&deleted))).expect("parse");
    let bob = revision_by(&original, "Bob");
    assert_eq!(
        persisted_reading(
            &select(&original, &[bob], ResolveSelectionAction::Accept),
            "accept Bob"
        ),
        ["Keep B.{-Old.-}¶-", "Z."]
    );
}

fn revision_by(document: &Document, author: &str) -> u32 {
    let mut ids: Vec<u32> = stemma::enumerate_revisions(&document.snapshot().canonical)
        .into_iter()
        .filter(|record| record.author.as_deref() == Some(author))
        .map(|record| record.revision_id)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 1, "one revision by {author}: {ids:?}");
    ids[0]
}

#[test]
fn joining_into_a_moved_away_paragraph_keeps_the_joined_text_untracked() {
    let mover = |id| rev(id, "Mover");
    let body = para(&run("A."))
        + &format!(r#"<w:moveToRangeStart {} w:name="m1"/>"#, mover(1))
        + &para_marked(
            &format!("moveTo {}", mover(7)),
            &format!("<w:moveTo {}>{}</w:moveTo>", mover(8), run("C.")),
        )
        + r#"<w:moveToRangeEnd w:id="1"/>"#
        + &para_marked(&format!("del {}", rev(9, "Bob")), &run("B."))
        + &format!(r#"<w:moveFromRangeStart {} w:name="m1"/>"#, mover(2))
        + &para_marked(
            &format!("moveFrom {}", mover(10)),
            &format!("<w:moveFrom {}>{}</w:moveFrom>", mover(11), run("C.")),
        )
        + r#"<w:moveFromRangeEnd w:id="2"/>"#
        + &para(&run("Z."));
    assert_every_path(&body, &["A.", "C.", "B.Z."], &["A.", "B.", "C.", "Z."]);
}

#[test]
fn joining_into_a_moved_away_final_paragraph_keeps_the_joined_text_untracked() {
    // The shape Stemma's own comparison writes for "B and C swapped": the
    // move source is the document's last paragraph.
    let mover = |id| rev(id, "Mover");
    let body = para(&run("A."))
        + &format!(r#"<w:moveToRangeStart {} w:name="m1"/>"#, mover(1))
        + &para_marked(
            &format!("moveTo {}", mover(7)),
            &format!("<w:moveTo {}>{}</w:moveTo>", mover(8), run("C.")),
        )
        + r#"<w:moveToRangeEnd w:id="1"/>"#
        + &para_marked(&format!("del {}", rev(9, "Bob")), &run("B."))
        + &format!(r#"<w:moveFromRangeStart {} w:name="m1"/>"#, mover(2))
        + &para_marked(
            &format!("moveFrom {}", mover(10)),
            &format!("<w:moveFrom {}>{}</w:moveFrom>", mover(11), run("C.")),
        )
        + r#"<w:moveFromRangeEnd w:id="2"/>"#;
    assert_every_path(&body, &["A.", "C.", "B."], &["A.", "B.", "C."]);
}

// ─── A move of text combined with an edit inside it ────────────────────────
//
// Word nests a later edit inside a move half: a deletion inside the moveTo
// (text moved, then deleted at its destination) and an insertion inside the
// moveFrom (text inserted, then moved away). The nested edit belongs to the
// moved content, so Reject All removes a deletion nested in the destination
// together with the destination (§17.13.5.25: rejecting a moveTo removes its
// content), and Accept All removes an insertion nested in the source together
// with the source. Rejecting an inserted-then-moved text restores it at its
// source: the Reject-All rule the engine already applies to whole moved
// paragraphs and the byte-level path applies to every shape (not checked
// against Word for text inside a paragraph).

#[test]
fn text_moved_then_partly_deleted_at_its_destination_resolves_without_duplicating() {
    let body = para(&format!(
        "{}{}{}",
        run("A "),
        inline_move("moveFrom", 10, 11, "moved words"),
        run(" B.")
    )) + &para(&format!(
        r#"{}<w:moveToRangeStart {} w:name="m1"/><w:moveTo {}>{}</w:moveTo><w:moveTo {}><w:del {}>{}</w:del></w:moveTo><w:moveToRangeEnd w:id="12"/>{}"#,
        run("C "),
        rev(12, "Mover"),
        rev(13, "Mover"),
        run("moved "),
        rev(15, "Mover"),
        rev(14, "Bob"),
        del_run("words"),
        run(" D.")
    ));
    assert_every_path(
        &body,
        &["A  B.", "C moved  D."],
        &["A moved words B.", "C  D."],
    );
}

#[test]
fn text_inserted_then_moved_away_resolves_without_duplicating() {
    let body = para(&format!(
        r#"{}<w:moveFromRangeStart {} w:name="m1"/><w:moveFrom {}><w:ins {}>{}</w:ins></w:moveFrom><w:moveFromRangeEnd w:id="10"/>{}"#,
        run("A "),
        rev(10, "Mover"),
        rev(11, "Mover"),
        rev(14, "Bob"),
        run("fresh"),
        run(" B.")
    )) + &para(&format!(
        "{}{}{}",
        run("C "),
        inline_move("moveTo", 12, 13, "fresh"),
        run(" D.")
    ));
    assert_every_path(&body, &["A  B.", "C fresh D."], &["A fresh B.", "C  D."]);
}

#[test]
fn a_paragraph_inserted_then_moved_resolves_the_same_in_any_order() {
    let source = format!(
        r#"<w:p><w:pPr><w:rPr><w:ins {}/><w:moveFrom {}/></w:rPr></w:pPr><w:moveFromRangeStart {} w:name="m1"/><w:moveFrom {}><w:ins {}>{}</w:ins></w:moveFrom><w:moveFromRangeEnd w:id="2"/></w:p>"#,
        rev(0, "Origin"),
        rev(1, "Mover"),
        rev(2, "Mover"),
        rev(3, "Mover"),
        rev(4, "Origin"),
        run("Inserted.")
    );
    let destination = format!(
        r#"<w:p><w:pPr><w:rPr><w:moveTo {}/></w:rPr></w:pPr><w:moveToRangeStart {} w:name="m1"/><w:moveTo {}>{}</w:moveTo><w:moveToRangeEnd w:id="6"/></w:p>"#,
        rev(5, "Mover"),
        rev(6, "Mover"),
        rev(7, "Mover"),
        run("Inserted.")
    );
    let body = source + &para(&run("Middle.")) + &destination + &para(&run("Tail."));
    assert_every_path(
        &body,
        &["Middle.", "Inserted.", "Tail."],
        &["Inserted.", "Middle.", "Tail."],
    );
}

#[test]
fn a_paragraph_moved_then_partly_deleted_resolves_the_same_in_any_order() {
    let source = format!(
        r#"<w:p><w:pPr><w:rPr><w:moveFrom {}/></w:rPr></w:pPr><w:moveFromRangeStart {} w:name="m1"/><w:moveFrom {}>{}</w:moveFrom><w:moveFromRangeEnd w:id="1"/></w:p>"#,
        rev(0, "Mover"),
        rev(1, "Mover"),
        rev(2, "Mover"),
        run("Alpha.")
    );
    let destination = format!(
        r#"<w:p><w:pPr><w:rPr><w:moveTo {}/></w:rPr></w:pPr><w:moveToRangeStart {} w:name="m1"/><w:moveTo {}>{}<w:del {}>{}</w:del>{}</w:moveTo><w:moveToRangeEnd w:id="4"/></w:p>"#,
        rev(3, "Mover"),
        rev(4, "Mover"),
        rev(5, "Mover"),
        run("Al"),
        rev(6, "Editor"),
        del_run("pha"),
        run(".")
    );
    let body = source + &para(&run("Middle.")) + &destination + &para(&run("Tail."));
    assert_every_path(
        &body,
        &["Middle.", "Al.", "Tail."],
        &["Alpha.", "Middle.", "Tail."],
    );
}

// ─── A paragraph join blocked only by a table another change removes ───────
//
// §17.13.5.15: accepting a deleted paragraph mark joins the paragraph into the
// following one. A surviving table in between blocks the join (the paragraph
// stays, its mark ending it); a table that the same resolution removes does
// not (Accept All joins across it). When the only blocker is a table whose
// removal is ANOTHER pending revision, accepting the mark deletion alone has no
// order-independent outcome: the join depends on that table's later fate. The
// selection is refused, naming the revisions that must be resolved with it —
// the same contract as a partly selected shared table grid.

fn mark_deletion_before_deleted_table() -> String {
    let deleted_row = format!(
        r#"<w:tbl><w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:trPr><w:del {}/></w:trPr><w:tc><w:p><w:del {}>{}</w:del></w:p></w:tc><w:tc><w:p><w:del {}>{}</w:del></w:p></w:tc></w:tr></w:tbl>"#,
        rev(2, "Carol"),
        rev(2, "Carol"),
        del_run("x"),
        rev(2, "Carol"),
        del_run("y")
    );
    para_marked(&format!("del {}", rev(1, "Bob")), &run("Left"))
        + &deleted_row
        + &para(&run(" right"))
}

#[test]
fn a_join_blocked_only_by_a_table_another_change_deletes_is_refused_alone() {
    let body = mark_deletion_before_deleted_table();
    let bytes = docx(&body);
    let original = Document::parse(&bytes).expect("parse");
    let bob = revision_by(&original, "Bob");
    let carol = revision_by(&original, "Carol");

    let Err(refused) = original.project(Resolution::Selective {
        ids: HashSet::from([bob]),
        action: ResolveSelectionAction::Accept,
    }) else {
        panic!("accepting the join alone has no order-independent result");
    };
    assert_eq!(
        refused.code,
        stemma::ErrorCode::UnsupportedEdit,
        "{refused}"
    );
    let context = refused.details.context.clone().unwrap_or_default();
    assert!(
        context.contains(&bob.to_string()) && context.contains(&carol.to_string()),
        "the refusal names the revisions to resolve together: {refused} / {context}"
    );

    // Accepting both, at once or the table first, is Accept All.
    let accepted = ["Left right"];
    assert_eq!(
        persisted_reading(
            &original.project(Resolution::AcceptAll).unwrap(),
            "accept all"
        ),
        accepted
    );
    assert_eq!(byte_reading(&bytes, true), accepted);
    assert_eq!(
        persisted_reading(
            &select(&original, &[bob, carol], ResolveSelectionAction::Accept),
            "accept both"
        ),
        accepted
    );
    let table_first = select(&original, &[carol], ResolveSelectionAction::Accept);
    assert_eq!(
        persisted_reading(
            &select(&table_first, &[bob], ResolveSelectionAction::Accept),
            "accept the table, then the join"
        ),
        accepted
    );
    // Rejecting the table first leaves a surviving table: the join is then
    // blocked for good and accepting it leaves both paragraphs.
    let table_kept = select(&original, &[carol], ResolveSelectionAction::Reject);
    assert_eq!(
        persisted_reading(
            &select(&table_kept, &[bob], ResolveSelectionAction::Accept),
            "reject the table, then accept the join"
        ),
        ["Left", "[x | y]", " right"]
    );
    // Rejecting is never blocked: the mark comes back.
    let rejected = ["Left", "[x | y]", " right"];
    assert_eq!(
        persisted_reading(
            &original.project(Resolution::RejectAll).unwrap(),
            "reject all"
        ),
        rejected
    );
    for order in [[bob, carol], [carol, bob]] {
        let first = select(&original, &[order[0]], ResolveSelectionAction::Reject);
        assert_eq!(
            persisted_reading(
                &select(&first, &[order[1]], ResolveSelectionAction::Reject),
                "reject one at a time"
            ),
            rejected
        );
    }
}

// ─── A tracked vertical cell merge (`w:cellMerge`, §17.13.5.3) ─────────────
//
// `w:cellMerge` records that a cell's vertical merge state was changed with
// tracking on; its `w:vMerge` / `w:vMergeOrig` attributes give the revised and
// the original state, and Accept / Reject apply one of them. The engine does
// not model that resolution (it rewrites the vertical merge geometry across
// rows), so such a document still opens and reads, the census reports the
// change (never "no pending changes"), and every resolution refuses, naming
// the change and its cell, rather than leave it behind and report success.
// A cellMerge with neither attribute "can be ignored" (§17.13.5.3) and is.

fn cell_merge_body(cell_merge: &str) -> String {
    let cell = |text: &str, tc_pr: &str| {
        format!(
            r#"<w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/>{tc_pr}</w:tcPr>{}</w:tc>"#,
            para(&run(text))
        )
    };
    format!(
        r#"<w:tbl><w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="2000"/></w:tblGrid><w:tr>{}{}</w:tr><w:tr>{}{}</w:tr></w:tbl>{}"#,
        cell("a", r#"<w:vMerge w:val="restart"/>"#),
        cell("b", ""),
        cell("", &format!("<w:vMerge/>{cell_merge}")),
        cell("d", ""),
        para(&format!(
            "{}<w:ins {}>{}</w:ins>",
            run("After"),
            rev(9, "Bob"),
            run(" more.")
        ))
    )
}

fn merge_changing_cell_merge() -> String {
    format!(r#"<w:cellMerge {} w:vMerge="cont"/>"#, rev(5, "Alice"))
}

#[test]
fn a_tracked_cell_merge_is_reported_by_the_census() {
    let document = Document::parse(&docx(&cell_merge_body(&merge_changing_cell_merge())))
        .expect("a document with a tracked cell merge still opens");
    let records = stemma::enumerate_revisions(&document.snapshot().canonical);
    let merges: Vec<_> = records
        .iter()
        .filter(|record| record.kind == stemma::RevisionKind::CellMerge)
        .collect();
    assert_eq!(
        merges.len(),
        1,
        "the cell merge is a pending change: {records:?}"
    );
    let merge = merges[0];
    assert_eq!(merge.author.as_deref(), Some("Alice"));
    assert_eq!(merge.location, stemma::StoryScope::Body);
    assert_eq!(
        merge.revision_id, 0,
        "reported, but not individually selectable while unmodeled"
    );
    assert!(
        merge.excerpt.contains("cell[1,0]"),
        "the record names its cell: {merge:?}"
    );

    // A cellMerge with neither attribute can be ignored, and is.
    let ignorable = Document::parse(&docx(&cell_merge_body(&format!(
        "<w:cellMerge {}/>",
        rev(5, "Alice")
    ))))
    .expect("parse");
    assert!(
        stemma::enumerate_revisions(&ignorable.snapshot().canonical)
            .iter()
            .all(|record| record.kind != stemma::RevisionKind::CellMerge)
    );
}

#[test]
fn every_resolution_refuses_while_a_tracked_cell_merge_is_pending() {
    let document =
        Document::parse(&docx(&cell_merge_body(&merge_changing_cell_merge()))).expect("parse");
    let bob = revision_by(&document, "Bob");
    for (label, resolution) in [
        ("accept all", Resolution::AcceptAll),
        ("reject all", Resolution::RejectAll),
        (
            "accept one other change",
            Resolution::Selective {
                ids: HashSet::from([bob]),
                action: ResolveSelectionAction::Accept,
            },
        ),
    ] {
        let Err(error) = document.project(resolution) else {
            panic!("{label} must not succeed past a pending cell merge");
        };
        assert_eq!(
            error.code,
            stemma::ErrorCode::UnsupportedEdit,
            "{label}: {error}"
        );
        assert!(
            error.message.contains("cellMerge")
                && error.message.contains("§17.13.5.3")
                && error.message.contains("cell[1,0]"),
            "{label}: the refusal names the change and its cell: {error}"
        );
        assert!(error.details.block_id.is_some(), "{label}: {error:?}");
    }

    let archive = DocxArchive::read(&docx(&cell_merge_body(&merge_changing_cell_merge())))
        .expect("read package");
    for (direction, result) in [
        ("accept", normalize_docx(&archive).map(|_| ())),
        ("reject", reject_all_docx(&archive).map(|_| ())),
    ] {
        let Err(error) = result else {
            panic!("byte-level {direction} all must not resolve past a pending cell merge");
        };
        let rendered = format!("{error:?}");
        assert!(
            rendered.contains("cellMerge") && rendered.contains("word/document.xml"),
            "{direction}: the refusal names the change and its part: {rendered}"
        );
    }
}

#[test]
fn an_ignorable_cell_merge_does_not_block_resolution() {
    let body = cell_merge_body(&format!("<w:cellMerge {}/>", rev(5, "Alice")));
    let document = Document::parse(&docx(&body)).expect("parse");
    for resolution in [Resolution::AcceptAll, Resolution::RejectAll] {
        document
            .project(resolution)
            .expect("an attribute-less cellMerge can be ignored (§17.13.5.3)");
    }
    let archive = DocxArchive::read(&docx(&body)).expect("read package");
    normalize_docx(&archive).expect("byte accept all");
    reject_all_docx(&archive).expect("byte reject all");
}
