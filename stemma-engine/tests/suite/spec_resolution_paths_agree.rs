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
                            let merge = match cell.v_merge {
                                stemma::domain::VerticalMerge::None => "",
                                stemma::domain::VerticalMerge::Restart => "(rest)",
                                stemma::domain::VerticalMerge::Continue => "(cont)",
                            };
                            let content = cell
                                .blocks
                                .iter()
                                .map(block_reading)
                                .collect::<Vec<_>>()
                                .join(" / ");
                            format!("{merge}{content}")
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

/// §17.13.5.22: a deletion beside a move's runs is its own revision, not part
/// of the move, even when one author made both at one time (Word lists them
/// separately; a comparison writes every change with one author and date).
/// Each resolves on its own: rejecting the deletion while accepting the move
/// keeps the deleted words and relocates the moved ones.
#[test]
fn a_deletion_beside_moved_text_is_its_own_revision() {
    let body = para(&format!(
        "{}<w:del {}>{}</w:del>{}{}",
        run("First "),
        rev(20, "Mover"),
        del_run("gone "),
        inline_move("moveFrom", 10, 11, "moved"),
        run(" end.")
    )) + &para(&format!(
        "{}{}{}",
        run("Second "),
        inline_move("moveTo", 12, 13, "moved"),
        run(" end.")
    ));
    let original = Document::parse(&docx(&body)).expect("parse fixture");
    let records = stemma::enumerate_revisions(&original.snapshot().canonical);
    let kinds: Vec<_> = records.iter().map(|record| record.kind).collect();
    assert_eq!(kinds.len(), 2, "a move and a deletion: {kinds:?}");
    assert_every_path(
        &body,
        &["First  end.", "Second moved end."],
        &["First gone moved end.", "Second  end."],
    );
    let deletion = records
        .iter()
        .find(|record| record.kind == stemma::RevisionKind::Delete)
        .expect("the deletion")
        .revision_id;
    let kept = select(&original, &[deletion], ResolveSelectionAction::Reject)
        .project(Resolution::AcceptAll)
        .expect("accept the move");
    assert_eq!(
        persisted_reading(&kept, "reject the deletion, accept the move"),
        ["First gone  end.", "Second moved end."]
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
    // Word's ruling (W10): here the order of rejections matters. Rejecting
    // the move (alone or with everything) returns it with the inserted text
    // ("A fresh B."); rejecting the insertion first takes the text from the
    // source and leaves the mover's plain insertion at the destination, whose
    // rejection then gives the base.
    assert_paths_with_word_order(
        &body,
        &["A  B.", "C fresh D."],
        &["A fresh B.", "C  D."],
        "Bob",
        &["A  B.", "C  D."],
    );
}

/// [`assert_every_path`] for a shape whose one-at-a-time Reject depends on
/// the order in Word: every Accept path and every Reject path that does not
/// reject `origin_author`'s revision before the others reaches the totals;
/// rejecting that revision first reaches `rejected_origin_first`.
fn assert_paths_with_word_order(
    body: &str,
    accepted: &[&str],
    rejected: &[&str],
    origin_author: &str,
    rejected_origin_first: &[&str],
) {
    let bytes = docx(body);
    let original = Document::parse(&bytes).expect("parse fixture");
    let ids = revision_ids(&original);
    let mut origin: Vec<u32> = stemma::enumerate_revisions(&original.snapshot().canonical)
        .into_iter()
        .filter(|r| r.author.as_deref() == Some(origin_author))
        .map(|r| r.revision_id)
        .collect();
    origin.dedup();
    assert_eq!(origin.len(), 1, "one revision by {origin_author}");
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
            persisted_reading(&total, "total"),
            expected,
            "{action:?} All"
        );
        assert_eq!(
            byte_reading(&bytes, accept),
            expected,
            "{action:?} All bytes"
        );
        assert_eq!(
            persisted_reading(&select(&original, &ids, action), "batch"),
            expected,
            "{action:?} batch"
        );
        for order in permutations(&ids) {
            for reopen_between in [false, true] {
                let mut document = Document::parse(&bytes).expect("parse fixture");
                for (step, id) in order.iter().enumerate() {
                    if !revision_ids(&document).contains(id) {
                        // What is left of the move after its origin was
                        // rejected re-keys: resolve whatever is pending.
                        let left = revision_ids(&document);
                        if left.is_empty() || step + 1 < order.len() {
                            continue;
                        }
                        document = select(&document, &left, action);
                        continue;
                    }
                    document = select(&document, &[*id], action);
                    if reopen_between {
                        document = reopen(&document, "step");
                    }
                }
                let left = revision_ids(&document);
                if !left.is_empty() {
                    document = select(&document, &left, action);
                }
                let want = if !accept && order[0] == origin[0] {
                    rejected_origin_first
                } else {
                    expected
                };
                assert_eq!(
                    persisted_reading(&document, "order"),
                    want,
                    "{action:?} in the order {order:?} (reopened between steps: {reopen_between})"
                );
            }
        }
    }
}

