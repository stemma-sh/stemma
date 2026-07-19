//! Regression: a `w:sectPrChange`'s embedded
//! previous-state header/footer references corrupt `word/_rels/document.xml.rels`
//! (I-REL-003) the SECOND time the document is reserialized while the change
//! is still pending — reached here via a second `Document::apply` call, and
//! (verified separately, over MCP) via an explicit save→reopen→
//! `accept_changes` sequence too. The common thread is not "save to disk" per
//! se but "the sectPrChange's snapshot bytes get resolved once, then asked to
//! resolve again" — see ROOT CAUSE below for exactly where.
//!
//! Surfaced alongside the story/section-properties enumeration work (see
//! `spec_revision_enumeration.rs` / `spec_story_and_section_resolution.rs`),
//! but independent of it.
//! CONFIRMED PRE-EXISTING and INDEPENDENT of that fix: reproduces identically
//! on the unmodified base commit (7c6f857), before the enumeration fix
//! existed at all — verified at that commit, over MCP
//! (`accept_changes{by_author}` on a doc where list_revisions couldn't even
//! see the sectPrChange yet). The enumeration fix does not cause this and
//! does not generally fix it; it only happens to avoid the trigger in the
//! single most common case (every pending revision, including the
//! sectPrChange, resolved by the SAME `accept_changes{by_author}` call —
//! nothing is left dangling for a later resolve to re-touch). Any case that
//! leaves an authored sectPrChange genuinely PENDING while the document gets
//! reserialized a second time still corrupts.
//!
//! Former root cause (traced precisely, not guessed):
//!
//! 1. `apply_set_page_setup` (edit/verbs/page_setup.rs) snapshots the PRIOR
//!    section properties into `SectionPropertyChange.previous_properties_raw`
//!    via `previous_sect_pr_raw`, which calls `section_properties_to_element`
//!    with `resolve_rid: None`. With no resolver, a header/footerReference's
//!    `r:id` is written as the literal story PART PATH (e.g.
//!    "synthesized-blank-header-default.xml") — an intentional PLACEHOLDER,
//!    not a real relationship id, to be resolved later at serialize time.
//! 2. The FIRST reserialize (`runtime.rs`, the `body_section_property_change`
//!    branch around line 4660 — reached by `Document::apply`'s internal
//!    rebuild as much as by an explicit save) calls
//!    `resolve_sect_pr_change_story_refs`, which walks the embedded
//!    snapshot's header/footerReference elements and resolves each
//!    PLACEHOLDER into a real, registered rId (e.g. "rId9") — correctly, the
//!    FIRST time. But it does this by REWRITING the r:id attribute IN PLACE
//!    in the bytes that become the new `previous_properties_raw` on the next
//!    import/rebuild.
//! 3. Whatever reads those bytes next (a reimport, or `Document::apply`'s own
//!    internal rebuild of its returned snapshot) captures
//!    `previous_properties_raw` VERBATIM — now containing the
//!    ALREADY-RESOLVED rId ("rId9"), not a placeholder. The round-trip is NOT
//!    idempotent: nothing marks this raw blob as "already resolved".
//! 4. A SECOND reserialize (still with the sectPrChange pending) calls
//!    `resolve_sect_pr_change_story_refs` AGAIN. `resolve_story_part_to_rid`
//!    (runtime.rs) receives "rId9" as if it were still an unresolved
//!    PART PATH. It looks for a relationship whose TARGET equals "rId9"
//!    (none — real relationships have file-path targets), fails to find the
//!    "part" anywhere (there is no file named "rId9"), and — the actual
//!    silent-fallback bug — proceeds anyway rather than failing loud,
//!    minting a BRAND NEW relationship whose Target is the literal string
//!    "rId9". `validate_docx` then correctly flags this nonsense relationship
//!    as I-REL-003 ("target does not exist in the package").
//!
//! This violated the no-silent-fallback rule. `resolve_story_part_to_rid` now
//! recognizes an already-registered rId of the expected relationship type and
//! returns it unchanged, making repeated snapshot resolution idempotent.
//!
//! Reproduction requires a header/footer reference that survives a
//! save→reimport cycle while its sectPrChange stays unresolved — a
//! synthesized blank header (no header/footer in the original document) is
//! the simplest trigger, matching the held-out benchmark fixture this was
//! found during held-out benchmark validation (a sectPrChange fixture).

use std::collections::HashSet;

use stemma::api::Document;
use stemma::edit::{
    EditStep, EditTransaction, MaterializationMode, PageMargins, PageSetupPatch, SectionTarget,
};
use stemma::tracked_model::{ResolveSelectionAction, enumerate_revisions};
use stemma::{Resolution, RevisionInfo, StoryScope};

