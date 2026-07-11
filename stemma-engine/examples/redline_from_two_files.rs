//! Turn two document versions into a redline by diffing them.
//!
//! Run with `cargo run -p stemma --example redline_from_two_files`.
//!
//! `diff` discovers the deltas between a base and a target and materializes
//! them as tracked changes on the returned document — so the two versions
//! collapse into one reviewable file whose accept-all reading IS the target
//! and whose reject-all reading IS the base. That round-trip identity is the
//! contract a diff must honor: no change is invented and none is lost.

use stemma::api::Document;

fn main() {
    let base_bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/simple-text/before.docx"
    ))
    .expect("read base docx");
    let target_bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/simple-text/after.docx"
    ))
    .expect("read target docx");

    let base = Document::parse(&base_bytes).expect("parse base");
    let target = Document::parse(&target_bytes).expect("parse target");
    let base_text = base.to_text();
    let target_text = target.to_text();
    assert_ne!(
        base_text, target_text,
        "the two versions differ (else there is nothing to redline)"
    );

    // The one call: discover the deltas and materialize them as tracked changes.
    let redline = base.diff(&target).expect("diff base -> target");

    // The redline is now three documents (see walk_the_document). The contract:
    //   reject-all == base   (undo every discovered change → the original)
    //   accept-all == target (take every discovered change → the new version)
    let rejected = redline.read_rejected().expect("reject-all").to_text();
    let accepted = redline.read_accepted().expect("accept-all").to_text();

    println!("base    : {base_text:?}");
    println!("target  : {target_text:?}");
    println!("reject-all == base   : {}", rejected == base_text);
    println!("accept-all == target : {}", accepted == target_text);

    assert_eq!(
        rejected, base_text,
        "reject-all must reconstruct the base exactly"
    );
    assert_eq!(
        accepted, target_text,
        "accept-all must reconstruct the target exactly"
    );

    println!(
        "\nredline_from_two_files OK: diff materialized a redline; accept-all==target, reject-all==base."
    );
}
