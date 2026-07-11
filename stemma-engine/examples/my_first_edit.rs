//! Your first tracked edit, end to end.
//!
//! Run with `cargo run -p stemma --example my_first_edit`.
//!
//! Parse a DOCX, author ONE tracked replacement as a typed transaction, read
//! the receipt (which block changed, which revision id was created), serialize,
//! and prove the bytes are validator-clean. Everything goes through the public
//! `stemma::api` facade and the v4 transaction wire path — the same path the
//! MCP server and HTTP API drive.

use stemma::ExportOptions;
use stemma::api::{Document, validate};
use stemma::edit_v4::parse_transaction;

// The facade's error types do not implement `std::error::Error`, so they cannot
// be `?`-propagated. This example uses `.expect(...)` to fail loud; a production
// caller matches on the structured `code` and reacts.
fn main() {
    // 1. Parse DOCX bytes into the typed model. Fails fast on anything
    //    unrecognized (encrypted package, missing word/document.xml, ...).
    let docx = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/safe-us-vs-canada/before.docx"
    ))
    .expect("read testdata docx");
    let doc = Document::parse(&docx).expect("parse DOCX bytes");

    // 2. Read the designed projection to find the block to edit and its
    //    staleness `guard`. Addressing (stable block ids) and the guard both
    //    come from here — never from raw XML or a byte offset.
    let view = doc.read();
    let target = view
        .blocks
        .iter()
        .find(|b| b.text.contains("[Company Name]"))
        .expect("the SAFE template has a [Company Name] placeholder");
    let block_id = target.id.to_string();
    let guard = target.guard.clone();

    // 3. Author ONE tracked edit as a typed, schema-validated transaction. The
    //    `guard` pins the op to the block we just read: if the block changed
    //    since the read, `apply` fails loud (StaleEdit) instead of editing the
    //    wrong text. No author already in the redline may be impersonated.
    let txn_json = format!(
        r#"{{
            "ops": [
                {{ "op": "replace",
                   "target": "{block_id}",
                   "guard": "{guard}",
                   "content": {{ "type": "paragraph",
                                 "content": [ {{ "type": "text", "text": "Acme Corp." }} ] }} }}
            ],
            "revision": {{ "author": "J. Osei" }},
            "summary": "fill in the company name"
        }}"#
    );
    let txn = parse_transaction(&txn_json)
        .expect("transaction JSON is schema-valid")
        .into_edit_transaction()
        .expect("v4 transaction translates to an EditTransaction");
    let edited = doc.apply(&txn).expect("apply the tracked edit");

    // 4. Read the receipt. `review()` audits everything this document changed
    //    since parse against the retained baseline. A replace is authored as a
    //    paired deletion (old text) + insertion (new text), so it surfaces as
    //    two revision records — each named by id, author, and affected block.
    let report = edited.review().expect("review the session");
    assert_eq!(
        report.new_revisions.len(),
        2,
        "a replace = one delete + one insert"
    );
    for rev in &report.new_revisions {
        println!(
            "receipt: revision {} by {} on block {} — {}",
            rev.revision_id,
            rev.author.as_deref().unwrap_or("<anonymous>"),
            rev.block_id,
            rev.excerpt
        );
    }

    // 5. Serialize back to DOCX. `ExportOptions::default()` runs the blocking
    //    OOXML validator: structurally-corrupt output is refused, not returned.
    let out = edited
        .serialize(&ExportOptions::default())
        .expect("serialize to validated DOCX");
    assert!(validate(&out).ok, "emitted bytes are a valid DOCX package");

    // 6. Re-parse and prove the edit is tracked: accept-all shows the new text,
    //    reject-all restores the baseline placeholder (reject-all == baseline).
    let reparsed = Document::parse(&out).expect("re-parse emitted DOCX");
    assert!(
        reparsed
            .read_accepted()
            .expect("accept-all")
            .to_text()
            .contains("Acme Corp.")
    );
    assert!(
        reparsed
            .read_rejected()
            .expect("reject-all")
            .to_text()
            .contains("[Company Name]")
    );

    println!(
        "my_first_edit OK: 1 tracked edit on {} blocks, {} validated bytes; \
         accept-all shows 'Acme Corp.', reject-all preserves the baseline.",
        view.blocks.len(),
        out.len()
    );
}
