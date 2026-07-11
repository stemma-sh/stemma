//! Story-addressing integration tests (§17.10): editing into a header/footer
//! story that is ONE OF SEVERAL must hit the addressed part and no other.
//!
//! These exercise the `story_addr` resolvers through the PUBLIC edit API over
//! committed multi-part fixtures — the disambiguating `find` that walks PAST a
//! non-matching story before landing on the right one, which the single-header
//! `headers_footers_edit.rs` fixture cannot reach.
//!
//! Covered:
//! - A1: HEADER-BY-PART disambiguation — an `EditHeader` of the EVEN header part
//!   changes only that story; the Default and First headers stay byte-identical,
//!   and on serialize the new text lands only in the matching `word/headerN.xml`.
//! - A2: FOOTER kind→part LINK — linking a footer by KIND resolves the existing
//!   even-footer part, adds exactly that `footerReference`, is idempotent on
//!   relink, and unlink removes it.
//! - A3: STORY-NOT-FOUND — an `EditHeader`/`EditFooter` at a non-existent part
//!   name fails `StoryNotFound`, never a silent fallback to the body / first
//!   story (CLAUDE.md prime directive).

use stemma::api::Document;
use stemma::domain::{BlockNode, CanonDoc, HeaderFooterKind, InlineNode, NodeId, RevisionInfo};
use stemma::edit::NoteKind;
use stemma::edit::{
    ContentFragment, EditError, EditStep, EditTransaction, HeaderFooterLink, MaterializationMode,
    ParagraphContent, StoryRef, apply_transaction,
};
use stemma::runtime::ExportOptions;

const HEADER_TYPES: &str = "testdata/spec-compliance/stories/header-types/input.docx";
const FOOTER_TYPES: &str = "testdata/spec-compliance/stories/footer-types/input.docx";
const FOOTNOTE_REFS: &str = "testdata/spec-compliance/stories/footnote-references/input.docx";

fn load(path: &str) -> Document {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    Document::parse(&bytes).unwrap_or_else(|e| panic!("parse {path}: {e:?}"))
}

fn txn(steps: Vec<EditStep>, mode: MaterializationMode) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: mode,
        revision: RevisionInfo {
            revision_id: 1,
            author: Some("Tester".to_string()),
            date: Some("2026-06-01T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

fn text_content(text: &str) -> ParagraphContent {
    ParagraphContent {
        fragments: vec![ContentFragment::Text(text.to_string())],
    }
}

/// Visible text of the header story with the given part name (Text inlines).
fn header_text(canon: &CanonDoc, part: &str) -> String {
    let story = canon
        .headers
        .iter()
        .find(|h| h.part_name == part)
        .unwrap_or_else(|| panic!("header story {part} present"));
    story_text(&story.blocks)
}

fn story_text(blocks: &[stemma::domain::TrackedBlock]) -> String {
    blocks
        .iter()
        .flat_map(|b| match &b.block {
            BlockNode::Paragraph(p) => p.segments.clone(),
            _ => vec![],
        })
        .flat_map(|s| s.inlines)
        .filter_map(|i| match i {
            InlineNode::Text(t) => Some(t.text),
            _ => None,
        })
        .collect()
}

/// `(part_name, first-paragraph block id)` for the header story of the given kind.
fn header_addr_for_kind(canon: &CanonDoc, kind: HeaderFooterKind) -> (String, NodeId) {
    let story = canon
        .headers
        .iter()
        .find(|h| h.kind == kind)
        .unwrap_or_else(|| panic!("header of kind {kind:?} present"));
    let block_id = story
        .blocks
        .iter()
        .find_map(|b| match &b.block {
            BlockNode::Paragraph(p) => Some(p.id.clone()),
            _ => None,
        })
        .expect("header story has a paragraph block");
    (story.part_name.clone(), block_id)
}

/// Read one part's bytes out of a serialized DOCX (zip).
fn part_bytes(docx: &[u8], part: &str) -> Vec<u8> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(docx)).expect("zip open");
    let mut file = zip
        .by_name(part)
        .unwrap_or_else(|_| panic!("part {part} present in serialized docx"));
    use std::io::Read;
    let mut out = Vec::new();
    file.read_to_end(&mut out).expect("read part");
    out
}

