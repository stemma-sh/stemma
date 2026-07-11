//! Walk a document's blocks, and see that one file is three documents.
//!
//! Run with `cargo run -p stemma --example walk_the_document`.
//!
//! Teaches two ideas from the concepts chapter:
//!   - every block has a STABLE id and a role — you address content by id,
//!     never by line number or byte offset;
//!   - a file carrying a tracked change is really THREE documents at once —
//!     the redline (as it stands), the accept-all reading, and the reject-all
//!     reading — and stemma projects each without mutating the stored document.

use stemma::api::Document;
use stemma::edit_v4::parse_transaction;

fn main() {
    let docx = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/paragraphs/before.docx"
    ))
    .expect("read testdata docx");
    let doc = Document::parse(&docx).expect("parse DOCX bytes");

    // Author one tracked change so the three projections have something to
    // disagree about.
    let edited = replace_tracked(
        &doc,
        "very interesting test",
        "genuinely riveting demonstration",
        "Reviewer",
    );

    // Walk the blocks: id, role, and a text preview. The ids (p_1, p_2, ...)
    // are stable handles — the same id an edit transaction targets.
    println!("blocks:");
    for block in &edited.read().blocks {
        let preview: String = block.text.chars().take(60).collect();
        println!(
            "  {:<6} {:<12?} {:?}",
            block.id.to_string(),
            block.role,
            preview
        );
    }

    // One file, three readings. `to_text()` is the redline (both the deletion
    // and the insertion are visible); `read_accepted()` resolves every change
    // as accepted; `read_rejected()` as rejected. Reads never mutate `edited`.
    let redline = edited.to_text();
    let accepted = edited.read_accepted().expect("accept-all").to_text();
    let rejected = edited.read_rejected().expect("reject-all").to_text();

    // Print the block that carries the change, one reading per row, so the
    // three documents visibly diverge.
    println!("\nthree projections of the edited block:");
    println!("  redline : {:?}", block_text(&edited, "p_2"));
    println!(
        "  accepted: {:?}",
        block_text(&edited.read_accepted().unwrap(), "p_2")
    );
    println!(
        "  rejected: {:?}",
        block_text(&edited.read_rejected().unwrap(), "p_2")
    );

    // The domain rules that make this more than pretty-printing:
    assert!(
        accepted.contains("genuinely riveting demonstration")
            && !accepted.contains("very interesting test"),
        "accept-all keeps ONLY the new text"
    );
    assert!(
        rejected.contains("very interesting test")
            && !rejected.contains("genuinely riveting demonstration"),
        "reject-all restores ONLY the prior text"
    );
    assert_ne!(accepted, rejected, "the two resolutions genuinely differ");
    // The redline carries BOTH sides — that is what a reviewer sees pending.
    assert!(redline.contains("very interesting test") && redline.contains("genuinely riveting"));

    println!("\nwalk_the_document OK: stable ids walked, three readings proven distinct.");
}

/// Author one tracked text replacement as `author`, via the v4 transaction wire
/// path. Reads the target block to pin the edit with its staleness `guard`,
/// then applies. (Shown fully inline in `my_first_edit`.)
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

/// The visible text of one block by id, from a document's read projection.
fn block_text(doc: &Document, id: &str) -> String {
    doc.read()
        .blocks
        .iter()
        .find(|b| b.id.to_string() == id)
        .map(|b| b.text.clone())
        .unwrap_or_default()
}
