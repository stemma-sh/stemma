//! THE COMPARE CONTRACT (flatten), pinned as behavior.
//!
//! Compare diffs the ACCEPTED READINGS of its inputs: `view()` runs
//! accept-all before the diff, so pending revisions in base or target —
//! plain Inserted/Deleted and the stacked state alike — are projected to
//! their accepted image, and the output redline re-attributes every change
//! to the compare's own author. This matches Word's own Compare (which
//! compares as-if-accepted when inputs carry revisions). The flattening is
//! disclosed via `FlattenedPendingRevisions` on the compare results.
//!
//! HISTORY: a "compare refuses stacked inputs" guard once existed briefly —
//! and never fired once, because it ran on the post-accept
//! canonicals where the stacked state cannot exist. The institutional memory
//! said "refuses" while the behavior was "flattens". These tests are the
//! discipline that closes that class: the contract each path claims is the
//! contract a fixture exercises — including the one refusal compare still
//! has (quarantined blocks), which must demonstrably FIRE.

use std::io::Write as _;

use stemma::docx::DocxArchive;
use stemma::{DocxRuntime, ErrorCode, SimpleRuntime, TransactionMeta};
use zip::write::FileOptions;

fn make_docx_with_body(body_inner: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
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

fn meta() -> TransactionMeta {
    TransactionMeta {
        author: "Compare".to_string(),
        reason: None,
        timestamp_utc: Some("2026-06-10T00:00:00Z".to_string()),
    }
}

#[test]
fn compare_flattens_pending_revisions_and_reattributes() {
    // Base carries every pending-revision shape: a plain insertion
    // (AuthorA), a plain deletion (AuthorB), and a stacked span (inserted by
    // AuthorA, deleted by AuthorB). Accepted reading: "Alpha added omega."
    let base_body = r#"<w:p><w:r><w:t xml:space="preserve">Alpha </w:t></w:r><w:ins w:id="10" w:author="AuthorA" w:date="2026-01-01T00:00:00Z"><w:r><w:t xml:space="preserve">added </w:t></w:r></w:ins><w:del w:id="11" w:author="AuthorB" w:date="2026-02-01T00:00:00Z"><w:r><w:delText xml:space="preserve">removed </w:delText></w:r></w:del><w:ins w:id="1" w:author="AuthorA" w:date="2026-01-01T00:00:00Z"><w:del w:id="2" w:author="AuthorB" w:date="2026-02-01T00:00:00Z"><w:r><w:delText xml:space="preserve">contested </w:delText></w:r></w:del></w:ins><w:r><w:t>omega.</w:t></w:r></w:p>"#;
    // Target carries its own pending insertion (AuthorC). Accepted reading:
    // "Alpha brand new omega."
    let target_body = r#"<w:p><w:r><w:t xml:space="preserve">Alpha </w:t></w:r><w:ins w:id="20" w:author="AuthorC" w:date="2026-03-01T00:00:00Z"><w:r><w:t xml:space="preserve">brand new </w:t></w:r></w:ins><w:r><w:t>omega.</w:t></w:r></w:p>"#;

    let runtime = SimpleRuntime::new();
    let base = runtime
        .import_docx(&make_docx_with_body(base_body))
        .unwrap();
    let target = runtime
        .import_docx(&make_docx_with_body(target_body))
        .unwrap();

    let result = runtime
        .compare_and_redline(&base.doc_handle, &target.doc_handle, meta())
        .expect("compare succeeds on inputs carrying pending revisions — the contract is flatten, not refuse");

    // The output reflects the ACCEPTED readings: text that left the accepted
    // base reading (the plain deletion, the stacked span) does not exist in
    // the redline in any form.
    let xml = {
        let archive = DocxArchive::read(&result.redline_bytes).unwrap();
        String::from_utf8(archive.get("word/document.xml").unwrap().to_vec()).unwrap()
    };
    assert!(
        !xml.contains("removed"),
        "pending-deleted base text is not part of the accepted reading"
    );
    assert!(
        !xml.contains("contested"),
        "stacked base text is not part of the accepted reading (origin rule 3)"
    );
    assert!(
        xml.contains("brand new"),
        "the accepted target reading is what the redline proposes"
    );

    // Attribution is re-stamped: every revision in the output belongs to the
    // compare's author; the inputs' negotiation record is gone from markup.
    assert!(xml.contains("Compare"), "compare author stamps the output");
    for original in ["AuthorA", "AuthorB", "AuthorC"] {
        assert!(
            !xml.contains(original),
            "{original} must not survive into the output markup — compare re-attributes"
        );
    }

    // ...and DISCLOSED: the result names what was flattened, per input.
    let notice = &result.flattened_pending_revisions;
    let base_summary: Vec<(Option<&str>, u32)> = notice
        .base
        .iter()
        .map(|a| (a.author.as_deref(), a.revision_count))
        .collect();
    assert_eq!(
        base_summary,
        vec![(Some("AuthorA"), 2), (Some("AuthorB"), 2)],
        "base: AuthorA = plain ins + stacked ins, AuthorB = plain del + stacked del"
    );
    let target_summary: Vec<(Option<&str>, u32)> = notice
        .target
        .iter()
        .map(|a| (a.author.as_deref(), a.revision_count))
        .collect();
    assert_eq!(target_summary, vec![(Some("AuthorC"), 1)]);
}

#[test]
fn compare_without_pending_revisions_discloses_nothing() {
    let base_body = r#"<w:p><w:r><w:t>Plain base.</w:t></w:r></w:p>"#;
    let target_body = r#"<w:p><w:r><w:t>Plain target.</w:t></w:r></w:p>"#;

    let runtime = SimpleRuntime::new();
    let base = runtime
        .import_docx(&make_docx_with_body(base_body))
        .unwrap();
    let target = runtime
        .import_docx(&make_docx_with_body(target_body))
        .unwrap();

    let result = runtime
        .compare_and_redline(&base.doc_handle, &target.doc_handle, meta())
        .expect("compare");
    assert!(result.flattened_pending_revisions.base.is_empty());
    assert!(result.flattened_pending_revisions.target.is_empty());
}

#[test]
fn compare_refuses_quarantined_input_and_the_refusal_fires() {
    // A move-mix nesting (w:moveFrom inside w:ins) is an unsupported nested
    // shape: import quarantines the body item byte-faithfully
    // (OpaqueKind::QuarantinedNestedTracking). Its placeholder has no
    // readable content, so compare must REFUSE — and this fixture proves the
    // refusal actually fires (a guard nobody can trip is worse than none:
    // the stacked-state arm of this same guard sat dead for its entire
    // lifetime because no fixture exercised it).
    let quarantined_body = r#"<w:p><w:ins w:id="1" w:author="AuthorA" w:date="2026-01-01T00:00:00Z"><w:moveFrom w:id="2" w:author="AuthorB" w:date="2026-02-01T00:00:00Z"><w:r><w:t>tangled</w:t></w:r></w:moveFrom></w:ins></w:p><w:p><w:r><w:t>Plain tail.</w:t></w:r></w:p>"#;
    let target_body = r#"<w:p><w:r><w:t>Plain target.</w:t></w:r></w:p>"#;

    let runtime = SimpleRuntime::new();
    let base = runtime
        .import_docx(&make_docx_with_body(quarantined_body))
        .expect("unsupported nesting quarantines at import rather than refusing");
    let target = runtime
        .import_docx(&make_docx_with_body(target_body))
        .unwrap();

    let err = match runtime.compare_and_redline(&base.doc_handle, &target.doc_handle, meta()) {
        Ok(_) => panic!("quarantined input must refuse compare"),
        Err(e) => e,
    };
    assert_eq!(err.code, ErrorCode::UnsupportedEdit);
    assert!(
        err.message.contains("quarantined"),
        "refusal names the cause: {}",
        err.message
    );
}
