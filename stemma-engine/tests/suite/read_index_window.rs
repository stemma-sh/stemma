//! Integration tests for the read-surface scale tier (roadmap A): the
//! structural index (`Document::outline`), the id-range windowing
//! (`Document::window`), and the `to_html` projection.
//!
//! Daily, corpus-free: every fixture is a synthesized in-memory DOCX.
//!
//! The load-bearing invariant is **windowed-read == slice-of-full-read**: for
//! each of the three slice renderers (plain text, extended markdown, HTML), the
//! window over `from..=to` must equal the same renderer applied to the
//! corresponding slice of the full read view. We assert this against an
//! INDEPENDENT oracle (the slice renderer applied to the full view's blocks),
//! never against `window` itself.

use stemma::api::{Document, WindowFormat};
use stemma::view::{block_range, build_outline};

// ─── Fixtures ──────────────────────────────────────────────────────────────

/// A multi-heading DOCX: H1, body, H2, body, H1, body. Heading levels come
/// from a `pStyle` referencing a Heading style; the import maps Heading{N}
/// styles to heading levels.
fn make_multi_heading_docx() -> Vec<u8> {
    let body = concat!(
        r#"<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Article One</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Body of article one.</w:t></w:r></w:p>"#,
        r#"<w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr><w:r><w:t>Section 1.1</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Body of section one one.</w:t></w:r></w:p>"#,
        r#"<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Article Two</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Body of article two.</w:t></w:r></w:p>"#,
    );
    make_docx_with_body(body)
}

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

// ─── Structural index ──────────────────────────────────────────────────────

#[test]
fn outline_shadows_blocks_one_to_one_and_totals_match() {
    // Faithfulness invariant: one entry per block, in order, with index == i and
    // id == blocks[i].id; totals are the block count and the char-sum.
    let doc = Document::parse(&make_multi_heading_docx()).expect("parse");
    let view = doc.read();
    let outline = doc.outline();

    assert_eq!(outline.entries.len(), view.blocks.len());
    assert_eq!(outline.total_blocks, view.blocks.len());
    let char_sum: usize = view.blocks.iter().map(|b| b.text.chars().count()).sum();
    assert_eq!(
        outline.total_chars, char_sum,
        "total_chars == sum of per-block char_len"
    );

    for (i, (entry, block)) in outline.entries.iter().zip(&view.blocks).enumerate() {
        assert_eq!(entry.index, i);
        assert_eq!(entry.id, block.id);
        assert_eq!(entry.role, block.role);
        assert_eq!(entry.char_len, block.text.chars().count());
        assert_eq!(entry.byte_len, block.text.len());
    }
}

#[test]
fn outline_depth_follows_heading_structure() {
    // Domain rule: depth tracks the nearest preceding heading level. The fixture
    // is H1, body, H2, body, H1, body → depths 1,1,2,2,1,1.
    let doc = Document::parse(&make_multi_heading_docx()).expect("parse");
    let outline = doc.outline();
    let depths: Vec<u8> = outline.entries.iter().map(|e| e.depth).collect();
    assert_eq!(
        depths,
        vec![1, 1, 2, 2, 1, 1],
        "depth follows heading nesting"
    );
}

// ─── Windowed-read == slice-of-full-read ───────────────────────────────────

#[test]
fn windowed_text_equals_slice_of_full_text() {
    let doc = Document::parse(&make_multi_heading_docx()).expect("parse");
    let view = doc.read();
    // Window the H2 section: blocks [2..=3].
    let from = view.blocks[2].id.to_string();
    let to = view.blocks[3].id.to_string();

    let oracle = stemma::view::to_plain_text_blocks(&view.blocks[2..=3]);
    let windowed = doc.window(&from, &to, WindowFormat::Text).expect("window");
    assert_eq!(windowed, oracle, "windowed text == slice-of-full text");
}

#[test]
fn windowed_markdown_equals_slice_of_full_markdown() {
    let doc = Document::parse(&make_multi_heading_docx()).expect("parse");
    let view = doc.read();
    let from = view.blocks[0].id.to_string();
    let to = view.blocks[3].id.to_string();

    let oracle = stemma::extended_markdown::to_extended_markdown_blocks(&view.blocks[0..=3]);
    let windowed = doc
        .window(&from, &to, WindowFormat::Markdown)
        .expect("window");
    assert_eq!(
        windowed, oracle,
        "windowed markdown == slice-of-full markdown"
    );
}

#[test]
fn windowed_html_equals_slice_of_full_html() {
    let doc = Document::parse(&make_multi_heading_docx()).expect("parse");
    let view = doc.read();
    let from = view.blocks[1].id.to_string();
    let to = view.blocks[5].id.to_string();

    let oracle = stemma::html::to_html_blocks(&view.blocks[1..=5]);
    let windowed = doc.window(&from, &to, WindowFormat::Html).expect("window");
    assert_eq!(windowed, oracle, "windowed html == slice-of-full html");

    // And the full-document window equals the full-document render.
    let all_from = view.blocks[0].id.to_string();
    let all_to = view.blocks[view.blocks.len() - 1].id.to_string();
    assert_eq!(
        doc.window(&all_from, &all_to, WindowFormat::Html)
            .expect("window all"),
        doc.to_html(),
        "the all-blocks window == to_html"
    );
}

// ─── Fail-loud window addressing ───────────────────────────────────────────

#[test]
fn window_fails_loud_on_unknown_id() {
    let doc = Document::parse(&make_multi_heading_docx()).expect("parse");
    let view = doc.read();
    let real = view.blocks[0].id.to_string();
    let err = doc
        .window("does_not_exist", &real, WindowFormat::Text)
        .expect_err("unknown from id must fail");
    assert_eq!(
        err,
        stemma::view::WindowError::AnchorNotFound("does_not_exist".to_string())
    );
}

#[test]
fn window_fails_loud_on_out_of_order_endpoints() {
    let doc = Document::parse(&make_multi_heading_docx()).expect("parse");
    let view = doc.read();
    let earlier = view.blocks[1].id.to_string();
    let later = view.blocks[4].id.to_string();
    // from after to.
    match doc.window(&later, &earlier, WindowFormat::Text) {
        Err(stemma::view::WindowError::OutOfOrder { from, to }) => {
            assert!(from > to, "from must come after to in document order");
        }
        other => panic!("expected OutOfOrder, got {other:?}"),
    }
}

#[test]
fn block_range_and_window_agree() {
    // The Document::window facade and the bare block_range projection resolve the
    // same slice (window is a thin wrapper).
    let doc = Document::parse(&make_multi_heading_docx()).expect("parse");
    let view = doc.read();
    let from = view.blocks[2].id.to_string();
    let to = view.blocks[4].id.to_string();
    let slice = block_range(&view, &from, &to).expect("range");
    let via_outline: Vec<usize> = build_outline(&view)
        .entries
        .iter()
        .filter(|e| e.index >= 2 && e.index <= 4)
        .map(|e| e.index)
        .collect();
    assert_eq!(slice.len(), via_outline.len());
    assert_eq!(
        doc.window(&from, &to, WindowFormat::Text).expect("window"),
        stemma::view::to_plain_text_blocks(slice),
    );
}
