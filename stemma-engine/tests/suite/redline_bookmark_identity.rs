//! Bookmark identity across merge/redline serialization.
//!
//! Domain model (ECMA-376 Part 1 §17.13.6): a bookmark's IDENTITY is its
//! `w:name` — "bookmarks refer to arbitrary regions of content which are
//! bounded and have a unique name associated with them". The `w:id` is only a
//! part-local pairing key linking one `bookmarkStart` to one `bookmarkEnd`
//! (§17.13.6.1/§17.13.6.2 id attribute: "a unique identifier for an
//! annotation"). Starts and ends may appear at run level or between
//! paragraphs (body level) and may span arbitrary content (§17.13.2
//! cross-structure annotations).
//!
//! Invariants encoded here (bookmark half of the redline identity contract):
//! - I1: every emitted bookmarkStart has exactly one bookmarkEnd with the
//!   same id, delimiting the same logical span as the source — across ALL
//!   emission paths (rebuilt paragraphs, raw-preserved blocks, body-level
//!   markers).
//! - I2/I3: identity redline (diff(A, A)) emits A's bookmarks unchanged —
//!   original ids, no orphans, no silent "repair" rewriting.
//! - I4: a genuine id collision (base and target contribute same-id,
//!   DIFFERENT-name bookmarks) keeps both pairs, each internally consistent,
//!   ids unique within the part.
//! - I5: the same NAME contributed by both sides yields exactly one pair
//!   (§17.13.6.2 name attribute: "If multiple bookmarks in a document share
//!   the same name, then the first bookmark ... shall be maintained, and all
//!   subsequent bookmarks should be ignored" — we do not emit markup Word is
//!   required to ignore).
//! - I6a: imbalance INHERITED from the input passes through byte-faithfully
//!   (opaque fidelity — the input's own state is not ours to "repair").

use std::io::{Cursor, Read, Write};

use stemma::{DocxRuntime, ExportMode, SimpleRuntime, TransactionMeta};
use zip::ZipArchive;
use zip::write::FileOptions;

// ── fixture builder ──────────────────────────────────────────────────────

/// Build a minimal valid DOCX whose <w:body> content is `body_inner_xml`
/// (caller controls paragraphs and body-level markers; sectPr appended).
fn make_docx(body_inner_xml: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner_xml}<w:sectPr/></w:body></w:document>"#
    );

    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;

    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
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

fn redline(base: &[u8], target: &[u8]) -> Vec<u8> {
    let runtime = SimpleRuntime::new();
    let import_base = runtime.import_docx(base).expect("import base");
    let import_target = runtime.import_docx(target).expect("import target");
    runtime
        .diff_and_redline(
            &import_base.doc_handle,
            &import_target.doc_handle,
            TransactionMeta {
                author: "bookmark_identity".to_string(),
                reason: Some("bookmark identity invariant".to_string()),
                timestamp_utc: Some("2026-06-01T00:00:00Z".to_string()),
            },
        )
        .expect("diff_and_redline");
    runtime
        .export_docx(&import_base.doc_handle, ExportMode::Redline)
        .expect("export redline")
}

fn document_xml_of(docx: &[u8]) -> String {
    let mut zip = ZipArchive::new(Cursor::new(docx)).expect("open zip");
    let mut file = zip.by_name("word/document.xml").expect("word/document.xml");
    let mut xml = String::new();
    file.read_to_string(&mut xml).expect("read document.xml");
    xml
}

// ── lightweight marker extraction (order-preserving) ─────────────────────

#[derive(Debug, PartialEq, Eq, Clone)]
struct Marker {
    /// "bookmarkStart" or "bookmarkEnd"
    kind: &'static str,
    id: String,
    /// Present on starts only.
    name: Option<String>,
    /// Byte offset in document.xml (for span/position assertions).
    offset: usize,
}

