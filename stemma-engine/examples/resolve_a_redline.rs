//! Resolve a redline authored by two people: accept one author, reject one
//! specific change — and verify the result by CONTENT, not by marker absence.
//!
//! Run with `cargo run -p stemma --example resolve_a_redline`.
//!
//! This is the revisions chapter's core lesson. Accepting and rejecting a
//! change both REMOVE its marker, so "the marker is gone" proves nothing about
//! which happened. The difference is content: accept keeps the new state,
//! reject restores the prior state exactly. So we assert on the restored text.

use std::collections::HashSet;

use stemma::api::{Document, TrackStatus};
use stemma::edit_v4::parse_transaction;
use stemma::{Resolution, ResolveSelectionAction};

const P2_ORIGINAL: &str = "This is a very interesting test that includes lots of stuff.";
const P3_ORIGINAL: &str = "This is a much longer sequence";

fn main() {
    let docx = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/paragraphs/before.docx"
    ))
    .expect("read testdata docx");
    let doc = Document::parse(&docx).expect("parse DOCX bytes");

    // Two authors each leave one tracked change, on different paragraphs.
    let doc = replace_tracked(&doc, P2_ORIGINAL, "Alice's revised clause.", "Alice");
    let doc = replace_tracked(&doc, "much longer sequence", "Bob's rewrite.", "Bob");

    // Enumerate the pending revisions the way a reviewer does — from the read
    // view, which is authoritative NOW (ids are session handles, not durable
    // file properties). Group them by author.
    println!("pending revisions:");
    for (rev_id, author) in revisions(&doc) {
        println!("  revision {rev_id} by {author}");
    }
    let alice_ids = revisions_by(&doc, "Alice");
    let bob_ids = revisions_by(&doc, "Bob");
    assert!(
        !alice_ids.is_empty() && !bob_ids.is_empty(),
        "both authors present"
    );

    // Accept everything Alice proposed; then, on that result, reject Bob's
    // change. Selective resolution leaves the untouched revisions in place.
    let after_accept = doc
        .project(Resolution::Selective {
            ids: alice_ids,
            action: ResolveSelectionAction::Accept,
        })
        .expect("accept Alice's changes");
    // Re-enumerate: after resolving Alice's, only Bob's remain — resolve against
    // what the document says now, never against ids remembered from before.
    let bob_now = revisions_by(&after_accept, "Bob");
    assert_eq!(
        bob_now, bob_ids,
        "Bob's untouched revision is preserved marker-for-marker"
    );
    let resolved = after_accept
        .project(Resolution::Selective {
            ids: bob_now,
            action: ResolveSelectionAction::Reject,
        })
        .expect("reject Bob's change");

    // Verify by CONTENT. Every marker is now gone from both paragraphs, so the
    // only honest check is the text itself:
    let p2 = block_text(&resolved, "p_2");
    let p3 = block_text(&resolved, "p_3");
    println!("\nafter resolution:");
    println!("  p_2 (Alice accepted): {p2:?}");
    println!("  p_3 (Bob rejected)  : {p3:?}");

    assert_eq!(
        p2, "Alice's revised clause.",
        "accepting Alice kept her new text"
    );
    assert!(
        p3.starts_with(P3_ORIGINAL) && !p3.contains("Bob's rewrite"),
        "rejecting Bob RESTORED the prior text, exactly — not merely removed a marker"
    );

    println!(
        "\nresolve_a_redline OK: one author accepted, one change rejected, verified by content."
    );
}

/// Every pending revision as `(revision_id, author)`, read from the projection.
/// A tracked change surfaces as inserted/deleted spans carrying revision
/// metadata; adjacent spans sharing a revision id are one change.
fn revisions(doc: &Document) -> Vec<(u32, String)> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut note = |st: &TrackStatus| {
        for (id, author) in status_revisions(st) {
            if seen.insert(id) {
                out.push((id, author));
            }
        }
    };
    for block in &doc.read().blocks {
        note(&block.block_status);
        note(&block.paragraph_mark_status);
        for seg in &block.segments {
            match seg {
                stemma::api::SegmentView::Text { status, .. } => note(status),
                stemma::api::SegmentView::Opaque { status, .. } => note(status),
            }
        }
    }
    out.sort_by_key(|(id, _)| *id);
    out
}

/// The revision ids authored by `author`.
fn revisions_by(doc: &Document, author: &str) -> HashSet<u32> {
    revisions(doc)
        .into_iter()
        .filter(|(_, a)| a == author)
        .map(|(id, _)| id)
        .collect()
}

/// The `(revision_id, author)` pairs a single tracked status carries.
fn status_revisions(status: &TrackStatus) -> Vec<(u32, String)> {
    let one = |r: &stemma::api::RevisionView| {
        (
            r.revision_id,
            r.author
                .clone()
                .unwrap_or_else(|| "<anonymous>".to_string()),
        )
    };
    match status {
        TrackStatus::Normal => vec![],
        TrackStatus::Inserted(r) | TrackStatus::Deleted(r) => vec![one(r)],
        TrackStatus::InsertedThenDeleted { inserted, deleted } => vec![one(inserted), one(deleted)],
    }
}

fn block_text(doc: &Document, id: &str) -> String {
    doc.read()
        .blocks
        .iter()
        .find(|b| b.id.to_string() == id)
        .map(|b| b.text.clone())
        .unwrap_or_default()
}

/// Author one tracked whole-paragraph replacement as `author`, via the v4 wire
/// path. Plain `apply` enforces no author policy, so two authors can seed the
/// same document in turn. (Shown fully inline in `my_first_edit`.)
fn replace_tracked(doc: &Document, needle: &str, replacement: &str, author: &str) -> Document {
    let view = doc.read();
    let target = view
        .blocks
        .iter()
        .find(|b| b.text.contains(needle))
        .unwrap_or_else(|| panic!("no block contains {needle:?}"));
    let txn_json = format!(
        r#"{{ "ops": [ {{ "op": "replace", "target": "{}", "guard": "{}",
               "content": {{ "type": "paragraph",
                             "content": [ {{ "type": "text", "text": "{replacement}" }} ] }} }} ],
             "revision": {{ "author": "{author}" }} }}"#,
        target.id, target.guard
    );
    let txn = parse_transaction(&txn_json)
        .expect("v4 schema-valid")
        .into_edit_transaction()
        .expect("v4 -> EditTransaction");
    doc.apply(&txn).expect("apply tracked replace")
}
