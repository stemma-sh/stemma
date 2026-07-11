//! Invariants for the agentic MCP surface (roadmap E):
//! `accept_changes`, `reject_changes`, `check_edit`, `validate_docx`,
//! `apply_batch`, `review_session`, `audit_docx`.
//!
//! The tool methods themselves live in the `stemma-mcp` *binary* crate, so they
//! are not importable here. Instead these tests drive `stemma::SimpleRuntime`
//! and `stemma::api` EXACTLY as the tool bodies do, and re-implement the one
//! piece of edge logic the tools own — lowering a `ChangeSelector` to a
//! `HashSet<u32>` of revision ids by walking the read view — so the
//! selector-lowering contract (fail-loud on empty/unmatched, never invent an
//! author, range ordering) is covered against the real engine surface.
//!
//! Daily-tier: every document is synthesized in-memory; no corpus, no env.

use std::collections::HashSet;
use std::io::Write;

use stemma::api::{Document, validate};
use stemma::domain::{NodeId, RevisionInfo};
use stemma::edit::{
    BlockSpec, ContentFragment, EditStep, EditTransaction, MaterializationMode, ParagraphContent,
};
use stemma::edit_v4::parse_transaction;
use stemma::view::{BlockView, SegmentView, TrackStatus};
use stemma::{DocHandle, DocxRuntime, ExportMode, ResolveSelectionAction, SimpleRuntime};

// ─── In-memory DOCX builder (mirrors stemma-engine/src/api.rs test helper) ───────────

