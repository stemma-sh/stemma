//! Projection-contract invariant tests.
//!
//! These tests pin the contract between the Rust canonical model
//! (`CanonDoc`) and the `FullDocBlock` projection consumed by the
//! frontend. They live at a load-bearing seam that prior coverage
//! misses:
//!
//! - Engine round-trips test canonical → canonical (`cargo test --lib`).
//! - Word Oracle tests engine → serializer → real Word (nightly).
//! - Frontend tests render mock `FullDocBlock`s in isolation (vitest).
//!
//! Nothing was testing the engine → projection contract end-to-end on
//! real fixtures, so silent fall-throughs in `project_tracked_document`
//! (e.g. setting `table_diff = None` on a `BlockType::Table` block)
//! shipped without anything noticing.  The frontend's `redlineBuilder`
//! falls back to paragraph rendering when `block.table_diff` is null,
//! flattening every cell's text into a single `<p>`.
//!
//! Per CLAUDE.md "tests encode desired behavior, not current behavior":
//! these are contract tests on the projection, not snapshots of what it
//! happens to produce today.

use std::collections::HashMap;
use std::fs;

use stemma::diff::project_tracked_document;
use stemma::domain::{BlockType, FullDocBlock};
use stemma::{DocxRuntime, SimpleRuntime};

/// Import a DOCX and run the tracked-document projection over it.
fn project_fixture(doc_bytes: &[u8]) -> Vec<FullDocBlock> {
    let runtime = SimpleRuntime::new();
    let import = runtime.import_docx(doc_bytes).expect("import_docx");
    let view = runtime.view(&import.doc_handle).expect("view");
    let image_lookup: HashMap<String, String> = HashMap::new();
    project_tracked_document(&view.canonical, &image_lookup)
}

/// **Projection contract:** every block whose `block_type == Table`
/// must carry a populated `table_diff`.
///
/// The frontend's `redlineBuilder.ts` renders tables only when
/// `block.block_type === 'table' && block.table_diff` — without
/// `table_diff` the table-block atom node can't be constructed and the
/// renderer falls back to flat-paragraph rendering with all cell text
/// concatenated.  A `Table`-typed projection without `table_diff` is
/// therefore unrenderable and must not ship.
fn assert_table_blocks_carry_table_diff(blocks: &[FullDocBlock], label: &str) {
    let violations: Vec<String> = blocks
        .iter()
        .filter(|b| b.block_type == BlockType::Table && b.table_diff.is_none())
        .map(|b| {
            format!(
                "block_id='{}' change_type={:?} doc1_block_id={:?} doc2_block_id={:?}",
                b.block_id, b.change_type, b.doc1_block_id, b.doc2_block_id
            )
        })
        .collect();

    assert!(
        violations.is_empty(),
        "{label}: {} Table-typed block(s) projected without table_diff. \
         The frontend renders these as <p> with all cell text flattened, not as tables.\n  {}",
        violations.len(),
        violations.join("\n  ")
    );
}

/// **Projection contract:** every block whose `block_type == Heading`
/// must carry a populated `heading_level`.  The frontend's render
/// branches on heading level to pick the right `<h{N}>` tag — without
/// a level, headings would either fail to render or fall through to
/// `<p>`, hiding document outline structure.
fn assert_heading_blocks_carry_level(blocks: &[FullDocBlock], label: &str) {
    let violations: Vec<String> = blocks
        .iter()
        .filter(|b| b.block_type == BlockType::Heading && b.heading_level.is_none())
        .map(|b| format!("block_id='{}' change_type={:?}", b.block_id, b.change_type))
        .collect();

    assert!(
        violations.is_empty(),
        "{label}: {} Heading-typed block(s) projected without heading_level:\n  {}",
        violations.len(),
        violations.join("\n  ")
    );
}

/// Run every projection-contract invariant against one fixture.
fn assert_all_invariants(blocks: &[FullDocBlock], label: &str) {
    assert_table_blocks_carry_table_diff(blocks, label);
    assert_heading_blocks_carry_level(blocks, label);
}

