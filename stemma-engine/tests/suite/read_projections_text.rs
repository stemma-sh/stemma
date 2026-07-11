//! Integration tests for the comprehension / read-surface projections
//! (roadmap A): `Document::to_text`, `read_accepted`, `read_rejected`, and the
//! extended-markdown redline. These are pure, read-only projections of the
//! already-materialized state — no EditStep, no Op, no materializer change.
//!
//! Daily, corpus-free: every fixture is a synthesized in-memory DOCX, so the
//! suite passes with all environment unset.
//!
//! The faithfulness checks compare `to_text` against an INDEPENDENT oracle (the
//! serialized `<w:t>` body text, excluding `<w:delText>`), never against
//! `to_plain_text` itself — that would be circular.

use stemma::api::{Document, validate};
use stemma::{ExportOptions, Resolution};

// ─── Fixtures ──────────────────────────────────────────────────────────────

/// Minimal plain-paragraph DOCX.
fn make_test_docx(paragraphs: &[&str]) -> Vec<u8> {
    let mut body = String::new();
    for para in paragraphs {
        body.push_str(&format!(r#"<w:p><w:r><w:t>{para}</w:t></w:r></w:p>"#));
    }
    make_docx_with_body(&body)
}

/// A DOCX whose body inner XML is `body_inner` (so a test can inject opaque
/// inlines such as `<w:fldSimple>`).
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

/// Independent text oracle: the concatenated `<w:t>` content (excluding
/// `<w:delText>`) of a document's serialized `word/document.xml`. This is the
/// accept-all body text as the *serializer* sees it — derived without touching
/// `to_plain_text`, so a faithfulness comparison is not circular.
fn serialized_wt_text(doc: &Document) -> String {
    let bytes = doc.serialize(&ExportOptions::default()).expect("serialize");
    let archive = stemma::docx::DocxArchive::read(&bytes).expect("read archive");
    let xml = String::from_utf8(
        archive
            .get("word/document.xml")
            .expect("document.xml present")
            .to_vec(),
    )
    .expect("utf8");
    extract_w_t_text(&xml)
}

/// Pull the content of every `<w:t ...>...</w:t>` (skipping `<w:tbl>`/`<w:tc>`
/// etc. and `<w:delText>`).
fn extract_w_t_text(xml: &str) -> String {
    let mut out = String::new();
    let bytes = xml.as_bytes();
    let mut i = 0;
    while let Some(rel) = xml[i..].find("<w:t") {
        let tag_start = i + rel;
        let after = tag_start + 4;
        if after >= bytes.len() || (bytes[after] != b' ' && bytes[after] != b'>') {
            i = after;
            continue;
        }
        let Some(gt) = xml[tag_start..].find('>') else {
            break;
        };
        let content_start = tag_start + gt + 1;
        let Some(close_rel) = xml[content_start..].find("</w:t>") else {
            break;
        };
        out.push_str(&xml[content_start..content_start + close_rel]);
        i = content_start + close_rel + "</w:t>".len();
    }
    out
}

fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[test]
fn to_text_of_accepted_matches_serialized_body_oracle() {
    // diff(base, target) is a redline; its accept-all `to_text` must equal the
    // independent serialized `<w:t>` oracle on the accepted document, and carry
    // the replacement word, not the original.
    let base = Document::parse(&make_test_docx(&["The term is thirty days."])).expect("base");
    let target = Document::parse(&make_test_docx(&["The term is sixty days."])).expect("target");
    let redlined = base.diff(&target).expect("diff");

    let accepted = redlined.read_accepted().expect("accept");
    assert_eq!(
        norm(&accepted.to_text()),
        norm(&serialized_wt_text(&accepted)),
        "to_text(read_accepted) == serialized accept-all body"
    );
    assert!(accepted.to_text().contains("sixty"), "replacement present");
    assert!(
        !accepted.to_text().contains("thirty"),
        "original word gone after accept: {:?}",
        accepted.to_text()
    );
}

#[test]
fn reject_equals_baseline_and_accept_equals_target_at_text_layer() {
    let base = Document::parse(&make_test_docx(&["Hello world"])).expect("base");
    let target = Document::parse(&make_test_docx(&["Hello brave world"])).expect("target");
    let redlined = base.diff(&target).expect("diff");

    assert_eq!(
        norm(&redlined.read_rejected().expect("reject").to_text()),
        norm(&base.to_text()),
        "reject-all text == baseline text"
    );
    assert_eq!(
        norm(&redlined.read_accepted().expect("accept").to_text()),
        norm(&target.to_text()),
        "accept-all text == target text"
    );
}

#[test]
fn read_redline_markdown_carries_ins_and_del() {
    // The redline comprehension surface keeps tracked changes intact: a tracked
    // replacement shows both <ins> and <del>.
    let base = Document::parse(&make_test_docx(&["Hello world"])).expect("base");
    let target = Document::parse(&make_test_docx(&["Goodbye world"])).expect("target");
    let redlined = base.diff(&target).expect("diff");
    let md = redlined.to_markdown();
    // The markdown tags carry attributes (<ins id=..>, <del id=..>); assert on
    // the opening tag prefix so the test pins the ins/del semantics, not the
    // exact attribute set.
    assert!(md.contains("<ins"), "redline markdown carries <ins>: {md}");
    assert!(md.contains("<del"), "redline markdown carries <del>: {md}");
}

#[test]
fn opaque_anchor_survives_accept_all_projection() {
    // Opaque preservation across the clean projection: a field opaque present
    // before AcceptAll still surfaces its anchor id in the accepted document's
    // markdown, and its cached result still reads as text in `to_text` (the
    // human-readable surface). The fixture's fldSimple result is "Section 2".
    let body = r#"<w:p><w:r><w:t>See </w:t></w:r><w:fldSimple w:instr=" REF Defs \h "><w:r><w:t>Section 2</w:t></w:r></w:fldSimple><w:r><w:t> now</w:t></w:r></w:p>"#;
    let doc = Document::parse(&make_docx_with_body(body)).expect("parse");

    // The anchor id as the read view sees it (durable across projection).
    let anchor_id = {
        let view = doc.read();
        view.blocks[0]
            .segments
            .iter()
            .find_map(|s| match s {
                stemma::view::SegmentView::Opaque { id, .. } => Some(id.to_string()),
                _ => None,
            })
            .expect("one opaque anchor in the fixture")
    };

    let accepted = doc.read_accepted().expect("accept-all");
    let md = accepted.to_markdown();
    assert!(
        md.contains(&format!("id={anchor_id}")),
        "field anchor id must survive accept-all and surface in markdown: {md}"
    );
    // Human-readable surface: the field's cached result reads as text (it is not
    // a no-text object to a reader); the structural anchor still survives in the
    // IR / markdown above.
    assert_eq!(
        accepted.to_text(),
        "See Section 2 now",
        "field cached result reads as text on the human-readable surface after accept-all"
    );
    assert_eq!(
        accepted.to_text().matches('\u{FFFC}').count(),
        0,
        "a field carrying a cached result contributes no U+FFFC to the human-readable surface"
    );
}

#[test]
fn projections_are_valid_docx_and_reads_do_not_mutate() {
    // read_accepted/read_rejected return real Documents that serialize to valid
    // DOCX, and reading does not mutate the source (the source still has its
    // tracked change → its own to_text still carries both words).
    let base = Document::parse(&make_test_docx(&["alpha beta"])).expect("base");
    let target = Document::parse(&make_test_docx(&["alpha gamma"])).expect("target");
    let redlined = base.diff(&target).expect("diff");

    let accepted = redlined.read_accepted().expect("accept");
    let rejected = redlined.read_rejected().expect("reject");
    assert!(
        validate(&accepted.serialize(&ExportOptions::default()).expect("ser")).ok,
        "accepted projection serializes to valid DOCX"
    );
    assert!(
        validate(&rejected.serialize(&ExportOptions::default()).expect("ser")).ok,
        "rejected projection serializes to valid DOCX"
    );

    // The source redline is untouched: it still resolves both ways.
    assert!(
        redlined
            .read_accepted()
            .expect("re-accept")
            .to_text()
            .contains("gamma")
    );
    assert!(
        redlined
            .read_rejected()
            .expect("re-reject")
            .to_text()
            .contains("beta")
    );
    let _ = Resolution::AcceptAll; // import is load-bearing for the projections above
}
