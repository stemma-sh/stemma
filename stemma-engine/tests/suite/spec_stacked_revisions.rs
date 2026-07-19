//! The STACKED state (`TrackingStatus::InsertedThenDeleted`): text inserted by
//! one pending revision and deleted by another, both still pending. "A deletion
//! remembers what it deletes." Pinned here:
//!
//!   - BOTH markup orders (`w:del`-in-`w:ins` and `w:ins`-in-`w:del`,
//!     ECMA-376 §17.13.5.14) parse to the SAME state;
//!   - the four origin rules:
//!       1. accept the insertion → the deletion now targets base text;
//!       2. reject the insertion → dropped, the nested deletion goes with it
//!          (the cascade — ENUMERATED in the result, never silent);
//!       3. accept the deletion → dropped (cascade settles the insertion);
//!       4. reject the deletion → back to a plain insertion;
//!
//!     Their composition: the MIXED resolution (accept-insert +
//!     reject-delete) keeps the contested text, the outcome all-or-nothing
//!     resolution can never reach;
//!   - accept-all and reject-all both drop the stacked text;
//!   - canonical serialization: one nesting order on output
//!     (`<w:ins><w:del>…`), whichever order came in;
//!   - the read surface: ONE span, compound status, both revisions visible;
//!     nested tags in the extended markdown;
//!   - the v2 guard distinguishes stacked from plain-inserted over identical
//!     bytes (the class transition is what the guard reform exists for);
//!   - the splice: a stacked segment is a WALL — edits layer beside it
//!     untouched; targeting it refuses (terminal state, v1).
//!
//! Resolution expectations mirror the accept/reject outcomes real Word
//! produces — the rules PREDICT those outcomes, they don't just match them.
//!
//! Daily, corpus-free.

use stemma::api::Document;
use stemma::edit::{
    ContentFragment, EditStep, EditTransaction, MaterializationMode, ParagraphContent,
    ResolvedSpanSelector,
};
use stemma::semantic_hash::block_guard;
use stemma::tracked_model::{RevisionRecord, enumerate_revisions};
use stemma::view::TrackStatus;
use stemma::{BlockNode, Resolution, ResolveSelectionAction, RevisionInfo};

// ─── Fixtures ──────────────────────────────────────────────────────────────

/// del-in-ins: "Start [ins#1 A: kept [del#2 B: cut ] tail ]end."
const DEL_IN_INS_P: &str = r#"<w:p><w:r><w:t xml:space="preserve">Start </w:t></w:r><w:ins w:id="1" w:author="AuthorA" w:date="2026-01-01T00:00:00Z"><w:r><w:t xml:space="preserve">kept </w:t></w:r><w:del w:id="2" w:author="AuthorB" w:date="2026-02-01T00:00:00Z"><w:r><w:delText xml:space="preserve">cut </w:delText></w:r></w:del><w:r><w:t xml:space="preserve">tail </w:t></w:r></w:ins><w:r><w:t xml:space="preserve">end.</w:t></w:r></w:p>"#;

/// ins-in-del: the converse order, same state (§17.13.5.14 deleted insertion).
const INS_IN_DEL_P: &str = r#"<w:p><w:r><w:t xml:space="preserve">Start </w:t></w:r><w:del w:id="2" w:author="AuthorB" w:date="2026-02-01T00:00:00Z"><w:ins w:id="1" w:author="AuthorA" w:date="2026-01-01T00:00:00Z"><w:r><w:delText xml:space="preserve">cut </w:delText></w:r></w:ins></w:del><w:r><w:t xml:space="preserve">end.</w:t></w:r></w:p>"#;