fn attr_value(tag: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let at = tag.find(&needle)? + needle.len();
    let rest = &tag[at..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn bookmark_markers(xml: &str) -> Vec<Marker> {
    let mut out = Vec::new();
    for (kind, open) in [
        ("bookmarkStart", "<w:bookmarkStart"),
        ("bookmarkEnd", "<w:bookmarkEnd"),
    ] {
        let mut idx = 0;
        while let Some(pos) = xml[idx..].find(open) {
            let start = idx + pos;
            let end = start + xml[start..].find('>').expect("tag close") + 1;
            let tag = &xml[start..end];
            out.push(Marker {
                kind,
                id: attr_value(tag, "w:id").expect("w:id attribute"),
                name: attr_value(tag, "w:name"),
                offset: start,
            });
            idx = end;
        }
    }
    out.sort_by_key(|m| m.offset);
    out
}

fn starts(markers: &[Marker]) -> Vec<&Marker> {
    markers
        .iter()
        .filter(|m| m.kind == "bookmarkStart")
        .collect()
}

fn ends(markers: &[Marker]) -> Vec<&Marker> {
    markers.iter().filter(|m| m.kind == "bookmarkEnd").collect()
}

// ── T1: the RP023 shape ──────────────────────────────────────────────────

/// T1 — inline `bookmarkStart` inside a paragraph + body-level `bookmarkEnd`
/// after it (the exact shape of `_GoBack` in TestFiles RP023 and in wild
/// documents at scale: Word's cursor bookmark is routinely emitted this way).
///
/// WHY this is correct: I1 + I3. §17.13.6.2/§17.13.6.1 — the pair is matched
/// by `w:id`, and the two halves legally live at different structural levels
/// (§17.13.2 cross-structure annotations). An identity redline (diff(A, A))
/// makes no change to the document, so A's bookmark must come through with
/// its ORIGINAL id on both halves and its original span: start inside the
/// second paragraph, end between the second and third paragraphs. The old
/// pipeline remapped the inline start (1→fresh) but not the body-level end,
/// then a silent repair pass collapsed the range to zero width.
#[test]
fn t1_rp023_shape_identity_redline_keeps_cross_level_pair_intact() {
    let body = concat!(
        r#"<w:p><w:r><w:t>First paragraph text.</w:t></w:r></w:p>"#,
        r#"<w:p><w:bookmarkStart w:id="1" w:name="_GoBack"/><w:r><w:t>Second paragraph text.</w:t></w:r></w:p>"#,
        r#"<w:bookmarkEnd w:id="1"/>"#,
        r#"<w:p><w:r><w:t>Third paragraph text.</w:t></w:r></w:p>"#,
    );
    let docx = make_docx(body);
    let out = redline(&docx, &docx);
    let xml = document_xml_of(&out);
    let markers = bookmark_markers(&xml);

    let s = starts(&markers);
    let e = ends(&markers);
    assert_eq!(s.len(), 1, "exactly one bookmarkStart, got {markers:?}");
    assert_eq!(e.len(), 1, "exactly one bookmarkEnd, got {markers:?}");
    assert_eq!(
        s[0].name.as_deref(),
        Some("_GoBack"),
        "the start must keep its name"
    );
    assert_eq!(
        s[0].id, "1",
        "identity redline must keep the ORIGINAL id on the start (no remap)"
    );
    assert_eq!(
        e[0].id, "1",
        "the end must carry the SAME id as its start (no torn pair)"
    );

    // Span preserved: start before the second paragraph's text, end after it
    // and before the third paragraph's text — NOT a collapsed zero-width pair.
    let second_text = xml.find("Second paragraph text.").expect("second para");
    let third_text = xml.find("Third paragraph text.").expect("third para");
    assert!(
        s[0].offset < second_text,
        "bookmarkStart must stay before the bookmarked text"
    );
    assert!(
        e[0].offset > second_text && e[0].offset < third_text,
        "bookmarkEnd must stay between the second and third paragraphs \
         (start at {}, end at {}, second text at {second_text}, third text at \
         {third_text})",
        s[0].offset,
        e[0].offset
    );
}

// ── T2: pair spanning an unchanged and an edited paragraph ───────────────

/// T2 — base bookmark starts in an UNCHANGED paragraph and ends inside a
/// paragraph that the target EDITS. The two halves take different emission
/// paths (verbatim-ish unchanged content vs rebuilt tracked paragraph).
///
/// WHY this is correct: I1 + I2. The bookmark belongs to the base document
/// and the base content is still present (the edit is tracked, not applied),
/// so the pair must survive with the base's original id on BOTH halves.
/// Additionally, the target document carries its own copy of the same
/// bookmark (same name "ClauseRef") whose end marker sits at a shifted text
/// offset — merge marker injection must not produce a SECOND end marker:
/// §17.13.6.1 requires the id to pair exactly one start with one end.
#[test]
fn t2_pair_spanning_unchanged_and_edited_paragraph_stays_paired() {
    let base_body = concat!(
        r#"<w:p><w:r><w:t>Alpha </w:t></w:r><w:bookmarkStart w:id="3" w:name="ClauseRef"/><w:r><w:t>one.</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Beta </w:t></w:r><w:bookmarkEnd w:id="3"/><w:r><w:t>two.</w:t></w:r></w:p>"#,
    );
    let target_body = concat!(
        r#"<w:p><w:r><w:t>Alpha </w:t></w:r><w:bookmarkStart w:id="3" w:name="ClauseRef"/><w:r><w:t>one.</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Beta revised </w:t></w:r><w:bookmarkEnd w:id="3"/><w:r><w:t>two.</w:t></w:r></w:p>"#,
    );
    let out = redline(&make_docx(base_body), &make_docx(target_body));
    let xml = document_xml_of(&out);
    let markers = bookmark_markers(&xml);

    let s = starts(&markers);
    let e = ends(&markers);
    assert_eq!(
        s.len(),
        1,
        "exactly one bookmarkStart (no duplicated target copy), got {markers:?}"
    );
    assert_eq!(
        e.len(),
        1,
        "exactly one bookmarkEnd (no duplicated target copy), got {markers:?}"
    );
    assert_eq!(s[0].name.as_deref(), Some("ClauseRef"));
    assert_eq!(
        s[0].id, e[0].id,
        "start and end must carry the same pairing id"
    );
    assert_eq!(
        s[0].id, "3",
        "the base half is untouched content — it keeps its original id, so \
         the pair keeps id 3"
    );
    assert!(
        s[0].offset < e[0].offset,
        "start must precede end in document order"
    );
}

// ── T3: genuine id collision, different names ────────────────────────────

/// T3 — base contributes bookmark "Alpha" with id 1; the target's INSERTED
/// paragraph contributes a DIFFERENT bookmark ("Beta") that happens to also
/// use id 1 (base and target are separate documents with independent
/// part-local id spaces — id collisions across a merge are ordinary).
///
/// WHY this is correct: I4. §17.13.6 — the two bookmarks are distinct
/// identities (different names) and both regions exist in the merged
/// document, so BOTH pairs must be emitted, each internally consistent
/// (start id == end id), with ids unique in the part (§17.13.6.2 id:
/// "unique identifier for an annotation"). The base pair keeps its original
/// id (untouched content, I2); the target pair gets a fresh id applied to
/// BOTH halves.
#[test]
fn t3_genuine_id_collision_different_names_keeps_both_pairs() {
    let base_body = r#"<w:p><w:bookmarkStart w:id="1" w:name="Alpha"/><w:r><w:t>Shared text.</w:t></w:r><w:bookmarkEnd w:id="1"/></w:p>"#;
    let target_body = concat!(
        r#"<w:p><w:bookmarkStart w:id="1" w:name="Alpha"/><w:r><w:t>Shared text.</w:t></w:r><w:bookmarkEnd w:id="1"/></w:p>"#,
        r#"<w:p><w:bookmarkStart w:id="1" w:name="Beta"/><w:r><w:t>New text from target.</w:t></w:r><w:bookmarkEnd w:id="1"/></w:p>"#,
    );
    // NOTE: the target document reuses id 1 for Beta. Within the TARGET part
    // that is its own (sloppy but real-world) state; what matters here is the
    // MERGED part: Alpha comes from base content, Beta from an inserted
    // target paragraph, and they collide on id 1.
    let out = redline(&make_docx(base_body), &make_docx(target_body));
    let xml = document_xml_of(&out);
    let markers = bookmark_markers(&xml);

    let s = starts(&markers);
    let e = ends(&markers);
    assert_eq!(s.len(), 2, "both bookmarks must survive, got {markers:?}");
    assert_eq!(e.len(), 2, "both ends must survive, got {markers:?}");

    let alpha = s
        .iter()
        .find(|m| m.name.as_deref() == Some("Alpha"))
        .expect("Alpha start");
    let beta = s
        .iter()
        .find(|m| m.name.as_deref() == Some("Beta"))
        .expect("Beta start");
    assert_eq!(
        alpha.id, "1",
        "base bookmark is untouched content — keeps its original id"
    );
    assert_ne!(
        beta.id, alpha.id,
        "ids must be unique within the part — the colliding target pair gets \
         a fresh id"
    );
    // Each pair internally consistent: one end per start id.
    for start in [alpha, beta] {
        let matching: Vec<_> = e.iter().filter(|m| m.id == start.id).collect();
        assert_eq!(
            matching.len(),
            1,
            "exactly one end for start id {} ({:?}), got {markers:?}",
            start.id,
            start.name
        );
    }
}

// ── T4: same name contributed by both sides ──────────────────────────────

/// T4 — both sides contribute a bookmark NAMED "_GoBack" (base id 5, target
/// id 9 inside an inserted paragraph).
///
/// WHY this is correct: I5. §17.13.6.2 name attribute: bookmark names are
/// unique per document — "If multiple bookmarks in a document share the same
/// name, then the first bookmark ... shall be maintained, and all subsequent
/// bookmarks should be ignored." The name IS the bookmark's identity, so the
/// two sides denote the SAME bookmark; emitting both would create dead
/// markup that Word is required to ignore. The output carries exactly one
/// pair — the base one (base is the document whose bytes we preserve; its
/// copy keeps its original id per I2).
#[test]
fn t4_same_name_from_both_sides_emits_one_pair() {
    let base_body = r#"<w:p><w:bookmarkStart w:id="5" w:name="_GoBack"/><w:r><w:t>Shared text.</w:t></w:r><w:bookmarkEnd w:id="5"/></w:p>"#;
    let target_body = concat!(
        r#"<w:p><w:bookmarkStart w:id="5" w:name="_GoBack"/><w:r><w:t>Shared text.</w:t></w:r><w:bookmarkEnd w:id="5"/></w:p>"#,
        r#"<w:p><w:bookmarkStart w:id="9" w:name="_GoBack"/><w:r><w:t>Inserted text.</w:t></w:r><w:bookmarkEnd w:id="9"/></w:p>"#,
    );
    // (The target document's own duplicate _GoBack mirrors what Word's
    // cursor bookmark does across save cycles; consumers keep the first.)
    let out = redline(&make_docx(base_body), &make_docx(target_body));
    let xml = document_xml_of(&out);
    let markers = bookmark_markers(&xml);

    let s = starts(&markers);
    let e = ends(&markers);
    assert_eq!(
        s.len(),
        1,
        "exactly one _GoBack pair must survive (names are document-unique), \
         got {markers:?}"
    );
    assert_eq!(e.len(), 1, "exactly one end, got {markers:?}");
    assert_eq!(s[0].name.as_deref(), Some("_GoBack"));
    assert_eq!(s[0].id, "5", "the BASE copy is the one maintained");
    assert_eq!(e[0].id, "5", "its end pairs by the same id");
}

// ── T6/T7: markers at table-structure level (cell/row/table children) ────

/// T6 — a bookmark pair whose start sits at TABLE-CELL level (a direct child
/// of `w:tc`, after a paragraph) and whose end sits inline in another cell's
/// paragraph. §17.13.2 (cross-structure annotations) explicitly allows
/// bookmark markers "at any location within a document's contents",
/// including as direct children of `w:tc`/`w:tr`/`w:tbl` — Word emits
/// `_GoBack` this way inside tables routinely (the dominant shape in the
/// stress-corpus failures).
///
/// WHY this is correct: I1. The import used to silently DROP these
/// structure-level markers (data loss), leaving the other half orphaned, and
/// the repair pass masked it by deleting the orphan. The pair must survive
/// an identity redline with its original id on both halves. The marker is
/// re-anchored at the nearest paragraph boundary (zero-width; the delimited
/// content is unchanged).
#[test]
fn t6_cell_level_marker_keeps_pair_intact() {
    let body = concat!(
        r#"<w:tbl><w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid>"#,
        r#"<w:tr><w:tc><w:p><w:r><w:t>Cell one.</w:t></w:r></w:p><w:bookmarkStart w:id="2" w:name="_GoBack"/></w:tc></w:tr>"#,
        r#"<w:tr><w:tc><w:p><w:bookmarkEnd w:id="2"/><w:r><w:t>Cell two.</w:t></w:r></w:p></w:tc></w:tr>"#,
        r#"</w:tbl>"#,
        r#"<w:p><w:r><w:t>After table.</w:t></w:r></w:p>"#,
    );
    let docx = make_docx(body);
    let out = redline(&docx, &docx);
    let xml = document_xml_of(&out);
    let markers = bookmark_markers(&xml);

    let s = starts(&markers);
    let e = ends(&markers);
    assert_eq!(
        s.len(),
        1,
        "the cell-level start must survive, got {markers:?}"
    );
    assert_eq!(e.len(), 1, "its end must survive, got {markers:?}");
    assert_eq!(s[0].name.as_deref(), Some("_GoBack"));
    assert_eq!(s[0].id, "2", "identity redline keeps the original id");
    assert_eq!(e[0].id, "2", "the pair stays keyed by the same id");

    // Span: start at/after "Cell one.", end at/before "Cell two." — the
    // bookmarked region (the boundary between the two cells) is preserved.
    let cell_one = xml.find("Cell one.").expect("cell one text");
    let cell_two = xml.find("Cell two.").expect("cell two text");
    assert!(
        s[0].offset > cell_one && s[0].offset < cell_two,
        "start must stay between the two cells' text (start at {}, texts at \
         {cell_one}/{cell_two})",
        s[0].offset
    );
    assert!(
        e[0].offset > s[0].offset && e[0].offset < cell_two,
        "end must stay after the start and before the second cell's text"
    );
}

/// T7 — bookmark END as a direct child of `w:tr` (after the last `w:tc`):
/// the EXACT shape §17.13.6.2 prescribes for table bookmarks ("the last row
/// ... is accomplished by placing the bookmarkEnd element at the end of that
/// table row"). The start sits inline in a cell paragraph.
///
/// WHY this is correct: I1 + §17.13.6.2 table bookmarks. Dropping the
/// row-level end (the old import behavior) tears the spec's own canonical
/// table-bookmark shape.
#[test]
fn t7_row_level_end_keeps_pair_intact() {
    let body = concat!(
        r#"<w:tbl><w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid>"#,
        r#"<w:tr><w:tc><w:p><w:bookmarkStart w:id="3" w:name="TableRegion"/><w:r><w:t>Row content.</w:t></w:r></w:p></w:tc><w:bookmarkEnd w:id="3"/></w:tr>"#,
        r#"</w:tbl>"#,
        r#"<w:p><w:r><w:t>After table.</w:t></w:r></w:p>"#,
    );
    let docx = make_docx(body);
    let out = redline(&docx, &docx);
    let xml = document_xml_of(&out);
    let markers = bookmark_markers(&xml);

    let s = starts(&markers);
    let e = ends(&markers);
    assert_eq!(s.len(), 1, "start must survive, got {markers:?}");
    assert_eq!(e.len(), 1, "row-level end must survive, got {markers:?}");
    assert_eq!(s[0].name.as_deref(), Some("TableRegion"));
    assert_eq!(s[0].id, "3");
    assert_eq!(e[0].id, "3", "pair keyed by the same id");

    let row_text = xml.find("Row content.").expect("row text");
    let after_text = xml.find("After table.").expect("after text");
    assert!(
        s[0].offset < row_text,
        "start before the bookmarked row text"
    );
    assert!(
        e[0].offset > row_text && e[0].offset < after_text,
        "end after the row text and inside the table region"
    );
}

// ── T8/T9: markers inside inline containers ──────────────────────────────

/// T8 — a bookmark END inside an inline `<w:ins>` while its start sits at
/// paragraph level (Word produces this whenever text containing a TOC
/// bookmark boundary is inserted with tracking on; CT_RunTrackChange
/// includes EG_RangeMarkupElements, ECMA-376 §17.13.5.18).
///
/// WHY this is correct: I1. The atom collector used to silently skip range
/// markers inside tracked containers, tearing the pair. The identity redline
/// must carry the pair through with its original id; the marker resolves
/// with the surrounding revision on accept/reject (rejecting the insertion
/// removes the marker created inside it, matching Word).
#[test]
fn t8_marker_inside_tracked_container_keeps_pair_intact() {
    let body = concat!(
        r#"<w:p><w:bookmarkStart w:id="201" w:name="_TocX"/>"#,
        r#"<w:ins w:id="204" w:author="A" w:date="2021-09-12T14:31:00Z">"#,
        r#"<w:r><w:t>Inserted heading text</w:t></w:r><w:bookmarkEnd w:id="201"/>"#,
        r#"</w:ins></w:p>"#,
    );
    let docx = make_docx(body);
    let out = redline(&docx, &docx);
    let xml = document_xml_of(&out);
    let markers = bookmark_markers(&xml);

    let s = starts(&markers);
    let e = ends(&markers);
    assert_eq!(s.len(), 1, "start must survive, got {markers:?}");
    assert_eq!(
        e.len(),
        1,
        "the in-container end must survive, got {markers:?}"
    );
    assert_eq!(s[0].name.as_deref(), Some("_TocX"));
    assert_eq!(s[0].id, "201", "identity redline keeps the original id");
    assert_eq!(e[0].id, "201", "pair keyed by the same id");
    assert!(
        s[0].offset < e[0].offset,
        "start must precede end in document order"
    );
}

/// T9 — a bookmark pair nested inside a `w:hyperlink` whose other half (the
/// end) sits OUTSIDE the link: Word's standard TOC-heading shape
/// (`<w:hyperlink><w:bookmarkStart w:name="_Toc…"/>…runs…</w:hyperlink>` with
/// the end after the link, or vice versa).
///
/// WHY this is correct: I1 + §17.13.2 (markers may sit at any location,
/// including inside EG_PContent containers). The hyperlink IR models display
/// text only, so the markers are hoisted to the link's edges — the pair must
/// survive with its original id and still bracket the link text.
#[test]
fn t9_marker_inside_hyperlink_keeps_pair_intact() {
    let body = concat!(
        r#"<w:p><w:r><w:t>See </w:t></w:r>"#,
        r#"<w:hyperlink w:anchor="_Toc74085637" w:history="1">"#,
        r#"<w:bookmarkStart w:id="18" w:name="_TocY"/>"#,
        r#"<w:r><w:t>Sensors used.</w:t></w:r>"#,
        r#"</w:hyperlink>"#,
        r#"<w:bookmarkEnd w:id="18"/>"#,
        r#"<w:r><w:t> End of sentence.</w:t></w:r></w:p>"#,
    );
    let docx = make_docx(body);
    let out = redline(&docx, &docx);
    let xml = document_xml_of(&out);
    let markers = bookmark_markers(&xml);

    let s = starts(&markers);
    let e = ends(&markers);
    assert_eq!(
        s.len(),
        1,
        "the in-hyperlink start must survive, got {markers:?}"
    );
    assert_eq!(e.len(), 1, "its end must survive, got {markers:?}");
    assert_eq!(s[0].name.as_deref(), Some("_TocY"));
    assert_eq!(s[0].id, "18", "identity redline keeps the original id");
    assert_eq!(e[0].id, "18", "pair keyed by the same id");

    let link_text = xml.find("Sensors used.").expect("link text");
    assert!(
        s[0].offset < link_text,
        "hoisted start must still precede the hyperlink text"
    );
    assert!(
        e[0].offset > link_text,
        "end must still follow the hyperlink text"
    );
}

// ── T5: inherited imbalance passes through ───────────────────────────────

/// T5 — the INPUT document itself carries an orphaned body-level
/// `bookmarkEnd` (no start anywhere). An identity redline must pass it
/// through byte-faithfully: not delete it, not synthesize a start.
///
/// WHY this is correct: I6a (opaque fidelity). The imbalance is the input's
/// own state, not something serialization introduced; "repairing" input
/// content silently is the fix-at-symptom pattern this codebase bans.
/// Consumers tolerate a dangling end (it bounds nothing); deleting it would
/// CHANGE the user's document on a no-op redline.
#[test]
fn t5_inherited_orphan_end_passes_through_byte_faithfully() {
    let body = concat!(
        r#"<w:p><w:r><w:t>Only paragraph.</w:t></w:r></w:p>"#,
        r#"<w:bookmarkEnd w:id="7"/>"#,
        r#"<w:p><w:r><w:t>Tail paragraph.</w:t></w:r></w:p>"#,
    );
    let docx = make_docx(body);
    let out = redline(&docx, &docx);
    let xml = document_xml_of(&out);
    let markers = bookmark_markers(&xml);

    assert_eq!(
        starts(&markers).len(),
        0,
        "no bookmarkStart may be synthesized for an inherited orphan end, \
         got {markers:?}"
    );
    let e = ends(&markers);
    assert_eq!(
        e.len(),
        1,
        "the inherited orphan end must pass through, got {markers:?}"
    );
    assert_eq!(e[0].id, "7", "with its original id");
}