fn make_docx_with_body(body_inner: &str) -> Vec<u8> {
    // No header/footer at all — the importer synthesizes a blank default
    // header/footer for the first section, which is the trigger condition.
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

fn revision(id: u32, author: &str) -> RevisionInfo {
    RevisionInfo {
        revision_id: id,
        identity: 0,
        author: Some(author.to_string()),
        date: Some("2026-06-12T00:00:00Z".to_string()),
        apply_op_id: None,
    }
}

#[test]
fn sectpr_change_survives_a_save_reimport_partial_resolve_save_cycle() {
    const ONE_PARA: &str =
        r#"<w:p><w:r><w:t>Safety glasses are recommended in the packing area.</w:t></w:r></w:p>"#;
    let doc = Document::parse(&make_docx_with_body(ONE_PARA))
        .expect("parse (synthesizes blank header/footer)");

    // Author A: a tracked body sectPrChange.
    let doc = doc
        .apply(&EditTransaction {
            steps: vec![EditStep::SetPageSetup {
                target: SectionTarget::Body,
                patch: PageSetupPatch {
                    margins: Some(PageMargins {
                        top: 1080,
                        bottom: 1080,
                        left: 1080,
                        right: 1080,
                        header: 720,
                        footer: 720,
                    }),
                    ..Default::default()
                },
                semantic_hash: None,
                rationale: None,
            }],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: revision(300, "Author A"),
        })
        .expect("tracked sectPrChange applies");

    // THE CRITICAL STEP: save, then REIMPORT — the round-trip that bakes a
    // resolved rId into previous_properties_raw (see step 2-3 in the module
    // doc). Skipping this step (editing on the same in-memory Document) does
    // NOT reproduce the bug — confirmed empirically.
    let saved_once = doc
        .serialize(&stemma::ExportOptions {
            mode: stemma::ExportMode::Redline,
            validator_level: stemma::ValidatorLevel::Blocking,
            validator: None,
        })
        .expect("the FIRST save, with the sectPrChange still pending, must succeed clean");
    let doc = Document::parse(&saved_once).expect("reimport the once-saved doc");

    // Sanity: Author A's sectPrChange survived the round-trip and is still
    // enumerable, pending, and correctly located in the body — the
    // enumeration/resolution fix this file's sibling tests cover is NOT at
    // fault for what follows.
    assert!(
        doc.snapshot()
            .canonical
            .body_section_property_change
            .is_some(),
        "sanity: Author A's sectPrChange is still pending after the round-trip"
    );
    assert!(
        enumerate_revisions(&doc.snapshot().canonical)
            .iter()
            .any(|r| r.location == StoryScope::Body && r.author.as_deref() == Some("Author A")),
        "sanity: Author A's sectPrChange remains enumerable"
    );

    // Author B: an ordinary body text edit, unrelated to Author A's change.
    // THE BUG fires HERE, not at some later explicit save: `Document::apply`
    // rebuilds/reserializes its snapshot internally as part of applying this
    // second transaction, which is the "second reserialize while the
    // sectPrChange is still pending" from step 4 of the module doc — one
    // call earlier than a naive reading of "save -> reimport -> resolve ->
    // save" would suggest. (Confirmed separately, over MCP, that the
    // MCP-layer `SimpleRuntime::apply_edit` does NOT reserialize between
    // calls on the same open handle — there, the identical corruption is
    // reachable instead at the next explicit `save_docx`/`project` call. The
    // trigger is "a second reserialize with the change still pending",
    // whichever API surface causes it.)
    let view = doc.read();
    let target = view
        .blocks
        .iter()
        .find(|b| b.text.contains("recommended"))
        .expect("target paragraph");
    let author_b_result = doc.apply(&EditTransaction {
        steps: vec![EditStep::ReplaceParagraphText {
            block_id: target.id.clone(),
            rationale: None,
            replacement_role: None,
            expect: "Safety glasses are recommended in the packing area.".to_string(),
            semantic_hash: Some(target.guard.clone()),
            content: stemma::edit::ParagraphContent {
                fragments: vec![stemma::edit::ContentFragment::Text(
                    "Safety glasses are required in the packing area.".to_string(),
                )],
            },
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: revision(400, "Author B"),
    });

    assert!(
        author_b_result.is_ok(),
        "DOMAIN RULE: an unrelated body text edit, applied while a body \
         sectPrChange sits pending from an earlier save/reimport cycle, must \
         never corrupt package relationships: {:?}",
        author_b_result.as_ref().err()
    );

    // Author A's sectPrChange remains selectively resolvable and the document
    // remains clean after another save.
    let doc = author_b_result.unwrap();
    let author_b_ids: HashSet<u32> = enumerate_revisions(&doc.snapshot().canonical)
        .into_iter()
        .filter(|r| r.author.as_deref() == Some("Author B"))
        .map(|r| r.revision_id)
        .collect();
    assert!(
        !author_b_ids.is_empty(),
        "Author B's edit must be enumerable"
    );
    let resolved = doc
        .project(Resolution::Selective {
            ids: author_b_ids,
            action: ResolveSelectionAction::Accept,
        })
        .expect("selectively accepting Author B's edit must succeed");
    assert!(
        resolved
            .snapshot()
            .canonical
            .body_section_property_change
            .is_some(),
        "sanity: Author A's sectPrChange is still pending, as intended"
    );
    let second_save = resolved.serialize(&stemma::ExportOptions {
        mode: stemma::ExportMode::Redline,
        validator_level: stemma::ValidatorLevel::Blocking,
        validator: None,
    });
    assert!(
        second_save.is_ok(),
        "DOMAIN RULE: leaving a sectPrChange pending across a save/reimport/save \
         cycle must never corrupt package relationships, regardless of what else \
         got resolved in between: {second_save:?}"
    );
}
