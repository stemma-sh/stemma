//! Accept/reject of a tracked paragraph-style change must re-resolve the runs'
//! STYLE-INHERITED marks against the resulting style (ECMA-376 §17.13.5.29 +
//! §17.7.2 style cascade).
//!
//! Domain rule: a run's effective formatting is resolved through the paragraph
//! style's `rPr` at import time — a caps-bearing style bakes `caps=On` onto a
//! run that authored no `w:caps` of its own. When a tracked `w:pPrChange` swaps
//! the paragraph style, accept/reject changes `w:pStyle` but the runs keep the
//! marks they inherited from the OTHER style unless they are re-resolved. So:
//!
//! - reject a change TO a caps style ⇒ runs lose the inherited caps (baseline);
//! - reject a change AWAY from a caps style ⇒ runs regain the caps;
//! - accept keeps the applied style's inherited marks.
//!
//! The witness is a wild-Word-authored pattern: a plain paragraph re-styled to a
//! caps-bearing style renders uppercase, and rejecting that suggestion must
//! bring back the original mixed case. Before the fix, reject and accept
//! produced the SAME (uppercased) text stream.
//!
//! These drive the real pipeline — apply a tracked style, serialize the redline,
//! re-import it (which bakes the style-inherited marks onto the runs), then
//! project accept / reject — so the model projection sees exactly what a saved,
//! reopened redline carries.

use std::collections::HashSet;
use std::io::Write;

use stemma::api::Document;
use stemma::domain::*;
use stemma::edit::*;
// `reject_all` is deprecated in favor of `reject_all_with_styles`; this file
// imports the bare form ON PURPOSE to characterize its documented degraded
// contract (see `bare_reject_all_is_degraded_without_style_table`).
#[allow(deprecated)]
use stemma::reject_all;
use stemma::{
    DocxRuntime, ExportOptions, Resolution, ResolveSelectionAction, SimpleRuntime,
    reject_all_with_styles, resolve_selected_revisions_with_styles, style_table_from_docx,
};
use zip::write::FileOptions;

/// styles.xml defining the paragraph styles the witnesses swap between. `Sigle`
/// and `Titre` carry `<w:caps/>` in their style `rPr`; `StrongBody` carries
/// `<w:b/>`; `Corps` and the default `Normal` carry neither.
const STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>
  <w:style w:type="paragraph" w:styleId="Sigle"><w:name w:val="Sigle"/><w:rPr><w:caps/></w:rPr></w:style>
  <w:style w:type="paragraph" w:styleId="Titre"><w:name w:val="Titre"/><w:rPr><w:caps/></w:rPr></w:style>
  <w:style w:type="paragraph" w:styleId="Corps"><w:name w:val="Corps"/></w:style>
  <w:style w:type="paragraph" w:styleId="StrongBody"><w:name w:val="StrongBody"/><w:rPr><w:b/></w:rPr></w:style>
</w:styles>"#;

