//! A comment range whose two halves straddle a tracked change must stay ANCHORED
//! when the change is resolved: the `commentRangeStart`/`commentRangeEnd` pair
//! collapses to a POINT at the surviving half (ECMA-376 §17.13.6, Word's behavior
//! when a commented range's interior is removed), the `commentReference` run keeps
//! the comment alive, and the comment stays retrievable from `word/comments.xml`.
//!
//! Before the fix the MODEL path (`Document::project`) left a lone half: a
//! `commentRangeStart`/`End` sitting inside a `w:ins`/`w:del` was dropped at
//! IMPORT (`word_ir::tracked_change_atoms` modeled bookmarks/permissions —
//! decorations — but not the typed comment-range markers), so the accept/reject
//! collapse never saw the removed half to re-pair it. The byte path
//! (`normalize`) already modeled and collapsed these, so this is the model path
//! catching up to byte-path parity. The collapse itself is unchanged.
//!
//! The collapse touches range MARKERS only — never the `commentReference` run nor
//! `word/comments.xml` — so the comment is preserved, only re-anchored.
//!
//! Daily tier: synthetic in-memory DOCX carrying a real comments part.

use std::io::{Read, Write};

use stemma::api::{Document, validate};
use stemma::docx::DocxArchive;
use stemma::domain::{BlockNode, InlineNode, TrackingStatus};
use stemma::normalize::{normalize_docx, reject_all_docx};
use stemma::{CommentStory, ExportOptions, Resolution};

/// Pack a one-body-paragraph DOCX that carries a real `word/comments.xml`
/// defining comments with the given ids. `body_inner` is the `w:body` content.
fn pack_with_comments(body_inner: &str, comment_ids: &[&str]) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
    );
    let comment_elems: String = comment_ids
        .iter()
        .map(|id| {
            format!(
                r#"<w:comment w:id="{id}" w:author="Reviewer" w:date="2026-06-01T00:00:00Z" w:initials="R"><w:p><w:r><w:t>Note {id}.</w:t></w:r></w:p></w:comment>"#
            )
        })
        .collect();
    let comments_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{comment_elems}</w:comments>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/></Relationships>"#;

    use zip::write::FileOptions;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        let mut write = |name: &str, data: &str| {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        };
        write("[Content_Types].xml", content_types);
        write("_rels/.rels", rels);
        write("word/_rels/document.xml.rels", doc_rels);
        write("word/document.xml", &document_xml);
        write("word/comments.xml", &comments_xml);
        zip.finish().unwrap();
    }
    buf
}

fn document_xml_of(docx: &[u8]) -> String {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(docx)).unwrap();
    let mut file = zip.by_name("word/document.xml").unwrap();
    let mut out = String::new();
    file.read_to_string(&mut out).unwrap();
    out
}

