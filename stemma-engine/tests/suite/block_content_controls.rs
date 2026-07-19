//! Integration tests for `WrapBlocksInContentControl` — the block-level sibling
//! of `WrapInContentControl`. It wraps a contiguous RANGE of body blocks
//! (paragraphs / tables) in a single block-level `w:sdt`.
//!
//! Contract under test (CLAUDE.md "no silent fallbacks"; domain-model §11):
//!  - the serialized `word/document.xml` carries ONE `w:sdt` whose
//!    `w:sdtContent` encloses the whole range (`w:sdtPr` carries the authored
//!    tag / alias / control kind);
//!  - the wrapped blocks' content and opaques are preserved EXACTLY — the wrap
//!    only adds the enclosing envelope, it never mutates inner blocks;
//!  - SDT structure is UNTRACKED — accept-all == reject-all == the wrapped doc
//!    (there is no `w:sdtChange` envelope), and a Direct (untracked) apply
//!    equals the tracked apply;
//!  - the output is post-serialization-validator clean;
//!  - fail-loud: `EmptyContentControlSpec`, `BlockRangeInvalid`,
//!    `BlockNotFound`, `BlockAlreadyWrapped`.
//!
//! Daily tier, corpus-free (synthesized in-memory DOCX).

use stemma::api::Document;
use stemma::domain::{BlockNode, CanonDoc, NodeId, RevisionInfo, SdtControl};
use stemma::edit::{
    EditError, EditStep, EditTransaction, MaterializationMode, SdtSpec, apply_transaction,
};
use stemma::{ExportMode, ExportOptions, Resolution, ValidatorLevel};

/// Multi-paragraph DOCX (the w14/w15 namespaces declared so a control that uses
/// `w14:checkbox` / `w15:repeatingSection` round-trips). The last paragraph
/// carries an opaque `w:fldSimple` so the opaque-inventory invariant has
/// something to preserve.
fn make_docx(paras: &[&str]) -> Vec<u8> {
    let mut document_xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" xmlns:w15="http://schemas.microsoft.com/office/word/2012/wordml"><w:body>"#,
    );
    for p in paras {
        document_xml.push_str(&format!(
            r#"<w:p><w:r><w:t xml:space="preserve">{p}</w:t></w:r></w:p>"#
        ));
    }
    document_xml.push_str("<w:sectPr/></w:body></w:document>");

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

/// The id of the `n`-th top-level block.
fn block_id_at(canon: &CanonDoc, n: usize) -> NodeId {
    match &canon.blocks[n].block {
        BlockNode::Paragraph(p) => p.id.clone(),
        BlockNode::Table(t) => t.id.clone(),
        BlockNode::OpaqueBlock(o) => o.id.clone(),
    }
}

