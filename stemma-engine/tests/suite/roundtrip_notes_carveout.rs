//! Byte-stability gate for the `serialize/notes.rs` carve-out.
//!
//! Moving `serialize_footnotes_part` / `serialize_endnotes_part` /
//! `serialize_comments_part` / `sync_note_like_part` out of `serialize.rs`
//! must be a pure refactor: the emitted `word/footnotes.xml` and
//! `word/endnotes.xml` parts for a note-bearing redline must be **identical**
//! to the bytes captured before the split.
//!
//! The golden bytes in `testdata/notes_carveout_baseline/` were captured from
//! the `safe-us-vs-singapore` redline export at HEAD before the split. If this
//! test fails, the carve-out changed serialization output — that is a
//! regression, not a test-expectation problem.

use std::fs;
use std::io::{Cursor, Read};

use stemma::{DocxRuntime, ExportMode, SimpleRuntime, TransactionMeta};
use zip::ZipArchive;

/// Reproduce the exact redline export the golden was captured from and assert
/// the note parts are byte-identical.
#[test]
fn notes_parts_byte_identical_after_carveout() {
    let before = fs::read("testdata/safe-us-vs-singapore/before.docx").expect("read before");
    let after = fs::read("testdata/safe-us-vs-singapore/after.docx").expect("read after");

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before).expect("import before");
    let import_after = runtime.import_docx(&after).expect("import after");
    runtime
        .diff_and_redline(
            &import_before.doc_handle,
            &import_after.doc_handle,
            TransactionMeta {
                author: "carveout".to_string(),
                reason: Some("notes carveout baseline".to_string()),
                timestamp_utc: Some("2026-03-26T00:00:00Z".to_string()),
            },
        )
        .expect("diff_and_redline");
    let redline = runtime
        .export_docx(&import_before.doc_handle, ExportMode::Redline)
        .expect("export redline");
    let mut zip = ZipArchive::new(Cursor::new(redline)).expect("open redline zip");

    // Each golden file is named "word__footnotes.xml" etc. (path-flattened).
    for (part, golden_name) in [
        ("word/footnotes.xml", "word__footnotes.xml"),
        ("word/endnotes.xml", "word__endnotes.xml"),
    ] {
        let golden_path = format!("testdata/notes_carveout_baseline/{golden_name}");
        let golden =
            fs::read(&golden_path).unwrap_or_else(|e| panic!("read golden {golden_path}: {e}"));

        let mut file = zip
            .by_name(part)
            .unwrap_or_else(|e| panic!("redline must contain {part}: {e}"));
        let mut emitted = Vec::new();
        file.read_to_end(&mut emitted)
            .unwrap_or_else(|e| panic!("read emitted {part}: {e}"));

        assert_eq!(
            emitted, golden,
            "{part} bytes drifted after the serialize/notes.rs carve-out \
             (golden = pre-split capture in {golden_path})"
        );
    }
}
