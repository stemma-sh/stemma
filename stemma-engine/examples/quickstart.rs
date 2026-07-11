//! End-to-end quickstart for the public `Document` facade.
//!
//! Run with `cargo run --example quickstart`. It exercises the full durable
//! loop the README describes — parse DOCX bytes, read the designed projection,
//! author one tracked edit as a typed transaction, serialize back to DOCX,
//! re-parse the output, and assert the edit landed.
//!
//! Everything here goes through `stemma::api` and the v4 transaction wire path;
//! nothing reaches past the facade into the IR.

use stemma::api::Document;
use stemma::edit_v4::parse_transaction;
use stemma::{ExportMode, ExportOptions, ValidatorLevel};

// NOTE: the facade's error types (`RuntimeError`, the v4 `SchemaError` /
// `AdapterError`) do not implement `std::error::Error`, so they cannot be
// `?`-propagated into `Box<dyn Error>`. They carry a structured `code` plus a
// message; handle them explicitly. This example uses `.expect(...)` to fail
// loud — production callers should match on the error and react.
fn main() {
    // 1. Parse DOCX bytes into the typed model. Fails fast on anything
    //    unrecognized (encrypted package, missing word/document.xml, ...).
    let docx_bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/safe-us-vs-canada/before.docx"
    ))
    .expect("read testdata docx");
    let doc = Document::parse(&docx_bytes).expect("parse DOCX bytes");

    // 2. Read the designed projection: block ids, roles, visible text, tracked
    //    status, opaque anchors, and the per-block staleness `guard`. Addressing
    //    and staleness pinning both come from here — never from the raw IR.
    let view = doc.read();
    let target = view
        .blocks
        .iter()
        .find(|b| b.text.contains("[Company Name]"))
        .expect("the SAFE template has a [Company Name] placeholder");
    let block_id = target.id.to_string();
    let guard = target.guard.clone();

    // 3. Author one tracked edit as a typed, schema-validated, precondition-
    //    checked transaction. The `guard` pins the op to the block we just read:
    //    if the block changed since the read, `apply` fails loud (StaleEdit)
    //    instead of editing the wrong text.
    let txn_json = format!(
        r#"{{
            "ops": [
                {{ "op": "replace",
                   "target": "{block_id}",
                   "guard": "{guard}",
                   "content": {{ "type": "paragraph",
                                 "content": [ {{ "type": "text", "text": "Acme Corp." }} ] }} }}
            ],
            "revision": {{ "author": "quickstart" }},
            "summary": "fill in the company name"
        }}"#
    );
    let txn = parse_transaction(&txn_json)
        .expect("transaction JSON is schema-valid")
        .into_edit_transaction()
        .expect("v4 transaction translates to an EditTransaction");
    let edited = doc.apply(&txn).expect("apply the tracked edit");

    // 4. Serialize back to DOCX. Opt into the built-in OOXML linker on the
    //    to-disk path so structurally-corrupt output is refused at the source.
    let out = edited
        .serialize(&ExportOptions {
            mode: ExportMode::Redline,
            validator_level: ValidatorLevel::Blocking,
            validator: None,
        })
        .expect("serialize to DOCX (linker-clean)");

    // 5. Re-parse the emitted bytes and assert the edit is present and tracked.
    //    The accept-all reading must contain the new text; the reject-all
    //    reading (the baseline) must still contain the original placeholder.
    let reparsed = Document::parse(&out).expect("re-parse emitted DOCX");
    let accepted = reparsed.read_accepted().expect("project accept-all");
    let rejected = reparsed.read_rejected().expect("project reject-all");

    assert!(
        accepted.to_text().contains("Acme Corp."),
        "accept-all reading must show the inserted text"
    );
    assert!(
        rejected.to_text().contains("[Company Name]"),
        "reject-all reading must equal the baseline (reject-all == baseline)"
    );

    println!(
        "quickstart OK: parsed {} blocks, applied 1 tracked edit, re-parsed {} bytes; \
         accept-all shows 'Acme Corp.', reject-all preserves the baseline placeholder.",
        view.blocks.len(),
        out.len()
    );
}
