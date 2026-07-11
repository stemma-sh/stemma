//! Spec compliance for the reject/restore path — two classes of bug found in
//! wild Word-authored documents:
//!
//! A. Rejecting a tracked-DELETED field must restore its `w:delInstrText` field
//!    code to `w:instrText` (the deleted↔plain run-content pairs), the same way
//!    `w:delText` restores to `w:t`. A bare `w:delInstrText` in a plain run is
//!    schema-invalid (§17.16.13) and Word repairs the file on open.
//!
//! B. A bookmark / comment / permission range whose two halves straddle a
//!    tracked-change boundary (start OUTSIDE a `w:ins`, end INSIDE it — the wild
//!    shape) must stay PAIRED after resolution. Reject removes the content under
//!    the inside half; dropping it while the outside half survives tears the
//!    pair (§17.13.6). The range collapses to a point at the survivor (Word's
//!    behavior when a bookmarked range's interior is deleted) instead.
//!
//! Both the archive path (`normalize::reject_all_docx`) and the runtime IR path
//! (`Document::project(Resolution::RejectAll)` + export) are covered.
//!
//! Daily tier: synthesized in-memory DOCX, no corpus, no real-Word oracle.

use std::io::Write;

use stemma::api::{Document, validate};
use stemma::docx::{DocxArchive, DocxFile};
use stemma::{ExportOptions, Resolution};

fn docx_with_body(body_inner: &str) -> Vec<u8> {
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
        use zip::write::FileOptions;
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

fn archive_with_body(body_inner: &str) -> DocxArchive {
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
    );
    DocxArchive::from_parts(vec![
        DocxFile {
            name: "[Content_Types].xml".to_string(),
            data: content_types.as_bytes().to_vec(),
        },
        DocxFile {
            name: "_rels/.rels".to_string(),
            data: root_rels.as_bytes().to_vec(),
        },
        DocxFile {
            name: "word/document.xml".to_string(),
            data: doc.into_bytes(),
        },
    ])
}

fn runtime_reject(body_inner: &str) -> Vec<u8> {
    let doc = Document::parse(&docx_with_body(body_inner)).expect("parse");
    let resolved = doc.project(Resolution::RejectAll).expect("reject");
    resolved
        .serialize(&ExportOptions::default())
        .expect("serialize")
}

fn runtime_accept(body_inner: &str) -> Vec<u8> {
    let doc = Document::parse(&docx_with_body(body_inner)).expect("parse");
    let resolved = doc.project(Resolution::AcceptAll).expect("accept");
    resolved
        .serialize(&ExportOptions::default())
        .expect("serialize")
}

/// Mirror of the conformance harness: resolve EVERY enumerated revision with
/// the given action via the SELECTIVE path (`Resolution::Selective`), which is
/// a different projection from accept-all/reject-all and must collapse torn
/// range pairs the same way.
fn selective_resolve_all(body_inner: &str, action: stemma::ResolveSelectionAction) -> Vec<u8> {
    use std::collections::HashSet;
    let doc = Document::parse(&docx_with_body(body_inner)).expect("parse");
    let ids: HashSet<u32> = stemma::enumerate_revisions(&doc.snapshot().canonical)
        .into_iter()
        .map(|r| r.revision_id)
        .collect();
    let resolved = doc
        .project(Resolution::Selective { ids, action })
        .expect("selective resolve");
    resolved
        .serialize(&ExportOptions::default())
        .expect("serialize")
}

fn archive_reject_xml(body_inner: &str) -> String {
    let (out, _) =
        stemma::normalize::reject_all_docx(&archive_with_body(body_inner)).expect("reject");
    String::from_utf8(out.get("word/document.xml").unwrap().to_vec()).unwrap()
}

fn document_xml_of(bytes: &[u8]) -> String {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut f = zip.by_name("word/document.xml").unwrap();
    let mut s = String::new();
    use std::io::Read;
    f.read_to_string(&mut s).unwrap();
    s
}

// ── Bug A: deleted field code restores on reject ─────────────────────────────

