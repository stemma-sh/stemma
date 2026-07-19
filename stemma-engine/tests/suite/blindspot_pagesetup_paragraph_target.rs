//! Blindspot regression: `SetPageSetup` with `SectionTarget::Paragraph`.
//!
//! Every existing page-setup test (`sections_page_setup.rs`,
//! `spec_section_page_setup.rs`) targets `SectionTarget::Body`. The
//! paragraph-target branch in `page_setup.rs::resolve_section_mut`
//! (lines 170-191) resolves a DIFFERENT pair of slots — the paragraph's
//! own `section_properties` / `section_property_change` — and the
//! serializer emits a *paragraph-scoped* `w:sectPr` / `w:sectPrChange`
//! inside that paragraph's `w:pPr` (`serialize/mod.rs::build_paragraph_sect_pr`,
//! around line 1119). That branch is never exercised.
//!
//! DOMAIN-CORRECT BEHAVIOR (the verb's documented contract +
//! ECMA-376 §17.6 / §17.13.5.32):
//!  - `SetPageSetup{ target: Paragraph(id) }` must write the patch into THAT
//!    paragraph's `section_properties` slot, leaving the body section untouched.
//!  - In `TrackedChange` mode it records the prior `w:sectPr` as a
//!    `w:sectPrChange` on that paragraph's `section_property_change` slot
//!    (NOT the body's).
//!  - `reject_all` restores the original paragraph sectPr; `accept_all` keeps
//!    the new layout and equals a `Direct` apply.
//!  - Serialization places the paragraph-scoped `w:sectPr` (with its
//!    `w:sectPrChange`) inside that paragraph's `w:pPr`, and the body sectPr
//!    keeps its own (unchanged) value.
//!
//! If the paragraph branch misroutes (writes the body slot) or panics → FAIL /
//! pipeline_bug_confirmed. If it routes correctly → PASS / gap now covered.

use stemma::api::Document;
use stemma::domain::{BlockNode, PageOrientation, RevisionInfo};
use stemma::edit::{
    EditStep, EditTransaction, MaterializationMode, PageMargins, PageSetupPatch, SectionTarget,
    apply_transaction,
};
use stemma::runtime::ExportOptions;
use stemma::{accept_all, reject_all_with_styles};

/// Build a DOCX with TWO body paragraphs where the FIRST paragraph owns a
/// mid-document section break (`w:sectPr` inside its `w:pPr`), landscape with a
/// two-column layout. The body's final `w:sectPr` is PORTRAIT (a different
/// orientation) so we can prove the edit touched only the paragraph slot.
fn make_paragraph_sectpr_docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:pPr><w:sectPr>
<w:pgSz w:w="16838" w:h="11906" w:orient="landscape"/>
<w:pgMar w:top="720" w:bottom="720" w:left="1440" w:right="1440" w:header="360" w:footer="360" w:gutter="0"/>
<w:cols w:num="2" w:space="708"/>
</w:sectPr></w:pPr><w:r><w:t>Section one ends here.</w:t></w:r></w:p>
<w:p><w:r><w:t>Section two body paragraph.</w:t></w:r></w:p>
<w:sectPr>
<w:pgSz w:w="11906" w:h="16838"/>
<w:pgMar w:top="1440" w:bottom="1440" w:left="1440" w:right="1440" w:header="720" w:footer="720" w:gutter="0"/>
</w:sectPr>
</w:body></w:document>"#;

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

