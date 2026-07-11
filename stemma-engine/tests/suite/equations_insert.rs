//! Integration tests for `InsertEquation` (Verb A) — inserting an OMML
//! equation (inline `m:oMath` / block `m:oMathPara`) as a tracked change.
//!
//! Contract under test (CLAUDE.md "no silent fallbacks"; domain-model §11):
//!  - the equation is added as a tracked insert: accept-all has the OMML opaque,
//!    reject-all reconstructs the baseline;
//!  - fail-loud at the edge: garbage XML ⇒ `EquationXmlInvalid`; a non-math
//!    fragment ⇒ `EquationNotMath`; a placement/root mismatch ⇒ `EquationNotMath`;
//!  - the opaque round-trips through serialize → reparse;
//!  - the post-serialization validator (Blocking) passes on the edited bytes.
//!
//! Daily tier, corpus-free (synthesized in-memory DOCX).

use stemma::api::Document;
use stemma::domain::{BlockNode, CanonDoc, InlineNode, NodeId, OpaqueKind, RevisionInfo};
use stemma::edit::{
    EditError, EditStep, EditTransaction, EquationPlacement, MaterializationMode, apply_transaction,
};
use stemma::{ExportMode, ExportOptions, Resolution, ValidatorLevel};

const M_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/math";

/// Minimal single-paragraph DOCX. The math namespace is declared on the root so
/// the inserted OMML fragment (which references the `m:` prefix) round-trips.
fn make_docx(text: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:m="{M_NS}"><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#
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

fn omath_inline() -> Vec<u8> {
    format!(r#"<m:oMath xmlns:m="{M_NS}"><m:r><m:t>x+1</m:t></m:r></m:oMath>"#).into_bytes()
}

fn omath_para() -> Vec<u8> {
    format!(
        r#"<m:oMathPara xmlns:m="{M_NS}"><m:oMath><m:r><m:t>E=mc^2</m:t></m:r></m:oMath></m:oMathPara>"#
    )
    .into_bytes()
}

fn first_block_id(canon: &CanonDoc) -> NodeId {
    match &canon.blocks[0].block {
        BlockNode::Paragraph(p) => p.id.clone(),
        _ => panic!("expected a paragraph"),
    }
}

fn txn(steps: Vec<EditStep>, mode: MaterializationMode) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: mode,
        revision: RevisionInfo {
            revision_id: 1,
            author: Some("Eq".to_string()),
            date: Some("2026-06-05T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

/// Count OMML opaque anchors (inline + block) in a canonical doc.
fn omml_anchor_count(canon: &CanonDoc) -> usize {
    let mut n = 0;
    for tb in &canon.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::OpaqueInline(o) = inline
                        && matches!(o.kind, OpaqueKind::OmmlInline | OpaqueKind::OmmlBlock)
                    {
                        n += 1;
                    }
                }
            }
        }
    }
    n
}

fn step(block_id: NodeId, expect: &str, omml: Vec<u8>, placement: EquationPlacement) -> EditStep {
    EditStep::InsertEquation {
        block_id,
        expect: expect.to_string(),
        semantic_hash: None,
        omml,
        placement,
        rationale: None,
    }
}

/// Inline equation: tracked insert. accept-all keeps the OMML opaque; reject-all
/// restores the baseline (zero OMML anchors).
#[test]
fn inline_equation_accept_has_omml_reject_is_baseline() {
    let doc = Document::parse(&make_docx("let be the unknown")).expect("parse");
    let block_id = first_block_id(&doc.snapshot().canonical);

    let edited = doc
        .apply(&txn(
            vec![step(
                block_id,
                "let",
                omath_inline(),
                EquationPlacement::Inline,
            )],
            MaterializationMode::TrackedChange,
        ))
        .expect("apply");

    // accept-all: the inline OMML opaque is present.
    let accepted = edited.project(Resolution::AcceptAll).expect("accept");
    assert_eq!(
        omml_anchor_count(&accepted.snapshot().canonical),
        1,
        "accept-all must keep the inserted inline equation"
    );

    // reject-all: baseline has no OMML.
    let rejected = edited.project(Resolution::RejectAll).expect("reject");
    assert_eq!(
        omml_anchor_count(&rejected.snapshot().canonical),
        0,
        "reject-all must reconstruct the baseline (no equation)"
    );
}

/// Block equation: tracked insert via the existing `m:oMathPara` tracked
/// container. accept-all keeps it; reject-all restores the baseline.
#[test]
fn block_equation_accept_has_omml_reject_is_baseline() {
    let doc = Document::parse(&make_docx("Einstein famously wrote")).expect("parse");
    let block_id = first_block_id(&doc.snapshot().canonical);

    let edited = doc
        .apply(&txn(
            vec![step(
                block_id,
                "wrote",
                omath_para(),
                EquationPlacement::Block,
            )],
            MaterializationMode::TrackedChange,
        ))
        .expect("apply");

    let accepted = edited.project(Resolution::AcceptAll).expect("accept");
    assert_eq!(
        omml_anchor_count(&accepted.snapshot().canonical),
        1,
        "accept-all must keep the inserted block equation"
    );

    let rejected = edited.project(Resolution::RejectAll).expect("reject");
    assert_eq!(
        omml_anchor_count(&rejected.snapshot().canonical),
        0,
        "reject-all must reconstruct the baseline (no equation)"
    );
}