/// Sweep every fixture that contains a `before.docx`.  Catches
/// projection-contract violations across the broadest possible
/// surface — the bug behind this test was unreachable from
/// programmatic CanonDocs but hits on every imported table.
///
/// `projection_table_changes_fixture_carries_table_diff` lives below
/// as a named test so failures point at the regression directly; the
/// sweep is the safety net for the rest of the corpus.
#[test]
fn projection_invariants_hold_for_all_testdata_fixtures() {
    let fixtures: Vec<String> = fs::read_dir("testdata")
        .expect("read testdata/")
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let before = entry.path().join("before.docx");
            before
                .exists()
                .then(|| before.to_string_lossy().to_string())
        })
        .collect();

    assert!(
        !fixtures.is_empty(),
        "no testdata/*/before.docx fixtures found; the sweep needs at least one"
    );

    let mut failures: Vec<(String, String)> = Vec::new();
    for fixture in &fixtures {
        let Ok(bytes) = fs::read(fixture) else {
            failures.push((fixture.clone(), "read failed".to_string()));
            continue;
        };
        // Capture any panic so a single bad fixture doesn't mask
        // failures in others.
        let result = std::panic::catch_unwind(|| {
            let blocks = project_fixture(&bytes);
            assert_all_invariants(&blocks, fixture);
        });
        if let Err(payload) = result {
            let msg = if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else if let Some(s) = payload.downcast_ref::<&str>() {
                s.to_string()
            } else {
                "panic with non-string payload".to_string()
            };
            failures.push((fixture.clone(), msg));
        }
    }

    assert!(
        failures.is_empty(),
        "projection-contract violations in {} of {} fixtures:\n{}",
        failures.len(),
        fixtures.len(),
        failures
            .iter()
            .map(|(f, m)| format!("  {f}:\n    {m}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The `table-changes` fixture is a single-document `before.docx`
/// containing a 2×2 table.  Running it through
/// `project_tracked_document` historically exposed the unchanged-table
/// projection path (around `diff.rs:5610`), which built a flat-text
/// segment from `extract_table_text` and set `table_diff: None`.  The
/// frontend then rendered the table as `<p data-block-id="tbl_1">…flat
/// text…</p>` instead of as a table.
///
/// This test is named explicitly so a regression on the same path
/// fails with a clear pointer.  The broader sweep above catches
/// fixtures we haven't named yet.
#[test]
fn projection_table_changes_fixture_carries_table_diff() {
    let bytes = fs::read("testdata/table-changes/before.docx")
        .expect("read testdata/table-changes/before.docx");
    let blocks = project_fixture(&bytes);
    assert_table_blocks_carry_table_diff(&blocks, "testdata/table-changes/before.docx");
}

// ═══════════════════════════════════════════════════════════════════════════
// Stacked carriers in the UN-resolved projection
// ═══════════════════════════════════════════════════════════════════════════

/// Minimal in-memory DOCX (same shape as cross_path_projection_equivalence.rs).
fn make_docx_with_body(body_inner: &str) -> Vec<u8> {
    use std::io::Write as _;
    use zip::write::FileOptions;
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

/// **Projection contract:** the legacy 3-value projection is TOTAL over the
/// stacked state (`InsertedThenDeleted`) on every carrier that can hold it —
/// inline segments, paragraph marks, table rows, table cells.
///
/// `single_document_view` (and the backend tracked-view endpoints) project
/// the UN-accepted snapshot canonical, so a real negotiated document with
/// stacked carriers reaches this projection unresolved. The contract is the
/// segment arm's documented coarsening: stacked content projects as a
/// PENDING DELETION (matching how Word renders the state — struck text); the
/// engine read view (`BlockView`) carries the full compound status for
/// consumers needing both attributions.
///
/// The paragraph-mark arm previously panicked
/// (`unreachable!("nothing constructs it")` — stale since the
/// stacked-carriers import landed).
#[test]
fn projection_is_total_over_stacked_carriers() {
    let ins_a = r#"w:id="1" w:author="AuthorA" w:date="2026-01-01T00:00:00Z""#;
    let del_b = r#"w:id="2" w:author="AuthorB" w:date="2026-02-01T00:00:00Z""#;

    // Stacked paragraph MARK + stacked inline SEGMENT + stacked table ROW
    // and CELL in one document.
    let body = format!(
        r#"<w:p><w:pPr><w:rPr><w:ins {ins_a}/><w:del {del_b}/></w:rPr></w:pPr><w:r><w:t xml:space="preserve">First part</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">Start </w:t></w:r><w:ins {ins_a}><w:del {del_b}><w:r><w:delText xml:space="preserve">contested </w:delText></w:r></w:del></w:ins><w:r><w:t>end.</w:t></w:r></w:p><w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="4675"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="4675" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>Row one.</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:trPr><w:ins {ins_a}/><w:del {del_b}/></w:trPr><w:tc><w:tcPr><w:tcW w:w="4675" w:type="dxa"/></w:tcPr><w:p><w:ins {ins_a}><w:del {del_b}><w:r><w:delText>Stacked row.</w:delText></w:r></w:del></w:ins></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:t>Tail.</w:t></w:r></w:p>"#
    );

    let runtime = SimpleRuntime::new();
    let import = runtime
        .import_docx(&make_docx_with_body(&body))
        .expect("import stacked-carriers fixture");
    // The UN-accepted snapshot canonical — what single_document_view projects.
    let image_lookup: HashMap<String, String> = HashMap::new();
    let blocks = project_tracked_document(&import.canonical, &image_lookup);

    // The stacked-mark paragraph must project (no panic) and its synthesized
    // paragraph-mark segment must be a pending DELETION of the break.
    let mark_block = &blocks[0];
    let has_deleted_break = mark_block
        .segments
        .iter()
        .any(|s| matches!(s, stemma::domain::InlineChange::Deleted { text, .. } if text == "\n"));
    assert!(
        has_deleted_break,
        "stacked paragraph mark must coarsen to a deleted break segment, got: {:?}",
        mark_block.segments
    );

    // The stacked inline segment coarsens to a pending deletion of its text.
    let seg_block = &blocks[1];
    let has_deleted_contested = seg_block.segments.iter().any(|s| {
        matches!(s, stemma::domain::InlineChange::Deleted { text, .. } if text.contains("contested"))
    });
    assert!(
        has_deleted_contested,
        "stacked inline segment must coarsen to a deletion, got: {:?}",
        seg_block.segments
    );

    // The table with a stacked row projects without panicking and surfaces
    // its tracked state (Modified with old-text carrying the stacked row's
    // content — stacked rows belong to neither the accepted nor the base
    // reading exclusively; the old/new split treats them like the resolution
    // rules do: absent from both readings).
    let table_block = blocks
        .iter()
        .find(|b| matches!(b.block_type, BlockType::Table))
        .expect("table block present");
    assert!(
        table_block.table_diff.is_some(),
        "tracked table must carry table_diff"
    );
}