fn txn(steps: Vec<EditStep>, mode: MaterializationMode) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: mode,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("Tester".to_string()),
            date: Some("2026-06-01T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

/// The first paragraph's addressable NodeId (matches `find_block_index`'s
/// `block_id_of`, which keys on the inner paragraph node's `id`).
fn first_para_id(doc: &stemma::domain::CanonDoc) -> stemma::domain::NodeId {
    match &doc.blocks[0].block {
        BlockNode::Paragraph(p) => p.id.clone(),
        _ => panic!("first block must be a paragraph"),
    }
}

/// Fetch the first paragraph's section_properties / section_property_change.
fn first_para_section(
    doc: &stemma::domain::CanonDoc,
) -> (
    &Option<stemma::domain::SectionProperties>,
    &Option<stemma::domain::SectionPropertyChange>,
) {
    match &doc.blocks[0].block {
        BlockNode::Paragraph(p) => (&p.section_properties, &p.section_property_change),
        _ => panic!("first block must be a paragraph"),
    }
}

/// Sanity: the fixture parses with a paragraph-owned landscape break and a
/// portrait body section. This is the precondition the verb operates on.
#[test]
fn fixture_has_paragraph_scoped_section_break() {
    let doc = Document::parse(&make_paragraph_sectpr_docx()).expect("parse");
    let base = doc.snapshot().canonical.clone();

    let (para_sp, _) = first_para_section(&base);
    let para_sp = para_sp
        .as_ref()
        .expect("first paragraph owns a mid-document section break");
    assert_eq!(
        para_sp.orientation,
        Some(PageOrientation::Landscape),
        "paragraph break is landscape"
    );
    assert_eq!(para_sp.columns, Some(2), "paragraph break has 2 columns");

    let body_sp = base
        .body_section_properties
        .as_ref()
        .expect("body sectPr parsed");
    // Body has no explicit w:orient → orientation None (portrait is the default).
    assert_eq!(
        body_sp.page_width,
        Some(11906),
        "body is portrait page size"
    );
}

/// Core blindspot: `SetPageSetup{ target: Paragraph }` writes the PARAGRAPH
/// slot (flips its orientation to portrait), records the prior sectPr in the
/// PARAGRAPH's `section_property_change`, and leaves the body section
/// completely untouched. reject_all restores the paragraph; accept_all keeps it
/// and equals Direct.
#[test]
fn set_page_setup_paragraph_target_routes_to_paragraph_slot() {
    let doc = Document::parse(&make_paragraph_sectpr_docx()).expect("parse");
    let base = doc.snapshot().canonical.clone();
    let first_id = first_para_id(&base);

    let body_before = base.body_section_properties.clone();
    let (para_before, _) = first_para_section(&base);
    let para_before = para_before.clone();
    assert_eq!(
        para_before.as_ref().unwrap().orientation,
        Some(PageOrientation::Landscape)
    );

    let steps = vec![EditStep::SetPageSetup {
        target: SectionTarget::Paragraph(first_id.clone()),
        patch: PageSetupPatch {
            orientation: Some(PageOrientation::Portrait),
            margins: Some(PageMargins {
                top: 2000,
                bottom: 2000,
                left: 2000,
                right: 2000,
                header: 900,
                footer: 900,
            }),
            ..Default::default()
        },
        semantic_hash: None,
        rationale: None,
    }];

    let tracked = apply_transaction(
        &base,
        &txn(steps.clone(), MaterializationMode::TrackedChange),
    )
    .expect("tracked apply")
    .0;

    // (1) The PARAGRAPH slot carries the new layout + a sectPrChange.
    let (para_props, para_change) = first_para_section(&tracked);
    let para_props = para_props
        .as_ref()
        .expect("paragraph still owns its section break");
    assert_eq!(
        para_props.orientation,
        Some(PageOrientation::Portrait),
        "paragraph break flipped to portrait"
    );
    assert_eq!(
        para_props.margin_top,
        Some(2000),
        "paragraph break got the new top margin"
    );
    assert!(
        para_change.is_some(),
        "tracked SetPageSetup on a paragraph records a paragraph-scoped w:sectPrChange"
    );

    // (2) The BODY section must be byte-for-byte untouched (no misroute).
    assert_eq!(
        tracked.body_section_properties, body_before,
        "body section properties must be untouched by a Paragraph-target edit"
    );
    assert!(
        tracked.body_section_property_change.is_none(),
        "body section must NOT receive the sectPrChange from a Paragraph-target edit"
    );

    // (3) reject_all restores the original PARAGRAPH sectPr exactly.
    let mut rejected = tracked.clone();
    reject_all_with_styles(&mut rejected, None);
    let (rej_props, rej_change) = first_para_section(&rejected);
    assert_eq!(
        rej_props.as_ref().unwrap().orientation,
        Some(PageOrientation::Landscape),
        "reject restores the prior paragraph orientation"
    );
    assert_eq!(
        rej_props.as_ref().unwrap().margin_top,
        para_before.as_ref().unwrap().margin_top,
        "reject restores the prior paragraph margins"
    );
    assert_eq!(
        rej_props.as_ref().unwrap().columns,
        Some(2),
        "reject restores the prior paragraph column layout"
    );
    assert!(
        rej_change.is_none(),
        "reject clears the paragraph sectPrChange"
    );

    // (4) accept_all keeps the new layout and equals a Direct apply.
    let mut accepted = tracked.clone();
    accept_all(&mut accepted);
    let direct = apply_transaction(&base, &txn(steps, MaterializationMode::Direct))
        .expect("direct apply")
        .0;
    let (acc_props, acc_change) = first_para_section(&accepted);
    let (dir_props, _) = first_para_section(&direct);
    assert_eq!(
        acc_props.as_ref().unwrap().orientation,
        dir_props.as_ref().unwrap().orientation,
        "accept paragraph orientation equals direct"
    );
    assert_eq!(
        acc_props.as_ref().unwrap().margin_top,
        dir_props.as_ref().unwrap().margin_top,
        "accept paragraph margins equal direct"
    );
    assert!(
        acc_change.is_none(),
        "accept clears the paragraph sectPrChange"
    );
}

/// The tracked paragraph-target edit serializes a paragraph-scoped
/// `w:sectPr` carrying a `w:sectPrChange`, and the new layout survives a
/// re-parse on the SAME paragraph slot (not the body).
#[test]
fn paragraph_target_serializes_paragraph_scoped_sectpr_change() {
    let doc = Document::parse(&make_paragraph_sectpr_docx()).expect("parse");
    let base = doc.snapshot().canonical.clone();
    let first_id = first_para_id(&base);

    let steps = vec![EditStep::SetPageSetup {
        target: SectionTarget::Paragraph(first_id),
        patch: PageSetupPatch {
            orientation: Some(PageOrientation::Portrait),
            ..Default::default()
        },
        semantic_hash: None,
        rationale: None,
    }];

    let edited = doc
        .apply(&txn(steps, MaterializationMode::TrackedChange))
        .expect("tracked paragraph SetPageSetup applies");
    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize tracked paragraph redline");

    // The serialized document must carry NO dangling relationship references
    // (the paragraph sectPrChange must resolve its story refs like the body
    // path does). A dangling ref is invalid OOXML (Word "needs repair").
    let validation = stemma::docx_validate::validate_docx(&bytes);
    let dangling: Vec<_> = validation
        .findings
        .iter()
        .filter(|f| f.rule_id == "I-REL-001")
        .collect();
    assert!(
        dangling.is_empty(),
        "paragraph-target SetPageSetup serialized {} dangling relationship reference(s): {:#?}",
        dangling.len(),
        dangling
    );

    // Re-parse: the new portrait layout is on the FIRST paragraph's slot, and
    // the body section keeps its own portrait page size (untouched).
    let reparsed = Document::parse(&bytes).expect("re-parse");
    let rebase = reparsed.snapshot().canonical.clone();
    let (rp_props, _) = first_para_section(&rebase);
    let rp_props = rp_props
        .as_ref()
        .expect("paragraph section break survives the byte trip");
    assert_eq!(
        rp_props.orientation,
        Some(PageOrientation::Portrait),
        "the new portrait layout survives roundtrip on the paragraph slot"
    );
    assert_eq!(
        rebase
            .body_section_properties
            .as_ref()
            .expect("body sectPr survives")
            .page_width,
        Some(11906),
        "body sectPr keeps its own page size across the edit + roundtrip"
    );
}