/// Ordered `(local-name, id)` range markers + commentReference in document order.
fn ordered_markers(xml: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(lt) = rest.find('<') {
        rest = &rest[lt..];
        let end = rest.find('>').map(|e| e + 1).unwrap_or(rest.len());
        let tag = &rest[..end];
        for name in [
            "w:bookmarkStart",
            "w:bookmarkEnd",
            "w:commentRangeStart",
            "w:commentRangeEnd",
            "w:commentReference",
        ] {
            if tag.starts_with(&format!("<{name}"))
                && let Some(idx) = tag.find(r#"w:id=""#)
            {
                let after = &tag[idx + 6..];
                let id: String = after.chars().take_while(|&c| c != '"').collect();
                out.push((name.trim_start_matches("w:").to_string(), id));
            }
        }
        rest = &rest[end..];
    }
    out
}

/// Model path: parse → project → serialize, returning the output document.xml.
fn model_resolve(bytes: &[u8], accept: bool) -> Vec<u8> {
    let doc = Document::parse(bytes).expect("parse");
    let res = if accept {
        Resolution::AcceptAll
    } else {
        Resolution::RejectAll
    };
    doc.project(res)
        .expect("project")
        .serialize(&ExportOptions::default())
        .expect("serialize")
}

/// Byte path: the archive-level resolver twin.
fn byte_resolve(bytes: &[u8], accept: bool) -> Vec<u8> {
    let arch = DocxArchive::read(bytes).expect("read");
    let (out, _) = if accept {
        normalize_docx(&arch).expect("normalize")
    } else {
        reject_all_docx(&arch).expect("reject")
    };
    out.write().expect("write archive")
}

fn comment_ids(bytes: &[u8]) -> Vec<String> {
    let doc = Document::parse(bytes).expect("re-parse output");
    doc.snapshot()
        .canonical
        .comments
        .iter()
        .map(|c: &CommentStory| c.id.clone())
        .collect()
}

/// Assert the comment range `id` collapsed to a paired point (exactly one start,
/// one end, ADJACENT in document order), the commentReference for `id` survived,
/// and the comment is still retrievable from comments.xml.
fn assert_comment_anchored_at_point(bytes: &[u8], id: &str) {
    let xml = document_xml_of(bytes);
    assert_eq!(
        xml.matches("<w:commentRangeStart").count(),
        1,
        "one commentRangeStart survives: {xml}"
    );
    assert_eq!(
        xml.matches("<w:commentRangeEnd").count(),
        1,
        "the torn commentRangeEnd is re-paired: {xml}"
    );
    // Collapsed to a point: start immediately followed by end for this id, with
    // no intervening content between the two range halves.
    let markers = ordered_markers(&xml);
    let range_only: Vec<(String, String)> = markers
        .iter()
        .filter(|(n, i)| n.starts_with("commentRange") && i == id)
        .cloned()
        .collect();
    assert_eq!(
        range_only,
        vec![
            ("commentRangeStart".to_string(), id.to_string()),
            ("commentRangeEnd".to_string(), id.to_string()),
        ],
        "comment range collapses to a point (start immediately followed by end): {xml}"
    );
    // The commentReference run survives (comment stays anchored). Match by
    // (name, id) rather than a literal attribute string: the serializer may emit
    // a redundant xmlns:w on the element, which a literal `<w:commentReference
    // w:id=` substring would miss.
    let reference_hits = markers
        .iter()
        .filter(|(n, i)| n == "commentReference" && i == id)
        .count();
    assert_eq!(
        reference_hits, 1,
        "the commentReference run survives (comment stays anchored): {xml}"
    );
    assert!(
        comment_ids(bytes).contains(&id.to_string()),
        "comment {id} stays retrievable from comments.xml"
    );
    // Range-marker balance validates (comment-range integrity is otherwise a
    // non-blocking advisory, so an unbalanced pair would slip past serialize).
    assert!(
        validate(bytes).ok,
        "resolved doc validates clean: {:?}",
        validate(bytes).issues
    );
}

// ── (a) survivor START-half, on REJECT ───────────────────────────────────────

/// commentRangeStart in base (survivor), its commentRangeEnd INSIDE a w:ins.
/// Rejecting removes the insertion (and the end); the pair must collapse to a
/// point at the surviving start, with the base commentReference intact.
#[test]
fn torn_comment_survivor_start_collapses_on_reject() {
    let body = r#"<w:p>
      <w:r><w:t xml:space="preserve">Before </w:t></w:r>
      <w:commentRangeStart w:id="1"/>
      <w:ins w:id="7" w:author="A" w:date="2024-01-01T00:00:00Z">
        <w:r><w:t>inserted</w:t></w:r>
        <w:commentRangeEnd w:id="1"/>
      </w:ins>
      <w:r><w:commentReference w:id="1"/></w:r>
      <w:r><w:t xml:space="preserve"> after</w:t></w:r>
    </w:p>"#;
    let bytes = pack_with_comments(body, &["1"]);
    let out = model_resolve(&bytes, /*accept=*/ false);
    assert_comment_anchored_at_point(&out, "1");
    assert!(
        !document_xml_of(&out).contains("inserted"),
        "the rejected insertion content is gone"
    );
}

/// IMPORT CONTRACT (the actual root cause): a commentRangeEnd sitting inside a
/// `w:ins` must be MODELED in the IR — as an `InlineNode::CommentRangeEnd` inside
/// a segment carrying the container's revision context (here the `w:ins` id 7) —
/// so the downstream tear-collapse can see and re-pair it. Pinning this
/// post-parse / pre-resolution guards the fix at its source: were the marker
/// dropped at import again (the original bug), this fails BEFORE any resolution.
#[test]
fn import_models_comment_end_inside_tracked_container() {
    let body = r#"<w:p>
      <w:r><w:t xml:space="preserve">Before </w:t></w:r>
      <w:commentRangeStart w:id="1"/>
      <w:ins w:id="7" w:author="A" w:date="2024-01-01T00:00:00Z">
        <w:r><w:t>inserted</w:t></w:r>
        <w:commentRangeEnd w:id="1"/>
      </w:ins>
      <w:r><w:commentReference w:id="1"/></w:r>
      <w:r><w:t xml:space="preserve"> after</w:t></w:r>
    </w:p>"#;
    let doc = Document::parse(&pack_with_comments(body, &["1"])).expect("parse");
    let snap = doc.snapshot();
    let mut found_end_with_ins_context = false;
    for tb in &snap.canonical.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for seg in &p.segments {
                let has_end = seg
                    .inlines
                    .iter()
                    .any(|i| matches!(i, InlineNode::CommentRangeEnd { id } if id == "1"));
                if has_end {
                    // The end must carry the w:ins revision context (id 7), so
                    // that rejecting the insertion resolves it away and the
                    // collapse then re-pairs the range.
                    match &seg.status {
                        TrackingStatus::Inserted(rev) if rev.revision_id == 7 => {
                            found_end_with_ins_context = true;
                        }
                        other => panic!(
                            "commentRangeEnd modeled but not under the w:ins context: {other:?}"
                        ),
                    }
                }
            }
        }
    }
    assert!(
        found_end_with_ins_context,
        "commentRangeEnd inside w:ins must be modeled as an inline carrying the \
         insertion's revision context (revision_id 7)"
    );
}

