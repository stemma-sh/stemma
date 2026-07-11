//! Regression test for comment extended roundtrip (bookmark ID collision).
//!
//! When story parts (comments, footnotes, etc.) contain pre-existing bookmarks,
//! our anchor ID allocator must skip those IDs. Otherwise strip_anchors_streaming
//! can remove a pre-existing bookmarkEnd whose w:id collides with an anchor we
//! inserted, leaving an orphaned bookmarkStart that triggers Word's repair dialog.
//!
//! Known issue: comment-extended-roundtrip.

use stemma::docx::DocxArchive;
use stemma::{DocxRuntime, ExportMode, SimpleRuntime};

use crate::common;

/// Relative paths within the stress directory for comment extended fixtures.
const COMMENT_EX_FIXTURES: &[&str] = &[
    "open-xml-sdk/TestDataStorage__O15Conformance__WD__CommentExTest__Comments-Sample-15-12-01__Comment006.docx",
    "open-xml-sdk/TestDataStorage__O15Conformance__WD__CommentExTest__Comments-Sample-15-12-01__Comment029.docx",
    "open-xml-sdk/TestDataStorage__O15Conformance__WD__CommentExTest__Comments-Sample-15-12-01__Comment033.docx",
    "open-xml-sdk/TestDataStorage__O15Conformance__WD__CommentExTest__Comments-Sample-15-12-01__Comment036.docx",
    "open-xml-sdk/TestDataStorage__O15Conformance__WD__CommentExTest__Comments-Sample-15-12-01__Comment060.docx",
];

/// Extract the (paraId, paraIdParent, done) set from a commentsExtended.xml.
/// Order-insensitive: commentsExtended is a flat set keyed by paraId, so
/// semantic equivalence is set-equality of these triples — NOT byte-identity.
fn extract_comment_ex(xml: &str) -> std::collections::BTreeSet<(String, String, bool)> {
    let attr = |hay: &str, name: &str| -> Option<String> {
        let needle = format!("{name}=\"");
        let i = hay.find(&needle)? + needle.len();
        let j = hay[i..].find('"').map(|k| i + k)?;
        Some(hay[i..j].to_string())
    };
    let mut out = std::collections::BTreeSet::new();
    for chunk in xml.split("<w15:commentEx").skip(1) {
        let el = &chunk[..chunk
            .find("/>")
            .or_else(|| chunk.find('>'))
            .unwrap_or(chunk.len())];
        let Some(para_id) = attr(el, "w15:paraId") else {
            continue;
        };
        let parent = attr(el, "w15:paraIdParent").unwrap_or_default();
        let done = matches!(attr(el, "w15:done").as_deref(), Some("1") | Some("true"));
        out.insert((para_id, parent, done));
    }
    out
}

/// Roundtrip all 5 comment extended fixtures and verify:
/// 1. commentsExtended is preserved with SEMANTIC equivalence (same
///    paraId/paraIdParent/done set) — NOT byte-identity. commentsExtended moved
///    from opaque byte-passthrough to a typed model (`CommentExtended`), so a
///    document we never author into re-serializes the part from the typed model:
///    the bytes may differ (attribute order, namespace prefixes, self-closing
///    form) but the meaning — the set of reply/resolve records — must be
///    identical. This is the deliberate test change per CLAUDE.md ("fix the test
///    to the correct spec behavior, then the code"): byte-identity was never the
///    contract; preserving the reply-threading + resolved state is.
/// 2. commentsExtended relationship survives in document.xml.rels
/// 3. w14:paraId values are preserved in comments.xml
/// 4. Bookmark pairing (bookmarkStart/End) is maintained
#[test]
fn spec_comment_extended_roundtrip_preserves_cross_references() {
    let mut failures = Vec::new();
    let stress_dir = common::stress_dir();

    for rel_path in COMMENT_EX_FIXTURES {
        let path = stress_dir.join(rel_path);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => {
                eprintln!("SKIP {}: fixture not found", path.display());
                continue;
            }
        };

        let runtime = SimpleRuntime::new();
        let import = runtime.import_docx(&bytes).expect("import");
        let exported = runtime
            .export_docx(&import.doc_handle, ExportMode::Redline)
            .expect("export");

        let orig = DocxArchive::read(&bytes).expect("read orig");
        let rt = DocxArchive::read(&exported).expect("read roundtripped");

        // commentsExtended.xml must be SEMANTICALLY equivalent (same
        // paraId/parent/done set) when present — see the doc comment above.
        if orig.get("word/commentsExtended.xml").is_some() {
            let orig_ex = extract_comment_ex(&String::from_utf8_lossy(
                orig.get("word/commentsExtended.xml").unwrap(),
            ));
            let rt_ex = match rt.get("word/commentsExtended.xml") {
                Some(b) => extract_comment_ex(&String::from_utf8_lossy(b)),
                None => Default::default(),
            };
            if orig_ex != rt_ex {
                failures.push(format!(
                    "{}: commentsExtended records changed: {orig_ex:?} vs {rt_ex:?}",
                    path.display()
                ));
            }

            // Relationship entry must survive.
            let orig_rels =
                String::from_utf8_lossy(orig.get("word/_rels/document.xml.rels").unwrap());
            let rt_rels = String::from_utf8_lossy(rt.get("word/_rels/document.xml.rels").unwrap());
            if orig_rels.contains("commentsExtended") && !rt_rels.contains("commentsExtended") {
                failures.push(format!(
                    "{}: commentsExtended relationship lost",
                    path.display()
                ));
            }
        }

        // paraId values must be preserved in order.
        let orig_comments = String::from_utf8_lossy(orig.get("word/comments.xml").unwrap());
        let rt_comments = String::from_utf8_lossy(rt.get("word/comments.xml").unwrap());
        let extract_paraids = |xml: &str| -> Vec<String> {
            xml.match_indices("w14:paraId=\"")
                .map(|(i, _)| {
                    let start = i + 12;
                    let end = xml[start..].find('"').map(|j| start + j).unwrap_or(start);
                    xml[start..end].to_string()
                })
                .collect()
        };
        let orig_ids = extract_paraids(&orig_comments);
        let rt_ids = extract_paraids(&rt_comments);
        if orig_ids != rt_ids {
            failures.push(format!(
                "{}: paraIds changed: {orig_ids:?} vs {rt_ids:?}",
                path.display()
            ));
        }

        // Bookmark pairing: every bookmarkStart must have a matching bookmarkEnd.
        let bm_starts = rt_comments.matches("bookmarkStart").count();
        let bm_ends = rt_comments.matches("bookmarkEnd").count();
        if bm_starts != bm_ends {
            failures.push(format!(
                "{}: orphaned bookmarks: {bm_starts} starts vs {bm_ends} ends",
                path.display()
            ));
        }

        eprintln!("OK {}", path.display());
    }

    assert!(failures.is_empty(), "Failures:\n{}", failures.join("\n"));
}
