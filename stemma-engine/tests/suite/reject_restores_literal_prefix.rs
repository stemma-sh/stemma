//! Rejecting a tracked deletion of a typed-in enumeration label ("1.\t")
//! restores it — and that restored label must be readable.
//!
//! A representative document has headings whose old hand-typed
//! numbers are tracked-DELETED runs, e.g.
//!
//! ```xml
//! <w:p><w:pPr><w:pStyle w:val="ListParagraph"/>...</w:pPr>
//!   <w:del w:id="25" w:author="Stemma"><w:r><w:delText>1.</w:delText></w:r></w:del>
//!   <w:del w:id="26" w:author="Stemma"><w:r><w:tab/></w:r></w:del>
//!   <w:r><w:t>Events</w:t></w:r>
//! </w:p>
//! ```
//!
//! An agent that rejects those deletions, then reads the document back, sees
//! `"Events"` with NO `"1."` — so it concludes the reject had failed and
//! manually re-types `"1.\t"`. Replaying
//! deterministically shows the reject was CORRECT: the saved document re-emits a
//! visible `"1.\t"` run. The defect is that the engine's READ projection hides
//! the restored label, because `strip_literal_prefix` (run post-projection by
//! `normalize_paragraph_after_projection`) hoists the restored `"1.\t"` into the
//! untracked `ParagraphNode::literal_prefix` field — and no read surface
//! (`build_document_view` text/segments, hence `read_block`/`read_redline`)
//! exposes `literal_prefix`. So the model and the saved bytes are right; the
//! agent-facing read lies, and that mismatch is what drives the redundant manual
//! repair (which would have produced a DOUBLED prefix on save).
//!
//! Two tests below:
//! - `reject_of_tracked_prefix_deletion_emits_visible_prefix_run` (daily): the
//!   correctness oracle — reject + serialize emits visible `<w:t>1.</w:t>`,
//!   never `<w:delText>`. This is what Word reads and what proves the reject
//!   itself is sound.
//! - `read_view_surfaces_rejected_literal_prefix` (ignored): the desired read
//!   behavior the engine does NOT yet provide — the agent-facing read must show
//!   the restored `"1."`. Un-ignore when the read-projection gap is fixed.

use std::collections::HashSet;
use std::io::{Cursor, Read, Write};

use stemma::view::build_document_view_from_canon;
use stemma::{DocxRuntime, ExportMode, ResolveSelectionAction, SimpleRuntime};
use zip::write::FileOptions;
use zip::{ZipArchive, ZipWriter};

const CONTENT_TYPES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

const RELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

const WORD_RELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
</Relationships>"#;

fn build_docx(document_xml: &str) -> Vec<u8> {
    let cursor = Cursor::new(Vec::new());
    let mut zip = ZipWriter::new(cursor);
    let options = FileOptions::default();
    zip.start_file("[Content_Types].xml", options).unwrap();
    zip.write_all(CONTENT_TYPES_XML.as_bytes()).unwrap();
    zip.start_file("_rels/.rels", options).unwrap();
    zip.write_all(RELS_XML.as_bytes()).unwrap();
    zip.start_file("word/_rels/document.xml.rels", options)
        .unwrap();
    zip.write_all(WORD_RELS_XML.as_bytes()).unwrap();
    zip.start_file("word/document.xml", options).unwrap();
    zip.write_all(document_xml.as_bytes()).unwrap();
    zip.finish().unwrap().into_inner()
}

/// One heading paragraph in this shape: a `ListParagraph`
/// whose old typed-in "1." + tab are tracked DELETIONS (ids 1 and 2), followed
/// by the live, untracked title text "Events".
fn heading_with_tracked_prefix_deletion() -> Vec<u8> {
    let body = r#"
    <w:p>
      <w:pPr><w:pStyle w:val="ListParagraph"/></w:pPr>
      <w:del w:id="1" w:author="Stemma" w:date="2026-01-26T09:01:46Z">
        <w:r><w:rPr><w:b/></w:rPr><w:delText>1.</w:delText></w:r>
      </w:del>
      <w:del w:id="2" w:author="Stemma" w:date="2026-01-26T09:01:46Z">
        <w:r><w:rPr><w:b/></w:rPr><w:tab/></w:r>
      </w:del>
      <w:r><w:rPr><w:b/><w:i/></w:rPr><w:t>Events</w:t></w:r>
    </w:p>"#;
    let doc = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
{body}
    <w:sectPr/>
  </w:body>
</w:document>"#
    );
    build_docx(&doc)
}