// ── (b) survivor END-half, on ACCEPT ─────────────────────────────────────────

/// commentRangeStart INSIDE a w:del, its commentRangeEnd in base (survivor).
/// Accepting removes the deletion (and the start); the pair must collapse to a
/// point at the surviving end.
#[test]
fn torn_comment_survivor_end_collapses_on_accept() {
    let body = r#"<w:p>
      <w:r><w:t xml:space="preserve">Before </w:t></w:r>
      <w:del w:id="8" w:author="A" w:date="2024-01-01T00:00:00Z">
        <w:r><w:delText>deleted</w:delText></w:r>
        <w:commentRangeStart w:id="2"/>
      </w:del>
      <w:r><w:t xml:space="preserve">mid</w:t></w:r>
      <w:commentRangeEnd w:id="2"/>
      <w:r><w:commentReference w:id="2"/></w:r>
      <w:r><w:t xml:space="preserve"> after</w:t></w:r>
    </w:p>"#;
    let bytes = pack_with_comments(body, &["2"]);
    let out = model_resolve(&bytes, /*accept=*/ true);
    assert_comment_anchored_at_point(&out, "2");
    assert!(
        !document_xml_of(&out).contains("deleted"),
        "the accepted deletion content is gone"
    );
}

// ── (c) comment tear + bookmark tear, same paragraph (family independence) ────

