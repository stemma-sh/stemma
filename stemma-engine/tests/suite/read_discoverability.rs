//! Read-surface discoverability invariants: the read view exposes, per block,
//! exactly what a COLD agent (read tools only) needs to author the verbs the
//! engine already has.
//!
//! A round-1 cold-agent baseline found every
//! failure was a read-surface gap: the verb existed but a cold agent could not
//! discover how to invoke it from read output. These tests pin the fix:
//!
//! 1. `BlockView::role_token` — an `insert`/`replace`-acceptable paragraph role
//!    is surfaced per block, INCLUDING for a `Normal`-styled doc where
//!    `style_id` is null. The token must be the SAME vocabulary the insert op
//!    validates against (asserted end-to-end: the surfaced token authors an
//!    insert that applies).
//! 2. `BlockView::cells` — a table block exposes each cell's `{row, col, text}`
//!    so a cold agent can locate "the cell containing X" and target
//!    `set_cell_text`.
//! 3. `BlockView::list` — a list paragraph exposes `{num_id, ilvl, ordered,
//!    marker_text}` so the granular list ops become targetable.

use stemma::api::Document;
use stemma::domain::{NodeId, RevisionInfo};
use stemma::edit::{
    BlockSpec, ContentFragment, EditStep, EditTransaction, InsertPosition, MaterializationMode,
    ParagraphBlockSpec, ParagraphContent,
};
use stemma::view::BlockRole;

// ─── Fixtures ─────────────────────────────────────────────────────────────────

/// A minimal DOCX whose body is `body_inner` (the inner-of-`w:body` XML), with
/// no styles part — so paragraphs are `Normal`-styled (style_id == null), the
/// exact case round 1 found unauthorable.
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