fn make_docx_with_body(body_inner: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    use std::io::Write;
    use zip::write::FileOptions;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        zip.start_file("[Content_Types].xml", opts).unwrap();
        zip.write_all(content_types.as_bytes()).unwrap();
        zip.start_file("_rels/.rels", opts).unwrap();
        zip.write_all(rels.as_bytes()).unwrap();
        zip.start_file("word/_rels/document.xml.rels", opts)
            .unwrap();
        zip.write_all(doc_rels.as_bytes()).unwrap();
        zip.start_file("word/document.xml", opts).unwrap();
        zip.write_all(document_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

fn revision() -> RevisionInfo {
    RevisionInfo {
        revision_id: 60,
        identity: 0,
        author: Some("stacked-test".to_string()),
        date: Some("2026-06-09T00:00:00Z".to_string()),
        apply_op_id: None,
    }
}

fn text_content(s: &str) -> ParagraphContent {
    ParagraphContent {
        fragments: vec![ContentFragment::Text(s.to_string())],
    }
}

/// The first paragraph's segments as (status-label, text) pairs, statuses
/// rendered with authors so attribution is asserted too.
fn fingerprint(doc: &Document) -> Vec<(String, String)> {
    let snap = doc.snapshot();
    let p = match &snap.canonical.blocks[0].block {
        BlockNode::Paragraph(p) => p,
        other => panic!("expected paragraph, got {other:?}"),
    };
    p.segments
        .iter()
        .map(|seg| {
            let status = match &seg.status {
                stemma::TrackingStatus::Normal => "normal".to_string(),
                stemma::TrackingStatus::Inserted(r) => {
                    format!("ins({})", r.author.as_deref().unwrap_or(""))
                }
                stemma::TrackingStatus::Deleted(r) => {
                    format!("del({})", r.author.as_deref().unwrap_or(""))
                }
                stemma::TrackingStatus::InsertedThenDeleted(sr) => format!(
                    "stacked(ins {} / del {})",
                    sr.inserted.author.as_deref().unwrap_or(""),
                    sr.deleted.author.as_deref().unwrap_or("")
                ),
            };
            let text: String = seg
                .inlines
                .iter()
                .filter_map(|i| match i {
                    stemma::InlineNode::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .collect();
            (status, text)
        })
        .collect()
}

fn selective(doc: &Document, ids: &[u32], action: ResolveSelectionAction) -> Document {
    doc.project(Resolution::Selective {
        ids: ids.iter().copied().collect(),
        action,
    })
    .unwrap_or_else(|e| panic!("selective resolution failed: {e:?}"))
}

/// H7: a revision is addressed by its document-unique MINTED identity
/// (surfaced as `RevisionRecord::revision_id`), never by the raw wire `w:id`,
/// which Word reuses and is not unique. Discover the identity from the document
/// by a stable property the test already knows. Deduped because one revision
/// can enumerate under several carriers (e.g. every segment of a multi-segment
/// insertion shares its identity).
fn identities_where(doc: &Document, pred: impl Fn(&RevisionRecord) -> bool) -> Vec<u32> {
    let mut ids: Vec<u32> = enumerate_revisions(&doc.snapshot().canonical)
        .iter()
        .filter(|r| pred(r))
        .map(|r| r.revision_id)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// The single minted identity of the revision authored by `author`.
fn identity_of_author(doc: &Document, author: &str) -> u32 {
    let ids = identities_where(doc, |r| r.author.as_deref() == Some(author));
    assert_eq!(
        ids.len(),
        1,
        "expected exactly one revision identity for {author}, got {ids:?}"
    );
    ids[0]
}

// ─── Both markup orders, one state ───────────────────────────────────────────

#[test]
fn both_markup_orders_parse_to_the_same_stacked_state() {
    let a = Document::parse(&make_docx_with_body(DEL_IN_INS_P)).expect("del-in-ins parses");
    let fp = fingerprint(&a);
    assert!(
        fp.iter()
            .any(|(s, t)| s == "stacked(ins AuthorA / del AuthorB)" && t == "cut "),
        "del-in-ins imports as the stacked state with both attributions: {fp:?}"
    );

    let b = Document::parse(&make_docx_with_body(INS_IN_DEL_P)).expect("ins-in-del parses");
    let fp = fingerprint(&b);
    assert!(
        fp.iter()
            .any(|(s, t)| s == "stacked(ins AuthorA / del AuthorB)" && t == "cut "),
        "ins-in-del (a deleted insertion) imports as the SAME state: {fp:?}"
    );
}

#[test]
fn the_read_surface_shows_one_span_with_compound_status() {
    let doc = Document::parse(&make_docx_with_body(DEL_IN_INS_P)).expect("parse");
    let view = doc.read();
    let stacked: Vec<_> = view.blocks[0]
        .segments
        .iter()
        .filter_map(|s| match s {
            stemma::view::SegmentView::Text {
                text,
                status: TrackStatus::InsertedThenDeleted { inserted, deleted },
                ..
            } => Some((text.clone(), inserted.clone(), deleted.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(stacked.len(), 1, "exactly ONE compound span");
    let (text, inserted, deleted) = &stacked[0];
    assert_eq!(text, "cut ");
    assert_eq!(inserted.author.as_deref(), Some("AuthorA"));
    assert_eq!(deleted.author.as_deref(), Some("AuthorB"));
}

#[test]
fn extended_markdown_nests_the_stacked_tags() {
    let doc = Document::parse(&make_docx_with_body(DEL_IN_INS_P)).expect("parse");
    let md = doc.to_markdown();
    // The markdown carries each leg's minted identity (what a caller resolves
    // by), not the raw wire ids — discover them from the document.
    let ins_id = identity_of_author(&doc, "AuthorA");
    let del_id = identity_of_author(&doc, "AuthorB");
    let expected =
        format!("<ins id={ins_id} by=\"AuthorA\"><del id={del_id} by=\"AuthorB\">cut </del></ins>");
    assert!(md.contains(&expected), "the markdown nests honestly: {md}");
}

// ─── Resolutions: the four origin rules ──────────────────────────────────────

#[test]
fn accept_all_and_reject_all_both_drop_the_stacked_text() {
    for (label, body) in [("del-in-ins", DEL_IN_INS_P), ("ins-in-del", INS_IN_DEL_P)] {
        let doc = Document::parse(&make_docx_with_body(body)).expect("parse");
        let accepted = doc.read_accepted().expect("accept").read();
        assert!(
            !accepted.blocks[0].text.contains("cut"),
            "[{label}] accept-all accepts the deletion too — the text goes"
        );
        let rejected = doc.read_rejected().expect("reject").read();
        assert!(
            !rejected.blocks[0].text.contains("cut"),
            "[{label}] reject-all rejects the insertion — the text never existed"
        );
    }
}

#[test]
fn rule_1_accepting_the_insertion_leaves_the_deletion_pending_over_base() {
    let doc = Document::parse(&make_docx_with_body(DEL_IN_INS_P)).expect("parse");
    let resolved = selective(
        &doc,
        &[identity_of_author(&doc, "AuthorA")],
        ResolveSelectionAction::Accept,
    );
    let fp = fingerprint(&resolved);
    assert!(
        fp.iter().any(|(s, t)| s == "del(AuthorB)" && t == "cut "),
        "the deletion now targets base text: {fp:?}"
    );
    assert!(
        !fp.iter().any(|(s, _)| s.starts_with("stacked")),
        "no stacked state remains: {fp:?}"
    );
}

#[test]
fn rule_2_rejecting_the_insertion_cascades_the_deletion_away() {
    let doc = Document::parse(&make_docx_with_body(DEL_IN_INS_P)).expect("parse");
    let resolved = selective(
        &doc,
        &[identity_of_author(&doc, "AuthorA")],
        ResolveSelectionAction::Reject,
    );
    let fp = fingerprint(&resolved);
    assert!(
        !fp.iter()
            .any(|(_, t)| t.contains("cut") || t.contains("kept") || t.contains("tail")),
        "rejecting the insertion drops ALL its content, including the stacked range: {fp:?}"
    );
}

#[test]
fn rule_3_accepting_the_deletion_drops_the_text() {
    let doc = Document::parse(&make_docx_with_body(DEL_IN_INS_P)).expect("parse");
    let resolved = selective(
        &doc,
        &[identity_of_author(&doc, "AuthorB")],
        ResolveSelectionAction::Accept,
    );
    let fp = fingerprint(&resolved);
    assert!(
        !fp.iter().any(|(_, t)| t.contains("cut")),
        "accepting the deletion removes the contested text: {fp:?}"
    );
    // A's surrounding insertion is untouched — still pending.
    assert!(
        fp.iter()
            .any(|(s, t)| s == "ins(AuthorA)" && t.contains("kept")),
        "the rest of A's insertion stays pending: {fp:?}"
    );
}

#[test]
fn rule_4_rejecting_the_deletion_restores_the_plain_insertion() {
    let doc = Document::parse(&make_docx_with_body(DEL_IN_INS_P)).expect("parse");
    let resolved = selective(
        &doc,
        &[identity_of_author(&doc, "AuthorB")],
        ResolveSelectionAction::Reject,
    );
    let fp = fingerprint(&resolved);
    assert!(
        fp.iter()
            .any(|(s, t)| s == "ins(AuthorA)" && t.contains("cut")),
        "rejecting the deletion restores the origin state — a plain pending insertion: {fp:?}"
    );
}

#[test]
fn the_mixed_resolution_keeps_the_contested_text_as_base() {
    // Accept A's insertion, then reject B's deletion: the text the
    // all-or-nothing oracle could never keep is now plain base text. This is
    // the resolution row that motivated the whole selective-resolution
    // prerequisite (verified against real Word).
    let doc = Document::parse(&make_docx_with_body(DEL_IN_INS_P)).expect("parse");
    let ins_id = identity_of_author(&doc, "AuthorA");
    let del_id = identity_of_author(&doc, "AuthorB");
    let step1 = selective(&doc, &[ins_id], ResolveSelectionAction::Accept);
    let step2 = selective(&step1, &[del_id], ResolveSelectionAction::Reject);
    let fp = fingerprint(&step2);
    assert!(
        fp.iter().any(|(s, t)| s == "normal" && t.contains("cut")),
        "accept-insert + reject-delete keeps the contested text as base: {fp:?}"
    );

    // And the composition commutes: reject the deletion first, then accept
    // the insertion.
    let step1 = selective(&doc, &[del_id], ResolveSelectionAction::Reject);
    let step2 = selective(&step1, &[ins_id], ResolveSelectionAction::Accept);
    let fp = fingerprint(&step2);
    assert!(
        fp.iter().any(|(s, t)| s == "normal" && t.contains("cut")),
        "the origin rules commute: {fp:?}"
    );
}

// ─── Cascades are enumerated, never silent ───────────────────────────────────

#[test]
fn cascaded_resolutions_are_enumerated_in_the_result() {
    use stemma::{DocxRuntime, SimpleRuntime};
    let runtime = SimpleRuntime::new();

    // The identities the caller resolves by (minting is deterministic for a
    // given import, so the parsed Document and the runtime import agree):
    // A's insertion and B's stacked deletion.
    let docx = make_docx_with_body(DEL_IN_INS_P);
    let doc = Document::parse(&docx).expect("parse");
    let ins_id = identity_of_author(&doc, "AuthorA");
    let del_id = identity_of_author(&doc, "AuthorB");

    // Rejecting A's insertion implicitly resolves B's stacked deletion — the
    // result must say so.
    let import = runtime.import_docx(&docx).expect("import");
    let result = runtime
        .resolve_tracked_revisions(
            &import.doc_handle,
            &std::collections::HashSet::from([ins_id]),
            ResolveSelectionAction::Reject,
        )
        .expect("reject the insertion");
    assert_eq!(
        result.cascaded_revision_ids,
        vec![del_id],
        "the cascade names B's deletion"
    );

    // Accepting B's deletion settles A's claim on the range.
    let import = runtime.import_docx(&docx).expect("import");
    let result = runtime
        .resolve_tracked_revisions(
            &import.doc_handle,
            &std::collections::HashSet::from([del_id]),
            ResolveSelectionAction::Accept,
        )
        .expect("accept the deletion");
    assert_eq!(
        result.cascaded_revision_ids,
        vec![ins_id],
        "the cascade names A's insertion"
    );
}

// ─── Canonical serialization ─────────────────────────────────────────────────

#[test]
fn both_orders_serialize_to_the_canonical_nesting() {
    for (label, body) in [("del-in-ins", DEL_IN_INS_P), ("ins-in-del", INS_IN_DEL_P)] {
        let doc = Document::parse(&make_docx_with_body(&format!(
            "{body}<w:p><w:r><w:t>Second.</w:t></w:r></w:p>"
        )))
        .expect("parse");
        let second_id = doc.read().blocks[1].id.clone();
        // Any edit forces a body rebuild from the IR.
        let edited = doc
            .apply(&EditTransaction {
                steps: vec![EditStep::ReplaceParagraphText {
                    block_id: second_id,
                    rationale: None,
                    replacement_role: None,
                    expect: "Second".to_string(),
                    semantic_hash: None,
                    content: text_content("Second, edited."),
                }],
                summary: None,
                materialization_mode: MaterializationMode::TrackedChange,
                revision: revision(),
            })
            .expect("edit elsewhere");
        let saved = edited
            .serialize(&stemma::ExportOptions::default())
            .expect("serialize");
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(saved)).expect("zip");
        let mut xml = String::new();
        use std::io::Read;
        zip.by_name("word/document.xml")
            .expect("doc xml")
            .read_to_string(&mut xml)
            .expect("utf8");

        // The canonical shape: an AuthorA ins wrapping an AuthorB del wrapping
        // the delText — regardless of the input order.
        let stacked_ins = xml.split("<w:ins ").find(|c| {
            c.contains("AuthorA")
                && c.split("</w:ins>").next().is_some_and(|body| {
                    body.contains("<w:del ") && body.contains("AuthorB") && body.contains("cut ")
                })
        });
        assert!(
            stacked_ins.is_some(),
            "[{label}] canonical emission is <w:ins A><w:del B>cut</w:del></w:ins>; xml: {xml}"
        );
        assert!(
            stacked_ins.unwrap().contains("<w:delText"),
            "[{label}] stacked text serializes as delText (it is pending-deleted)"
        );
    }
}

// ─── Guard: the class transition moves the v2 guard ──────────────────────────

#[test]
fn the_guard_distinguishes_stacked_from_plain_inserted_over_identical_bytes() {
    let plain = Document::parse(&make_docx_with_body(
        r#"<w:p><w:r><w:t xml:space="preserve">Start </w:t></w:r><w:ins w:id="1" w:author="AuthorA" w:date="2026-01-01T00:00:00Z"><w:r><w:t xml:space="preserve">kept </w:t></w:r><w:r><w:t xml:space="preserve">cut </w:t></w:r><w:r><w:t xml:space="preserve">tail </w:t></w:r></w:ins><w:r><w:t xml:space="preserve">end.</w:t></w:r></w:p>"#,
    ))
    .expect("parse");
    let stacked = Document::parse(&make_docx_with_body(DEL_IN_INS_P)).expect("parse");

    let plain_block = plain.snapshot().canonical.blocks[0].block.clone();
    let stacked_block = stacked.snapshot().canonical.blocks[0].block.clone();
    assert_ne!(
        block_guard(&plain_block),
        block_guard(&stacked_block),
        "stacking a deletion changes the (char, class) stream — the guard MUST move \
         (this is the exact transition the v2 reform exists for)"
    );
}

// ─── The splice: stacked segments are walls ──────────────────────────────────

#[test]
fn the_splice_layers_beside_a_stacked_segment_untouched() {
    let doc = Document::parse(&make_docx_with_body(DEL_IN_INS_P)).expect("parse");
    let view = doc.read();
    let (block_id, guard) = (view.blocks[0].id.clone(), view.blocks[0].guard.clone());
    // s_0 is the leading Normal "Start ".
    let edited = doc
        .apply(&EditTransaction {
            steps: vec![EditStep::ReplaceSpanText {
                block_id: block_id.clone(),
                guard,
                expect: Some("Start ".to_string()),
                span: ResolvedSpanSelector::Handle("s_0".to_string()),
                content: text_content("Beginning "),
                rationale: None,
            }],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: revision(),
        })
        .expect("the splice layers beside the stacked wall");
    let fp = fingerprint(&edited);
    assert!(
        fp.iter()
            .any(|(s, t)| s == "stacked(ins AuthorA / del AuthorB)" && t == "cut "),
        "the stacked segment is carried through untouched: {fp:?}"
    );
}

#[test]
fn targeting_the_stacked_span_itself_refuses() {
    let doc = Document::parse(&make_docx_with_body(DEL_IN_INS_P)).expect("parse");
    let view = doc.read();
    let (block_id, guard) = (view.blocks[0].id.clone(), view.blocks[0].guard.clone());
    // Find the stacked span's handle.
    let handle = view.blocks[0]
        .segments
        .iter()
        .find_map(|s| match s {
            stemma::view::SegmentView::Text {
                status: TrackStatus::InsertedThenDeleted { .. },
                handle,
                ..
            } => handle.clone(),
            _ => None,
        })
        .expect("the stacked span has a handle")
        .0;

    let err = doc
        .apply(&EditTransaction {
            steps: vec![EditStep::ReplaceSpanText {
                block_id,
                guard,
                expect: None,
                span: ResolvedSpanSelector::Handle(handle),
                content: text_content("rewrite"),
                rationale: None,
            }],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: revision(),
        })
        .err()
        .expect("the stacked state is terminal: resolve it, don't edit it");
    assert_eq!(err.code, stemma::ErrorCode::UnsupportedEdit, "{err:?}");
}

// ─── Stories: the shape parses everywhere paragraphs parse ───────────────────

#[test]
fn header_story_with_stacked_revisions_parses() {
    // The corpus regression that motivated this: comment/header stories in
    // real negotiated documents carry the stacked shape, and the 3.0 interim
    // refusal made such documents unopenable. After 3a the shape parses in
    // every story.
    let header_p = r#"<w:p><w:r><w:t xml:space="preserve">H </w:t></w:r><w:ins w:id="1" w:author="A" w:date="2026-01-01T00:00:00Z"><w:del w:id="2" w:author="B" w:date="2026-02-01T00:00:00Z"><w:r><w:delText>x</w:delText></w:r></w:del></w:ins></w:p>"#;
    let sect_pr = r#"<w:sectPr><w:headerReference w:type="default" r:id="rIdH"/></w:sectPr>"#;
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body><w:p><w:r><w:t>Body.</w:t></w:r></w:p>{sect_pr}</w:body></w:document>"#
    );
    let header_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{header_p}</w:hdr>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdH" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/></Relationships>"#;
    use std::io::Write;
    use zip::write::FileOptions;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        zip.start_file("[Content_Types].xml", opts).unwrap();
        zip.write_all(content_types.as_bytes()).unwrap();
        zip.start_file("_rels/.rels", opts).unwrap();
        zip.write_all(rels.as_bytes()).unwrap();
        zip.start_file("word/_rels/document.xml.rels", opts)
            .unwrap();
        zip.write_all(doc_rels.as_bytes()).unwrap();
        zip.start_file("word/document.xml", opts).unwrap();
        zip.write_all(document_xml.as_bytes()).unwrap();
        zip.start_file("word/header1.xml", opts).unwrap();
        zip.write_all(header_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }

    let doc = Document::parse(&buf).expect("a doc with a stacked header story opens");
    assert_eq!(doc.read().blocks[0].text, "Body.");
}

// ─── Structural carriers: rows, paragraph marks, cells ──────────────────────
//
// The stacked state is not inline-only. A table row's trPr, a cell's tcPr,
// and a paragraph mark's pPr/rPr can each carry BOTH revision markers —
// inserted by one pending revision, deleted by another. Real EBA/EMA corpus
// documents have rows and marks in this shape. Word's semantics
// (verified in Word via accept, reject, and resolve on a live
// fixture) are the SAME four origin rules: both full resolutions drop the
// carrier; the mixed accept-insert + reject-delete resolution keeps it.

const STACKED_ROW_TABLE: &str = r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="4675"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="4675" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>Row one.</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:trPr><w:ins w:id="11" w:author="AuthorA" w:date="2026-01-01T00:00:00Z"/><w:del w:id="12" w:author="AuthorB" w:date="2026-01-02T00:00:00Z"/></w:trPr><w:tc><w:tcPr><w:tcW w:w="4675" w:type="dxa"/></w:tcPr><w:p><w:ins w:id="13" w:author="AuthorA" w:date="2026-01-01T00:00:00Z"><w:del w:id="14" w:author="AuthorB" w:date="2026-01-02T00:00:00Z"><w:r><w:delText>Stacked row.</w:delText></w:r></w:del></w:ins></w:p></w:tc></w:tr><w:tr><w:tc><w:tcPr><w:tcW w:w="4675" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>Row three.</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;

fn stacked_row_doc() -> Document {
    Document::parse(&make_docx_with_body(&format!(
        "{STACKED_ROW_TABLE}<w:p><w:r><w:t>After table.</w:t></w:r></w:p>"
    )))
    .expect("a stacked row parses")
}

fn table_texts(doc: &Document) -> Vec<String> {
    let view = doc.read();
    view.blocks
        .iter()
        .flat_map(|b| b.cells.iter().map(|c| c.text.clone()))
        .collect()
}

#[test]
fn row_with_both_markers_parses_as_stacked() {
    let doc = stacked_row_doc();
    let snap = doc.snapshot();
    let stemma::BlockNode::Table(t) = &snap.canonical.blocks[0].block else {
        panic!("table");
    };
    assert_eq!(t.rows.len(), 3);
    match &t.rows[1].tracking_status {
        Some(stemma::TrackingStatus::InsertedThenDeleted(sr)) => {
            assert_eq!(sr.inserted.author.as_deref(), Some("AuthorA"));
            assert_eq!(sr.deleted.author.as_deref(), Some("AuthorB"));
        }
        other => panic!("expected stacked row status, got {other:?}"),
    }
}

#[test]
fn stacked_row_drops_in_both_full_resolutions() {
    // Word-oracle-verified: accept-all AND reject-all both remove the row.
    let doc = stacked_row_doc();
    for projected in [
        doc.read_accepted().expect("accept"),
        doc.read_rejected().expect("reject"),
    ] {
        let texts = table_texts(&projected);
        assert_eq!(texts, vec!["Row one.", "Row three."]);
    }
}

#[test]
fn stacked_row_mixed_resolution_keeps_the_row() {
    // Accept AuthorA's insertion, reject AuthorB's deletion — the contested
    // row becomes a plain row. The outcome all-or-nothing can never reach
    // (Word /resolve gives the same answer).
    let doc = stacked_row_doc();
    // Both of AuthorA's insertions (the row-structural trPr insert and the
    // cell-content insert) and both of AuthorB's deletions.
    let a_ins = identities_where(&doc, |r| r.author.as_deref() == Some("AuthorA"));
    let b_del = identities_where(&doc, |r| r.author.as_deref() == Some("AuthorB"));
    let resolved = selective(
        &selective(&doc, &a_ins, ResolveSelectionAction::Accept),
        &b_del,
        ResolveSelectionAction::Reject,
    );
    let texts = table_texts(&resolved);
    assert_eq!(texts, vec!["Row one.", "Stacked row.", "Row three."]);
    let snap = resolved.snapshot();
    let stemma::BlockNode::Table(t) = &snap.canonical.blocks[0].block else {
        panic!("table");
    };
    assert!(t.rows[1].tracking_status.is_none(), "row settles to normal");
}

#[test]
fn stacked_row_reject_insertion_cascades_the_deletion() {
    let doc = stacked_row_doc();
    // The row-STRUCTURAL revision pair (trPr ins/del), disambiguated from the
    // cell-content pair by its "row[..]" excerpt.
    let row_ins = identities_where(&doc, |r| {
        r.author.as_deref() == Some("AuthorA") && r.excerpt.starts_with("row[")
    });
    let row_del = identities_where(&doc, |r| {
        r.author.as_deref() == Some("AuthorB") && r.excerpt.starts_with("row[")
    });
    assert_eq!(
        row_ins.len(),
        1,
        "one row-structural insertion: {row_ins:?}"
    );
    assert_eq!(row_del.len(), 1, "one row-structural deletion: {row_del:?}");
    let snap = doc.snapshot();
    let cascaded = stemma::tracked_model::cascaded_resolution_ids(
        &snap.canonical,
        &row_ins.iter().copied().collect(),
        ResolveSelectionAction::Reject,
    );
    assert!(
        cascaded.contains(&row_del[0]),
        "the row deletion cascades: {cascaded:?}"
    );
    // And resolving drops the row entirely: reject both of AuthorA's insertions.
    let a_ins = identities_where(&doc, |r| r.author.as_deref() == Some("AuthorA"));
    let resolved = selective(&doc, &a_ins, ResolveSelectionAction::Reject);
    assert_eq!(table_texts(&resolved), vec!["Row one.", "Row three."]);
}

#[test]
fn stacked_row_serializes_both_markers_canonically() {
    let doc = stacked_row_doc();
    let out = doc
        .serialize(&stemma::ExportOptions::default())
        .expect("serialize");
    let xml = document_xml_of(&out);
    let tr_pr_start = xml.find("<w:trPr><w:ins").expect("trPr with markers");
    let window = &xml[tr_pr_start..tr_pr_start + 300];
    let ins_pos = window.find("<w:ins ").expect("ins marker");
    let del_pos = window.find("<w:del ").expect("del marker");
    assert!(ins_pos < del_pos, "canonical order: ins before del in trPr");
    // Reimport: same state.
    let reparsed = Document::parse(&out).expect("roundtrip");
    let snap = reparsed.snapshot();
    let stemma::BlockNode::Table(t) = &snap.canonical.blocks[0].block else {
        panic!("table");
    };
    assert!(matches!(
        &t.rows[1].tracking_status,
        Some(stemma::TrackingStatus::InsertedThenDeleted(_))
    ));
}

// ─── Stacked paragraph marks ────────────────────────────────────────────────

const STACKED_MARK_BODY: &str = r#"<w:p><w:pPr><w:rPr><w:ins w:id="21" w:author="AuthorA" w:date="2026-01-01T00:00:00Z"/><w:del w:id="22" w:author="AuthorB" w:date="2026-01-02T00:00:00Z"/></w:rPr></w:pPr><w:r><w:t xml:space="preserve">First part</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve"> second part.</w:t></w:r></w:p>"#;

fn stacked_mark_doc() -> Document {
    Document::parse(&make_docx_with_body(STACKED_MARK_BODY)).expect("a stacked mark parses")
}

#[test]
fn paragraph_mark_with_both_markers_parses_as_stacked() {
    let doc = stacked_mark_doc();
    let snap = doc.snapshot();
    let stemma::BlockNode::Paragraph(p) = &snap.canonical.blocks[0].block else {
        panic!("paragraph");
    };
    match &p.para_mark_status {
        Some(stemma::TrackingStatus::InsertedThenDeleted(sr)) => {
            assert_eq!(sr.inserted.author.as_deref(), Some("AuthorA"));
            assert_eq!(sr.deleted.author.as_deref(), Some("AuthorB"));
        }
        other => panic!("expected stacked mark, got {other:?}"),
    }
}

#[test]
fn stacked_mark_merges_paragraphs_in_both_full_resolutions() {
    // The break survives NEITHER full resolution: accept-all applies its
    // deletion, reject-all un-proposes it. Either way the paragraphs join —
    // this is exactly the merge real Word performs on the EBA corpus doc.
    let doc = stacked_mark_doc();
    for projected in [
        doc.read_accepted().expect("accept"),
        doc.read_rejected().expect("reject"),
    ] {
        let view = projected.read();
        assert_eq!(view.blocks.len(), 1, "paragraphs merged");
        assert_eq!(view.blocks[0].text, "First part second part.");
    }
}

#[test]
fn stacked_mark_mixed_resolution_keeps_the_break() {
    let doc = stacked_mark_doc();
    let resolved = selective(
        &selective(
            &doc,
            &[identity_of_author(&doc, "AuthorA")],
            ResolveSelectionAction::Accept,
        ),
        &[identity_of_author(&doc, "AuthorB")],
        ResolveSelectionAction::Reject,
    );
    let view = resolved.read();
    assert_eq!(view.blocks.len(), 2, "the contested break survives");
    assert_eq!(view.blocks[0].text, "First part");
}

#[test]
fn stacked_mark_serializes_both_markers() {
    let doc = stacked_mark_doc();
    let out = doc
        .serialize(&stemma::ExportOptions::default())
        .expect("serialize");
    let xml = document_xml_of(&out);
    let rpr = xml
        .find("<w:rPr><w:ins")
        .expect("para-mark rPr with markers");
    let window = &xml[rpr..rpr + 250];
    assert!(
        window.contains("<w:del "),
        "both markers serialized: {window}"
    );
    let reparsed = Document::parse(&out).expect("roundtrip");
    let snap = reparsed.snapshot();
    let stemma::BlockNode::Paragraph(p) = &snap.canonical.blocks[0].block else {
        panic!("paragraph");
    };
    assert!(matches!(
        &p.para_mark_status,
        Some(stemma::TrackingStatus::InsertedThenDeleted(_))
    ));
}

// ─── Stacked cells ──────────────────────────────────────────────────────────

#[test]
fn cell_with_both_markers_parses_as_stacked_and_drops_both_ways() {
    // cellIns + cellDel in one tcPr: the cell-level stacked state (a tracked
    // vertical split later revoked by a tracked merge). Same origin rules.
    let table = r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>Keep.</w:t></w:r></w:p></w:tc><w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/><w:cellIns w:id="31" w:author="AuthorA" w:date="2026-01-01T00:00:00Z"/><w:cellDel w:id="32" w:author="AuthorB" w:date="2026-01-02T00:00:00Z"/></w:tcPr><w:p><w:r><w:t>Contested.</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;
    let doc = Document::parse(&make_docx_with_body(&format!(
        "{table}<w:p><w:r><w:t>Tail.</w:t></w:r></w:p>"
    )))
    .expect("a stacked cell parses");
    let snap = doc.snapshot();
    let stemma::BlockNode::Table(t) = &snap.canonical.blocks[0].block else {
        panic!("table");
    };
    assert!(matches!(
        &t.rows[0].cells[1].tracking_status,
        Some(stemma::TrackingStatus::InsertedThenDeleted(_))
    ));
    for projected in [
        doc.read_accepted().expect("accept"),
        doc.read_rejected().expect("reject"),
    ] {
        let texts = table_texts(&projected);
        assert_eq!(texts, vec!["Keep."], "the contested cell drops");
    }
}

fn document_xml_of(docx: &[u8]) -> String {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(docx.to_vec())).expect("zip");
    let mut s = String::new();
    use std::io::Read;
    zip.by_name("word/document.xml")
        .expect("document.xml")
        .read_to_string(&mut s)
        .expect("utf8");
    s
}
