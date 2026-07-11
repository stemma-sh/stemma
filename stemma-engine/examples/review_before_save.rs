//! Review before you save: prove what you changed — and what you didn't —
//! before bytes leave.
//!
//! Run with `cargo run -p stemma --example review_before_save`.
//!
//! The editing chapter's discipline: "looked right in my live view" is the
//! failure mode that survives everything else. `review()` is the read-back, as
//! one call — the tracked-change census this session authored, the untracked
//! (direct) delta, a proof that every OTHER block is untouched, and the
//! validator verdict on the would-be save bytes. It REPORTS; it never gates.

use stemma::ExportOptions;
use stemma::api::Document;
use stemma::edit_v4::parse_transaction;

fn main() {
    let docx = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/paragraphs/before.docx"
    ))
    .expect("read testdata docx");
    let doc = Document::parse(&docx).expect("parse DOCX bytes");
    let total_blocks = doc.read().blocks.len();

    // Make two tracked edits, on two of the three paragraphs.
    let doc = replace_tracked(
        &doc,
        "very interesting test",
        "a tightened clause.",
        "J. Osei",
    );
    let doc = replace_tracked(
        &doc,
        "much longer sequence",
        "a shorter sequence.",
        "J. Osei",
    );

    // Review against the parse-time baseline — the census, the direct delta,
    // the untouched proof, and the package verdict, all engine-derived.
    let report = doc.review().expect("review the session");

    println!("session review:");
    println!(
        "  tracked-change census : {} revision(s)",
        report.new_revisions.len()
    );
    for rev in &report.new_revisions {
        println!(
            "    - revision {} ({}) on {}: {:?}",
            rev.revision_id, rev.kind, rev.block_id, rev.excerpt
        );
    }
    println!(
        "  untracked (direct) delta: {} change(s)",
        report.direct_changes.len()
    );
    println!(
        "  untouched proof         : {} block(s) verified across {:?}, {} violation(s)",
        report.untouched.verified_blocks,
        report.untouched.parts,
        report.untouched.violations.len()
    );
    println!(
        "  package verdict         : {}",
        if report.validator.ok {
            "valid"
        } else {
            "INVALID"
        }
    );

    // The invariants that make review trustworthy as a pre-save gate of your
    // own making:
    assert!(
        !report.new_revisions.is_empty(),
        "we authored tracked changes"
    );
    assert!(
        report.direct_changes.is_empty(),
        "every edit was tracked — an untracked delta here would itself be a finding"
    );
    assert!(
        report.untouched.violations.is_empty(),
        "everything outside the two edited paragraphs is provably untouched"
    );
    assert!(
        report.untouched.verified_blocks >= total_blocks - 2,
        "the third paragraph is proven untouched"
    );
    assert!(
        report.validator.ok,
        "the would-be save bytes are structurally valid"
    );

    // Only now — having read back what the recipient will actually receive —
    // commit to bytes. `serialize` runs the blocking validator itself; review
    // reported validity, save is the one place bytes are refused.
    let out = doc
        .serialize(&ExportOptions::default())
        .expect("serialize validated DOCX");

    println!(
        "\nreview_before_save OK: reviewed, proven, then saved {} bytes.",
        out.len()
    );
}

/// Author one tracked whole-paragraph replacement as `author`, via the v4 wire
/// path. (Shown fully inline in `my_first_edit`.)
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