/// Read `word/document.xml` from a DOCX byte blob.
fn read_document_xml(docx: &[u8]) -> String {
    let mut zip = ZipArchive::new(Cursor::new(docx)).expect("zip");
    let mut f = zip.by_name("word/document.xml").expect("document.xml");
    let mut s = String::new();
    f.read_to_string(&mut s).unwrap();
    s
}

/// Reject revisions 1 and 2 (the "1." and tab deletions) on the imported doc,
/// returning the runtime + handle for further inspection.
fn import_and_reject_prefix(docx: &[u8]) -> (SimpleRuntime, stemma::DocHandle) {
    let rt = SimpleRuntime::new();
    let import = rt.import_docx(docx).expect("import");
    let handle = import.doc_handle.clone();
    let ids: HashSet<u32> = [1, 2].into_iter().collect();
    rt.resolve_tracked_revisions(&handle, &ids, ResolveSelectionAction::Reject)
        .expect("reject ids 1,2");
    (rt, handle)
}

/// CORRECTNESS ORACLE (daily): rejecting the tracked deletion of a typed "1.\t"
/// must restore it as VISIBLE text in the serialized document — what Word reads.
/// A `<w:delText>` survivor would mean the reject silently kept the deletion;
/// the absence of any "1." run would mean the reject dropped the restored text.
/// This is the post-condition that proves the reject resolution itself is sound.
#[test]
fn reject_of_tracked_prefix_deletion_emits_visible_prefix_run() {
    let docx = heading_with_tracked_prefix_deletion();
    let (rt, handle) = import_and_reject_prefix(&docx);
    let out = rt
        .export_docx(&handle, ExportMode::Redline)
        .expect("export after reject");
    let xml = read_document_xml(&out);

    // The restored "1." is visible body text, not a tracked deletion.
    assert!(
        xml.contains("<w:t>1.</w:t>") || xml.contains(">1.</w:t>"),
        "rejecting the deletion must restore \"1.\" as a visible <w:t> run; document.xml was:\n{xml}"
    );
    assert!(
        !xml.contains("<w:delText>1.</w:delText>") && !xml.contains(">1.</w:delText>"),
        "the \"1.\" deletion must be gone after reject, not preserved as <w:delText>:\n{xml}"
    );
    // The title text is intact and the tab separator came back too.
    assert!(
        xml.contains(">Events</w:t>"),
        "title text must survive:\n{xml}"
    );
    assert!(
        xml.contains("<w:tab"),
        "the tab separator must be restored:\n{xml}"
    );
}

/// DESIRED READ BEHAVIOR (not yet implemented): the agent-facing read view must
/// show the restored enumeration label. Today `build_document_view` omits
/// `ParagraphNode::literal_prefix` (where the restored "1.\t" lands), so a cold
/// agent reading over MCP sees only "Events" and cannot tell the reject worked.
/// That blind spot is what drives the redundant manual re-typing.
///
/// Un-ignore once the read projection surfaces `literal_prefix` (mirroring the
/// frontend's CSS `[data-numbering-text]::before`, which already shows it to a
/// human). The serializer already proves the document itself is correct
/// (see the daily test above), so the desired read string is unambiguous —
/// no Word oracle is needed to know the read should read "1.".
#[test]
fn read_view_surfaces_rejected_literal_prefix() {
    let docx = heading_with_tracked_prefix_deletion();
    let (rt, handle) = import_and_reject_prefix(&docx);
    let vr = rt.view(&handle).expect("view after reject");
    let view = build_document_view_from_canon(&vr.canonical);
    let heading = view
        .blocks
        .iter()
        .find(|b| b.text.contains("Events"))
        .expect("heading block present");
    assert!(
        heading.text.starts_with("1."),
        "the read view must show the restored enumeration label; got text = {:?}",
        heading.text
    );
}