#[test]
fn a_paragraph_inserted_then_moved_resolves_in_words_orders() {
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
    // Word's ruling (W10, whole paragraph): rejecting the move returns the
    // text and leaves the origin's inserted break pending, so Reject All and
    // move-then-origin join the text onto "Middle."; rejecting the origin
    // first leaves the mover's plain insertion, whose rejection gives the
    // base.
    assert_paths_with_word_order(
        &body,
        &["Middle.", "Inserted.", "Tail."],
        &["Inserted.Middle.", "Tail."],
        "Origin",
        &["Middle.", "Tail."],
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
    assert!(
        refused
            .message
            .contains(&format!("resolve the table's revision [{carol}] first")),
        "the refusal names the table's revision to resolve first: {refused}"
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

// The same join inside a table cell, blocked by a nested table another change
// deletes. Word (16, oracle) resolves the mark alone by pulling the paragraph
// into the first cell of the still-pending nested table, and that order then
// either loses the paragraph's text (accepting the table deletes it with the
// row) or leaves a revision Word can no longer reach. Accept All, and the
// table first then the mark, join the two paragraphs. So the mark alone is
// refused in a cell exactly as in the body.

fn mark_deletion_before_deleted_nested_table() -> String {
    let nested = format!(
        r#"<w:tbl><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:trPr><w:del {}/></w:trPr><w:tc><w:p><w:pPr><w:rPr><w:del {}/></w:rPr></w:pPr><w:del {}>{}</w:del></w:p></w:tc></w:tr></w:tbl>"#,
        rev(2, "Carol"),
        rev(2, "Carol"),
        rev(2, "Carol"),
        del_run("x")
    );
    let cell = para_marked(&format!("del {}", rev(1, "Bob")), &run("Left"))
        + &nested
        + &para(&run(" right"));
    format!(
        r#"<w:tbl><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc>{cell}</w:tc></w:tr></w:tbl>{}"#,
        para(&run("After"))
    )
}

#[test]
fn a_cell_join_blocked_only_by_a_nested_table_another_change_deletes_is_refused_alone() {
    let body = mark_deletion_before_deleted_nested_table();
    let bytes = docx(&body);
    let original = Document::parse(&bytes).expect("parse");
    let bob = revision_by(&original, "Bob");
    let carol = revision_by(&original, "Carol");

    let Err(refused) = original.project(Resolution::Selective {
        ids: HashSet::from([bob]),
        action: ResolveSelectionAction::Accept,
    }) else {
        panic!("accepting the cell join alone has no order-independent result");
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
    assert!(
        refused
            .message
            .contains(&format!("resolve the table's revision [{carol}] first")),
        "the refusal names the table's revision to resolve first: {refused}"
    );

    let accepted = ["[Left right]", "After"];
    assert_eq!(
        persisted_reading(
            &original.project(Resolution::AcceptAll).unwrap(),
            "accept all"
        ),
        accepted
    );
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
}

#[test]
fn a_cell_join_blocked_only_by_a_nested_table_another_change_inserts_is_refused_alone() {
    let nested = format!(
        r#"<w:tbl><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:trPr><w:ins {}/></w:trPr><w:tc><w:p><w:pPr><w:rPr><w:ins {}/></w:rPr></w:pPr><w:ins {}>{}</w:ins></w:p></w:tc></w:tr></w:tbl>"#,
        rev(2, "Carol"),
        rev(2, "Carol"),
        rev(2, "Carol"),
        run("x")
    );
    let cell = para_marked(&format!("ins {}", rev(1, "Bob")), &run("Left"))
        + &nested
        + &para(&run(" right"));
    let body = format!(
        r#"<w:tbl><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc>{cell}</w:tc></w:tr></w:tbl>{}"#,
        para(&run("After"))
    );
    let original = Document::parse(&docx(&body)).expect("parse");
    let bob = revision_by(&original, "Bob");
    let carol = revision_by(&original, "Carol");
    let Err(refused) = original.project(Resolution::Selective {
        ids: HashSet::from([bob]),
        action: ResolveSelectionAction::Reject,
    }) else {
        panic!("rejecting the cell join alone has no order-independent result");
    };
    let context = refused.details.context.clone().unwrap_or_default();
    assert!(
        context.contains(&bob.to_string()) && context.contains(&carol.to_string()),
        "the refusal names the revisions to resolve together: {refused} / {context}"
    );
    assert!(
        refused
            .message
            .contains(&format!("resolve the table's revision [{carol}] first")),
        "the refusal names the table's revision to resolve first: {refused}"
    );
    let table_first = select(&original, &[carol], ResolveSelectionAction::Reject);
    assert_eq!(
        persisted_reading(
            &select(&table_first, &[bob], ResolveSelectionAction::Reject),
            "reject the table, then the join"
        ),
        persisted_reading(
            &original.project(Resolution::RejectAll).unwrap(),
            "reject all"
        )
    );
}

// ─── A tracked vertical cell merge (`w:cellMerge`, §17.13.5.3) ─────────────
//
// `w:cellMerge` records that a cell's vertical merge was changed with tracking
// on: `w:vMerge` is the merge after the change and `w:vMergeOrig` the merge
// before it, a missing attribute meaning not merged. The expected readings are
// Word 16's, from probe files resolved in Word by Accept All and Reject All:
// the attribute decides, not the cell's own `w:vMerge` element; a cell the
// resolution leaves as a continuation is emptied, pending changes inside it
// included; and around a resolved cell a continuation with no merge above and
// a restart with no continuation below are saved unmerged. The cell merges of
// one vertically contiguous run in one grid column are one revision. A
// cellMerge with neither attribute "can be ignored" (§17.13.5.3) and is.

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

/// A `w:cellMerge` by Alice with the given merge attributes.
fn cell_merge(id: u32, attributes: &str) -> String {
    format!(r#"<w:cellMerge {} {attributes}/>"#, rev(id, "Alice"))
}

const RESTART: &str = r#"<w:vMerge w:val="restart"/>"#;
const CONTINUE: &str = "<w:vMerge/>";

/// A cell holding one paragraph of `content` (runs), with `tc_pr` after its
/// width.
fn merge_cell(tc_pr: &str, content: &str) -> String {
    format!(
        r#"<w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/>{tc_pr}</w:tcPr>{}</w:tc>"#,
        para(content)
    )
}

/// A two-column table; each row is its two cells.
fn merge_table(rows: &[[String; 2]]) -> String {
    let rows: String = rows
        .iter()
        .map(|[first, second]| format!("<w:tr>{first}{second}</w:tr>"))
        .collect();
    format!(
        r#"<w:tbl><w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="2000"/></w:tblGrid>{rows}</w:tbl>"#
    )
}

/// Three rows: column one holds `first`, `second` and `third` with the given
/// tcPr children; column two holds untracked filler text.
fn three_rows(first: (&str, &str), second: (&str, &str), third: (&str, &str)) -> String {
    merge_table(&[
        [
            merge_cell(first.0, &run(first.1)),
            merge_cell("", &run("One")),
        ],
        [
            merge_cell(second.0, &run(second.1)),
            merge_cell("", &run("Two")),
        ],
        [
            merge_cell(third.0, &run(third.1)),
            merge_cell("", &run("Three")),
        ],
    ])
}

const UNMERGED: &str = "[Alpha | One] [Bravo | Two] [Charlie | Three]";

#[test]
fn accepting_a_cell_merge_merges_and_empties_the_continuation() {
    // Word: Accept merges Bravo's cell into Alpha's and deletes "Bravo";
    // Reject leaves the cells unmerged, and the restart alone is unmerged.
    let body = three_rows(
        (RESTART, "Alpha"),
        (
            &format!("{CONTINUE}{}", cell_merge(5, r#"w:vMerge="cont""#)),
            "Bravo",
        ),
        ("", "Charlie"),
    );
    assert_every_path(
        &body,
        &["[(rest)Alpha | One] [(cont) | Two] [Charlie | Three]"],
        &[UNMERGED],
    );
}

#[test]
fn the_cell_merge_attribute_decides_not_the_cells_own_merge_element() {
    // No `w:vMerge` element at all, as Word's Compare writes it.
    let body = three_rows(
        (&cell_merge(4, r#"w:vMerge="rest""#), "Alpha"),
        (&cell_merge(5, r#"w:vMerge="cont""#), "Bravo"),
        ("", "Charlie"),
    );
    assert_every_path(
        &body,
        &["[(rest)Alpha | One] [(cont) | Two] [Charlie | Three]"],
        &[UNMERGED],
    );
    // A restart element under a `cont` attribute.
    let body = three_rows(
        (RESTART, "Alpha"),
        (
            &format!("{RESTART}{}", cell_merge(5, r#"w:vMerge="cont""#)),
            "Bravo",
        ),
        ("", "Charlie"),
    );
    assert_every_path(
        &body,
        &["[(rest)Alpha | One] [(cont) | Two] [Charlie | Three]"],
        &[UNMERGED],
    );
    // A continuation element with only `vMergeOrig`: the change split it.
    let body = three_rows(
        (RESTART, "Alpha"),
        (
            &format!("{CONTINUE}{}", cell_merge(5, r#"w:vMergeOrig="cont""#)),
            "Bravo",
        ),
        ("", "Charlie"),
    );
    assert_every_path(
        &body,
        &[UNMERGED],
        &["[(rest)Alpha | One] [(cont) | Two] [Charlie | Three]"],
    );
}

#[test]
fn rejecting_a_split_merges_back_and_empties_the_continuation() {
    let body = three_rows(
        (&cell_merge(4, r#"w:vMergeOrig="rest""#), "Alpha"),
        (&cell_merge(5, r#"w:vMergeOrig="cont""#), "Bravo"),
        ("", "Charlie"),
    );
    assert_every_path(
        &body,
        &[UNMERGED],
        &["[(rest)Alpha | One] [(cont) | Two] [Charlie | Three]"],
    );
    // With no restart recorded above it, Word deletes the text and saves the
    // cell unmerged.
    let body = three_rows(
        ("", "Alpha"),
        (&cell_merge(5, r#"w:vMergeOrig="cont""#), "Bravo"),
        ("", "Charlie"),
    );
    assert_every_path(
        &body,
        &[UNMERGED],
        &["[Alpha | One] [ | Two] [Charlie | Three]"],
    );
}

#[test]
fn a_rejected_restart_merge_under_a_restart_leaves_both_unmerged() {
    let body = three_rows(
        (RESTART, "Alpha"),
        (
            &format!(
                "{CONTINUE}{}",
                cell_merge(5, r#"w:vMerge="cont" w:vMergeOrig="rest""#)
            ),
            "Bravo",
        ),
        ("", "Charlie"),
    );
    assert_every_path(
        &body,
        &["[(rest)Alpha | One] [(cont) | Two] [Charlie | Three]"],
        &[UNMERGED],
    );
}

#[test]
fn a_run_of_cell_merges_in_one_column_is_one_revision() {
    let body = three_rows(
        (RESTART, "Alpha"),
        (
            &format!("{CONTINUE}{}", cell_merge(5, r#"w:vMerge="cont""#)),
            "Bravo",
        ),
        (
            &format!("{CONTINUE}{}", cell_merge(6, r#"w:vMerge="cont""#)),
            "Charlie",
        ),
    );
    let document = Document::parse(&docx(&body)).expect("parse");
    assert_eq!(revision_ids(&document).len(), 1, "Word lists one revision");
    assert_every_path(
        &body,
        &["[(rest)Alpha | One] [(cont) | Two] [(cont) | Three]"],
        &[UNMERGED],
    );
}

/// `cell` wrapped in a cell-level content control (CT_SdtCell, §17.5.2.32):
/// still one cell of its row, in its grid column.
fn sdt_cell(cell: &str) -> String {
    format!(
        r#"<w:sdt><w:sdtPr><w:id w:val="77"/></w:sdtPr><w:sdtContent>{cell}</w:sdtContent></w:sdt>"#
    )
}

#[test]
fn a_content_control_around_an_earlier_cell_keeps_the_merge_column() {
    // The control wraps the first cell of the first row only; the merge is
    // in the second grid column of every row.
    let body = merge_table(&[
        [
            sdt_cell(&merge_cell("", &run("One"))),
            merge_cell(RESTART, &run("Alpha")),
        ],
        [
            merge_cell("", &run("Two")),
            merge_cell(
                &format!("{CONTINUE}{}", cell_merge(5, r#"w:vMerge="cont""#)),
                &run("Bravo"),
            ),
        ],
        [
            merge_cell("", &run("Three")),
            merge_cell("", &run("Charlie")),
        ],
    ]);
    assert_every_path(
        &body,
        &["[One | (rest)Alpha] [Two | (cont)] [Three | Charlie]"],
        &["[One | Alpha] [Two | Bravo] [Three | Charlie]"],
    );
}

#[test]
fn a_cell_merge_on_a_cell_inside_a_content_control_resolves() {
    let body = merge_table(&[
        [
            merge_cell("", &run("One")),
            merge_cell(RESTART, &run("Alpha")),
        ],
        [
            merge_cell("", &run("Two")),
            sdt_cell(&merge_cell(
                &format!("{CONTINUE}{}", cell_merge(5, r#"w:vMerge="cont""#)),
                &run("Bravo"),
            )),
        ],
        [
            merge_cell("", &run("Three")),
            merge_cell("", &run("Charlie")),
        ],
    ]);
    assert_every_path(
        &body,
        &["[One | (rest)Alpha] [Two | (cont)] [Three | Charlie]"],
        &["[One | Alpha] [Two | Bravo] [Three | Charlie]"],
    );
    // The byte path's working mark on a resolved cell is never written.
    for archive in [
        normalize_docx(&DocxArchive::read(&docx(&body)).unwrap())
            .unwrap()
            .0,
        reject_all_docx(&DocxArchive::read(&docx(&body)).unwrap())
            .unwrap()
            .0,
    ] {
        let xml = String::from_utf8(archive.get("word/document.xml").unwrap().to_vec()).unwrap();
        assert!(!xml.contains("stemma-"), "{xml}");
    }
}

#[test]
fn cell_merges_in_two_columns_or_two_runs_are_separate_revisions() {
    let body = merge_table(&[
        [
            merge_cell(RESTART, &run("Alpha")),
            merge_cell(RESTART, &run("One")),
        ],
        [
            merge_cell(
                &format!("{CONTINUE}{}", cell_merge(5, r#"w:vMerge="cont""#)),
                &run("Bravo"),
            ),
            merge_cell(
                &format!("{CONTINUE}{}", cell_merge(6, r#"w:vMerge="cont""#)),
                &run("Two"),
            ),
        ],
    ]);
    let document = Document::parse(&docx(&body)).expect("parse");
    assert_eq!(
        revision_ids(&document).len(),
        2,
        "Word lists one per column"
    );
    assert_every_path(
        &body,
        &["[(rest)Alpha | (rest)One] [(cont) | (cont)]"],
        &["[Alpha | One] [Bravo | Two]"],
    );

    let body = merge_table(&[
        [
            merge_cell(RESTART, &run("Alpha")),
            merge_cell("", &run("One")),
        ],
        [
            merge_cell(
                &format!("{CONTINUE}{}", cell_merge(5, r#"w:vMerge="cont""#)),
                &run("Bravo"),
            ),
            merge_cell("", &run("Two")),
        ],
        [
            merge_cell("", &run("Charlie")),
            merge_cell("", &run("Three")),
        ],
        [
            merge_cell(RESTART, &run("Delta")),
            merge_cell("", &run("Four")),
        ],
        [
            merge_cell(
                &format!("{CONTINUE}{}", cell_merge(7, r#"w:vMerge="cont""#)),
                &run("Echo"),
            ),
            merge_cell("", &run("Five")),
        ],
    ]);
    let document = Document::parse(&docx(&body)).expect("parse");
    assert_eq!(revision_ids(&document).len(), 2, "Word lists one per run");
    assert_every_path(
        &body,
        &[
            "[(rest)Alpha | One] [(cont) | Two] [Charlie | Three] [(rest)Delta | Four] [(cont) | Five]",
        ],
        &["[Alpha | One] [Bravo | Two] [Charlie | Three] [Delta | Four] [Echo | Five]"],
    );
}

#[test]
fn a_continuation_takes_the_pending_changes_inside_it() {
    // Word: Accept All, or accepting the merge alone, deletes the inserted
    // "Bravo" with no pending change left; rejecting the merge alone keeps
    // the insertion pending, and Reject All removes it, leaving the cell
    // empty.
    let body = three_rows((RESTART, "Alpha"), ("", "Charlie"), ("", "Delta")).replacen(
        &merge_cell("", &run("Charlie")),
        &merge_cell(
            &format!("{CONTINUE}{}", cell_merge(5, r#"w:vMerge="cont""#)),
            &format!("<w:ins {}>{}</w:ins>", rev(8, "Bob"), run("Bravo")),
        ),
        1,
    );
    assert_every_path(
        &body,
        &["[(rest)Alpha | One] [(cont) | Two] [Delta | Three]"],
        &["[Alpha | One] [ | Two] [Delta | Three]"],
    );
    let document = Document::parse(&docx(&body)).expect("parse");
    let merge = stemma::enumerate_revisions(&document.snapshot().canonical)
        .into_iter()
        .find(|record| record.kind == stemma::RevisionKind::CellMerge)
        .expect("the cell merge is listed")
        .revision_id;
    let accepted = select(&document, &[merge], ResolveSelectionAction::Accept);
    assert!(
        revision_ids(&accepted).is_empty(),
        "accepting the merge alone leaves no pending change"
    );
    let rejected = select(&document, &[merge], ResolveSelectionAction::Reject);
    assert_eq!(
        reading(&rejected)[0],
        "[Alpha | One] [{+Bravo+} | Two] [Delta | Three]",
        "rejecting the merge alone keeps the insertion pending"
    );
}

#[test]
fn a_compare_style_cell_merge_with_a_cell_property_change_resolves_as_word() {
    // Word's Compare writes each cellMerge with no `w:vMerge` element, a
    // `w:tcPrChange` on every cell whose prior tcPr repeats the cellMerge,
    // and the absorbed text as a deletion.
    let compare = |tc_pr: &str| {
        format!(
            r#"{tc_pr}<w:tcPrChange {}><w:tcPr><w:tcW w:w="2000" w:type="dxa"/>{tc_pr}</w:tcPr></w:tcPrChange>"#,
            rev(20, "Alice")
        )
    };
    let body = merge_table(&[
        [
            merge_cell(
                &compare(&cell_merge(4, r#"w:vMerge="rest""#)),
                &run("Alpha"),
            ),
            merge_cell(&compare(""), &run("One")),
        ],
        [
            merge_cell(
                &compare(&cell_merge(5, r#"w:vMerge="cont""#)),
                &format!("<w:del {}>{}</w:del>", rev(6, "Alice"), del_run("Bravo")),
            ),
            merge_cell(&compare(""), &run("Two")),
        ],
        [
            merge_cell(&compare(""), &run("Charlie")),
            merge_cell(&compare(""), &run("Three")),
        ],
    ]);
    assert_every_path(
        &body,
        &["[(rest)Alpha | One] [(cont) | Two] [Charlie | Three]"],
        &[UNMERGED],
    );
}

#[test]
fn a_tracked_cell_merge_is_reported_by_the_census() {
    let document = Document::parse(&docx(&cell_merge_body(&merge_changing_cell_merge())))
        .expect("a document with a tracked cell merge opens");
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
    assert_ne!(
        merge.revision_id, 0,
        "a cell merge is individually selectable"
    );
    assert!(
        merge.excerpt.contains("cell[1,0]") && merge.excerpt.contains("vMerge=cont"),
        "the record names its cell and its merge: {merge:?}"
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
fn a_cell_merge_resolves_beside_another_change() {
    assert_every_path(
        &cell_merge_body(&merge_changing_cell_merge()),
        &["[(rest)a | b] [(cont) | d]", "After more."],
        &["[a | b] [ | d]", "After"],
    );
}

#[test]
fn a_malformed_cell_merge_is_refused_at_open_and_by_the_byte_path() {
    let body = cell_merge_body(&cell_merge(5, r#"w:vMerge="continue""#));
    let Err(error) = Document::parse(&docx(&body)) else {
        panic!("a malformed cellMerge is refused at open");
    };
    assert!(
        error.message.contains("ST_AnnotationVMerge"),
        "the refusal names the value's type: {error}"
    );
    let archive = DocxArchive::read(&docx(&body)).expect("read package");
    for (direction, result) in [
        ("accept", normalize_docx(&archive).map(|_| ())),
        ("reject", reject_all_docx(&archive).map(|_| ())),
    ] {
        let rendered = format!("{:?}", result.expect_err(direction));
        assert!(
            rendered.contains("cellMerge") && rendered.contains("word/document.xml"),
            "{direction}: the refusal names the element and its part: {rendered}"
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

// ─── A row whose every cell is a tracked cell deletion ─────────────────────
//
// §17.13.5.1: accepting a w:cellDel removes the cell. A row holds at least one
// cell (CT_Row, §17.4.79), so a row whose every cell is deleted goes with its
// cells on Accept, and a table left with no row goes too (§17.4.37). Reject
// keeps every cell. No row marker (w:trPr/w:del) is involved.

fn deleted_cell(id: u32, text: &str) -> String {
    format!(
        r#"<w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/><w:cellDel {}/></w:tcPr>{}</w:tc>"#,
        rev(id, "Carol"),
        para(&run(text))
    )
}

fn kept_cell(text: &str) -> String {
    format!(
        r#"<w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/></w:tcPr>{}</w:tc>"#,
        para(&run(text))
    )
}

fn two_column_table(rows: &[String]) -> String {
    format!(
        r#"<w:tbl><w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="2000"/></w:tblGrid>{}</w:tbl>"#,
        rows.iter()
            .map(|cells| format!("<w:tr>{cells}</w:tr>"))
            .collect::<String>()
    )
}

#[test]
fn a_row_of_deleted_cells_goes_with_its_cells_on_accept() {
    let body = para(&run("Before"))
        + &two_column_table(&[
            kept_cell("Apple") + &kept_cell("Pear"),
            deleted_cell(1, "Oak") + &deleted_cell(2, "Elm"),
            kept_cell("Lime") + &kept_cell("Fig"),
        ])
        + &para(&run("After"));
    assert_every_path(
        &body,
        &["Before", "[Apple | Pear] [Lime | Fig]", "After"],
        &["Before", "[Apple | Pear] [Oak | Elm] [Lime | Fig]", "After"],
    );
}

#[test]
fn a_table_of_deleted_cells_goes_with_its_cells_on_accept() {
    let body = para(&run("Before"))
        + &two_column_table(&[deleted_cell(1, "Oak") + &deleted_cell(2, "Elm")])
        + &para(&run("After"));
    assert_every_path(
        &body,
        &["Before", "After"],
        &["Before", "[Oak | Elm]", "After"],
    );
}

// ─── A tracked cell merge inside a textbox ─────────────────────────────────
//
// A textbox's content stays raw XML in the model, and its tracked changes are
// reported as `opaque_interior` records and resolved by both full
// resolutions. A cell merge there is one of them.

fn textbox_with(table: &str) -> String {
    format!(
        r#"<w:p><w:r><w:pict><v:shape xmlns:v="urn:schemas-microsoft-com:vml"><v:textbox><w:txbxContent>{table}<w:p/></w:txbxContent></v:textbox></v:shape></w:pict></w:r></w:p>"#
    )
}

fn document_xml(bytes: &[u8]) -> String {
    let archive = DocxArchive::read(bytes).expect("read package");
    String::from_utf8(
        archive
            .get("word/document.xml")
            .expect("document part")
            .to_vec(),
    )
    .expect("utf-8")
}

#[test]
fn a_cell_merge_inside_a_textbox_is_reported_and_resolved() {
    let table = three_rows(
        (RESTART, "Alpha"),
        (
            &format!("{CONTINUE}{}", cell_merge(5, r#"w:vMerge="cont""#)),
            "Bravo",
        ),
        ("", "Charlie"),
    );
    let bytes = docx(&textbox_with(&table));
    let document = Document::parse(&bytes).expect("parse");
    let records = stemma::enumerate_revisions(&document.snapshot().canonical);
    assert_eq!(
        records
            .iter()
            .filter(|record| record.kind == stemma::RevisionKind::OpaqueInterior)
            .count(),
        1,
        "the census reports the cell merge inside the textbox: {records:?}"
    );
    // An attribute-less one changes nothing and is not reported.
    let ignorable = Document::parse(&docx(&textbox_with(&three_rows(
        (RESTART, "Alpha"),
        (&format!("{CONTINUE}{}", cell_merge(5, "")), "Bravo"),
        ("", "Charlie"),
    ))))
    .expect("parse");
    assert!(stemma::enumerate_revisions(&ignorable.snapshot().canonical).is_empty());

    let accepted = |xml: &str| {
        assert!(
            !xml.contains("cellMerge") && !xml.contains("Bravo"),
            "{xml}"
        );
        assert!(xml.contains("Alpha") && xml.contains("<w:vMerge"), "{xml}");
    };
    let rejected = |xml: &str| {
        assert!(!xml.contains("cellMerge") && xml.contains("Bravo"), "{xml}");
        assert!(
            !xml.contains("<w:vMerge"),
            "a lone restart is unmerged: {xml}"
        );
    };
    let model = |resolution| {
        document_xml(
            &document
                .project(resolution)
                .expect("resolve")
                .serialize(&ExportOptions::default())
                .expect("serialize"),
        )
    };
    accepted(&model(Resolution::AcceptAll));
    rejected(&model(Resolution::RejectAll));
    let archive = DocxArchive::read(&bytes).expect("read package");
    let bytes_of = |result: Result<(DocxArchive, _), _>| {
        document_xml(&result.expect("byte path").0.write().expect("write"))
    };
    accepted(&bytes_of(normalize_docx(&archive)));
    rejected(&bytes_of(reject_all_docx(&archive)));
}
