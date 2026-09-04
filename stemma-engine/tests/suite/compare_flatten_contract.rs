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
#[allow(unused_imports)]
use stemma_diff::test_support::{DocumentComparisonExt as _, RuntimeComparisonExt as _};

use crate::common;
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

fn make_docx_with_body_and_comment(body_inner: &str, comment_id: &str) -> Vec<u8> {
    let mut archive = DocxArchive::read(&make_docx_with_body(body_inner)).expect("base package");
    let content_types = String::from_utf8(
        archive
            .get("[Content_Types].xml")
            .expect("content types")
            .to_vec(),
    )
    .unwrap()
    .replace(
        "</Types>",
        r#"<Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/></Types>"#,
    );
    archive
        .set("[Content_Types].xml", content_types.into_bytes())
        .unwrap();
    let document_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdComment" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/></Relationships>"#;
    archive
        .set(
            "word/_rels/document.xml.rels",
            document_rels.as_bytes().to_vec(),
        )
        .unwrap();
    archive.upsert(
        "word/comments.xml",
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:comment w:id="{comment_id}" w:author="Reviewer"><w:p><w:r><w:t>Review note.</w:t></w:r></w:p></w:comment></w:comments>"#,
        )
        .into_bytes(),
    );
    archive.write().expect("comment package")
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

fn accepted_body_text(bytes: &[u8]) -> String {
    let runtime = SimpleRuntime::new();
    let imported = runtime.import_docx(bytes).expect("import projection");
    let view = runtime.view(&imported.doc_handle).expect("accepted view");
    common::all_paragraphs(&view.canonical)
        .into_iter()
        .flat_map(|paragraph| paragraph.all_inlines())
        .filter_map(|inline| match inline {
            stemma::InlineNode::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn projected_docx(bytes: &[u8], accept: bool) -> Vec<u8> {
    let archive = DocxArchive::read(bytes).expect("redline archive");
    let (projected, _) = if accept {
        stemma::normalize::normalize_docx(&archive).expect("accept redline")
    } else {
        stemma::normalize::reject_all_docx(&archive).expect("reject redline")
    };
    projected.write().expect("write projection")
}

#[test]
fn compare_tracks_a_target_body_comment_end_after_flattening_shifts_its_anchor() {
    let base_bytes = make_docx_with_body("<w:p><w:r><w:t>Base.</w:t></w:r></w:p>");
    // The accepted view drops the direct-body w:del before the comment end, so
    // the marker's raw body index cannot be used against the normalized tree.
    let target_body = r#"
        <w:del w:id="1" w:author="Prior" w:date="2026-01-01T00:00:00Z"><w:p><w:r><w:delText>Gone.</w:delText></w:r></w:p></w:del>
        <w:p><w:commentRangeStart w:id="7"/><w:r><w:t>Target.</w:t></w:r><w:r><w:commentReference w:id="7"/></w:r></w:p>
        <w:commentRangeEnd w:id="7"/>
    "#;
    let target_bytes = make_docx_with_body_and_comment(target_body, "7");
    let runtime = SimpleRuntime::new();
    let base = runtime.import_docx(&base_bytes).expect("base");
    let target = runtime.import_docx(&target_bytes).expect("target");
    assert_eq!(target.canonical.comments.len(), 1, "target comment story");

    let result = runtime
        .compare_and_redline(&base.doc_handle, &target.doc_handle, meta())
        .expect("the raw target proof anchor is resolved before relocation");
    let xml = {
        let archive = DocxArchive::read(&result.redline_bytes).expect("redline archive");
        assert!(
            archive.get("word/comments.xml").is_some(),
            "the target comment story must enter the output package"
        );
        String::from_utf8(archive.get("word/document.xml").unwrap().to_vec()).unwrap()
    };
    let end = xml.find("commentRangeEnd").expect("comment end emitted");
    let preceding = &xml[end.saturating_sub(200)..end];
    assert!(
        preceding.contains("<w:ins"),
        "the target-only body marker must be a real pending insertion"
    );

    let accepted = projected_docx(&result.redline_bytes, true);
    let rejected = projected_docx(&result.redline_bytes, false);
    assert_eq!(
        accepted_body_text(&accepted),
        accepted_body_text(&target_bytes)
    );
    assert_eq!(
        accepted_body_text(&rejected),
        accepted_body_text(&base_bytes)
    );
}

#[test]
fn compare_relocates_a_target_body_bookmark_as_one_terminal_specific_pair() {
    let base_bytes = make_docx_with_body("<w:p><w:r><w:t>Base.</w:t></w:r></w:p>");
    let target_bytes = make_docx_with_body(
        r#"<w:p><w:r><w:t>Target.</w:t></w:r></w:p><w:bookmarkStart w:id="4" w:name="whole"/><w:bookmarkEnd w:id="4"/>"#,
    );
    let runtime = SimpleRuntime::new();
    let base = runtime.import_docx(&base_bytes).expect("base");
    let target = runtime.import_docx(&target_bytes).expect("target");
    let result = runtime
        .compare_and_redline(&base.doc_handle, &target.doc_handle, meta())
        .expect("target body bookmark is structurally relocatable");

    for accept in [true, false] {
        let projected = projected_docx(&result.redline_bytes, accept);
        let archive = DocxArchive::read(&projected).expect("projection archive");
        let xml = String::from_utf8(archive.get("word/document.xml").unwrap().to_vec()).unwrap();
        let expected = usize::from(accept);
        assert_eq!(xml.matches("bookmarkStart").count(), expected);
        assert_eq!(xml.matches("bookmarkEnd").count(), expected);
    }
    assert_eq!(
        accepted_body_text(&projected_docx(&result.redline_bytes, true)),
        accepted_body_text(&target_bytes)
    );
    assert_eq!(
        accepted_body_text(&projected_docx(&result.redline_bytes, false)),
        accepted_body_text(&base_bytes)
    );
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