/// Garbage that is not well-formed XML fails loud at the edge.
#[test]
fn garbage_fragment_fails_equation_xml_invalid() {
    let base = Document::parse(&make_docx("anchor here")).expect("parse");
    let canon = base.snapshot().canonical.clone();
    let block_id = first_block_id(&canon);
    let err = apply_transaction(
        &canon,
        &txn(
            vec![step(
                block_id,
                "anchor",
                b"<m:oMath <<not xml".to_vec(),
                EquationPlacement::Inline,
            )],
            MaterializationMode::TrackedChange,
        ),
    )
    .expect_err("must fail");
    assert!(
        matches!(err, EditError::EquationXmlInvalid { .. }),
        "got {err:?}"
    );
}

/// A well-formed but non-math fragment fails loud (`EquationNotMath`).
#[test]
fn non_math_fragment_fails_equation_not_math() {
    let base = Document::parse(&make_docx("anchor here")).expect("parse");
    let canon = base.snapshot().canonical.clone();
    let block_id = first_block_id(&canon);
    let err = apply_transaction(
        &canon,
        &txn(
            vec![step(
                block_id,
                "anchor",
                br#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:r><w:t>not math</w:t></w:r></w:p>"#.to_vec(),
                EquationPlacement::Inline,
            )],
            MaterializationMode::TrackedChange,
        ),
    )
    .expect_err("must fail");
    assert!(
        matches!(err, EditError::EquationNotMath { .. }),
        "got {err:?}"
    );
}

/// A placement/root mismatch (block placement, inline `m:oMath` root) is refused
/// rather than silently re-wrapped.
#[test]
fn placement_root_mismatch_fails_equation_not_math() {
    let base = Document::parse(&make_docx("anchor here")).expect("parse");
    let canon = base.snapshot().canonical.clone();
    let block_id = first_block_id(&canon);
    let err = apply_transaction(
        &canon,
        &txn(
            vec![step(
                block_id,
                "anchor",
                omath_inline(), // m:oMath, but placement says Block (wants m:oMathPara)
                EquationPlacement::Block,
            )],
            MaterializationMode::TrackedChange,
        ),
    )
    .expect_err("must fail");
    match err {
        EditError::EquationNotMath {
            actual_root,
            expected_root,
            ..
        } => {
            assert_eq!(actual_root, "oMath");
            assert_eq!(expected_root, "oMathPara");
        }
        other => panic!("got {other:?}"),
    }
}

/// The equation opaque round-trips through serialize → reparse, and the
/// post-serialization validator (Blocking) passes on the edited bytes.
#[test]
fn inline_equation_roundtrips_and_validates() {
    let doc = Document::parse(&make_docx("let be the unknown")).expect("parse");
    let block_id = first_block_id(&doc.snapshot().canonical);
    let edited = doc
        .apply(&txn(
            vec![step(
                block_id,
                "let",
                omath_inline(),
                EquationPlacement::Inline,
            )],
            MaterializationMode::TrackedChange,
        ))
        .expect("apply");

    // Serialize with the Blocking validator gate — must pass.
    let bytes = edited
        .serialize(&ExportOptions {
            mode: ExportMode::Redline,
            validator_level: ValidatorLevel::Blocking,
            validator: None,
        })
        .expect("serialize+validate");

    // Reparse: the OMML opaque survives the roundtrip.
    let reparsed = Document::parse(&bytes).expect("reparse");
    assert_eq!(
        omml_anchor_count(&reparsed.snapshot().canonical),
        1,
        "the inline equation must survive serialize → reparse"
    );
}

#[test]
fn block_equation_roundtrips_and_validates() {
    let doc = Document::parse(&make_docx("Einstein famously wrote")).expect("parse");
    let block_id = first_block_id(&doc.snapshot().canonical);
    let edited = doc
        .apply(&txn(
            vec![step(
                block_id,
                "wrote",
                omath_para(),
                EquationPlacement::Block,
            )],
            MaterializationMode::TrackedChange,
        ))
        .expect("apply");

    let bytes = edited
        .serialize(&ExportOptions {
            mode: ExportMode::Redline,
            validator_level: ValidatorLevel::Blocking,
            validator: None,
        })
        .expect("serialize+validate");

    let reparsed = Document::parse(&bytes).expect("reparse");
    assert_eq!(
        omml_anchor_count(&reparsed.snapshot().canonical),
        1,
        "the block equation must survive serialize → reparse"
    );
}