fn make_test_docx(paragraphs: &[&str]) -> Vec<u8> {
    let mut document_xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#,
    );
    for para in paragraphs {
        document_xml.push_str(&format!(r#"<w:p><w:r><w:t>{para}</w:t></w:r></w:p>"#));
    }
    document_xml.push_str("<w:sectPr/></w:body></w:document>");

    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;

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

/// A replace-paragraph transaction authored as `author` with a chosen
/// `revision_id` — the same shape the v4 adapter produces for an MCP edit.
fn replace_txn(
    block_id: &str,
    expect: &str,
    replacement: &str,
    revision_id: u32,
    author: &str,
) -> EditTransaction {
    EditTransaction {
        steps: vec![EditStep::ReplaceParagraphText {
            block_id: NodeId::from(block_id),
            rationale: None,
            replacement_role: None,
            expect: expect.to_string(),
            semantic_hash: None,
            content: ParagraphContent {
                fragments: vec![ContentFragment::Text(replacement.to_string())],
            },
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id,
            author: Some(author.to_string()),
            date: Some("2026-06-01T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

/// Open `bytes` in a fresh runtime and return its handle.
fn open(rt: &SimpleRuntime, bytes: &[u8]) -> DocHandle {
    rt.import_docx(bytes).expect("import").doc_handle
}

/// Accept-all visible text the document serializes to, ignoring `w:delText`.
/// Mirrors the api.rs test reader so we assert on post-accept text, not the
/// engine-bound read-view shape.
fn exported_text(rt: &SimpleRuntime, handle: &DocHandle) -> String {
    let bytes = rt.export_docx(handle, ExportMode::Redline).expect("export");
    w_t_text(&bytes)
}

fn w_t_text(docx: &[u8]) -> String {
    let archive = stemma::docx::DocxArchive::read(docx).expect("archive");
    let xml = String::from_utf8(archive.get("word/document.xml").expect("doc").to_vec()).unwrap();
    extract_w_t(&xml)
}

fn extract_w_t(xml: &str) -> String {
    let mut out = String::new();
    let bytes = xml.as_bytes();
    let mut i = 0;
    while let Some(rel) = xml[i..].find("<w:t") {
        let tag_start = i + rel;
        let after = tag_start + 4;
        if after >= bytes.len() || (bytes[after] != b' ' && bytes[after] != b'>') {
            i = after;
            continue;
        }
        let Some(gt) = xml[tag_start..].find('>') else {
            break;
        };
        let content_start = tag_start + gt + 1;
        let Some(close_rel) = xml[content_start..].find("</w:t>") else {
            break;
        };
        out.push_str(&xml[content_start..content_start + close_rel]);
        i = content_start + close_rel + "</w:t>".len();
    }
    out
}

// ─── ChangeSelector lowering (copy of the tool-edge logic under test) ─────────

#[derive(Clone)]
enum Selector {
    ByIds(Vec<u32>),
    ByAuthor(String),
    ByRange { from: String, to: String },
    All,
}

/// What `resolve_revision_ids` does in the binary: walk the read view of the
/// LIVE snapshot, lower a selector to a revision-id set, fail loud on
/// empty/unmatched. Returns the stable wire `ErrorCode` string on failure,
/// exactly as the tool reports.
///
/// CRITICAL: the tool resolves against the open in-memory snapshot's read view
/// (`runtime.with(&handle, build_document_view)`), NOT a re-parsed export. The
/// DOCX `w:id` re-numbering on export/re-parse changes revision ids, so the
/// id-set is only stable against the live snapshot — which is exactly what
/// `accept_changes` / `reject_changes` then act on.
fn resolve_ids_live(
    rt: &SimpleRuntime,
    handle: &DocHandle,
    sel: Selector,
) -> Result<HashSet<u32>, &'static str> {
    let view = rt
        .with(handle, stemma::view::build_document_view)
        .expect("doc open");
    resolve_ids_view(&view, sel)
}

/// Same lowering against a `DocumentView` already in hand.
fn resolve_ids_view(
    view: &stemma::view::DocumentView,
    sel: Selector,
) -> Result<HashSet<u32>, &'static str> {
    fn rev_of(s: &TrackStatus) -> Vec<(u32, Option<String>)> {
        match s {
            TrackStatus::Normal => vec![],
            TrackStatus::Inserted(r) | TrackStatus::Deleted(r) => {
                vec![(r.revision_id, r.author.clone())]
            }
            TrackStatus::InsertedThenDeleted { inserted, deleted } => vec![
                (inserted.revision_id, inserted.author.clone()),
                (deleted.revision_id, deleted.author.clone()),
            ],
        }
    }
    let block_revs = |b: &BlockView| -> Vec<(u32, Option<String>)> {
        let mut out = Vec::new();
        out.extend(rev_of(&b.block_status));
        out.extend(rev_of(&b.paragraph_mark_status));
        for seg in &b.segments {
            let st = match seg {
                SegmentView::Text { status, .. } => status,
                SegmentView::Opaque { status, .. } => status,
            };
            out.extend(rev_of(st));
        }
        out
    };

    let ids: HashSet<u32> = match sel {
        Selector::ByIds(req) => {
            let present: HashSet<u32> = view
                .blocks
                .iter()
                .flat_map(|b| block_revs(b).into_iter().map(|(id, _)| id))
                .collect();
            if req.iter().any(|id| !present.contains(id)) {
                return Err("InvalidRange");
            }
            req.into_iter().collect()
        }
        Selector::ByAuthor(author) => view
            .blocks
            .iter()
            .flat_map(&block_revs)
            .filter_map(|(id, a)| (a.as_deref() == Some(author.as_str())).then_some(id))
            .collect(),
        Selector::ByRange { from, to } => {
            let pos = |bid: &str| view.blocks.iter().position(|b| b.id.to_string() == bid);
            let (Some(f), Some(t)) = (pos(&from), pos(&to)) else {
                return Err("AnchorNotFound");
            };
            if f > t {
                return Err("InvalidRange");
            }
            view.blocks[f..=t]
                .iter()
                .flat_map(|b| block_revs(b).into_iter().map(|(id, _)| id))
                .collect()
        }
        Selector::All => view
            .blocks
            .iter()
            .flat_map(|b| block_revs(b).into_iter().map(|(id, _)| id))
            .collect(),
    };

    if ids.is_empty() {
        return Err("InvalidRange");
    }
    Ok(ids)
}

/// Lowering against a `Document`'s read view (for docs not held in a runtime).
fn resolve_ids_doc(doc: &Document, sel: Selector) -> Result<HashSet<u32>, &'static str> {
    resolve_ids_view(&doc.read(), sel)
}

fn block_ids(doc: &Document) -> Vec<String> {
    doc.read().blocks.iter().map(|b| b.id.to_string()).collect()
}

/// Manufacture a two-paragraph doc with two distinct authored tracked changes:
/// revision 1 by "Alice" on p0, revision 2 by "Bob" on p1. Returns (runtime,
/// handle, [p0_id, p1_id]).
fn two_authored_changes() -> (SimpleRuntime, DocHandle, Vec<String>) {
    let rt = SimpleRuntime::new();
    let base = make_test_docx(&["First clause text", "Second clause text"]);
    let handle = open(&rt, &base);
    // Read block ids off the read view (same surface the tool targets).
    let ids: Vec<String> = rt
        .with(&handle, |snap| {
            stemma::view::build_document_view(snap)
                .blocks
                .iter()
                .map(|b| b.id.to_string())
                .collect::<Vec<_>>()
        })
        .expect("read");
    let a = replace_txn(
        &ids[0],
        "First clause text",
        "First clause AMENDED",
        1,
        "Alice",
    );
    rt.apply_edit(&handle, &a).expect("apply alice");
    let b = replace_txn(
        &ids[1],
        "Second clause text",
        "Second clause AMENDED",
        2,
        "Bob",
    );
    rt.apply_edit(&handle, &b).expect("apply bob");
    (rt, handle, ids)
}

// ─── T1: accept-all / reject-all via selector ─────────────────────────────────

#[test]
fn t1_accept_all_via_selector_equals_target_reject_all_equals_baseline() {
    // Single change so "accept-all" and "reject-all" have a clean target/base.
    let rt = SimpleRuntime::new();
    let base = make_test_docx(&["Hello world"]);
    let handle = open(&rt, &base);
    let ids = block_ids(&Document::parse(&base).unwrap());
    let txn = replace_txn(&ids[0], "Hello world", "Goodbye world", 1, "Alice");
    rt.apply_edit(&handle, &txn).expect("apply");

    // accept_changes(All) == target. Resolve the `All` selector against the
    // accept runtime's own live snapshot, exactly as the tool does.
    let accept_rt = SimpleRuntime::new();
    let h = open(&accept_rt, &base);
    accept_rt.apply_edit(&h, &txn).unwrap();
    let all_accept = resolve_ids_live(&accept_rt, &h, Selector::All).expect("All resolves");
    accept_rt
        .resolve_tracked_revisions(&h, &all_accept, ResolveSelectionAction::Accept)
        .expect("accept all");
    assert!(
        exported_text(&accept_rt, &h).contains("Goodbye world"),
        "accept-all via selector must equal the target text"
    );

    // reject_changes(All) == baseline.
    let reject_rt = SimpleRuntime::new();
    let h2 = open(&reject_rt, &base);
    reject_rt.apply_edit(&h2, &txn).unwrap();
    let all_reject = resolve_ids_live(&reject_rt, &h2, Selector::All).expect("All resolves");
    reject_rt
        .resolve_tracked_revisions(&h2, &all_reject, ResolveSelectionAction::Reject)
        .expect("reject all");
    assert!(
        exported_text(&reject_rt, &h2).contains("Hello world"),
        "reject-all via selector must equal the baseline text"
    );
}

// ─── T2: by-author resolves exactly; accepting A leaves B tracked ─────────────

#[test]
fn t2_by_author_resolves_exactly_and_leaves_other_author_tracked() {
    let (rt, handle, _ids) = two_authored_changes();

    // Resolve each author against the LIVE snapshot (the tool's surface). The
    // engine assigns its own revision ids; the contract under test is that
    // ByAuthor partitions the changes — Alice's set and Bob's set are
    // non-empty and disjoint, and together they are the full change set.
    let alice = resolve_ids_live(&rt, &handle, Selector::ByAuthor("Alice".into())).expect("alice");
    let bob = resolve_ids_live(&rt, &handle, Selector::ByAuthor("Bob".into())).expect("bob");
    let all = resolve_ids_live(&rt, &handle, Selector::All).expect("all");
    assert!(
        !alice.is_empty() && !bob.is_empty(),
        "each author has changes"
    );
    assert!(alice.is_disjoint(&bob), "ByAuthor sets are disjoint");
    assert_eq!(
        &alice | &bob,
        all,
        "Alice's and Bob's changes together are the full change set"
    );

    // Accept only Alice's change.
    rt.resolve_tracked_revisions(&handle, &alice, ResolveSelectionAction::Accept)
        .expect("accept alice");

    // Alice's edit is now baked into the visible text; Bob's is still tracked,
    // so it must still be resolvable by author against the updated live view.
    assert!(
        exported_text(&rt, &handle).contains("First clause AMENDED"),
        "Alice's accepted change must be in the visible text"
    );
    let bob_after = resolve_ids_live(&rt, &handle, Selector::ByAuthor("Bob".into()))
        .expect("Bob's revision must still be tracked after accepting Alice");
    assert!(
        !bob_after.is_empty(),
        "Bob's span still tracked after accepting Alice"
    );
    // Accepting Bob too now yields the full target.
    rt.resolve_tracked_revisions(&handle, &bob_after, ResolveSelectionAction::Accept)
        .expect("accept bob");
    assert!(
        exported_text(&rt, &handle).contains("Second clause AMENDED"),
        "accepting Bob bakes his change in too"
    );
}

// ─── T3: dry-run mutates nothing ──────────────────────────────────────────────

#[test]
fn t3_check_edit_is_dry_run_and_stale_check_leaves_state_equal() {
    let rt = SimpleRuntime::new();
    let base = make_test_docx(&["Hello world"]);
    let handle = open(&rt, &base);
    let ids = block_ids(&Document::parse(&base).unwrap());

    let before = exported_text(&rt, &handle);

    // A valid check (dry run on a clone of the canonical) must mutate nothing.
    let good = replace_txn(&ids[0], "Hello world", "Goodbye world", 1, "Alice");
    rt.with(&handle, |snap| {
        stemma::edit::apply_transaction(&snap.canonical.clone(), &good).map(|_| ())
    })
    .expect("with")
    .expect("good check passes");
    assert_eq!(
        before,
        exported_text(&rt, &handle),
        "good check mutates nothing"
    );

    // A stale check must fail AND leave state equal.
    let stale = replace_txn(&ids[0], "NOT THE TEXT", "x", 1, "Alice");
    let stale_outcome = rt
        .with(&handle, |snap| {
            stemma::edit::apply_transaction(&snap.canonical.clone(), &stale).map(|_| ())
        })
        .expect("with");
    assert!(stale_outcome.is_err(), "stale check must fail");
    assert_eq!(
        before,
        exported_text(&rt, &handle),
        "stale check must leave exported text byte-identical"
    );
}

// ─── T4: empty / unmatched selector fails loud ────────────────────────────────

#[test]
fn t4_empty_or_unmatched_selector_is_invalid_range_never_silent() {
    // Document with NO tracked changes: every selector resolves to {} and must
    // fail loud rather than silently no-op.
    let clean = Document::parse(&make_test_docx(&["Hello world"])).unwrap();
    assert_eq!(resolve_ids_doc(&clean, Selector::All), Err("InvalidRange"));
    assert_eq!(
        resolve_ids_doc(&clean, Selector::ByAuthor("Nobody".into())),
        Err("InvalidRange")
    );

    // A doc WITH a change: an unmatched author / a non-present id still fail.
    let (rt, handle, ids) = two_authored_changes();
    assert_eq!(
        resolve_ids_live(&rt, &handle, Selector::ByAuthor("Ghost".into())),
        Err("InvalidRange"),
        "unmatched author => InvalidRange, never silent {{}}"
    );
    assert_eq!(
        resolve_ids_live(&rt, &handle, Selector::ByIds(vec![9999])),
        Err("InvalidRange"),
        "non-present id => InvalidRange"
    );
    // Unknown range endpoint => AnchorNotFound; out-of-order => InvalidRange.
    assert_eq!(
        resolve_ids_live(
            &rt,
            &handle,
            Selector::ByRange {
                from: "p_does_not_exist".into(),
                to: ids[1].clone()
            }
        ),
        Err("AnchorNotFound")
    );
    assert_eq!(
        resolve_ids_live(
            &rt,
            &handle,
            Selector::ByRange {
                from: ids[1].clone(),
                to: ids[0].clone()
            }
        ),
        Err("InvalidRange"),
        "out-of-order endpoints => InvalidRange"
    );
}

// ─── T5: anonymized revision is unmatched-because-anonymous, never invented ───

#[test]
fn t5_by_author_never_matches_anonymized_revision() {
    // Author an UNATTRIBUTED change (author = None) directly through the IR-
    // shaped transaction. ByAuthor must not match it under any name; ByIds /
    // All still collect it (it has a revision id).
    let rt = SimpleRuntime::new();
    let base = make_test_docx(&["Hello world"]);
    let handle = open(&rt, &base);
    let ids = block_ids(&Document::parse(&base).unwrap());
    let mut anon = replace_txn(&ids[0], "Hello world", "Anon edit", 7, "placeholder");
    anon.revision.author = None; // anonymized: no w:author
    rt.apply_edit(&handle, &anon).expect("apply anon");

    // No author string matches an anonymized (author = None) revision.
    assert_eq!(
        resolve_ids_live(&rt, &handle, Selector::ByAuthor("placeholder".into())),
        Err("InvalidRange"),
        "anonymized revision must be unmatched-because-anonymous"
    );
    // But it IS still collectible by All (and thus by id). Discover the
    // engine-assigned revision id(s) from the live view, then prove ByIds
    // collects exactly them.
    let by_all = resolve_ids_live(&rt, &handle, Selector::All).expect("All collects anonymized");
    assert!(
        !by_all.is_empty(),
        "the anonymized change has tracked revision ids"
    );
    let anon_ids: Vec<u32> = by_all.iter().copied().collect();
    let by_id =
        resolve_ids_live(&rt, &handle, Selector::ByIds(anon_ids.clone())).expect("ByIds collects");
    assert_eq!(
        by_id, by_all,
        "ByIds(anon ids) == the full anonymized change set"
    );
}

// ─── T6: validate_docx — fresh serialize ok; corrupt bytes not ok ─────────────

#[test]
fn t6_validate_docx_ok_for_fresh_and_not_ok_for_corrupt() {
    let rt = SimpleRuntime::new();
    let base = make_test_docx(&["Hello world"]);
    let handle = open(&rt, &base);
    let bytes = rt
        .export_docx(&handle, ExportMode::Redline)
        .expect("export");

    let report = validate(&bytes);
    assert!(report.ok, "fresh serialize must validate ok");
    assert!(report.issues.is_empty(), "ok report carries no issues");

    let corrupt = validate(b"not a docx at all");
    assert!(!corrupt.ok, "corrupt bytes must not validate");
    assert!(
        !corrupt.issues.is_empty(),
        "invalid report must carry issues"
    );
    // Every issue carries one of the known codes — no catch-all "other".
    use stemma::ValidationIssueCode::*;
    for issue in &corrupt.issues {
        assert!(
            matches!(
                issue.code,
                PackageInvariant | WordprocessingInvariant | SchemaInvariant
            ),
            "every issue must be a known ValidationIssueCode"
        );
    }
}

// ─── T7: selective accept then export re-parses into a valid Document ─────────

#[test]
fn t7_selective_accept_export_reparses_and_validates() {
    let (rt, handle, _ids) = two_authored_changes();
    let alice = resolve_ids_live(&rt, &handle, Selector::ByAuthor("Alice".into())).expect("alice");

    rt.resolve_tracked_revisions(&handle, &alice, ResolveSelectionAction::Accept)
        .expect("accept alice");

    let bytes = rt
        .export_docx(&handle, ExportMode::Redline)
        .expect("export");
    // Re-parses into a Document (no orphan vMerge/ins/del structure).
    Document::parse(&bytes).expect("re-parse after selective accept");
    // And validates clean.
    assert!(
        validate(&bytes).ok,
        "selective-accept export must validate ok (no orphan tracked-change markup)"
    );
}

// ─── T8: apply_batch preview path persists nothing ────────────────────────────

#[test]
fn t8_batch_preview_builds_outline_without_persisting() {
    let rt = SimpleRuntime::new();
    let base = make_test_docx(&["Hello world"]);
    let handle = open(&rt, &base);
    let ids = block_ids(&Document::parse(&base).unwrap());

    let before = exported_text(&rt, &handle);

    // preview=true path: run the verb core on a clone and build a preview
    // outline from the discarded canonical; the live snapshot is untouched.
    let txn = replace_txn(&ids[0], "Hello world", "Previewed text", 1, "Alice");
    let preview_text = rt
        .with(&handle, |snap| {
            stemma::edit::apply_transaction(&snap.canonical.clone(), &txn).map(
                |(canon, _pending)| {
                    canon
                        .blocks
                        .iter()
                        .map(|tb| stemma::import::extract_block_text(&tb.block))
                        .collect::<Vec<_>>()
                        .join(" ")
                },
            )
        })
        .expect("with")
        .expect("preview applies");
    assert!(
        preview_text.contains("Previewed text"),
        "preview outline must reflect the (discarded) edit"
    );
    assert_eq!(
        before,
        exported_text(&rt, &handle),
        "preview must persist nothing"
    );

    // preview=false path: actually apply, and now the change is live. Resolve
    // `All` against the live snapshot and accept it (the accept_changes path),
    // then the target text is in the exported document.
    rt.apply_edit(&handle, &txn).expect("apply for real");
    let accepted = resolve_ids_live(&rt, &handle, Selector::All).unwrap();
    rt.resolve_tracked_revisions(&handle, &accepted, ResolveSelectionAction::Accept)
        .expect("accept applied batch");
    assert!(
        exported_text(&rt, &handle).contains("Previewed text"),
        "applied (non-preview) batch must change the document"
    );
}

// ─── v4 wire: paragraph content `list: {num_id, ilvl}` round-trips ────────────
//
// The MCP `apply_edit` tool parses the edit JSON through `parse_transaction`
// (schema check) and `into_edit_transaction` (adapter) — exactly the path this
// test drives. The new `list` field on an inserted paragraph must survive both
// steps and arrive at the engine as the inserted paragraph's numbering spec, so
// an agent can author a list sub-point as a single tracked insert.

/// The wire JSON parses, schema-validates, and translates so the inserted
/// paragraph carries the `{num_id, ilvl}` it was authored with.
#[test]
fn insert_content_list_field_round_trips_through_the_wire() {
    let json = r#"{
      "ops": [{
        "op": "insert",
        "target": { "anchor": "p_1", "position": "after" },
        "content": [{
          "type": "paragraph",
          "role": "default",
          "content": [{ "type": "text", "text": "Freshly nested" }],
          "list": { "num_id": 4, "ilvl": 2 }
        }]
      }],
      "revision": { "author": "wire" }
    }"#;

    let txn = parse_transaction(json)
        .expect("schema check passes with the list field")
        .into_edit_transaction()
        .expect("adapter translates the list field");

    assert_eq!(txn.steps.len(), 1);
    let EditStep::InsertParagraphs { blocks, .. } = &txn.steps[0] else {
        panic!(
            "insert op must translate to InsertParagraphs, got {:?}",
            txn.steps[0]
        );
    };
    assert_eq!(blocks.len(), 1);
    let BlockSpec::Paragraph(p) = &blocks[0] else {
        panic!("inserted block must be a paragraph");
    };
    let list = p
        .list
        .expect("the list field must survive the wire round-trip");
    assert_eq!(list.num_id, 4, "num_id round-trips");
    assert_eq!(list.ilvl, 2, "ilvl round-trips");
}

/// An inserted paragraph WITHOUT a `list` field translates to `list: None` —
/// the field is optional and defaults to absent (back-compat with every
/// existing insert).
#[test]
fn insert_without_list_field_defaults_to_none() {
    let json = r#"{
      "ops": [{
        "op": "insert",
        "target": { "anchor": "p_1", "position": "after" },
        "content": [{
          "type": "paragraph",
          "role": "default",
          "content": [{ "type": "text", "text": "Plain insert" }]
        }]
      }],
      "revision": { "author": "wire" }
    }"#;

    let txn = parse_transaction(json)
        .expect("schema ok")
        .into_edit_transaction()
        .expect("adapter ok");
    let EditStep::InsertParagraphs { blocks, .. } = &txn.steps[0] else {
        panic!("expected InsertParagraphs");
    };
    let BlockSpec::Paragraph(p) = &blocks[0] else {
        panic!("expected paragraph block");
    };
    assert!(p.list.is_none(), "absent list field defaults to None");
}

/// An inserted paragraph with `list.ilvl` outside 0..=8 is refused at the schema
/// layer (no silent clamp).
#[test]
fn insert_list_ilvl_out_of_bounds_is_refused() {
    let json = r#"{
      "ops": [{
        "op": "insert",
        "target": { "anchor": "p_1", "position": "after" },
        "content": [{
          "type": "paragraph",
          "role": "default",
          "content": [{ "type": "text", "text": "too deep" }],
          "list": { "num_id": 4, "ilvl": 9 }
        }]
      }],
      "revision": { "author": "wire" }
    }"#;
    let err = parse_transaction(json).expect_err("ilvl 9 must be refused");
    let msg = format!("{err}");
    assert!(
        msg.contains("0..=8") || msg.contains("ilvl"),
        "the schema refusal names the list-level bound: {msg}"
    );
}

// ─── review_session / audit_docx (RFC 0001) ──────────────────────────────────

/// The session baseline is the OPEN-TIME state: after tracked edits, review
/// reports the census against it, proves the rest untouched, and neither
/// saving (exporting) nor further reviews reset it. This is the runtime
/// contract `review_session` (the tool) is a thin wrapper over.
#[test]
fn review_session_reports_since_open_and_export_does_not_reset_baseline() {
    let rt = SimpleRuntime::new();
    let base = make_test_docx(&["First clause text", "Second clause text"]);
    let handle = open(&rt, &base);
    let ids = block_ids(&Document::parse(&base).unwrap());
    let txn = replace_txn(
        &ids[0],
        "First clause text",
        "First clause AMENDED",
        1,
        "Alice",
    );
    rt.apply_edit(&handle, &txn).expect("apply");

    let report = rt.review_session(&handle).expect("review");
    assert!(
        !report.new_revisions.is_empty(),
        "the tracked edit is in the census: {report:?}"
    );
    assert!(report.direct_changes.is_empty(), "{report:?}");
    assert!(report.untouched.violations.is_empty(), "{report:?}");
    assert!(report.validator.ok, "{report:?}");

    // "Save" (export) — then review again: same since-open census, because
    // re-opening is the only baseline reset.
    let _saved = rt
        .export_docx(&handle, ExportMode::Redline)
        .expect("export");
    let again = rt.review_session(&handle).expect("review after export");
    assert_eq!(
        report.new_revisions, again.new_revisions,
        "export does not reset the review baseline"
    );
}

/// The stateless form certifies out-of-band work: `stemma::audit` over the
/// session's source bytes and its exported bytes agrees with the in-session
/// review on the census — the two doors share one core.
#[test]
fn stateless_audit_of_exported_bytes_agrees_with_session_review() {
    let rt = SimpleRuntime::new();
    let base = make_test_docx(&["First clause text", "Second clause text"]);
    let handle = open(&rt, &base);
    let ids = block_ids(&Document::parse(&base).unwrap());
    let txn = replace_txn(
        &ids[0],
        "First clause text",
        "First clause AMENDED",
        1,
        "Alice",
    );
    rt.apply_edit(&handle, &txn).expect("apply");

    let session = rt.review_session(&handle).expect("session review");
    let source = rt.session_source_bytes(&handle).expect("source bytes");
    let exported = rt
        .export_docx(&handle, ExportMode::Redline)
        .expect("export");
    let stateless = stemma::audit(&source, &exported).expect("stateless audit");

    // Ids are NOT comparable across the doors: export re-numbers `w:id`
    // (see `resolve_ids_live`'s doc comment above) — which is exactly why
    // the audit census matches by record content, never by raw ids. The
    // cross-door invariant is the (kind, author, excerpt) census itself.
    let content =
        |r: &stemma::tracked_model::RevisionRecord| (r.kind, r.author.clone(), r.excerpt.clone());
    let session_census: Vec<_> = session.new_revisions.iter().map(content).collect();
    let stateless_census: Vec<_> = stateless.new_revisions.iter().map(content).collect();
    assert_eq!(
        session_census, stateless_census,
        "session and stateless censuses agree on the same edit"
    );
    assert!(stateless.untouched.violations.is_empty(), "{stateless:?}");
}

/// A cloned handle continues the same session lineage: its review baseline
/// is the ORIGIN's open-time state, so edits made before the clone still
/// appear in the clone's review.
#[test]
fn cloned_handle_carries_the_review_baseline() {
    let rt = SimpleRuntime::new();
    let base = make_test_docx(&["First clause text"]);
    let handle = open(&rt, &base);
    let ids = block_ids(&Document::parse(&base).unwrap());
    let txn = replace_txn(
        &ids[0],
        "First clause text",
        "First clause AMENDED",
        1,
        "Alice",
    );
    rt.apply_edit(&handle, &txn).expect("apply");

    let clone = rt.clone_handle(&handle).expect("clone");
    let report = rt.review_session(&clone).expect("review clone");
    assert!(
        !report.new_revisions.is_empty(),
        "the pre-clone edit is still in the clone's since-open census: {report:?}"
    );
}