/// A DOCX carrying a real `word/numbering.xml` with `numId=1` (decimal) and
/// `numId=2` (bullet). `paras` is `(text, Some((num_id, ilvl)) | None)`.
/// Mirrors `tests/list_ops.rs`'s fixture so a parsed paragraph carries a real
/// `NumberingInfo` (the view's `list` projection reads it).
fn make_list_docx(paras: &[(&str, Option<(u32, u32)>)]) -> Vec<u8> {
    let mut document_xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#,
    );
    for (text, numbering) in paras {
        document_xml.push_str("<w:p>");
        if let Some((num_id, ilvl)) = numbering {
            document_xml.push_str(&format!(
                r#"<w:pPr><w:numPr><w:ilvl w:val="{ilvl}"/><w:numId w:val="{num_id}"/></w:numPr></w:pPr>"#
            ));
        }
        document_xml.push_str(&format!(r#"<w:r><w:t>{text}</w:t></w:r></w:p>"#));
    }
    document_xml.push_str("<w:sectPr/></w:body></w:document>");

    let numbering_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl><w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="lowerLetter"/><w:lvlText w:val="(%2)"/></w:lvl></w:abstractNum><w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/><w:lvlText w:val="&#8226;"/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num><w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num></w:numbering>"#;
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId10" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/></Relationships>"#;

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
        zip.start_file("word/numbering.xml", opts).unwrap();
        zip.write_all(numbering_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

// ─── 1. INSERT ROLE: a Normal doc surfaces an insert-acceptable role token ────

#[test]
fn normal_styled_block_surfaces_a_role_token_even_though_style_id_is_null() {
    // Domain rule: a paragraph with no named style ("Normal") carries style_id
    // == null in the read view — round 1's unauthorable case. The view must
    // ALSO surface `role_token`: a non-null token from the document's private
    // role vocabulary that the insert op accepts.
    let docx = make_docx_with_body(
        r#"<w:p><w:r><w:t>First body paragraph.</w:t></w:r></w:p><w:p><w:r><w:t>Second body paragraph.</w:t></w:r></w:p>"#,
    );
    let doc = Document::parse(&docx).expect("parse");
    let view = doc.read();

    for block in &view.blocks {
        assert_eq!(block.role, BlockRole::Paragraph);
        assert_eq!(
            block.style_id, None,
            "Normal-styled paragraph has null style_id"
        );
        let token = block
            .role_token
            .as_ref()
            .expect("a paragraph must surface an insert-acceptable role_token");
        assert!(
            !token.is_empty(),
            "role_token must be a usable token, not empty"
        );
    }
}

#[test]
fn the_surfaced_role_token_is_accepted_by_an_insert_op() {
    // Post-condition (the discoverability fix, proven end-to-end): the EXACT
    // token the read view surfaces resolves in the insert op's role lookup, so
    // a cold agent that read it can author a new paragraph. This is the
    // round-1 `insert-role` gap (8 tasks) closed.
    let docx =
        make_docx_with_body(r#"<w:p><w:r><w:t>An existing body paragraph.</w:t></w:r></w:p>"#);
    let doc = Document::parse(&docx).expect("parse");
    let view = doc.read();
    let anchor = view.blocks[0].id.clone();
    let role_token = view.blocks[0]
        .role_token
        .clone()
        .expect("anchor surfaces a role_token");

    let txn = insert_after(&anchor, &role_token, "A newly inserted paragraph.");
    let edited = doc
        .apply(&txn)
        .expect("insert with the surfaced role_token must apply");

    // The inserted paragraph is present in the accept-all reading.
    let accepted = edited.read_accepted().expect("accept-all");
    let accepted_view = accepted.read();
    let texts: Vec<&str> = accepted_view
        .blocks
        .iter()
        .map(|b| b.text.as_str())
        .collect();
    assert!(
        texts
            .iter()
            .any(|t| t.contains("A newly inserted paragraph.")),
        "the inserted paragraph must appear: {texts:?}"
    );
}

#[test]
fn the_default_alias_resolves_to_the_body_role_for_insert() {
    // Documented default (CLAUDE.md: an intentional, named default): a cold
    // agent that does not want to copy a specific block can pass the alias
    // "default" and the insert op maps it to the document's body role. No
    // silent fallback — an unknown role still fails (asserted below).
    let docx =
        make_docx_with_body(r#"<w:p><w:r><w:t>An existing body paragraph.</w:t></w:r></w:p>"#);
    let doc = Document::parse(&docx).expect("parse");
    let anchor = doc.read().blocks[0].id.clone();

    let ok = insert_after(&anchor, "default", "Inserted via the default alias.");
    doc.apply(&ok)
        .expect("the 'default' alias must resolve to the body role");

    // A genuinely unknown role fails loud (the error names what to pass).
    let bad = insert_after(&anchor, "no_such_role_xyz", "Should fail.");
    let err = match doc.apply(&bad) {
        Ok(_) => panic!("an unknown role must fail, never silently default"),
        Err(e) => e,
    };
    let msg = format!("{err:?}");
    assert!(
        msg.contains("no_such_role_xyz") && msg.contains("available roles"),
        "the error must name the bad role and the available roles: {msg}"
    );
}

// ─── 2. TABLE CELL COORDINATES: a table block exposes {row, col, text} ────────

#[test]
fn table_block_surfaces_each_cell_with_grid_coordinates_and_text() {
    // Domain rule: a Table block carries per-cell addressing — each cell's
    // 0-based {row, col} grid position and visible text — so a cold agent can
    // locate "the cell containing X" and target set_cell_text. (Round-1
    // `table-cell-coords` gap, 4 tasks.)
    let body = r#"<w:tbl>
        <w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr>
        <w:tr><w:tc><w:p><w:r><w:t>Region</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Status</w:t></w:r></w:p></w:tc></w:tr>
        <w:tr><w:tc><w:p><w:r><w:t>North</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Open</w:t></w:r></w:p></w:tc></w:tr>
    </w:tbl>"#;
    let doc = Document::parse(&make_docx_with_body(body)).expect("parse table docx");
    let view = doc.read();

    let table = view
        .blocks
        .iter()
        .find(|b| b.role == BlockRole::Table)
        .expect("a table block is present");
    assert_eq!(table.cells.len(), 4, "2x2 table → 4 addressable cells");

    // Each cell carries its true grid coordinates and its text.
    let at = |row: usize, col: usize| {
        table
            .cells
            .iter()
            .find(|c| c.row == row && c.col == col)
            .unwrap_or_else(|| panic!("cell ({row},{col}) must be addressable"))
    };
    assert_eq!(at(0, 0).text, "Region");
    assert_eq!(at(0, 1).text, "Status");
    assert_eq!(at(1, 0).text, "North");
    assert_eq!(at(1, 1).text, "Open");

    // "Locate the cell containing X" — the cold-agent workflow.
    let target = table
        .cells
        .iter()
        .find(|c| c.text == "Open")
        .expect("find the cell containing 'Open'");
    assert_eq!((target.row, target.col), (1, 1));
}

// ─── 3. LIST / NUMBERING METADATA: list paragraphs expose num_id/ilvl/kind ────

#[test]
fn list_paragraphs_surface_num_id_ilvl_and_ordered_vs_bullet() {
    // Domain rule: a paragraph using Word auto-numbering surfaces its list
    // membership — num_id, ilvl, and ordered-vs-bullet — so the granular list
    // ops (SetType/Indent/Outdent/Restart) are targetable. A non-list
    // paragraph surfaces `None`. (Round-1 list gaps, 5 tasks.)
    let docx = make_list_docx(&[
        ("Decimal level 0", Some((1, 0))),
        ("Decimal level 1", Some((1, 1))),
        ("Bullet item", Some((2, 0))),
        ("Plain paragraph", None),
    ]);
    let doc = Document::parse(&docx).expect("parse list docx");
    let view = doc.read();

    let l0 = view.blocks[0].list.as_ref().expect("decimal l0 is a list");
    assert_eq!((l0.num_id, l0.ilvl, l0.ordered), (1, 0, true));

    let l1 = view.blocks[1].list.as_ref().expect("decimal l1 is a list");
    assert_eq!((l1.num_id, l1.ilvl, l1.ordered), (1, 1, true));

    let bullet = view.blocks[2].list.as_ref().expect("bullet is a list");
    assert_eq!((bullet.num_id, bullet.ilvl, bullet.ordered), (2, 0, false));

    assert!(
        view.blocks[3].list.is_none(),
        "a non-numbered paragraph carries no list membership"
    );

    // A non-table paragraph carries no cell addressing.
    assert!(view.blocks[0].cells.is_empty(), "a paragraph has no cells");
}

// ─── helpers ──────────────────────────────────────────────────────────────────

fn insert_after(anchor: &NodeId, role: &str, text: &str) -> EditTransaction {
    EditTransaction {
        steps: vec![EditStep::InsertParagraphs {
            anchor_block_id: anchor.clone(),
            position: InsertPosition::After,
            rationale: None,
            blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                role: Some(role.to_string()),
                content: ParagraphContent {
                    fragments: vec![ContentFragment::Text(text.to_string())],
                },
                restart_numbering: false,
                list: None,
            })],
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("Test".to_string()),
            date: Some("2026-06-05T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}