const DELETED_FIELD_BODY: &str = r#"<w:p>
  <w:r><w:t xml:space="preserve">Before </w:t></w:r>
  <w:del w:id="5" w:author="A" w:date="2024-01-01T00:00:00Z">
    <w:r><w:fldChar w:fldCharType="begin"/></w:r>
    <w:r><w:delInstrText xml:space="preserve"> HYPERLINK "http://example.com" </w:delInstrText></w:r>
    <w:r><w:fldChar w:fldCharType="separate"/></w:r>
    <w:r><w:delText>link</w:delText></w:r>
    <w:r><w:fldChar w:fldCharType="end"/></w:r>
  </w:del>
  <w:r><w:t xml:space="preserve"> after</w:t></w:r>
</w:p>"#;

#[test]
fn bug_a_runtime_reject_restores_field_code_to_instr_text() {
    let bytes = runtime_reject(DELETED_FIELD_BODY);
    let xml = document_xml_of(&bytes);
    // delInstrText restored to instrText; delText restored to t.
    assert!(
        !xml.contains("delInstrText"),
        "delInstrText must restore to instrText: {xml}"
    );
    assert!(!xml.contains("delText"), "delText must restore to t: {xml}");
    assert!(
        xml.contains("<w:instrText"),
        "the field code must survive as instrText: {xml}"
    );
    assert!(
        xml.contains(">link<"),
        "the field result text must be restored: {xml}"
    );
    // Whole field intact and the doc validates.
    Document::parse(&bytes).expect("rejected doc re-parses");
    assert!(validate(&bytes).ok, "rejected doc must validate clean");
}

#[test]
fn bug_a_archive_reject_restores_field_code_to_instr_text() {
    let xml = archive_reject_xml(DELETED_FIELD_BODY);
    assert!(
        !xml.contains("delInstrText"),
        "archive reject must restore instrText: {xml}"
    );
    assert!(
        !xml.contains("delText"),
        "archive reject must restore t: {xml}"
    );
    assert!(
        xml.contains("<w:instrText"),
        "field code survives as instrText: {xml}"
    );
}

#[test]
fn bug_a_accept_removes_the_deleted_field_entirely() {
    let bytes = runtime_accept(DELETED_FIELD_BODY);
    let xml = document_xml_of(&bytes);
    // Accepting a deletion confirms removal: no field code or result remains.
    assert!(
        !xml.contains("instrText"),
        "accepted deletion drops the field code: {xml}"
    );
    assert!(
        !xml.contains("fldChar"),
        "accepted deletion drops the field chars: {xml}"
    );
    assert!(
        !xml.contains(">link<"),
        "accepted deletion drops the field result: {xml}"
    );
    assert!(
        xml.contains("Before") && xml.contains("after"),
        "surrounding text survives: {xml}"
    );
    assert!(validate(&bytes).ok, "accepted doc must validate clean");
}

// ── Bug B: torn range pair collapses to a point on resolution ────────────────

// Wild shape: bookmarkStart is a paragraph child OUTSIDE the insertion, the
// paired bookmarkEnd sits INSIDE it.
const TORN_BOOKMARK_START_OUTSIDE: &str = r#"<w:p>
  <w:r><w:t xml:space="preserve">Alpha </w:t></w:r>
  <w:bookmarkStart w:id="211" w:name="_Ref_Clause"/>
  <w:ins w:id="7" w:author="A" w:date="2024-01-01T00:00:00Z">
    <w:r><w:t>inserted text</w:t></w:r>
    <w:bookmarkEnd w:id="211"/>
  </w:ins>
  <w:r><w:t xml:space="preserve"> Omega</w:t></w:r>
</w:p>"#;

// Symmetric shape: bookmarkStart INSIDE the insertion, bookmarkEnd OUTSIDE.
const TORN_BOOKMARK_END_OUTSIDE: &str = r#"<w:p>
  <w:r><w:t xml:space="preserve">Alpha </w:t></w:r>
  <w:ins w:id="8" w:author="A" w:date="2024-01-01T00:00:00Z">
    <w:bookmarkStart w:id="212" w:name="_Sym"/>
    <w:r><w:t>inserted text</w:t></w:r>
  </w:ins>
  <w:bookmarkEnd w:id="212"/>
  <w:r><w:t xml:space="preserve"> Omega</w:t></w:r>
</w:p>"#;