/// Pack a DOCX whose body is `body_xml` and that carries the `STYLES_XML`
/// style table (loaded by well-known path — no explicit relationship needed).
fn make_docx(body_xml: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_xml}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#;
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
        zip.start_file("word/styles.xml", opts).unwrap();
        zip.write_all(STYLES_XML.as_bytes()).unwrap();
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
            author: Some("Styler".to_string()),
            date: Some("2026-06-01T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

/// Apply a TRACKED paragraph-style change to the first block and serialize the
/// redline — the bytes a saved suggestion carries. Re-importing these bakes the
/// style-inherited marks onto the runs (resolved against the applied style).
fn tracked_restyle_redline_bytes(body_xml: &str, new_style: &str) -> Vec<u8> {
    let doc = Document::parse(&make_docx(body_xml)).expect("parse base");
    let block_id = NodeId::from(doc.read().blocks[0].id.to_string());
    let edited = doc
        .apply(&txn(
            vec![EditStep::ApplyStyle {
                block_id,
                semantic_hash: None,
                style_id: new_style.to_string(),
                rationale: None,
            }],
            MaterializationMode::TrackedChange,
        ))
        .expect("apply tracked style");
    edited
        .serialize(&ExportOptions::default())
        .expect("serialize redline")
}

/// The redline round-tripped back through import — what a reopened suggestion is.
fn tracked_restyle_then_reimport(body_xml: &str, new_style: &str) -> Document {
    Document::parse(&tracked_restyle_redline_bytes(body_xml, new_style)).expect("reimport redline")
}

/// A bare `CanonDoc` imported from `bytes`, the way an embedder (and the wild-
/// corpus harness) obtains one: `SimpleRuntime::import_docx` then clone the
/// canonical. Carries no style table of its own.
fn bare_canon_from_docx(bytes: &[u8]) -> CanonDoc {
    let runtime = SimpleRuntime::new();
    let import = runtime.import_docx(bytes).expect("import_docx");
    (*import.canonical).clone()
}

fn first_para(canon: &CanonDoc) -> &ParagraphNode {
    match &canon.blocks[0].block {
        BlockNode::Paragraph(p) => p,
        _ => panic!("first block is not a paragraph"),
    }
}

fn first_run(canon: &CanonDoc) -> &TextNode {
    for seg in &first_para(canon).segments {
        for inline in &seg.inlines {
            if let InlineNode::Text(t) = inline {
                return t;
            }
        }
    }
    panic!("first paragraph has no text run");
}

fn first_run_caps(canon: &CanonDoc) -> MarkValue {
    first_run(canon).style_props.caps.clone()
}

/// The paragraph's visible text after applying the `w:caps` render rule
/// (`style_props.caps == On` uppercases the run) — the "text stream" the redline
/// extraction produces.
fn rendered_text(canon: &CanonDoc) -> String {
    let mut out = String::new();
    for seg in &first_para(canon).segments {
        for inline in &seg.inlines {
            if let InlineNode::Text(t) = inline {
                if t.style_props.caps == MarkValue::On {
                    out.push_str(&t.text.to_uppercase());
                } else {
                    out.push_str(&t.text);
                }
            }
        }
    }
    out
}

/// The pPrChange's MINTED IDENTITY on the first paragraph — the handle a
/// caller addresses for selective resolution (H7), not the raw wire
/// `revision_id`.
fn first_para_pprchange_id(canon: &CanonDoc) -> u32 {
    first_para(canon)
        .formatting_change
        .as_ref()
        .expect("first paragraph carries a pPrChange")
        .identity
}

// ─── Forward direction: plain paragraph re-styled to a caps-bearing style ─────

#[test]
fn reject_change_to_caps_style_restores_original_case() {
    let reimported = tracked_restyle_then_reimport(
        r#"<w:p><w:r><w:t>Formulaire de demande</w:t></w:r></w:p>"#,
        "Sigle",
    );

    // Precondition: the reopened redline resolved the run against `Sigle`, so
    // the inherited caps is baked on and the text renders uppercase.
    assert_eq!(
        first_run_caps(&reimported.snapshot().canonical),
        MarkValue::On,
        "reimported redline must carry style-inherited caps=On from Sigle"
    );
    assert_eq!(
        rendered_text(&reimported.snapshot().canonical),
        "FORMULAIRE DE DEMANDE"
    );

    // Reject: the style reverts to Normal (no caps), so the run must lose the
    // inherited caps and the ORIGINAL mixed case must come back.
    let rejected = reimported.read_rejected().expect("reject-all");
    assert_ne!(
        first_run_caps(&rejected.snapshot().canonical),
        MarkValue::On,
        "reject must re-resolve the run against Normal and drop the inherited caps"
    );
    assert_eq!(
        rendered_text(&rejected.snapshot().canonical),
        "Formulaire de demande",
        "reject-all text stream must equal the original mixed case"
    );

    // Accept: the style stays Sigle, so the caps rendering is kept.
    let accepted = reimported.read_accepted().expect("accept-all");
    assert_eq!(
        first_run_caps(&accepted.snapshot().canonical),
        MarkValue::On
    );
    assert_eq!(
        rendered_text(&accepted.snapshot().canonical),
        "FORMULAIRE DE DEMANDE"
    );
}

// ─── Reverse direction: caps heading re-styled to a plain style ───────────────

#[test]
fn reject_change_away_from_caps_style_restores_caps() {
    let reimported = tracked_restyle_then_reimport(
        r#"<w:p><w:pPr><w:pStyle w:val="Titre"/></w:pPr><w:r><w:t>Section 102800</w:t></w:r></w:p>"#,
        "Corps",
    );

    // Precondition: under `Corps` the run inherits no caps.
    assert_ne!(
        first_run_caps(&reimported.snapshot().canonical),
        MarkValue::On,
        "reimported redline under Corps must carry no inherited caps"
    );
    assert_eq!(
        rendered_text(&reimported.snapshot().canonical),
        "Section 102800"
    );

    // Reject: the style reverts to `Titre` (caps), so the run must REGAIN the
    // inherited caps and render uppercase again.
    let rejected = reimported.read_rejected().expect("reject-all");
    assert_eq!(
        first_run_caps(&rejected.snapshot().canonical),
        MarkValue::On,
        "reject must re-resolve the run against Titre and restore the inherited caps"
    );
    assert_eq!(
        rendered_text(&rejected.snapshot().canonical),
        "SECTION 102800",
        "reject-all must restore the caps heading's uppercase rendering"
    );

    // Accept keeps `Corps` — no caps.
    let accepted = reimported.read_accepted().expect("accept-all");
    assert_ne!(
        first_run_caps(&accepted.snapshot().canonical),
        MarkValue::On
    );
    assert_eq!(
        rendered_text(&accepted.snapshot().canonical),
        "Section 102800"
    );
}

// ─── A second style-inherited mark (bold) travels the same path ───────────────

#[test]
fn reject_change_to_bold_style_drops_inherited_bold() {
    let reimported = tracked_restyle_then_reimport(
        r#"<w:p><w:r><w:t>Total due</w:t></w:r></w:p>"#,
        "StrongBody",
    );

    // Bold is a `Vec<Mark>` member, not a `style_props` toggle — the same
    // style-cascade baking applies to it.
    assert!(
        first_run(&reimported.snapshot().canonical)
            .marks
            .contains(&Mark::Bold),
        "reimported redline under StrongBody must carry inherited bold"
    );

    let rejected = reimported.read_rejected().expect("reject-all");
    assert!(
        !first_run(&rejected.snapshot().canonical)
            .marks
            .contains(&Mark::Bold),
        "reject must re-resolve against Normal and drop the inherited bold"
    );

    let accepted = reimported.read_accepted().expect("accept-all");
    assert!(
        first_run(&accepted.snapshot().canonical)
            .marks
            .contains(&Mark::Bold),
        "accept keeps StrongBody, so the inherited bold stays"
    );
}

// ─── Selective (by-id) reject exercises the same re-resolution ────────────────

#[test]
fn selective_reject_of_pprchange_restores_original_case() {
    let reimported = tracked_restyle_then_reimport(
        r#"<w:p><w:r><w:t>Formulaire de demande</w:t></w:r></w:p>"#,
        "Sigle",
    );
    let pprchange_id = first_para_pprchange_id(&reimported.snapshot().canonical);

    let ids: HashSet<u32> = std::iter::once(pprchange_id).collect();
    let projected = reimported
        .snapshot()
        .project(Resolution::Selective {
            ids,
            action: ResolveSelectionAction::Reject,
        })
        .expect("selective reject");

    assert_ne!(
        first_run_caps(&projected.canonical),
        MarkValue::On,
        "selective reject of the pPrChange must re-resolve the run against Normal"
    );
    assert_eq!(rendered_text(&projected.canonical), "Formulaire de demande");
}

// ─── Public bare-CanonDoc path (the wild-corpus harness's normal form) ────────

/// The style-table-carrying public entry point re-resolves on a BARE `CanonDoc`
/// (no runtime projection) — the shape a corpus/embedder pipeline uses:
/// import → clone canonical → resolve. This is the correct replacement for the
/// bare `reject_all` on documents that may carry a tracked paragraph-style change.
#[test]
fn public_reject_all_with_styles_restores_case_on_bare_canondoc() {
    let bytes = tracked_restyle_redline_bytes(
        r#"<w:p><w:r><w:t>Formulaire de demande</w:t></w:r></w:p>"#,
        "Sigle",
    );
    let mut canon = bare_canon_from_docx(&bytes);

    // Baked precondition: import resolved the run against Sigle → caps=On.
    assert_eq!(first_run_caps(&canon), MarkValue::On);

    let styles = style_table_from_docx(&bytes).expect("parse style table");
    reject_all_with_styles(&mut canon, styles.as_ref());

    assert_ne!(
        first_run_caps(&canon),
        MarkValue::On,
        "reject_all_with_styles must re-resolve the run against Normal on a bare CanonDoc"
    );
    assert_eq!(rendered_text(&canon), "Formulaire de demande");
}

/// Selective by-id reject through the public style-table entry point, on a bare
/// `CanonDoc`.
#[test]
fn public_selective_reject_with_styles_restores_case_on_bare_canondoc() {
    let bytes = tracked_restyle_redline_bytes(
        r#"<w:p><w:r><w:t>Formulaire de demande</w:t></w:r></w:p>"#,
        "Sigle",
    );
    let mut canon = bare_canon_from_docx(&bytes);
    let pprchange_id = first_para_pprchange_id(&canon);
    let styles = style_table_from_docx(&bytes).expect("parse style table");

    let ids: HashSet<u32> = std::iter::once(pprchange_id).collect();
    resolve_selected_revisions_with_styles(
        &mut canon,
        &ids,
        ResolveSelectionAction::Reject,
        styles.as_ref(),
    )
    .expect("selective reject");

    assert_ne!(first_run_caps(&canon), MarkValue::On);
    assert_eq!(rendered_text(&canon), "Formulaire de demande");
}

/// CHARACTERIZATION of the DOCUMENTED degraded contract on the bare
/// `stemma::reject_all` (see its rustdoc): without the style table it cannot
/// undo the import-time baking, so a rejected caps-style leaves the run caps=On.
/// Pinned so the degraded surface can't silently drift — the correct entry point
/// is `reject_all_with_styles`, asserted above.
///
/// `#[allow(deprecated)]`: this test EXISTS to exercise the deprecated bare
/// `reject_all` and assert its degraded behavior, so the deprecation lint would
/// be firing on exactly the call the test is about. This is the one sanctioned
/// use of the bare function in the codebase.
#[allow(deprecated)]
#[test]
fn bare_reject_all_is_degraded_without_style_table() {
    let bytes = tracked_restyle_redline_bytes(
        r#"<w:p><w:r><w:t>Formulaire de demande</w:t></w:r></w:p>"#,
        "Sigle",
    );
    let mut canon = bare_canon_from_docx(&bytes);
    assert_eq!(first_run_caps(&canon), MarkValue::On);

    reject_all(&mut canon);

    assert_eq!(
        first_run_caps(&canon),
        MarkValue::On,
        "documented degraded behavior: bare reject_all has no style table to re-resolve against, \
         so the style-inherited caps stays baked — use reject_all_with_styles for fidelity"
    );
}