/// A1 — HEADER-BY-PART disambiguation.
///
/// DOMAIN RULE (§17.10.5): header stories are INDEPENDENT, addressed by part
/// name; an Even-page header is a distinct story from the Default and First
/// headers. Editing the Even header by its part name must change ONLY that
/// story. The resolver must walk past the Default and First stories (which sort
/// before it) to land on the addressed one — never fall through to the first.
#[test]
fn edit_header_targets_only_the_addressed_even_part() {
    let doc = load(HEADER_TYPES);
    let base = doc.snapshot().canonical.clone();

    // Three independent header stories with distinct text.
    assert_eq!(header_text(&base, "header1.xml"), "Default Header");
    assert_eq!(header_text(&base, "header2.xml"), "First Page Header");
    assert_eq!(header_text(&base, "header3.xml"), "Even Page Header");

    let (even_part, even_block) = header_addr_for_kind(&base, HeaderFooterKind::Even);
    assert_eq!(even_part, "header3.xml", "even header is the third part");

    let edited = apply_transaction(
        &base,
        &txn(
            vec![EditStep::EditHeader {
                story: StoryRef::Header(even_part.clone()),
                block_id: even_block,
                expect: "Even Page Header".to_string(),
                semantic_hash: None,
                content: text_content("Even Page Heading"),
                rationale: None,
            }],
            MaterializationMode::Direct,
        ),
    )
    .expect("edit even header")
    .0;

    // Only the even story changed in the IR; the other two are untouched.
    assert_eq!(header_text(&edited, "header3.xml"), "Even Page Heading");
    assert_eq!(
        header_text(&edited, "header1.xml"),
        "Default Header",
        "default header untouched by an even-targeted edit"
    );
    assert_eq!(
        header_text(&edited, "header2.xml"),
        "First Page Header",
        "first header untouched by an even-targeted edit"
    );

    // On serialize, the new text lands ONLY in the matching part. We assert the
    // strong end-to-end property: serialize the edited doc, REPARSE it, and read
    // each header story back. Story scoping holds iff the even story carries the
    // new text while the default and first stories read back unchanged.
    //
    // (We do NOT byte-compare the other parts against the original input: the
    // serializer re-canonicalizes EVERY part's XML on any edit — namespace
    // expansion, quote style — so raw-byte identity is a serializer-normalization
    // property, not a story-scoping one. Reparsed text is the honest invariant.)
    let edited_doc = doc
        .apply(&txn(
            vec![EditStep::EditHeader {
                story: StoryRef::Header(even_part.clone()),
                block_id: header_addr_for_kind(&base, HeaderFooterKind::Even).1,
                expect: "Even Page Header".to_string(),
                semantic_hash: None,
                content: text_content("Even Page Heading"),
                rationale: None,
            }],
            MaterializationMode::Direct,
        ))
        .expect("apply even-header edit");
    let after = edited_doc
        .serialize(&ExportOptions::default())
        .expect("serialize edited");

    // The new word lands in the even part's serialized XML; the old word is gone.
    // (The word-level materializer preserves the "Even Page " prefix and edits
    // only the changed word, so the new text may span two runs.)
    let even_xml = String::from_utf8_lossy(&part_bytes(&after, "word/header3.xml")).into_owned();
    assert!(
        even_xml.contains("Heading"),
        "edited word lands in word/header3.xml: {even_xml}"
    );
    assert!(
        !even_xml.contains("<w:t>Header</w:t>"),
        "the old header word is gone from word/header3.xml"
    );

    // Reparse and confirm each story reads back correctly: only the even story
    // changed; default and first are intact.
    let reparsed = Document::parse(&after).expect("reparse edited docx");
    let rcanon = reparsed.snapshot().canonical.clone();
    assert_eq!(header_text(&rcanon, "header3.xml"), "Even Page Heading");
    assert_eq!(
        header_text(&rcanon, "header1.xml"),
        "Default Header",
        "default header reads back unchanged after the even edit"
    );
    assert_eq!(
        header_text(&rcanon, "header2.xml"),
        "First Page Header",
        "first header reads back unchanged after the even edit"
    );
}

