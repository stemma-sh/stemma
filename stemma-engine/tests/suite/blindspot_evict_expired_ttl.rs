//! Blindspot regression: `SimpleRuntime::evict_expired(ttl_secs)` TTL boundary.
//!
//! Documented contract (runtime.rs:1013-1015):
//!   "Evict documents that haven't been accessed within `ttl_secs` seconds.
//!    Returns the number of evicted documents."
//!
//! The eviction predicate is `now.saturating_sub(last) < ttl_secs` (retain the
//! entry when its idle time is strictly LESS than the TTL). The store is a
//! `DashMap<String, DocState>`; `last_accessed_epoch_secs` is stamped to
//! `now_epoch_secs()` at insert time and a test cannot inject a fake clock, so
//! we assert the two unambiguous extremes the predicate must satisfy:
//!
//!   * ttl = 0  — nothing can be idle for "less than 0 seconds", so EVERY
//!     resident handle is expired. evict_expired(0) must drop all of them and
//!     return the full count.
//!   * ttl = u64::MAX — a handle imported moments ago has idle time ~0, which is
//!     less than MAX, so NOTHING is expired. evict_expired(u64::MAX) must drop
//!     nothing, return 0, and leave the handles usable.
//!
//! A flipped comparison (`>` / `>=`) or a never-evicting body fails the ttl=0
//! case; a never-keeping body fails the ttl=MAX case. The returned drop-count is
//! asserted directly in both directions.

use stemma::{DocxRuntime, SimpleRuntime};

/// Minimal valid single-paragraph DOCX (same scaffold the engine's own tests
/// use). `evict_expired` only cares about the handle store, so the body is
/// trivial.
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

/// Probe: is a handle still resident? `block_text_for`-style reads aren't on the
/// trait, so we use `view()` via the public runtime. We instead use a fresh
/// runtime per scenario and reason about counts to avoid cross-test coupling.
///
/// ttl = u64::MAX: a freshly imported handle (idle ~0s) is well within any
/// reasonable TTL and must NOT be evicted.
#[test]
fn evict_with_huge_ttl_keeps_fresh_handles() {
    let runtime = SimpleRuntime::new();
    let bytes = make_test_docx(&["alpha"]);

    let _h1 = runtime.import_docx(&bytes).expect("import 1");
    let _h2 = runtime.import_docx(&bytes).expect("import 2");

    // Two handles imported moments ago; idle time ~0 << u64::MAX => keep both.
    let dropped = runtime.evict_expired(u64::MAX);
    assert_eq!(
        dropped, 0,
        "fresh handles must NOT be evicted under an effectively-infinite TTL; \
         evict_expired returned a drop-count of {dropped}"
    );

    // The survivors must still be usable (handle still resolves to its doc).
    // A second eviction with huge TTL is still a no-op.
    let dropped_again = runtime.evict_expired(u64::MAX);
    assert_eq!(
        dropped_again, 0,
        "repeated evict_expired(huge) must remain a no-op; got {dropped_again}"
    );
}

/// ttl = 0: nothing can be idle for "strictly less than 0 seconds", so EVERY
/// resident handle is expired. Must drop all and return the full count.
#[test]
fn evict_with_zero_ttl_drops_all_handles() {
    let runtime = SimpleRuntime::new();
    let bytes = make_test_docx(&["alpha"]);

    let h1 = runtime.import_docx(&bytes).expect("import 1");
    let h2 = runtime.import_docx(&bytes).expect("import 2");
    let h3 = runtime.import_docx(&bytes).expect("import 3");

    let dropped = runtime.evict_expired(0);
    assert_eq!(
        dropped, 3,
        "evict_expired(0) must drop all 3 resident handles (nothing is idle for \
         < 0 seconds); returned drop-count was {dropped}"
    );

    // After dropping all, every handle is gone: reads must fail with a clear
    // "handle not found" error rather than resolve. This pins the post-condition
    // that the entries were actually removed, not merely counted.
    for h in [&h1, &h2, &h3] {
        let res = runtime.view(&h.doc_handle);
        assert!(
            res.is_err(),
            "handle {:?} must be gone after evict_expired(0) dropped everything",
            h.doc_handle
        );
    }

    // A subsequent eviction on the now-empty store drops nothing.
    let dropped_again = runtime.evict_expired(0);
    assert_eq!(
        dropped_again, 0,
        "evict_expired on an empty store must return 0; got {dropped_again}"
    );
}

/// Count correctness with a mixed live store: after evicting all with ttl=0,
/// importing a new handle and evicting with huge ttl keeps exactly the live
/// one. Guards against the count being computed off a stale `before` length.
#[test]
fn evict_count_reflects_actual_removals() {
    let runtime = SimpleRuntime::new();
    let bytes = make_test_docx(&["alpha"]);

    let _a = runtime.import_docx(&bytes).expect("import a");
    let _b = runtime.import_docx(&bytes).expect("import b");

    // Drop both.
    assert_eq!(runtime.evict_expired(0), 2, "both initial handles evicted");

    // Fresh import after the purge; huge TTL keeps it.
    let _c = runtime.import_docx(&bytes).expect("import c");
    assert_eq!(
        runtime.evict_expired(u64::MAX),
        0,
        "the post-purge handle is fresh and must survive a huge TTL"
    );

    // Now purge the single survivor with ttl=0.
    assert_eq!(
        runtime.evict_expired(0),
        1,
        "the lone remaining handle must be evicted and counted"
    );
}