/// A comment range and a bookmark that share the numeric id 3, both torn by the
/// same rejected insertion. Each must collapse to its OWN family's point — the
/// family-keyed repair must not cross-pair a bookmarkEnd with a commentRangeStart
/// — and the comment must stay anchored/retrievable.
#[test]
fn comment_and_bookmark_tears_collapse_independently() {
    let body = r#"<w:p>
      <w:r><w:t xml:space="preserve">Before </w:t></w:r>
      <w:bookmarkStart w:id="3" w:name="_BM"/>
      <w:commentRangeStart w:id="3"/>
      <w:ins w:id="7" w:author="A" w:date="2024-01-01T00:00:00Z">
        <w:r><w:t>inserted</w:t></w:r>
        <w:commentRangeEnd w:id="3"/>
        <w:bookmarkEnd w:id="3"/>
      </w:ins>
      <w:r><w:commentReference w:id="3"/></w:r>
      <w:r><w:t xml:space="preserve"> after</w:t></w:r>
    </w:p>"#;
    let bytes = pack_with_comments(body, &["3"]);
    let out = model_resolve(&bytes, /*accept=*/ false);

    // Comment collapsed and anchored.
    assert_comment_anchored_at_point(&out, "3");

    // Bookmark collapsed independently: exactly one start + one end, paired.
    let xml = document_xml_of(&out);
    assert_eq!(
        xml.matches("<w:bookmarkStart").count(),
        1,
        "one bookmarkStart: {xml}"
    );
    assert_eq!(
        xml.matches("<w:bookmarkEnd").count(),
        1,
        "bookmarkEnd re-paired: {xml}"
    );
    // No cross-pairing: restricting to the range markers, each family's start is
    // immediately followed by its own end.
    let markers: Vec<(String, String)> = ordered_markers(&xml)
        .into_iter()
        .filter(|(n, _)| n.starts_with("bookmark") || n.starts_with("commentRange"))
        .collect();
    assert_eq!(
        markers,
        vec![
            ("bookmarkStart".to_string(), "3".to_string()),
            ("bookmarkEnd".to_string(), "3".to_string()),
            ("commentRangeStart".to_string(), "3".to_string()),
            ("commentRangeEnd".to_string(), "3".to_string()),
        ],
        "each family collapses to its own point, no cross-pairing: {xml}"
    );
}

// ── (d) wire/model parity ────────────────────────────────────────────────────

/// The model path (project) and the byte path (normalize/reject_all_docx) must
/// leave the SAME paired comment-range state on every tear shape and direction.
#[test]
fn wire_model_parity_on_comment_tears() {
    struct Case {
        name: &'static str,
        body: &'static str,
        ids: &'static [&'static str],
        accept: bool,
    }
    let cases = [
        Case {
            name: "start-survivor/reject",
            body: r#"<w:p><w:r><w:t xml:space="preserve">Before </w:t></w:r><w:commentRangeStart w:id="1"/><w:ins w:id="7" w:author="A" w:date="2024-01-01T00:00:00Z"><w:r><w:t>inserted</w:t></w:r><w:commentRangeEnd w:id="1"/></w:ins><w:r><w:commentReference w:id="1"/></w:r><w:r><w:t xml:space="preserve"> after</w:t></w:r></w:p>"#,
            ids: &["1"],
            accept: false,
        },
        Case {
            name: "end-survivor/accept",
            body: r#"<w:p><w:r><w:t xml:space="preserve">Before </w:t></w:r><w:del w:id="8" w:author="A" w:date="2024-01-01T00:00:00Z"><w:r><w:delText>deleted</w:delText></w:r><w:commentRangeStart w:id="2"/></w:del><w:r><w:t xml:space="preserve">mid</w:t></w:r><w:commentRangeEnd w:id="2"/><w:r><w:commentReference w:id="2"/></w:r><w:r><w:t xml:space="preserve"> after</w:t></w:r></w:p>"#,
            ids: &["2"],
            accept: true,
        },
    ];
    for c in cases {
        let bytes = pack_with_comments(c.body, c.ids);
        let model = document_xml_of(&model_resolve(&bytes, c.accept));
        let byte = document_xml_of(&byte_resolve(&bytes, c.accept));
        let m_start = model.matches("<w:commentRangeStart").count();
        let m_end = model.matches("<w:commentRangeEnd").count();
        let b_start = byte.matches("<w:commentRangeStart").count();
        let b_end = byte.matches("<w:commentRangeEnd").count();
        assert_eq!(
            (m_start, m_end),
            (b_start, b_end),
            "{}: model marker counts (start={m_start},end={m_end}) must match byte \
             (start={b_start},end={b_end})",
            c.name
        );
        assert_eq!(m_start, 1, "{}: model leaves exactly one start", c.name);
        assert_eq!(
            m_end, 1,
            "{}: model leaves exactly one end (paired)",
            c.name
        );
    }
}