/// A2 — FOOTER kind→part LINK twin.
///
/// DOMAIN RULE (§17.10.3): a `SetHeaderFooterMode` LINK addresses a footer by
/// KIND; the verb resolves the kind to the EXISTING footer story's part name
/// (`footer_part_for_kind`) and adds exactly that `footerReference`. Relinking
/// is idempotent (no duplicate reference); unlink drops it.
#[test]
fn link_footer_by_kind_resolves_existing_even_part_idempotently() {
    let doc = load(FOOTER_TYPES);
    let base = doc.snapshot().canonical.clone();

    // The even-footer story exists as a part; capture its part name.
    let even_part = base
        .footers
        .iter()
        .find(|f| f.kind == HeaderFooterKind::Even)
        .map(|f| f.part_name.clone())
        .expect("even footer story present");
    assert_eq!(even_part, "footer3.xml");

    // Unlink the even footer reference so we start from a known no-even state,
    // then link it back BY KIND and assert the kind→part resolution.
    let unlinked = apply_transaction(
        &base,
        &txn(
            vec![EditStep::SetHeaderFooterMode {
                title_page: None,
                even_and_odd: None,
                link: Some(HeaderFooterLink {
                    is_header: false,
                    kind: HeaderFooterKind::Even,
                    link: false,
                }),
                rationale: None,
            }],
            MaterializationMode::Direct,
        ),
    )
    .expect("unlink even footer")
    .0;
    assert!(
        !unlinked
            .body_section_properties
            .as_ref()
            .unwrap()
            .footer_refs
            .iter()
            .any(|r| r.kind == HeaderFooterKind::Even),
        "even footer reference removed by unlink"
    );

    // LINK by kind → resolves to the existing even-footer part.
    let linked = apply_transaction(
        &unlinked,
        &txn(
            vec![EditStep::SetHeaderFooterMode {
                title_page: None,
                even_and_odd: None,
                link: Some(HeaderFooterLink {
                    is_header: false,
                    kind: HeaderFooterKind::Even,
                    link: true,
                }),
                rationale: None,
            }],
            MaterializationMode::Direct,
        ),
    )
    .expect("link even footer by kind")
    .0;
    let even_refs: Vec<_> = linked
        .body_section_properties
        .as_ref()
        .unwrap()
        .footer_refs
        .iter()
        .filter(|r| r.kind == HeaderFooterKind::Even)
        .collect();
    assert_eq!(
        even_refs.len(),
        1,
        "exactly one even footer reference added"
    );
    assert_eq!(
        even_refs[0].part_path, even_part,
        "kind→part resolved to the existing even-footer part"
    );

    // Relinking is idempotent — no duplicate reference.
    let relinked = apply_transaction(
        &linked,
        &txn(
            vec![EditStep::SetHeaderFooterMode {
                title_page: None,
                even_and_odd: None,
                link: Some(HeaderFooterLink {
                    is_header: false,
                    kind: HeaderFooterKind::Even,
                    link: true,
                }),
                rationale: None,
            }],
            MaterializationMode::Direct,
        ),
    )
    .expect("relink even footer")
    .0;
    assert_eq!(
        relinked
            .body_section_properties
            .as_ref()
            .unwrap()
            .footer_refs
            .iter()
            .filter(|r| r.kind == HeaderFooterKind::Even)
            .count(),
        1,
        "relink is idempotent — still exactly one even footer reference"
    );
}

/// A2b — linking a footer KIND with no existing story fails loud, never
/// best-effort synthesizes a part. (`footer_part_for_kind` returns None.)
///
/// We delete the even-footer story first so the kind has no resolvable part.
#[test]
fn link_footer_kind_without_story_fails_loud() {
    let doc = load(FOOTER_TYPES);
    // Own the IR so we can sculpt the precondition (drop the even-footer story).
    let mut base: CanonDoc = (*doc.snapshot().canonical).clone();

    // Remove the even-footer story AND its reference, leaving the Even kind with
    // no story to resolve. This is the precondition for the fail-loud path.
    base.footers.retain(|f| f.kind != HeaderFooterKind::Even);
    if let Some(sp) = base.body_section_properties.as_mut() {
        sp.footer_refs.retain(|r| r.kind != HeaderFooterKind::Even);
    }

    let err = apply_transaction(
        &base,
        &txn(
            vec![EditStep::SetHeaderFooterMode {
                title_page: None,
                even_and_odd: None,
                link: Some(HeaderFooterLink {
                    is_header: false,
                    kind: HeaderFooterKind::Even,
                    link: true,
                }),
                rationale: None,
            }],
            MaterializationMode::Direct,
        ),
    )
    .expect_err("linking an even footer with no story must fail loud");
    assert!(
        matches!(
            err,
            EditError::HeaderFooterRefNotResolvable {
                is_header: false,
                ..
            }
        ),
        "got {err:?}"
    );
}

/// A3 — STORY-NOT-FOUND fail-fast for header and footer edits.
///
/// DOMAIN RULE (CLAUDE.md prime directive / story_addr): a `(StoryRef, block)`
/// edit at a part name that doesn't exist is a hard `StoryNotFound` error,
/// carrying the failing story — never a silent fallback to the body or the
/// first story.
#[test]
fn edit_header_at_unknown_part_is_story_not_found() {
    let doc = load(HEADER_TYPES);
    let base = doc.snapshot().canonical.clone();

    let err = apply_transaction(
        &base,
        &txn(
            vec![EditStep::EditHeader {
                story: StoryRef::Header("header99.xml".to_string()),
                block_id: NodeId::from("p_1"),
                expect: "anything".to_string(),
                semantic_hash: None,
                content: text_content("new"),
                rationale: None,
            }],
            MaterializationMode::Direct,
        ),
    )
    .expect_err("editing a non-existent header part must fail");
    match err {
        EditError::StoryNotFound { story, .. } => {
            assert_eq!(story, StoryRef::Header("header99.xml".to_string()));
        }
        other => panic!("expected StoryNotFound, got {other:?}"),
    }

    // The body is unchanged: no silent fallback occurred (the error returned
    // before any mutation, and the body text is still its original).
    assert_eq!(
        story_text(&base.blocks),
        "Body content for header types test."
    );
}