// Both halves INSIDE the insertion — rejecting removes the whole range cleanly.
const BOOKMARK_BOTH_INSIDE: &str = r#"<w:p>
  <w:r><w:t xml:space="preserve">Alpha </w:t></w:r>
  <w:ins w:id="9" w:author="A" w:date="2024-01-01T00:00:00Z">
    <w:bookmarkStart w:id="213" w:name="_Both"/>
    <w:r><w:t>inserted text</w:t></w:r>
    <w:bookmarkEnd w:id="213"/>
  </w:ins>
  <w:r><w:t xml:space="preserve"> Omega</w:t></w:r>
</w:p>"#;

fn assert_bookmark_paired_and_collapsed(xml: &str, id: &str) {
    // Both halves carry the pairing id (the runtime path may emit a redundant
    // xmlns:w on a re-inserted decoration, so match by tag + id, not adjacency).
    let starts = xml.matches("<w:bookmarkStart").count();
    let ends = xml.matches("<w:bookmarkEnd").count();
    let id_occurrences = xml.matches(&format!(r#"w:id="{id}""#)).count();
    assert_eq!(starts, 1, "exactly one surviving bookmarkStart: {xml}");
    assert_eq!(
        ends, 1,
        "exactly one surviving bookmarkEnd (re-paired): {xml}"
    );
    assert_eq!(
        id_occurrences, 2,
        "both halves keep the pairing id {id}: {xml}"
    );
    assert!(
        !xml.contains("inserted text"),
        "the rejected insertion content is gone: {xml}"
    );
    assert!(
        xml.contains("Alpha") && xml.contains("Omega"),
        "surrounding base text survives: {xml}"
    );
}

#[test]
fn bug_b_runtime_reject_collapses_torn_bookmark_start_outside() {
    let bytes = runtime_reject(TORN_BOOKMARK_START_OUTSIDE);
    let xml = document_xml_of(&bytes);
    assert_bookmark_paired_and_collapsed(&xml, "211");
    Document::parse(&bytes).expect("re-parses");
    assert!(
        validate(&bytes).ok,
        "must validate clean: {:?}",
        validate(&bytes).issues
    );
}

#[test]
fn bug_b_runtime_reject_collapses_torn_bookmark_end_outside() {
    let bytes = runtime_reject(TORN_BOOKMARK_END_OUTSIDE);
    let xml = document_xml_of(&bytes);
    assert_bookmark_paired_and_collapsed(&xml, "212");
    Document::parse(&bytes).expect("re-parses");
    assert!(
        validate(&bytes).ok,
        "must validate clean: {:?}",
        validate(&bytes).issues
    );
}

#[test]
fn bug_b_archive_reject_collapses_torn_bookmark_both_shapes() {
    let xml_a = archive_reject_xml(TORN_BOOKMARK_START_OUTSIDE);
    assert_bookmark_paired_and_collapsed(&xml_a, "211");
    let xml_b = archive_reject_xml(TORN_BOOKMARK_END_OUTSIDE);
    assert_bookmark_paired_and_collapsed(&xml_b, "212");
}

// The wild witness shape (auto-redlined legal doc): a heading paragraph whose
// paragraph mark is a tracked insertion, a bookmarkStart as a paragraph child
// OUTSIDE the run insertion, and the paired bookmarkEnd INSIDE it. The
// conformance harness resolves this via the SELECTIVE path (resolve every
// enumerated id), which is where the wild failure surfaced.
const WILD_TORN_BOOKMARK: &str = r#"<w:p>
  <w:pPr><w:pStyle w:val="Heading3"/><w:rPr><w:ins w:id="210" w:author="M" w:date="2025-06-20T12:54:00Z"/></w:rPr></w:pPr>
  <w:bookmarkStart w:id="211" w:name="_Toc181959853"/>
  <w:ins w:id="212" w:author="M" w:date="2025-06-20T12:54:00Z">
    <w:r><w:t>Uncommenced provisions table</w:t></w:r>
    <w:bookmarkEnd w:id="211"/>
  </w:ins>
</w:p>
<w:p><w:r><w:t>Following paragraph</w:t></w:r></w:p>"#;

#[test]
fn bug_b_selective_reject_all_collapses_wild_torn_bookmark() {
    // Reproduces the wild witness through the same SELECTIVE path the harness
    // uses; before the fix this failed with "serialization introduced unpaired
    // bookmarks" (id 211).
    let bytes = selective_resolve_all(WILD_TORN_BOOKMARK, stemma::ResolveSelectionAction::Reject);
    let xml = document_xml_of(&bytes);
    let starts = xml.matches("<w:bookmarkStart").count();
    let ends = xml.matches("<w:bookmarkEnd").count();
    assert_eq!(starts, 1, "one surviving bookmarkStart: {xml}");
    assert_eq!(ends, 1, "the torn bookmarkEnd is re-paired: {xml}");
    assert_eq!(
        xml.matches(r#"w:id="211""#).count(),
        2,
        "both halves keep id 211: {xml}"
    );
    assert!(
        !xml.contains("Uncommenced provisions table"),
        "rejected insertion gone: {xml}"
    );
    Document::parse(&bytes).expect("re-parses");
    assert!(
        validate(&bytes).ok,
        "must validate clean: {:?}",
        validate(&bytes).issues
    );
}

#[test]
fn bug_b_selective_accept_all_keeps_wild_bookmark_paired() {
    // The symmetric selective ACCEPT must also keep the pair well-formed.
    let bytes = selective_resolve_all(WILD_TORN_BOOKMARK, stemma::ResolveSelectionAction::Accept);
    let xml = document_xml_of(&bytes);
    assert_eq!(
        xml.matches("<w:bookmarkStart").count(),
        1,
        "bookmarkStart present: {xml}"
    );
    assert_eq!(
        xml.matches("<w:bookmarkEnd").count(),
        1,
        "bookmarkEnd present: {xml}"
    );
    assert!(
        xml.contains("Uncommenced provisions table"),
        "accepted insertion kept: {xml}"
    );
    assert!(validate(&bytes).ok, "must validate clean");
}

// A paragraph-mark revision resolved via the SELECTIVE path whose merge
// is structurally blocked (a table follows, or it is the last block) must have
// its now-resolved para_mark_status CLEARED — otherwise the serializer re-emits
// it under a fresh next_annotation_id, leaving a "revision that was never in the
// enumeration" in the export. Post-condition: after resolving EVERY enumerated
// id, a reimport of the export enumerates ZERO revisions.
const PARA_MARK_MERGE_BOUNDARY: &str = r#"<w:p>
    <w:pPr><w:rPr><w:ins w:id="301" w:author="M" w:date="2025-06-20T12:54:00Z"/></w:rPr></w:pPr>
    <w:r><w:t>Alpha before table</w:t></w:r>
  </w:p>
  <w:tbl>
    <w:tblPr><w:tblW w:w="5000" w:type="dxa"/></w:tblPr>
    <w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid>
    <w:tr><w:tc><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr>
  </w:tbl>
  <w:p>
    <w:pPr><w:rPr><w:del w:id="302" w:author="M" w:date="2025-06-20T12:54:00Z"/></w:rPr></w:pPr>
    <w:r><w:t>Beta pilcrow deleted</w:t></w:r>
  </w:p>
  <w:p><w:r><w:t>Gamma normal</w:t></w:r></w:p>"#;

fn reimport_revision_count(bytes: &[u8]) -> usize {
    let doc = Document::parse(bytes).expect("reimport export");
    stemma::enumerate_revisions(&doc.snapshot().canonical).len()
}

#[test]
fn selective_resolve_all_leaves_zero_para_mark_residue() {
    // The witness carries a para-mark INSERT whose merge is blocked by a table
    // AND a para-mark DELETE at a merge boundary. Resolving every enumerated id
    // (both directions) must leave nothing pending in the export.
    for action in [
        stemma::ResolveSelectionAction::Reject,
        stemma::ResolveSelectionAction::Accept,
    ] {
        let bytes = selective_resolve_all(PARA_MARK_MERGE_BOUNDARY, action);
        let xml = document_xml_of(&bytes);
        assert!(
            !xml.contains("<w:ins") && !xml.contains("<w:del"),
            "no tracked-change markers remain after resolve-all ({action:?}): {xml}"
        );
        assert_eq!(
            reimport_revision_count(&bytes),
            0,
            "reimport of the resolve-all export must enumerate zero revisions ({action:?})"
        );
        assert!(
            validate(&bytes).ok,
            "resolved export validates clean ({action:?})"
        );
    }
}

#[test]
fn bug_b_both_inside_reject_removes_whole_range_no_orphan() {
    // Both halves are inside the rejected insertion — the bookmark disappears
    // entirely, and that is correct (no torn pair, no orphan).
    let bytes = runtime_reject(BOOKMARK_BOTH_INSIDE);
    let xml = document_xml_of(&bytes);
    assert!(
        !xml.contains(r#"w:id="213""#),
        "the wholly-inserted bookmark is removed: {xml}"
    );
    assert!(validate(&bytes).ok, "must validate clean");

    let axml = archive_reject_xml(BOOKMARK_BOTH_INSIDE);
    assert!(
        !axml.contains(r#"w:id="213""#),
        "archive: wholly-inserted bookmark removed: {axml}"
    );
}

// Characterization for the stretch investigation (accept-all leaving a
// paragraph-mark deletion residue on wild docs). The minimal reproduction of a
// pilcrow deletion — including one on the LAST paragraph with no following
// paragraph to merge into — resolves cleanly: accept-all merges the paragraphs
// and leaves NO w:del residue. The recurring synthetic residue seen on wild
// documents is therefore NOT this shape; it needs the wild witness to reproduce.
#[test]
fn accept_all_pilcrow_deletion_leaves_no_del_residue() {
    let body = r#"<w:p>
      <w:pPr><w:rPr><w:del w:id="101" w:author="A" w:date="2024-01-01T00:00:00Z"/></w:rPr></w:pPr>
      <w:r><w:t>First</w:t></w:r>
    </w:p>
    <w:p>
      <w:pPr><w:rPr><w:del w:id="102" w:author="A" w:date="2024-01-01T00:00:00Z"/></w:rPr></w:pPr>
      <w:r><w:t>Last</w:t></w:r>
    </w:p>"#;
    let bytes = runtime_accept(body);
    let xml = document_xml_of(&bytes);
    assert_eq!(
        xml.matches("<w:del").count(),
        0,
        "accept-all leaves no del residue: {xml}"
    );
    assert!(
        xml.contains("First") && xml.contains("Last"),
        "text survives: {xml}"
    );
    assert!(validate(&bytes).ok, "accepted doc validates clean");
}

// ── A paragraph-mark merge blocked by an intervening table ───────────────────
//
// ECMA-376 §17.13.5.20: rejecting an inserted paragraph mark REMOVES the mark,
// joining the paragraph's content with the FOLLOWING paragraph. "Blocked" is not
// a spec concept. When one logical paragraph was split into N by inserted
// paragraph marks, and the redline interleaved all-tracked tables between the
// fragments, rejecting resolves BOTH: every table loses all its rows (so the
// rowless shell is removed) AND every inserted mark is undone — so the fragments
// rejoin into ONE paragraph across the vanished tables. Word does exactly this
// (wild-witnessed: a 6-way-split legal note rejoins to a single paragraph, and
// our reject-all reproduced Word's 1346-paragraph body exactly).

/// Reimport the export and return `(top-level paragraph count, table count)`.
fn body_block_counts(bytes: &[u8]) -> (usize, usize) {
    let doc = Document::parse(bytes).expect("reimport export");
    let snap = doc.snapshot();
    let mut paras = 0;
    let mut tables = 0;
    for tb in &snap.canonical.blocks {
        match tb.block {
            stemma::BlockNode::Paragraph(_) => paras += 1,
            stemma::BlockNode::Table(_) => tables += 1,
            stemma::BlockNode::OpaqueBlock(_) => {}
        }
    }
    (paras, tables)
}

/// Concatenated text of every top-level body paragraph, in order.
fn body_paragraph_texts(bytes: &[u8]) -> Vec<String> {
    let doc = Document::parse(bytes).expect("reimport export");
    let snap = doc.snapshot();
    let mut out = Vec::new();
    for tb in &snap.canonical.blocks {
        if let stemma::BlockNode::Paragraph(p) = &tb.block {
            let mut s = String::new();
            for inl in p.all_inlines_owned() {
                if let stemma::InlineNode::Text(t) = inl {
                    s.push_str(&t.text);
                }
            }
            out.push(s);
        }
    }
    out
}

// One logical paragraph "Alpha Beta Gamma" split into three by two inserted
// paragraph marks, with an all-tracked (row-inserted) table between each
// fragment. The fragment TEXT is original (normal), so it must survive reject;
// only the marks and the tables' rows are tracked. The last split paragraph
// (Beta) is directly followed by a table — the blocked case.
const SPLIT_AROUND_VANISHING_TABLES: &str = r#"<w:p>
    <w:pPr><w:rPr><w:ins w:id="401" w:author="M" w:date="2025-06-20T12:54:00Z"/></w:rPr></w:pPr>
    <w:r><w:t xml:space="preserve">Alpha </w:t></w:r>
  </w:p>
  <w:tbl>
    <w:tblPr><w:tblW w:w="5000" w:type="dxa"/></w:tblPr>
    <w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid>
    <w:tr><w:trPr><w:ins w:id="402" w:author="M" w:date="2025-06-20T12:54:00Z"/></w:trPr>
      <w:tc><w:tcPr><w:tcW w:w="5000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>r1</w:t></w:r></w:p></w:tc></w:tr>
  </w:tbl>
  <w:p>
    <w:pPr><w:rPr><w:ins w:id="411" w:author="M" w:date="2025-06-20T12:54:00Z"/></w:rPr></w:pPr>
    <w:r><w:t xml:space="preserve">Beta </w:t></w:r>
  </w:p>
  <w:tbl>
    <w:tblPr><w:tblW w:w="5000" w:type="dxa"/></w:tblPr>
    <w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid>
    <w:tr><w:trPr><w:ins w:id="412" w:author="M" w:date="2025-06-20T12:54:00Z"/></w:trPr>
      <w:tc><w:tcPr><w:tcW w:w="5000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>r2</w:t></w:r></w:p></w:tc></w:tr>
  </w:tbl>
  <w:p><w:r><w:t>Gamma</w:t></w:r></w:p>"#;

#[test]
fn reject_joins_split_across_vanishing_tables() {
    // reject_all AND selective-reject-all: the two vanished tables let the three
    // fragments rejoin into exactly ONE paragraph, and nothing is left pending.
    for bytes in [
        runtime_reject(SPLIT_AROUND_VANISHING_TABLES),
        selective_resolve_all(
            SPLIT_AROUND_VANISHING_TABLES,
            stemma::ResolveSelectionAction::Reject,
        ),
    ] {
        assert_eq!(
            body_paragraph_texts(&bytes),
            vec!["Alpha Beta Gamma".to_string()],
            "the three fragments rejoin into one paragraph across the vanished tables"
        );
        assert_eq!(
            body_block_counts(&bytes),
            (1, 0),
            "one paragraph, zero tables"
        );
        assert_eq!(
            reimport_revision_count(&bytes),
            0,
            "reimport enumerates zero revisions"
        );
        assert!(validate(&bytes).ok, "rejected export validates clean");
    }
}

#[test]
fn accept_keeps_split_paragraphs_and_tables() {
    // Accepting keeps every inserted mark (the breaks stay) and every inserted
    // row (the tables stay): three paragraphs, two tables.
    for bytes in [
        runtime_accept(SPLIT_AROUND_VANISHING_TABLES),
        selective_resolve_all(
            SPLIT_AROUND_VANISHING_TABLES,
            stemma::ResolveSelectionAction::Accept,
        ),
    ] {
        assert_eq!(
            body_block_counts(&bytes),
            (3, 2),
            "three paragraphs, two tables"
        );
        assert_eq!(
            body_paragraph_texts(&bytes),
            vec![
                "Alpha ".to_string(),
                "Beta ".to_string(),
                "Gamma".to_string()
            ]
        );
        assert_eq!(
            reimport_revision_count(&bytes),
            0,
            "reimport enumerates zero revisions"
        );
        assert!(validate(&bytes).ok, "accepted export validates clean");
    }
}

// A wholly-inserted paragraph (content AND paragraph mark inserted) directly
// before a RETAINED table. On reject its content vanishes and its mark has no
// join target (the table survives), so Word removes the whole paragraph rather
// than leaving an empty husk. On accept it stays.
const INSERTED_PARAGRAPH_BEFORE_TABLE: &str = r#"<w:p><w:r><w:t>Intro</w:t></w:r></w:p>
  <w:p>
    <w:pPr><w:rPr><w:ins w:id="501" w:author="M" w:date="2025-06-20T12:54:00Z"/></w:rPr></w:pPr>
    <w:ins w:id="502" w:author="M" w:date="2025-06-20T12:54:00Z"><w:r><w:t>Heading</w:t></w:r></w:ins>
  </w:p>
  <w:tbl>
    <w:tblPr><w:tblW w:w="5000" w:type="dxa"/></w:tblPr>
    <w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid>
    <w:tr><w:tc><w:tcPr><w:tcW w:w="5000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>data</w:t></w:r></w:p></w:tc></w:tr>
  </w:tbl>"#;

#[test]
fn reject_drops_empty_inserted_paragraph_before_retained_table() {
    for bytes in [
        runtime_reject(INSERTED_PARAGRAPH_BEFORE_TABLE),
        selective_resolve_all(
            INSERTED_PARAGRAPH_BEFORE_TABLE,
            stemma::ResolveSelectionAction::Reject,
        ),
    ] {
        assert_eq!(
            body_paragraph_texts(&bytes),
            vec!["Intro".to_string()],
            "the emptied inserted paragraph is dropped, not left as an empty husk"
        );
        assert_eq!(
            body_block_counts(&bytes),
            (1, 1),
            "one paragraph and the retained table"
        );
        assert_eq!(
            reimport_revision_count(&bytes),
            0,
            "reimport enumerates zero revisions"
        );
        assert!(validate(&bytes).ok, "rejected export validates clean");
    }

    // Accept keeps the inserted heading paragraph.
    let bytes = runtime_accept(INSERTED_PARAGRAPH_BEFORE_TABLE);
    assert_eq!(
        body_block_counts(&bytes),
        (2, 1),
        "Intro + Heading + the table"
    );
    assert!(validate(&bytes).ok, "accepted export validates clean");
}

// A NON-empty paragraph whose inserted mark's merge is blocked by a retained
// table must NOT be dropped — its content has nowhere to merge, so it stays as
// its own paragraph (its mark resolves to that paragraph's terminating mark).
// Guards against the empty-husk drop over-reaching to content-bearing donors.
const NONEMPTY_BLOCKED_DONOR: &str = r#"<w:p>
    <w:pPr><w:rPr><w:ins w:id="601" w:author="M" w:date="2025-06-20T12:54:00Z"/></w:rPr></w:pPr>
    <w:r><w:t>Kept content</w:t></w:r>
  </w:p>
  <w:tbl>
    <w:tblPr><w:tblW w:w="5000" w:type="dxa"/></w:tblPr>
    <w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid>
    <w:tr><w:tc><w:tcPr><w:tcW w:w="5000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>data</w:t></w:r></w:p></w:tc></w:tr>
  </w:tbl>"#;

#[test]
fn reject_keeps_nonempty_blocked_paragraph() {
    for bytes in [
        runtime_reject(NONEMPTY_BLOCKED_DONOR),
        selective_resolve_all(
            NONEMPTY_BLOCKED_DONOR,
            stemma::ResolveSelectionAction::Reject,
        ),
    ] {
        assert_eq!(
            body_paragraph_texts(&bytes),
            vec!["Kept content".to_string()],
            "content with nowhere to merge stays as its own paragraph"
        );
        assert_eq!(
            body_block_counts(&bytes),
            (1, 1),
            "the kept paragraph and the retained table"
        );
        assert_eq!(
            reimport_revision_count(&bytes),
            0,
            "reimport enumerates zero revisions"
        );
        assert!(validate(&bytes).ok, "rejected export validates clean");
    }
}

// Cell variant: inside a table cell, one logical paragraph split by an inserted
// mark around an all-tracked NESTED table. Reject must rejoin the cell paragraph
// across the vanished nested table.
const CELL_SPLIT_AROUND_VANISHING_NESTED_TABLE: &str = r#"<w:tbl>
    <w:tblPr><w:tblW w:w="5000" w:type="dxa"/></w:tblPr>
    <w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid>
    <w:tr><w:tc><w:tcPr><w:tcW w:w="5000" w:type="dxa"/></w:tcPr>
      <w:p>
        <w:pPr><w:rPr><w:ins w:id="701" w:author="M" w:date="2025-06-20T12:54:00Z"/></w:rPr></w:pPr>
        <w:r><w:t xml:space="preserve">CellA </w:t></w:r>
      </w:p>
      <w:tbl>
        <w:tblPr><w:tblW w:w="2000" w:type="dxa"/></w:tblPr>
        <w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid>
        <w:tr><w:trPr><w:ins w:id="702" w:author="M" w:date="2025-06-20T12:54:00Z"/></w:trPr>
          <w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>n1</w:t></w:r></w:p></w:tc></w:tr>
      </w:tbl>
      <w:p><w:r><w:t>CellB</w:t></w:r></w:p>
    </w:tc></w:tr>
  </w:tbl>"#;

#[test]
fn reject_joins_cell_paragraph_across_vanishing_nested_table() {
    for bytes in [
        runtime_reject(CELL_SPLIT_AROUND_VANISHING_NESTED_TABLE),
        selective_resolve_all(
            CELL_SPLIT_AROUND_VANISHING_NESTED_TABLE,
            stemma::ResolveSelectionAction::Reject,
        ),
    ] {
        let xml = document_xml_of(&bytes);
        // The two fragments rejoin — the cell holds a single paragraph, the
        // nested table is gone, and nothing is left pending.
        assert!(
            xml.contains("CellA ") && xml.contains("CellB"),
            "cell fragments rejoin across the vanished nested table: {xml}"
        );
        let tables = xml.matches("<w:tbl>").count() + xml.matches("<w:tbl ").count();
        assert_eq!(
            tables, 1,
            "only the outer cell's table remains; emptied nested table dropped: {xml}"
        );
        assert_eq!(
            reimport_revision_count(&bytes),
            0,
            "reimport enumerates zero revisions"
        );
        assert!(validate(&bytes).ok, "rejected export validates clean");
    }
}

// Last-block-in-cell: a cell whose LAST paragraph is a wholly-inserted husk. On
// reject its content vanishes and its mark cannot merge (it is the last block),
// but a cell MUST end with a paragraph — so the empty terminating paragraph is
// KEPT (schema safety), not dropped. Nothing is left pending.
const CELL_LAST_PARAGRAPH_INSERTED_HUSK: &str = r#"<w:tbl>
    <w:tblPr><w:tblW w:w="5000" w:type="dxa"/></w:tblPr>
    <w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid>
    <w:tr><w:tc><w:tcPr><w:tcW w:w="5000" w:type="dxa"/></w:tcPr>
      <w:p><w:r><w:t>CellHead</w:t></w:r></w:p>
      <w:p>
        <w:pPr><w:rPr><w:ins w:id="801" w:author="M" w:date="2025-06-20T12:54:00Z"/></w:rPr></w:pPr>
        <w:ins w:id="802" w:author="M" w:date="2025-06-20T12:54:00Z"><w:r><w:t>Ins</w:t></w:r></w:ins>
      </w:p>
    </w:tc></w:tr>
  </w:tbl>"#;

#[test]
fn reject_keeps_cell_terminating_paragraph() {
    for bytes in [
        runtime_reject(CELL_LAST_PARAGRAPH_INSERTED_HUSK),
        selective_resolve_all(
            CELL_LAST_PARAGRAPH_INSERTED_HUSK,
            stemma::ResolveSelectionAction::Reject,
        ),
    ] {
        let xml = document_xml_of(&bytes);
        assert!(
            xml.contains("CellHead"),
            "the retained cell content survives: {xml}"
        );
        assert!(
            !xml.contains("<w:ins"),
            "no tracked-change markers remain: {xml}"
        );
        assert_eq!(
            reimport_revision_count(&bytes),
            0,
            "reimport enumerates zero revisions"
        );
        // A cell must end with a paragraph — the export must validate clean.
        assert!(
            validate(&bytes).ok,
            "rejected export validates clean (cell keeps a terminating para)"
        );
    }
}