fn txn(steps: Vec<EditStep>) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        // SDT structure is untracked; the mode does not change behavior.
        materialization_mode: MaterializationMode::Direct,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("BCC".to_string()),
            date: Some("2026-06-05T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

fn doc_xml(bytes: &[u8]) -> String {
    let archive = stemma::docx::DocxArchive::read(bytes).expect("read docx");
    String::from_utf8(
        archive
            .get("word/document.xml")
            .expect("document.xml")
            .to_vec(),
    )
    .expect("utf8")
}

fn serialize_validated(doc: &Document) -> Vec<u8> {
    doc.serialize(&ExportOptions {
        mode: ExportMode::Redline,
        validator_level: ValidatorLevel::Blocking,
        validator: None,
    })
    .expect("serialize+validate")
}

/// Count substring occurrences (non-overlapping).
fn count(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

// ─── happy path ───────────────────────────────────────────────────────────────

/// Wrapping a two-paragraph range produces exactly ONE block-level `w:sdt`
/// whose `w:sdtContent` encloses BOTH paragraphs (both run texts appear between
/// the single `<w:sdtContent>` and its close), with the authored sdtPr. The
/// output passes the Blocking post-serialization validator.
#[test]
fn wrap_range_emits_one_block_sdt_around_the_range() {
    let base = Document::parse(&make_docx(&[
        "First clause paragraph.",
        "Second clause paragraph.",
        "Outside the wrap.",
    ]))
    .expect("parse");
    let canon = base.snapshot().canonical.clone();
    let start = block_id_at(&canon, 0);
    let end = block_id_at(&canon, 1);

    let edited = base
        .apply(&txn(vec![EditStep::WrapBlocksInContentControl {
            start_block_id: start,
            end_block_id: end,
            spec: SdtSpec {
                tag: Some("clause".to_string()),
                alias: Some("Clause 1".to_string()),
                control: SdtControl::RichText,
                binding: None,
            },
            rationale: None,
        }]))
        .expect("apply");

    let out = serialize_validated(&edited);
    let xml = doc_xml(&out);

    // Exactly one block-level sdt + one sdtContent for the whole range.
    assert_eq!(
        count(&xml, "<w:sdt>"),
        1,
        "one block-level w:sdt; xml={xml}"
    );
    assert_eq!(
        count(&xml, "<w:sdtContent>"),
        1,
        "one w:sdtContent; xml={xml}"
    );
    assert_eq!(count(&xml, "</w:sdt>"), 1, "balanced close; xml={xml}");

    // The authored sdtPr (alias + tag) is present. The serializer may emit a
    // self-closing element with or without a space (`.../>` vs `... />`), so
    // assert on the attribute pair, not the exact tag close.
    assert!(
        xml.contains(r#"<w:tag w:val="clause""#),
        "tag on sdtPr; xml={xml}"
    );
    assert!(
        xml.contains(r#"<w:alias w:val="Clause 1""#),
        "alias on sdtPr; xml={xml}"
    );

    // Both wrapped paragraphs live INSIDE the single sdtContent; the outside
    // paragraph lives AFTER the close.
    let content_open = xml.find("<w:sdtContent>").expect("content open");
    let content_close = xml.find("</w:sdtContent>").expect("content close");
    let inside = &xml[content_open..content_close];
    assert!(
        inside.contains("First clause paragraph."),
        "p0 inside; inside={inside}"
    );
    assert!(
        inside.contains("Second clause paragraph."),
        "p1 inside; inside={inside}"
    );
    assert!(
        !inside.contains("Outside the wrap."),
        "p2 must be outside; inside={inside}"
    );
    let after = &xml[content_close..];
    assert!(
        after.contains("Outside the wrap."),
        "p2 after the wrap; after={after}"
    );
}

/// A single-block range (`start == end`, span 1) wraps exactly that one block.
#[test]
fn wrap_single_block_range() {
    let base = Document::parse(&make_docx(&["Only me.", "Not me."])).expect("parse");
    let canon = base.snapshot().canonical.clone();
    let id = block_id_at(&canon, 0);

    let edited = base
        .apply(&txn(vec![EditStep::WrapBlocksInContentControl {
            start_block_id: id.clone(),
            end_block_id: id,
            spec: SdtSpec {
                tag: Some("one".to_string()),
                alias: None,
                control: SdtControl::PlainText,
                binding: None,
            },
            rationale: None,
        }]))
        .expect("apply");

    let xml = doc_xml(&serialize_validated(&edited));
    assert_eq!(count(&xml, "<w:sdt>"), 1);
    let inside = &xml[xml.find("<w:sdtContent>").unwrap()..xml.find("</w:sdtContent>").unwrap()];
    assert!(inside.contains("Only me."), "inside={inside}");
    assert!(
        !inside.contains("Not me."),
        "second block stays out; inside={inside}"
    );
}

// ─── invariants (untracked verb) ──────────────────────────────────────────────

/// Untracked structure: accept-all and reject-all of the serialized doc both
/// reproduce the *wrapped* document (the `w:sdt` survives both, with no
/// `w:sdtChange` / `w:ins` / `w:del` envelope), AND a Direct (untracked) apply
/// equals the tracked apply. This is the block-level mirror of
/// `wrap_serialized_markup_is_untracked_under_accept_and_reject`.
#[test]
fn wrap_is_untracked_accept_equals_reject_equals_wrapped() {
    let base = Document::parse(&make_docx(&["Alpha block.", "Beta block."])).expect("parse");
    let canon = base.snapshot().canonical.clone();
    let start = block_id_at(&canon, 0);
    let end = block_id_at(&canon, 1);
    let steps = vec![EditStep::WrapBlocksInContentControl {
        start_block_id: start,
        end_block_id: end,
        spec: SdtSpec {
            tag: Some("grp".to_string()),
            alias: None,
            control: SdtControl::RichText,
            binding: None,
        },
        rationale: None,
    }];

    let edited = base.apply(&txn(steps)).expect("apply");
    let edited_xml = doc_xml(&serialize_validated(&edited));

    let accepted = serialize_validated(&edited.project(Resolution::AcceptAll).expect("accept"));
    let rejected = serialize_validated(&edited.project(Resolution::RejectAll).expect("reject"));

    for (label, bytes) in [("accept-all", &accepted), ("reject-all", &rejected)] {
        let xml = doc_xml(bytes);
        assert!(
            xml.contains("<w:sdt>") && xml.contains("<w:sdtContent>"),
            "{label}: the block-level w:sdt must survive (untracked structure); xml={xml}"
        );
        assert!(
            !xml.contains("w:sdtChange"),
            "{label}: an SDT has no w:sdtChange envelope; xml={xml}"
        );
        assert!(
            !xml.contains("<w:ins") && !xml.contains("<w:del"),
            "{label}: the wrap must not be a tracked insertion/deletion; xml={xml}"
        );
        // Both wrapped paragraphs persist.
        assert!(
            xml.contains("Alpha block.") && xml.contains("Beta block."),
            "{label}: content; xml={xml}"
        );
    }

    // accept-all and reject-all both equal the (untracked) edited document.
    assert_eq!(doc_xml(&accepted), edited_xml, "accept-all == wrapped");
    assert_eq!(doc_xml(&rejected), edited_xml, "reject-all == wrapped");
}

/// Wrapping in Direct (untracked) mode equals wrapping in TrackedChange mode —
/// the mode does not change behavior for a structural SDT wrap.
#[test]
fn tracked_mode_equals_direct_mode() {
    let docx = make_docx(&["P one.", "P two."]);
    let canon = Document::parse(&docx)
        .expect("parse")
        .snapshot()
        .canonical
        .clone();
    let start = block_id_at(&canon, 0);
    let end = block_id_at(&canon, 1);
    let step = EditStep::WrapBlocksInContentControl {
        start_block_id: start,
        end_block_id: end,
        spec: SdtSpec {
            tag: Some("m".to_string()),
            alias: None,
            control: SdtControl::PlainText,
            binding: None,
        },
        rationale: None,
    };

    let direct = {
        let mut t = txn(vec![step.clone()]);
        t.materialization_mode = MaterializationMode::Direct;
        doc_xml(&serialize_validated(
            &Document::parse(&docx).unwrap().apply(&t).expect("direct"),
        ))
    };
    let tracked = {
        let mut t = txn(vec![step]);
        t.materialization_mode = MaterializationMode::TrackedChange;
        doc_xml(&serialize_validated(
            &Document::parse(&docx).unwrap().apply(&t).expect("tracked"),
        ))
    };
    assert_eq!(
        direct, tracked,
        "untracked SDT wrap: mode must not change the output"
    );
}

/// The opaque inventory must not shrink: an opaque inline present before the
/// wrap (here a `w:fldSimple` field) is still present after — the wrap encloses
/// it, it does not drop it.
#[test]
fn opaque_inventory_does_not_shrink() {
    // A paragraph carrying a field opaque inside the wrapped range.
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Lead.</w:t></w:r></w:p><w:p><w:fldSimple w:instr="PAGE"><w:r><w:t>1</w:t></w:r></w:fldSimple></w:p><w:sectPr/></w:body></w:document>"#;
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    use std::io::Write;
    use zip::write::FileOptions;
    let mut docx = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut docx));
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

    let base = Document::parse(&docx).expect("parse");
    let before = doc_xml(&serialize_validated(&base));
    assert!(
        before.contains("fldSimple"),
        "field present before; before={before}"
    );

    let canon = base.snapshot().canonical.clone();
    let start = block_id_at(&canon, 0);
    let end = block_id_at(&canon, 1);
    let edited = base
        .apply(&txn(vec![EditStep::WrapBlocksInContentControl {
            start_block_id: start,
            end_block_id: end,
            spec: SdtSpec {
                tag: Some("g".to_string()),
                alias: None,
                control: SdtControl::RichText,
                binding: None,
            },
            rationale: None,
        }]))
        .expect("apply");

    let after = doc_xml(&serialize_validated(&edited));
    assert!(
        after.contains("fldSimple"),
        "field opaque preserved after wrap; after={after}"
    );
    // And it is inside the sdtContent (the wrap encloses it).
    let inside =
        &after[after.find("<w:sdtContent>").unwrap()..after.find("</w:sdtContent>").unwrap()];
    assert!(
        inside.contains("fldSimple"),
        "field inside the wrap; inside={inside}"
    );
}

// ─── fail-loud ────────────────────────────────────────────────────────────────

#[test]
fn empty_spec_fails_loud() {
    let base = Document::parse(&make_docx(&["a", "b"])).expect("parse");
    let canon = base.snapshot().canonical.clone();
    let start = block_id_at(&canon, 0);
    let end = block_id_at(&canon, 1);
    let err = apply_transaction(
        &canon,
        &txn(vec![EditStep::WrapBlocksInContentControl {
            start_block_id: start,
            end_block_id: end,
            spec: SdtSpec {
                tag: None,
                alias: None,
                control: SdtControl::RichText,
                binding: None,
            },
            rationale: None,
        }]),
    )
    .expect_err("empty spec must be refused");
    assert!(
        matches!(err, EditError::EmptyContentControlSpec { .. }),
        "got {err:?}"
    );
}

#[test]
fn backward_range_fails_loud() {
    let base = Document::parse(&make_docx(&["a", "b", "c"])).expect("parse");
    let canon = base.snapshot().canonical.clone();
    let start = block_id_at(&canon, 2);
    let end = block_id_at(&canon, 0);
    let err = apply_transaction(
        &canon,
        &txn(vec![EditStep::WrapBlocksInContentControl {
            start_block_id: start,
            end_block_id: end,
            spec: SdtSpec {
                tag: Some("t".to_string()),
                alias: None,
                control: SdtControl::RichText,
                binding: None,
            },
            rationale: None,
        }]),
    )
    .expect_err("backward range must be refused");
    assert!(
        matches!(err, EditError::BlockRangeInvalid { .. }),
        "got {err:?}"
    );
}

#[test]
fn missing_block_fails_loud() {
    let base = Document::parse(&make_docx(&["a", "b"])).expect("parse");
    let canon = base.snapshot().canonical.clone();
    let start = block_id_at(&canon, 0);
    let err = apply_transaction(
        &canon,
        &txn(vec![EditStep::WrapBlocksInContentControl {
            start_block_id: start,
            end_block_id: NodeId::from("does_not_exist"),
            spec: SdtSpec {
                tag: Some("t".to_string()),
                alias: None,
                control: SdtControl::RichText,
                binding: None,
            },
            rationale: None,
        }]),
    )
    .expect_err("missing end block must be refused");
    assert!(
        matches!(err, EditError::BlockNotFound { .. }),
        "got {err:?}"
    );
}

#[test]
fn overlapping_wrap_fails_loud() {
    // Wrap [0,1], then attempt to wrap [1,2] — block 1 is already wrapped.
    let base = Document::parse(&make_docx(&["a", "b", "c"])).expect("parse");
    let canon0 = base.snapshot().canonical.clone();
    let b0 = block_id_at(&canon0, 0);
    let b1 = block_id_at(&canon0, 1);
    let b2 = block_id_at(&canon0, 2);

    let once = base
        .apply(&txn(vec![EditStep::WrapBlocksInContentControl {
            start_block_id: b0,
            end_block_id: b1.clone(),
            spec: SdtSpec {
                tag: Some("first".to_string()),
                alias: None,
                control: SdtControl::RichText,
                binding: None,
            },
            rationale: None,
        }]))
        .expect("first wrap");

    let canon1 = once.snapshot().canonical.clone();
    let err = apply_transaction(
        &canon1,
        &txn(vec![EditStep::WrapBlocksInContentControl {
            start_block_id: b1,
            end_block_id: b2,
            spec: SdtSpec {
                tag: Some("second".to_string()),
                alias: None,
                control: SdtControl::RichText,
                binding: None,
            },
            rationale: None,
        }]),
    )
    .expect_err("overlapping wrap must be refused");
    assert!(
        matches!(err, EditError::BlockAlreadyWrapped { .. }),
        "got {err:?}"
    );
}

// ─── v4 wire op ───────────────────────────────────────────────────────────────

/// The `wrap_blocks_content_control` v4 op parses, passes schema validation, and
/// translates to `EditStep::WrapBlocksInContentControl` with the typed spec.
#[test]
fn v4_wrap_blocks_op_parses_and_translates() {
    use stemma::edit_v4::parse_transaction;
    let json = r#"
    {
      "ops": [
        { "op": "wrap_blocks_content_control",
          "start_block": "p_1",
          "end_block": "p_3",
          "tag": "clause",
          "alias": "Clause 1",
          "control": { "kind": "plain_text" } }
      ],
      "revision": { "author": "A" }
    }"#;
    let parsed = parse_transaction(json).expect("schema accepts the op");
    let txn = parsed
        .into_edit_transaction()
        .expect("adapter translates the op");
    assert_eq!(txn.steps.len(), 1);
    match &txn.steps[0] {
        EditStep::WrapBlocksInContentControl {
            start_block_id,
            end_block_id,
            spec,
            ..
        } => {
            assert_eq!(start_block_id.to_string(), "p_1");
            assert_eq!(end_block_id.to_string(), "p_3");
            assert_eq!(spec.tag.as_deref(), Some("clause"));
            assert_eq!(spec.alias.as_deref(), Some("Clause 1"));
            assert!(matches!(spec.control, SdtControl::PlainText));
        }
        other => panic!("expected WrapBlocksInContentControl, got {other:?}"),
    }
}

/// An all-empty spec (no tag/alias, default rich-text) is refused at the v4
/// schema layer — no silent fallback.
#[test]
fn v4_wrap_blocks_op_empty_spec_rejected_at_schema() {
    use stemma::edit_v4::parse_transaction;
    let json = r#"
    {
      "ops": [
        { "op": "wrap_blocks_content_control",
          "start_block": "p_1",
          "end_block": "p_2",
          "control": { "kind": "rich_text" } }
      ],
      "revision": { "author": "A" }
    }"#;
    parse_transaction(json).expect_err("empty distinguishing spec must fail schema validation");
}