#[test]
fn edit_footer_at_unknown_part_is_story_not_found() {
    let doc = load(FOOTER_TYPES);
    let base = doc.snapshot().canonical.clone();

    let err = apply_transaction(
        &base,
        &txn(
            vec![EditStep::EditFooter {
                story: StoryRef::Footer("footer99.xml".to_string()),
                block_id: NodeId::from("p_1"),
                expect: "anything".to_string(),
                semantic_hash: None,
                content: text_content("new"),
                rationale: None,
            }],
            MaterializationMode::Direct,
        ),
    )
    .expect_err("editing a non-existent footer part must fail");
    match err {
        EditError::StoryNotFound { story, .. } => {
            assert_eq!(story, StoryRef::Footer("footer99.xml".to_string()));
        }
        other => panic!("expected StoryNotFound, got {other:?}"),
    }
}

// ─── Part B: cross-story id-collision PROBE ──────────────────────────────────
//
// Question: can `EditNote` (which bypasses the `story_addr` resolvers and does
// its own `iter().find()` over `doc.footnotes` by note id) hit the WRONG story
// when a footnote block and a BODY block share the same bare block id?
//
// We construct the adversarial precondition the `story_addr` module exists to
// guard against: a body paragraph and a footnote-story paragraph that BOTH carry
// the bare block id "p_1". (The importer never produces this — it namespaces
// story blocks as `story_p1` — so we force the collision by renaming the
// footnote's first block to the body's id.) Then we drive the PUBLIC `EditNote`
// verb and assert it lands in the NOTE, leaving the colliding BODY block intact.
//
// EXPECTED (domain rule): `EditNote` addresses the story by its NOTE ID within
// `doc.footnotes`, then wholesale-replaces THAT story's blocks. It never resolves
// a block by a bare id across stories, so a colliding `p_1` is harmless. If this
// holds, the `StoryRef::Footnote/Endnote/Comment` resolver arms have no
// production caller and are genuinely dead.

/// Visible text of the footnote story with the given note id.
fn footnote_text(canon: &CanonDoc, note_id: &str) -> String {
    let story = canon
        .footnotes
        .iter()
        .find(|f| f.id == note_id)
        .unwrap_or_else(|| panic!("footnote {note_id} present"));
    story_text(&story.blocks)
}

#[test]
fn edit_note_with_colliding_body_block_id_lands_in_the_note_not_the_body() {
    let doc = load(FOOTNOTE_REFS);
    // Own the IR so we can manufacture the id collision.
    let mut base: CanonDoc = (*doc.snapshot().canonical).clone();

    // The body paragraph is "p_1" and footnote 1 says "This is footnote one.".
    assert_eq!(story_text(&base.blocks), "See footnote");
    let body_block_id = match &base.blocks[0].block {
        BlockNode::Paragraph(p) => p.id.0.to_string(),
        _ => panic!("body[0] is a paragraph"),
    };
    assert_eq!(body_block_id, "p_1");
    assert_eq!(footnote_text(&base, "1"), "This is footnote one.");

    // Force the collision: rename footnote 1's first block to the body's id.
    let note_story = base
        .footnotes
        .iter_mut()
        .find(|f| f.id == "1")
        .expect("footnote 1");
    for tb in &mut note_story.blocks {
        if let BlockNode::Paragraph(p) = &mut tb.block {
            p.id = NodeId::from("p_1");
        }
    }
    // Sanity: the colliding id now lives in BOTH the body and footnote 1.
    assert!(matches!(&base.blocks[0].block, BlockNode::Paragraph(p) if p.id.0.as_ref() == "p_1"));
    assert!(
        base.footnotes
            .iter()
            .find(|f| f.id == "1")
            .unwrap()
            .blocks
            .iter()
            .any(|tb| matches!(&tb.block, BlockNode::Paragraph(p) if p.id.0.as_ref() == "p_1"))
    );

    // Drive the PUBLIC EditNote verb on footnote 1.
    let edited = apply_transaction(
        &base,
        &txn(
            vec![EditStep::EditNote {
                note_id: "1".to_string(),
                note_kind: NoteKind::Footnote,
                body: "This is the AMENDED footnote.".to_string(),
                rationale: None,
            }],
            MaterializationMode::Direct,
        ),
    )
    .expect("edit footnote 1")
    .0;

    // The edit landed in the NOTE.
    assert_eq!(
        footnote_text(&edited, "1"),
        "This is the AMENDED footnote.",
        "EditNote landed in footnote 1's story"
    );
    // The colliding BODY block is UNTOUCHED — no cross-story leak.
    assert_eq!(
        story_text(&edited.blocks),
        "See footnote",
        "the body block sharing id p_1 is untouched by EditNote"
    );
}
