//! Real-structure SENTINEL CORPUS — relational invariants run DAILY over the
//! structural classes that today only the DOCX corpus / Word-oracle tiers touch.
//!
//! # Why this file exists
//!
//! The daily merge gate (`just -f stemma-engine/Justfile gate`) is hermetic: it has no
//! DOCX corpus and no Word VM. The ~679 checked-in fixtures are swept by
//! `roundtrip_fidelity.rs`, and `redline_fixpoint_daily.rs` runs the fixpoint
//! chain over a curated category list — but BOTH skip gracefully when the corpus
//! is absent, so in the hermetic gate they check **zero real structures**. That
//! means a whole structural class (a tracked move across a paragraph boundary, a
//! pre-existing ins/del spanning a paragraph mark, a field inside a changed run,
//! a footnote-body edit, a header/footer redline, a nested-table cell change, the
//! Tier-2 formatting bundle) can regress while the daily gate stays green.
//!
//! This file closes that blind spot. Every witness is a small **in-memory**
//! (before, after) DOCX pair (the existing `pack(document_xml)` idiom — no
//! corpus dependency), so the relational invariants below run on EVERY
//! `cargo test -p stemma`, ratcheting the daily gate toward Word.
//!
//! # Invariants asserted per witness (catalog numbers from
//! `stemma-engine/docs/testing_strategy.md`)
//!
//! - **#6** accept/reject: `accept_all(merge_diff(A,B)) == text(B)` and
//!   `reject_all(merge_diff(A,B)) == text(A)`, in canonical space.
//! - **#7** fixpoint: `diff -> merge -> accept_all -> re-diff(merged, B)` is empty.
//! - **#10** identity: `diff(A,A)` empty AND `redline(A,A)` emits no tracked spans.
//! - **#18 (Tier-2)** formatting parity: `style_id` / `heading_level` /
//!   `num_id` / `ilvl` / `Bold` / opaque survive the accept projection on the
//!   DIFF/redline path (the catalog only covers this via Word today).
//! - **#13 (hermetic proxy)** `serialize(redline(A,B))` is validator-clean
//!   (no Error-severity `validate_docx` findings) — the in-process stand-in for
//!   "opens clean in Word".
//!
//! Tests encode the **domain rule** (what accept/reject *should* yield per
//! ECMA-376 §17.13.5), not whatever the code happens to produce today. A witness
//! that exposes a real bug is a coverage WIN: it is either fixed at the source or
//! `#[ignore]`d with a precise description of where the data first goes wrong.
//!
//! Daily tier, corpus-free.

use stemma::edit::{
    ContentFragment, EditStep, EditTransaction, MaterializationMode, ParagraphContent,
    apply_transaction,
};
use stemma::{
    BlockNode, CanonDoc, DocxRuntime, ExportMode, InlineNode, Mark, RevisionInfo, SimpleRuntime,
    TransactionMeta, accept_all,
    redline_extract::{RedlineSpan, extract_redline},
};
use stemma_diff::test_support::{DiffChange, diff_documents, merge_diff};
#[allow(unused_imports)]
use stemma_diff::test_support::{DocumentComparisonExt as _, RuntimeComparisonExt as _};

use crate::common;

#[test]
fn sentinel_unresolved_property_changes_preserve_explicit_utc_dates() {
    // MS-DOCX §2.12.2.1: an unresolved revision retains its explicit UTC
    // date information independently of its legacy date representation.
    let body = r#"<w:tbl><w:tblPr><w:tblW w:w="3000" w:type="dxa"/>
      <w:tblPrChange w:id="17" w:author="Reviewer" w:date="2025-01-02T04:04:05+01:00" w16du:dateUtc="2025-01-02T03:04:05Z"><w:tblPr><w:tblW w:w="2400" w:type="dxa"/></w:tblPr></w:tblPrChange></w:tblPr>
      <w:tblGrid><w:gridCol w:w="3000"/></w:tblGrid><w:tr><w:trPr><w:cantSplit/>
      <w:trPrChange w:id="18" w:author="Reviewer" w:date="2025-01-03T04:04:05+01:00" w16du:dateUtc="2025-01-03T03:04:05Z"><w:trPr/></w:trPrChange></w:trPr>
      <w:tc><w:tcPr><w:tcW w:w="3000" w:type="dxa"/>
      <w:tcPrChange w:id="19" w:author="Reviewer" w:date="2025-01-04T04:04:05+01:00" w16du:dateUtc="2025-01-04T03:04:05Z"><w:tcPr><w:tcW w:w="2400" w:type="dxa"/></w:tcPr></w:tcPrChange></w:tcPr>
      <w:p><w:r><w:t>Unchanged content.</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
      <w:p><w:r><w:t>Unrelated source paragraph.</w:t></w:r></w:p>"#;
    let xml = document(body).replace(
        "<w:document ",
        "<w:document xmlns:w16du=\"http://schemas.microsoft.com/office/word/2023/wordml/word16du\" ",
    );
    let original = stemma::api::Document::parse(&pack(&xml)).unwrap();
    let BlockNode::Paragraph(anchor) = &original.snapshot().canonical.blocks[1].block else {
        panic!("unrelated paragraph witness")
    };
    let wire = serde_json::json!({
        "revision":{"author":"Current"},
        "ops":[{"op":"insert","target":{"anchor":anchor.id.0,"position":"after"},
            "content":[{"type":"paragraph","content":[{"type":"text","text":"Unrelated addition."}]}]}]
    }).to_string();
    let transaction = stemma::edit_v4::parse_transaction(&wire)
        .unwrap()
        .into_edit_transaction()
        .unwrap();
    let edited = original.apply(&transaction).unwrap();
    let bytes = edited.serialize(&stemma::ExportOptions::default()).unwrap();
    let exported = document_xml(&bytes);
    let utc_values = [
        "2025-01-02T03:04:05Z",
        "2025-01-03T03:04:05Z",
        "2025-01-04T03:04:05Z",
    ];
    for value in utc_values {
        assert!(
            exported.contains(&format!("dateUtc=\"{value}\"")),
            "unresolved property revision lost explicit UTC date {value}"
        );
    }
    let reopened = stemma::api::Document::parse(&bytes).unwrap();
    for action in [
        stemma::tracked_model::ResolveSelectionAction::Accept,
        stemma::tracked_model::ResolveSelectionAction::Reject,
    ] {
        let row_id = reopened
            .revisions()
            .iter()
            .find(|r| r.wire_id == 18)
            .unwrap()
            .revision_id;
        let partial = reopened
            .project(stemma::Resolution::Selective {
                ids: [row_id].into_iter().collect(),
                action,
            })
            .unwrap();
        let saved = partial
            .serialize(&stemma::ExportOptions::default())
            .unwrap();
        let xml = document_xml(&saved);
        assert!(xml.contains(&format!("dateUtc=\"{}\"", utc_values[0])));
        assert!(!xml.contains(&format!("dateUtc=\"{}\"", utc_values[1])));
        assert!(xml.contains(&format!("dateUtc=\"{}\"", utc_values[2])));
        let again = stemma::api::Document::parse(&saved).unwrap();
        assert!(!again.revisions().iter().any(|r| r.revision_id == row_id));
    }
    for resolution in [stemma::Resolution::AcceptAll, stemma::Resolution::RejectAll] {
        let resolved = reopened.project(resolution).unwrap();
        let saved = resolved
            .serialize(&stemma::ExportOptions::default())
            .unwrap();
        let again = stemma::api::Document::parse(&saved).unwrap();
        assert!(again.revisions().is_empty());
        let xml = document_xml(&saved);
        for value in utc_values {
            assert!(!xml.contains(value));
        }
    }
    let aliased = xml
        .replace("xmlns:w16du=", "xmlns:utc=")
        .replace("w16du:dateUtc=", "utc:dateUtc=");
    let mut absent = xml.clone();
    for value in utc_values {
        absent = absent.replace(&format!(" w16du:dateUtc=\"{value}\""), "");
    }
    let mut no_legacy = xml.clone();
    for day in ["02", "03", "04"] {
        no_legacy = no_legacy.replace(&format!(" w:date=\"2025-01-{day}T04:04:05+01:00\""), "");
    }
    for (input, explicit) in [(aliased, true), (absent, false), (no_legacy, true)] {
        let source = stemma::api::Document::parse(&pack(&input)).unwrap();
        let changed = source.apply(&transaction).unwrap();
        let saved = changed
            .serialize(&stemma::ExportOptions::default())
            .unwrap();
        let again = stemma::api::Document::parse(&saved).unwrap();
        let snapshot = again.snapshot();
        let BlockNode::Table(table) = &snapshot.canonical.blocks[0].block else {
            panic!("table witness")
        };
        let actual = [
            table
                .formatting_change
                .as_ref()
                .unwrap()
                .date_utc
                .as_deref(),
            table.rows[0]
                .formatting_change
                .as_ref()
                .unwrap()
                .date_utc
                .as_deref(),
            table.rows[0].cells[0]
                .formatting_change
                .as_ref()
                .unwrap()
                .date_utc
                .as_deref(),
        ];
        for (index, value) in actual.iter().enumerate() {
            assert_eq!(*value, explicit.then_some(utc_values[index]));
        }
    }
}

#[test]
fn sentinel_tracked_move_has_distinct_persisted_paragraph_owners() {
    use std::collections::HashSet;
    use stemma::edit::InsertPosition;

    // MS-DOCX §2.6.2.3 requires part-local paraId uniqueness. The two
    // physical paragraphs of a pending move are not AlternateContent
    // alternatives, so distinct runtime NodeIds alone are insufficient.
    let source = pack(&document(
        r#"<w:p w14:paraId="00000111" w14:textId="00000211"><w:r><w:t>First.</w:t></w:r></w:p>
        <w:p w14:paraId="00000112" w14:textId="00000212"><w:r><w:t>Second.</w:t></w:r></w:p>
        <w:p w14:paraId="00000113" w14:textId="00000213"><w:r><w:t>Third.</w:t></w:r></w:p>"#,
    ));
    let original = stemma::api::Document::parse(&source).unwrap();
    let snapshot = original.snapshot();
    let first = match &snapshot.canonical.blocks[0].block {
        BlockNode::Paragraph(p) => p.id.clone(),
        _ => panic!("paragraph witness"),
    };
    let last = match &snapshot.canonical.blocks[2].block {
        BlockNode::Paragraph(p) => p.id.clone(),
        _ => panic!("paragraph witness"),
    };
    let edited = original
        .apply(&EditTransaction {
            steps: vec![EditStep::MoveBlockRange {
                from_block_id: first.clone(),
                to_block_id: first,
                dest_anchor_id: last,
                dest_position: InsertPosition::After,
                rationale: None,
                expect: None,
                semantic_hash: None,
            }],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: revision(),
        })
        .unwrap();
    let bytes = edited.serialize(&stemma::ExportOptions::default()).unwrap();
    let archive = stemma::docx::DocxArchive::read(&bytes).unwrap();
    let root = xmltree::Element::parse(archive.get("word/document.xml").unwrap()).unwrap();
    let owners: Vec<_> = root
        .get_child("body")
        .unwrap()
        .children
        .iter()
        .filter_map(xmltree::XMLNode::as_element)
        .filter(|node| node.name == "p")
        .map(|node| {
            node.attributes
                .iter()
                .find(|(name, _)| {
                    name.local_name == "paraId"
                        && name.namespace.as_deref()
                            == Some("http://schemas.microsoft.com/office/word/2010/wordml")
                })
                .map(|(_, value)| value.clone())
                .expect("each pending paragraph needs its persisted owner")
        })
        .collect();
    assert_eq!(
        owners.len(),
        4,
        "three source paragraphs plus the move copy"
    );
    assert_eq!(
        owners.iter().collect::<HashSet<_>>().len(),
        owners.len(),
        "move source and destination must not share a persisted paragraph identity"
    );
    let reopened = stemma::api::Document::parse(&bytes).unwrap();
    let before = edited.revisions();
    let after = reopened.revisions();
    let paragraph_text = |paragraph: &stemma::domain::ParagraphNode| {
        paragraph
            .all_inlines()
            .filter_map(|inline| match inline {
                InlineNode::Text(run) => Some(run.text.as_str()),
                _ => None,
            })
            .collect::<String>()
    };
    assert_eq!(
        before
            .iter()
            .filter(|record| record.kind == stemma::tracked_model::RevisionKind::Move)
            .count(),
        1,
        "one atomic logical move"
    );
    let identities: HashSet<_> = before.iter().map(|record| record.revision_id).collect();
    assert_eq!(
        identities,
        after.iter().map(|record| record.revision_id).collect(),
        "every move/boundary identity survives save and reopen"
    );
    for accept in [true, false] {
        let resolution = if accept {
            stemma::Resolution::AcceptAll
        } else {
            stemma::Resolution::RejectAll
        };
        let selected = stemma::Resolution::Selective {
            // Moving after the terminal paragraph also introduces its
            // preceding break. Select the complete authored operation,
            // including that boundary, when comparing with full resolution.
            ids: identities.clone(),
            action: if accept {
                stemma::ResolveSelectionAction::Accept
            } else {
                stemma::ResolveSelectionAction::Reject
            },
        };
        for projection in [resolution, selected] {
            let projected = reopened.project(projection).unwrap();
            let saved = projected
                .serialize(&stemma::ExportOptions::default())
                .unwrap();
            assert!(stemma::api::validate(&saved).ok);
            let again = stemma::api::Document::parse(&saved).unwrap();
            assert!(again.revisions().is_empty());
            let order: Vec<_> = again
                .snapshot()
                .canonical
                .blocks
                .iter()
                .map(|block| match &block.block {
                    BlockNode::Paragraph(p) => paragraph_text(p),
                    _ => panic!("paragraph witness"),
                })
                .collect();
            assert_eq!(
                order,
                if accept {
                    vec!["Second.", "Third.", "First."]
                } else {
                    vec!["First.", "Second.", "Third."]
                }
            );
            let moved = again
                .snapshot()
                .canonical
                .blocks
                .iter()
                .find_map(|block| match &block.block {
                    BlockNode::Paragraph(p) if paragraph_text(p) == "First." => Some(p),
                    _ => None,
                })
                .unwrap();
            if accept {
                assert_ne!(moved.para_id.as_deref(), Some("00000111"));
            } else {
                assert_eq!(moved.para_id.as_deref(), Some("00000111"));
            }
            assert_eq!(moved.text_id.as_deref(), Some("00000211"));
        }
    }
}

#[test]
fn sentinel_tracked_move_reserves_opaque_paragraph_owners_and_refuses_unsafe_copies() {
    use stemma::edit::InsertPosition;

    let embedded = r#"<w:r><w:pict><v:shape xmlns:v="urn:schemas-microsoft-com:vml" id="Card"><v:textbox><w:txbxContent><w:p w14:paraId="00000001"><w:r><w:t>Box.</w:t></w:r></w:p></w:txbxContent></v:textbox></v:shape></w:pict></w:r>"#;
    for move_opaque in [false, true] {
        let first = if move_opaque { embedded } else { "" };
        let last = if move_opaque { "" } else { embedded };
        let source = pack(&document(&format!(
            r#"<w:p w14:paraId="00000111"><w:r><w:t>Move.</w:t></w:r>{first}</w:p>
            <w:p w14:paraId="00000112"><w:r><w:t>Anchor.</w:t></w:r></w:p>
            <w:p w14:paraId="00000113"><w:r><w:t>Keep.</w:t></w:r>{last}</w:p>"#,
        )));
        let original = stemma::api::Document::parse(&source).unwrap();
        let options = stemma::ExportOptions::default();
        let before = original.serialize(&options).unwrap();
        let snapshot = original.snapshot();
        let first = match &snapshot.canonical.blocks[0].block {
            BlockNode::Paragraph(p) => p.id.clone(),
            _ => panic!("paragraph witness"),
        };
        let anchor = match &snapshot.canonical.blocks[1].block {
            BlockNode::Paragraph(p) => p.id.clone(),
            _ => panic!("paragraph witness"),
        };
        let result = original.apply(&EditTransaction {
            steps: vec![EditStep::MoveBlockRange {
                from_block_id: first.clone(),
                to_block_id: first,
                dest_anchor_id: anchor,
                dest_position: InsertPosition::After,
                rationale: None,
                expect: None,
                semantic_hash: None,
            }],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: revision(),
        });
        if move_opaque {
            let error = result
                .err()
                .expect("opaque owners require explicit remapping");
            assert!(
                error
                    .to_string()
                    .contains("cannot preserve persisted identities")
            );
            assert!(error.to_string().contains("opaque paragraph identities"));
        } else {
            let edited = result.unwrap();
            let clone = edited
                .snapshot()
                .canonical
                .blocks
                .iter()
                .find_map(|block| match (&block.status, &block.block) {
                    (stemma::TrackingStatus::Inserted(_), BlockNode::Paragraph(p)) => Some(p),
                    _ => None,
                })
                .unwrap();
            assert_ne!(
                clone.para_id.as_deref(),
                Some("00000001"),
                "an untouched opaque owner still occupies the story's ID space"
            );
            let saved = edited.serialize(&options).unwrap();
            assert!(stemma::api::validate(&saved).ok);
            let reopened = stemma::api::Document::parse(&saved).unwrap();
            assert_eq!(reopened.revisions().len(), 1);
        }
        assert_eq!(original.serialize(&options).unwrap(), before);
    }
}

#[test]
fn sentinel_tracked_move_remints_nested_table_paragraph_and_row_owners() {
    use stemma::edit::InsertPosition;

    let source = pack(&document(
        r#"<w:p w14:paraId="00000111"><w:r><w:t>Anchor.</w:t></w:r></w:p>
        <w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="2400"/></w:tblGrid>
          <w:tr w14:paraId="00000112"><w:tc><w:tcPr><w:tcW w:w="2400" w:type="dxa"/></w:tcPr>
            <w:p w14:paraId="00000113"><w:r><w:t>Outer.</w:t></w:r></w:p>
            <w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="1200"/></w:tblGrid>
              <w:tr w14:paraId="00000114"><w:tc><w:tcPr><w:tcW w:w="1200" w:type="dxa"/></w:tcPr>
                <w:p w14:paraId="00000115"><w:r><w:t>Inner.</w:t></w:r></w:p>
              </w:tc></w:tr>
            </w:tbl><w:p w14:paraId="00000116"/>
          </w:tc></w:tr>
        </w:tbl><w:p w14:paraId="00000117"><w:r><w:t>Tail.</w:t></w:r></w:p>"#,
    ));
    let original = stemma::api::Document::parse(&source).unwrap();
    let snapshot = original.snapshot();
    let anchor = match &snapshot.canonical.blocks[0].block {
        BlockNode::Paragraph(p) => p.id.clone(),
        _ => panic!("paragraph anchor"),
    };
    let table = match &snapshot.canonical.blocks[1].block {
        BlockNode::Table(t) => t.id.clone(),
        _ => panic!("nested table witness"),
    };
    let edited = original
        .apply(&EditTransaction {
            steps: vec![EditStep::MoveBlockRange {
                from_block_id: table.clone(),
                to_block_id: table,
                dest_anchor_id: anchor,
                dest_position: InsertPosition::Before,
                rationale: None,
                expect: None,
                semantic_hash: None,
            }],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: revision(),
        })
        .unwrap();
    fn collect(block: &BlockNode, ids: &mut Vec<String>) {
        match block {
            BlockNode::Paragraph(p) => ids.extend(p.para_id.clone()),
            BlockNode::Table(t) => {
                for row in &t.rows {
                    ids.extend(row.para_id.clone());
                    for block in row.cells.iter().flat_map(|cell| &cell.blocks) {
                        collect(block, ids);
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => panic!("modeled witness"),
        }
    }
    let mut ids = Vec::new();
    for block in &edited.snapshot().canonical.blocks {
        collect(&block.block, &mut ids);
    }
    assert_eq!(ids.len(), 12, "seven source owners and five clone owners");
    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        ids.len(),
        "row and nested paragraph owners share the same part-local ID space"
    );
}

#[test]
fn sentinel_empty_run_inside_field_survives_save_and_full_resolution() {
    // CT_R permits rPr with zero inner-content children (§17.3.2.25).
    // Resolving a different paragraph must not erase this field's authored
    // empty run merely because that run contributes no displayed text.
    let source = pack(&document(
        r#"<w:p>
          <w:r><w:fldChar w:fldCharType="begin"/></w:r>
          <w:r><w:instrText xml:space="preserve"> PAGE </w:instrText></w:r>
          <w:r w:rsidRPr="00112233"><w:rPr><w:highlight w:val="yellow"/></w:rPr></w:r>
          <w:r><w:fldChar w:fldCharType="separate"/></w:r>
          <w:r><w:t>1</w:t></w:r>
          <w:r><w:fldChar w:fldCharType="end"/></w:r>
        </w:p>
        <w:p><w:r><w:t>Kept.</w:t></w:r><w:ins w:id="42" w:author="Editor"><w:r><w:t> Added.</w:t></w:r></w:ins></w:p>"#,
    ));
    let original = stemma::api::Document::parse(&source).unwrap();
    let options = stemma::ExportOptions::default();
    let saved = [
        original.serialize(&options).unwrap(),
        original
            .project(stemma::Resolution::AcceptAll)
            .unwrap()
            .serialize(&options)
            .unwrap(),
        original
            .project(stemma::Resolution::RejectAll)
            .unwrap()
            .serialize(&options)
            .unwrap(),
    ];
    for bytes in saved {
        let archive = stemma::docx::DocxArchive::read(&bytes).unwrap();
        let tree = xmltree::Element::parse(archive.get("word/document.xml").unwrap()).unwrap();
        let field = tree.get_child("body").unwrap().get_child("p").unwrap();
        let runs: Vec<_> = field
            .children
            .iter()
            .filter_map(xmltree::XMLNode::as_element)
            .filter(|child| child.name == "r")
            .collect();
        assert_eq!(
            runs.len(),
            6,
            "retain every field-frame run, including the empty one"
        );
        let empty = runs[2];
        assert_eq!(empty.attributes.len(), 1);
        assert!(
            empty
                .attributes
                .iter()
                .any(|(name, value)| name.local_name == "rsidRPr" && value == "00112233")
        );
        let properties = empty.get_child("rPr").unwrap();
        assert_eq!(
            empty
                .children
                .iter()
                .filter_map(xmltree::XMLNode::as_element)
                .count(),
            1
        );
        let highlight = properties.get_child("highlight").unwrap();
        assert!(
            highlight
                .attributes
                .iter()
                .any(|(name, value)| name.local_name == "val" && value == "yellow")
        );
        let reopened = stemma::api::Document::parse(&bytes).unwrap();
        let resaved = reopened.serialize(&options).unwrap();
        let second = stemma::docx::DocxArchive::read(&resaved).unwrap();
        let second_tree =
            xmltree::Element::parse(second.get("word/document.xml").unwrap()).unwrap();
        let second_field = second_tree
            .get_child("body")
            .unwrap()
            .get_child("p")
            .unwrap();
        assert_eq!(
            second_field
                .children
                .iter()
                .filter_map(xmltree::XMLNode::as_element)
                .filter(|child| child.name == "r")
                .count(),
            6,
            "preservation must survive reopening"
        );
    }
}

#[test]
fn sentinel_full_resolution_preserves_virtual_running_heads() {
    let bytes = pack_with_parts(&document(
        r#"<w:p><w:r><w:t>Kept.</w:t></w:r><w:ins w:id="1" w:author="Editor"><w:r><w:t> Added.</w:t></w:r></w:ins></w:p>"#,
    ), &[ExtraPart {
        name: "word/styles.xml",
        content_type: "application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml",
        rel: ("rStyle", "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles", "styles.xml"),
        xml: r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:pPrDefault><w:pPr><w:spacing w:after="200" w:line="276" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style></w:styles>"#.into(),
    }]);
    let original = stemma::api::Document::parse(&bytes).unwrap();
    let source = &original.snapshot().canonical;
    assert!(!source.headers.is_empty() && !source.footers.is_empty());
    assert!(source.headers.iter().all(|story| story.synthesized));
    assert!(source.footers.iter().all(|story| story.synthesized));
    for resolution in [stemma::Resolution::AcceptAll, stemma::Resolution::RejectAll] {
        let projected = original.project(resolution).unwrap();
        assert_eq!(
            projected.snapshot().canonical.headers,
            source.headers,
            "virtual blanks are not physical paragraph edits"
        );
        assert_eq!(projected.snapshot().canonical.footers, source.footers);
        let saved = projected
            .serialize(&stemma::ExportOptions::default())
            .unwrap();
        let reopened = stemma::api::Document::parse(&saved).unwrap();
        assert!(
            stemma::roundtrip_compare::compare_canon_docs(
                &projected.snapshot().canonical,
                &reopened.snapshot().canonical
            )
            .is_empty()
        );
        let archive = stemma::docx::DocxArchive::read(&saved).unwrap();
        let content_types =
            std::str::from_utf8(archive.get("[Content_Types].xml").unwrap()).unwrap();
        assert!(
            !content_types.contains("wordprocessingml.header+xml")
                && !content_types.contains("wordprocessingml.footer+xml")
        );
    }
}

#[test]
fn sentinel_literal_replacement_bounds_exclude_unrequested_run_properties() {
    use stemma::edit::{
        BarrierPolicy, ExpectedMatches, MatchMode, ReplaceTextOptions, ReplaceTextScope,
        plan_replace_text,
    };
    let bytes = pack(&document(
        r#"<w:p><w:r><w:rPr><w:color w:val="334455"/></w:rPr><w:t xml:space="preserve">Patient: </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>Émile</w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">&#160;&#160;&#160;&#160;&#160;</w:t></w:r><w:r><w:rPr><w:color w:val="334455"/></w:rPr><w:t xml:space="preserve">&#160;Species: cat</w:t></w:r></w:p>"#,
    ));
    let doc = stemma::api::Document::parse(&bytes).unwrap();
    let canonical = &doc.snapshot().canonical;
    let BlockNode::Paragraph(para) = &canonical.blocks[0].block else {
        panic!("paragraph witness");
    };
    let plan = plan_replace_text(
        canonical,
        &ReplaceTextOptions {
            old: "Émile\u{a0}\u{a0}\u{a0}".into(),
            new: "Émile\u{a0}II".into(),
            author: "Reviewer".into(),
            scope: ReplaceTextScope::SingleBlock(para.id.clone()),
            expected: ExpectedMatches::Count(1),
            match_mode: MatchMode::Exact,
            on_barrier_match: BarrierPolicy::Fail,
        },
    )
    .unwrap();
    let [EditStep::ReplaceSpanText { expect, .. }] = plan.steps.as_slice() else {
        panic!("one exact local splice required");
    };
    assert_eq!(
        expect.as_deref(),
        Some("Émile\u{a0}\u{a0}\u{a0}\u{a0}\u{a0}")
    );
    let edited = doc
        .apply(&EditTransaction {
            steps: plan.steps,
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: revision(),
        })
        .unwrap();
    let options = stemma::ExportOptions::default();
    let pending = edited.serialize(&options).unwrap();
    assert!(stemma::api::validate(&pending).ok);
    let reopened = stemma::api::Document::parse(&pending).unwrap();
    let styled_chars = |doc: &stemma::api::Document| {
        let BlockNode::Paragraph(p) = &doc.snapshot().canonical.blocks[0].block else {
            panic!("paragraph witness");
        };
        p.all_inlines()
            .flat_map(|inline| {
                let InlineNode::Text(t) = inline else {
                    panic!("text-only witness")
                };
                t.text
                    .chars()
                    .map(|ch| (ch, t.marks.clone(), t.style_props.clone()))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    let baseline = styled_chars(&doc);
    let original_ids = edited
        .revisions()
        .iter()
        .map(|r| r.revision_id)
        .collect::<std::collections::BTreeSet<_>>();
    let saved_ids = reopened
        .revisions()
        .iter()
        .map(|r| r.revision_id)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        saved_ids, original_ids,
        "local edit identities survive reopening"
    );
    assert!(!saved_ids.is_empty());
    for (resolution, accepted) in [
        (stemma::Resolution::AcceptAll, true),
        (stemma::Resolution::RejectAll, false),
        (
            stemma::Resolution::Selective {
                ids: saved_ids.iter().copied().collect(),
                action: stemma::tracked_model::ResolveSelectionAction::Accept,
            },
            true,
        ),
        (
            stemma::Resolution::Selective {
                ids: saved_ids.iter().copied().collect(),
                action: stemma::tracked_model::ResolveSelectionAction::Reject,
            },
            false,
        ),
    ] {
        let terminal = reopened.project(resolution).unwrap();
        let terminal_bytes = terminal.serialize(&options).unwrap();
        assert!(stemma::api::validate(&terminal_bytes).ok);
        let terminal = stemma::api::Document::parse(&terminal_bytes).unwrap();
        assert!(terminal.revisions().is_empty());
        let actual = styled_chars(&terminal);
        if accepted {
            let mut expected = baseline.clone();
            // The request consumes three bold padding characters and replaces
            // them with NBSP + II. Every other character property is unchanged.
            let start = "Patient: Émile".chars().count();
            for (index, ch) in "\u{a0}II".chars().enumerate() {
                expected[start + index].0 = ch;
            }
            assert_eq!(
                actual, expected,
                "Accept must preserve field and neighbor styling"
            );
        } else {
            assert_eq!(actual, baseline, "Reject restores all character properties");
        }
    }
}

#[test]
fn sentinel_table_replacement_pagination_override_is_explicit() {
    for (base, property) in [
        (None, ""),
        (Some(true), "<w:cantSplit/>"),
        (Some(false), "<w:cantSplit w:val=\"0\"/>"),
    ] {
        let source = pack(&document(&format!(
            r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="3000"/></w:tblGrid><w:tr><w:trPr>{property}</w:trPr><w:tc><w:tcPr><w:tcW w:w="3000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>Before</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
        )));
        let original = stemma::api::Document::parse(&source).unwrap();
        let BlockNode::Table(table) = &original.snapshot().canonical.blocks[0].block else {
            panic!("table witness")
        };
        let BlockNode::Paragraph(template) = &table.rows[0].cells[0].blocks[0] else {
            panic!("paragraph witness")
        };
        for requested in [None, Some(true), Some(false)] {
            let mut row = serde_json::json!({"content":[{"content":[{
                "type":"paragraph","attrs":{"format_from":template.id.0},
                "content":[{"type":"text","text":"After"}]
            }]}]});
            if let Some(value) = requested {
                row["attrs"] = serde_json::json!({"cant_split":value});
            }
            let wire = serde_json::json!({"revision":{"author":"Reviewer"},"ops":[{
                "op":"replace","target":table.id.0,
                "content":{"type":"table","content":[row]}
            }]})
            .to_string();
            let mut transaction = stemma::edit_v4::parse_transaction(&wire)
                .unwrap()
                .into_edit_transaction()
                .unwrap();
            if requested.is_some() {
                // A whole-table tracked replacement cannot silently pretend
                // that caller-supplied pagination is a reversible property edit.
                let error = original.apply(&transaction).err().expect("typed refusal");
                assert_eq!(error.code, stemma::ErrorCode::UnsupportedEdit);
                assert!(error.message.contains("carried caller-set formatting"));
            }
            transaction.materialization_mode = MaterializationMode::Direct;
            let changed = original.apply(&transaction).unwrap();
            let bytes = changed
                .serialize(&stemma::ExportOptions::default())
                .unwrap();
            assert_validator_clean("direct-replace-row-pagination", &bytes);
            let reopened = stemma::api::Document::parse(&bytes).unwrap();
            let BlockNode::Table(table) = &reopened.snapshot().canonical.blocks[0].block else {
                panic!("table witness")
            };
            assert_eq!(table.rows[0].cant_split, requested.or(base));
            assert_eq!(all_text(&reopened.snapshot().canonical), "After");
        }
    }
}

#[test]
fn sentinel_row_pagination_retains_directness_and_previous_state() {
    // ISO 29500-1 §§17.4.6, 17.13.5.37: absent inherits the table style;
    // false overrides that style; Reject restores the complete prior trPr.
    let states = [
        (None, ""),
        (Some(true), "<w:cantSplit/>"),
        (Some(false), "<w:cantSplit w:val=\"0\"/>"),
    ];
    for (current, current_xml) in states {
        for (previous, previous_xml) in states {
            let body = format!(
                r#"<w:tbl><w:tblPr><w:tblStyle w:val="Kept"/></w:tblPr><w:tblGrid><w:gridCol w:w="3000"/></w:tblGrid><w:tr><w:trPr>{current_xml}<w:trPrChange w:id="17" w:author="Reviewer"><w:trPr>{previous_xml}</w:trPr></w:trPrChange></w:trPr><w:tc><w:tcPr><w:tcW w:w="3000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>Unchanged cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
            );
            let source = pack_with_parts(&document(&body), &[ExtraPart {
                name: "word/styles.xml",
                content_type: "application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml",
                rel: ("rStyle", "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles", "styles.xml"),
                xml: r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style w:type="table" w:styleId="Kept"><w:name w:val="Kept"/><w:trPr><w:cantSplit/></w:trPr></w:style></w:styles>"#.into(),
            }]);
            let original = stemma::api::Document::parse(&source).unwrap();
            let saved = original
                .serialize(&stemma::ExportOptions::default())
                .unwrap();
            assert_validator_clean("row-pagination-pending", &saved);
            let reopened = stemma::api::Document::parse(&saved).unwrap();
            let BlockNode::Table(table) = &reopened.snapshot().canonical.blocks[0].block else {
                panic!("table witness")
            };
            assert_eq!(table.rows[0].cant_split, current);
            assert_eq!(
                table.rows[0]
                    .formatting_change
                    .as_ref()
                    .unwrap()
                    .previous_cant_split,
                previous
            );
            for (resolution, expected) in [
                (stemma::Resolution::AcceptAll, current),
                (stemma::Resolution::RejectAll, previous),
            ] {
                let terminal = reopened.project(resolution).unwrap();
                let bytes = terminal
                    .serialize(&stemma::ExportOptions::default())
                    .unwrap();
                assert_validator_clean("row-pagination-terminal", &bytes);
                let terminal = stemma::api::Document::parse(&bytes).unwrap();
                let BlockNode::Table(table) = &terminal.snapshot().canonical.blocks[0].block else {
                    panic!("table witness")
                };
                assert_eq!(table.rows[0].cant_split, expected);
                assert!(table.rows[0].formatting_change.is_none());
                assert_eq!(
                    all_text(&terminal.snapshot().canonical),
                    all_text(&original.snapshot().canonical)
                );
            }
        }
    }
}

#[test]
fn sentinel_insert_table_authors_keep_rows_together() {
    // ISO 29500-1 §17.4.6: an explicitly kept row must carry cantSplit;
    // an unstyled new row without the property may split across pages.
    let source = pack(&document(
        r#"<w:p><w:r><w:t>Unchanged anchor</w:t></w:r></w:p>"#,
    ));
    let original = stemma::api::Document::parse(&source).unwrap();
    let BlockNode::Paragraph(anchor) = &original.snapshot().canonical.blocks[0].block else {
        panic!("paragraph witness")
    };
    let rows = [
        (Some(true), "Kept"),
        (Some(false), "May split"),
        (None, "Default"),
    ]
    .into_iter()
    .map(|(value, text)| {
        let mut row = serde_json::json!({"content":[{"content":[{
            "type":"paragraph","attrs":{"format_from":anchor.id.0},
            "content":[{"type":"text","text":text}]
        }]}]});
        if let Some(value) = value {
            row["attrs"] = serde_json::json!({"cant_split":value});
        }
        row
    })
    .collect::<Vec<_>>();
    let json = serde_json::json!({"revision":{"author":"Reviewer"},"ops":[{
        "op":"insert","target":{"anchor":anchor.id.0,"position":"after"},
        "content":[{"type":"table","content":rows}]
    }]})
    .to_string();
    let transaction = stemma::edit_v4::parse_transaction(&json)
        .expect("keep-row-together is explicit typed authoring intent")
        .into_edit_transaction()
        .unwrap();
    let pending = original.apply(&transaction).unwrap();
    let review = pending.review().unwrap();
    assert!(
        review.direct_changes.is_empty(),
        "native row carriers cover the complete insertion"
    );
    let proposal_ids = pending
        .revisions()
        .iter()
        .map(|r| r.revision_id)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        proposal_ids.len(),
        1,
        "one requested table insertion proposal"
    );
    let bytes = pending
        .serialize(&stemma::ExportOptions::default())
        .unwrap();
    assert_validator_clean("insert-kept-table-rows", &bytes);
    let reopened = stemma::api::Document::parse(&bytes).unwrap();
    // Enumerable revisions are durable: one table insertion must not turn
    // into independently identified row proposals merely by saving/reopening.
    assert_eq!(
        reopened
            .revisions()
            .iter()
            .map(|r| r.revision_id)
            .collect::<std::collections::BTreeSet<_>>(),
        proposal_ids,
        "authored table proposal identities must survive serialization and reopening"
    );
    for (action, expected_blocks) in [
        (stemma::tracked_model::ResolveSelectionAction::Accept, 2),
        (stemma::tracked_model::ResolveSelectionAction::Reject, 1),
    ] {
        let selected = reopened
            .project(stemma::Resolution::Selective {
                ids: proposal_ids.iter().copied().collect(),
                action,
            })
            .unwrap();
        let bytes = selected
            .serialize(&stemma::ExportOptions::default())
            .unwrap();
        assert_validator_clean("selective-inserted-table", &bytes);
        let saved = stemma::api::Document::parse(&bytes).unwrap();
        assert!(
            saved.revisions().is_empty(),
            "one selector must resolve all row carriers"
        );
        assert_eq!(saved.snapshot().canonical.blocks.len(), expected_blocks);
        if expected_blocks == 2 {
            let BlockNode::Table(table) = &saved.snapshot().canonical.blocks[1].block else {
                panic!("selected table insertion")
            };
            assert_eq!(
                table.rows.iter().map(|r| r.cant_split).collect::<Vec<_>>(),
                [Some(true), Some(false), None]
            );
            assert_eq!(
                all_text(&saved.snapshot().canonical),
                "Unchanged anchor Kept May split Default"
            );
        } else {
            assert_eq!(
                all_text(&saved.snapshot().canonical),
                all_text(&original.snapshot().canonical)
            );
        }
    }
    let accepted = reopened.project(stemma::Resolution::AcceptAll).unwrap();
    let accepted_bytes = accepted
        .serialize(&stemma::ExportOptions::default())
        .unwrap();
    assert_validator_clean("accept-kept-table-rows", &accepted_bytes);
    let accepted = stemma::api::Document::parse(&accepted_bytes).unwrap();
    let BlockNode::Table(table) = &accepted.snapshot().canonical.blocks[1].block else {
        panic!("inserted table witness")
    };
    assert_eq!(
        table.rows.iter().map(|r| r.cant_split).collect::<Vec<_>>(),
        [Some(true), Some(false), None]
    );
    assert_eq!(
        all_text(&accepted.snapshot().canonical),
        "Unchanged anchor Kept May split Default"
    );
    let rejected = reopened.project(stemma::Resolution::RejectAll).unwrap();
    let rejected_bytes = rejected
        .serialize(&stemma::ExportOptions::default())
        .unwrap();
    assert_validator_clean("reject-kept-table-rows", &rejected_bytes);
    let rejected = stemma::api::Document::parse(&rejected_bytes).unwrap();
    assert_eq!(rejected.snapshot().canonical.blocks.len(), 1);
    assert_eq!(
        all_text(&rejected.snapshot().canonical),
        all_text(&original.snapshot().canonical)
    );
}

#[test]
fn sentinel_insert_tabs_distinguishes_inheritance_empty_and_clear() {
    let source = pack_with_parts(&document(
        r#"<w:p><w:pPr><w:pStyle w:val="Tabbed"/><w:tabs><w:tab w:val="left" w:pos="720"/></w:tabs></w:pPr><w:r><w:t>Template</w:t></w:r></w:p>"#,
    ), &[ExtraPart {
        name: "word/styles.xml",
        content_type: "application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml",
        rel: ("rStyle", "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles", "styles.xml"),
        xml: r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style w:type="paragraph" w:styleId="Tabbed"><w:name w:val="Tabbed"/><w:pPr><w:tabs><w:tab w:val="left" w:pos="1234"/></w:tabs></w:pPr></w:style></w:styles>"#.into(),
    }]);
    for (use_template, override_stops, expected_direct, inherited_visible) in [
        (true, None, Some((720, stemma::TabAlignment::Left)), true),
        (true, Some(serde_json::json!([])), None, true),
        (
            true,
            Some(serde_json::json!([{"position":1234,"alignment":"clear"}])),
            Some((1234, stemma::TabAlignment::Clear)),
            false,
        ),
        (false, None, Some((720, stemma::TabAlignment::Left)), true),
        (false, Some(serde_json::json!([])), None, true),
        (
            false,
            Some(serde_json::json!([{"position":1234,"alignment":"clear"}])),
            Some((1234, stemma::TabAlignment::Clear)),
            false,
        ),
    ] {
        let original = stemma::api::Document::parse(&source).unwrap();
        let BlockNode::Paragraph(p) = &original.snapshot().canonical.blocks[0].block else {
            panic!("paragraph witness")
        };
        let mut attrs = serde_json::json!({});
        if use_template {
            attrs["format_from"] = serde_json::json!(p.id.0);
        }
        if let Some(stops) = override_stops {
            attrs["tab_stops"] = stops;
        }
        let json = serde_json::json!({"revision":{"author":"Reviewer"},"ops":[{
            "op":"insert","target":{"anchor":p.id.0,"position":"after"},
            "content":[{"type":"paragraph","attrs":attrs,"content":[{"type":"text","text":"New\tbody"}]}]
        }]}).to_string();
        let transaction = stemma::edit_v4::parse_transaction(&json)
            .unwrap()
            .into_edit_transaction()
            .unwrap();
        let pending = original
            .apply(&transaction)
            .unwrap()
            .serialize(&stemma::ExportOptions::default())
            .unwrap();
        assert_validator_clean("insert-tab-inheritance", &pending);
        let reopened = stemma::api::Document::parse(&pending).unwrap();
        let accepted = reopened.project(stemma::Resolution::AcceptAll).unwrap();
        let saved = accepted
            .serialize(&stemma::ExportOptions::default())
            .unwrap();
        let accepted = stemma::api::Document::parse(&saved).unwrap();
        let BlockNode::Paragraph(inserted) = &accepted.snapshot().canonical.blocks[1].block else {
            panic!("paragraph witness")
        };
        let actual: Vec<_> = inserted
            .tab_stops
            .iter()
            .map(|t| (t.position, t.alignment.clone()))
            .collect();
        assert_eq!(actual, expected_direct.into_iter().collect::<Vec<_>>());
        assert_eq!(
            inserted
                .effective_tab_stops_rel
                .iter()
                .any(|t| t.position == 1234),
            inherited_visible
        );
        let rejected = reopened.project(stemma::Resolution::RejectAll).unwrap();
        let rejected_bytes = rejected
            .serialize(&stemma::ExportOptions::default())
            .unwrap();
        assert_validator_clean("reject-insert-tab-inheritance", &rejected_bytes);
        let rejected = stemma::api::Document::parse(&rejected_bytes).unwrap();
        assert_eq!(rejected.snapshot().canonical.blocks.len(), 1);
        assert_eq!(
            all_text(&rejected.snapshot().canonical),
            all_text(&original.snapshot().canonical)
        );
        let BlockNode::Paragraph(restored) = &rejected.snapshot().canonical.blocks[0].block else {
            panic!("paragraph witness")
        };
        assert_eq!(restored.tab_stops, p.tab_stops);
        assert_eq!(restored.effective_tab_stops_rel, p.effective_tab_stops_rel);
    }
}

#[test]
fn sentinel_insert_authors_explicit_tabs_without_changing_its_template() {
    let source = pack(&document(
        r#"<w:p><w:pPr><w:tabs><w:tab w:val="left" w:pos="720"/></w:tabs></w:pPr><w:r><w:t>Template</w:t><w:tab/><w:t>body</w:t></w:r></w:p>"#,
    ));
    let original = stemma::api::Document::parse(&source).expect("parse tab template");
    let BlockNode::Paragraph(template) = &original.snapshot().canonical.blocks[0].block else {
        panic!("paragraph witness")
    };
    let id = &template.id;
    let json = serde_json::json!({
        "revision": {"author": "Reviewer"},
        "ops": [{"op": "insert", "target": {"anchor": id.0, "position": "after"},
            "content": [{"type": "paragraph", "attrs": {"format_from": id.0,
                "tab_stops": [{"position": 1440, "alignment": "left", "leader": "dot"}]},
                "content": [{"type": "text", "text": "Info:\tNew body"}]}]}]
    })
    .to_string();
    let transaction = stemma::edit_v4::parse_transaction(&json)
        .expect("explicit tab authoring grammar")
        .into_edit_transaction()
        .expect("typed tab intent");
    let pending = original
        .apply(&transaction)
        .expect("insert with explicit tabs");
    let bytes = pending
        .serialize(&stemma::ExportOptions::default())
        .expect("save insertion");
    assert_validator_clean("insert-explicit-tabs", &bytes);
    let reopened = stemma::api::Document::parse(&bytes).expect("reopen insertion");
    let accepted = reopened
        .project(stemma::Resolution::AcceptAll)
        .expect("accept insertion");
    let paragraphs: Vec<_> = accepted
        .snapshot()
        .canonical
        .blocks
        .iter()
        .map(|b| {
            let BlockNode::Paragraph(p) = &b.block else {
                panic!("paragraph witness")
            };
            p
        })
        .collect();
    assert_eq!(paragraphs.len(), 2);
    assert_eq!(
        paragraphs[0].tab_stops[0].position, 720,
        "template remains untouched"
    );
    assert_eq!(
        paragraphs[1].tab_stops,
        vec![stemma::TabStopDef {
            position: 1440,
            alignment: stemma::TabAlignment::Left,
            leader: Some(stemma::TabLeader::Dot),
        }]
    );
    let rejected = reopened
        .project(stemma::Resolution::RejectAll)
        .expect("reject insertion");
    assert_eq!(
        all_text(&rejected.snapshot().canonical),
        all_text(&original.snapshot().canonical)
    );
    assert_eq!(rejected.snapshot().canonical.blocks.len(), 1);
    let BlockNode::Paragraph(p) = &rejected.snapshot().canonical.blocks[0].block else {
        panic!("paragraph witness")
    };
    assert_eq!(p.tab_stops[0].position, 720);
}

// ── packaging: minimal in-memory DOCX (the repo-wide test idiom) ───────────

/// Pack a `word/document.xml` body into a minimal single-part DOCX.
fn pack(document_xml: &str) -> Vec<u8> {
    pack_full(document_xml, &[], &[])
}

/// Pack `word/document.xml` plus extra parts (e.g. footnotes / headers). Each
/// part contributes a `[Content_Types]` override AND a `document.xml.rels`
/// relationship pointing at it.
fn pack_with_parts(document_xml: &str, extra: &[ExtraPart]) -> Vec<u8> {
    pack_full(document_xml, extra, &[])
}

/// An internal OPC part (footnotes / header / footer) referenced from
/// `document.xml` by a relationship.
#[derive(Clone)]
struct ExtraPart {
    /// Part name inside the package, e.g. `word/footnotes.xml`.
    name: &'static str,
    /// `<Override>` content-type for `[Content_Types].xml`.
    content_type: &'static str,
    /// Relationship entry (id, type, target) for `document.xml.rels`.
    rel: (&'static str, &'static str, &'static str),
    xml: String,
}

/// An external relationship (e.g. a hyperlink) — `TargetMode="External"`, no
/// part, no content-type override.
#[derive(Clone, Copy)]
struct ExternalRel {
    id: &'static str,
    rel_type: &'static str,
    target: &'static str,
}

fn pack_full(document_xml: &str, parts: &[ExtraPart], external_rels: &[ExternalRel]) -> Vec<u8> {
    use std::io::Write;
    use zip::write::FileOptions;

    let overrides: String = parts
        .iter()
        .map(|p| {
            format!(
                r#"<Override PartName="/{}" ContentType="{}"/>"#,
                p.name, p.content_type
            )
        })
        .collect();
    let content_types = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>{overrides}</Types>"#
    );

    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;

    let mut doc_rel_entries = String::new();
    for p in parts {
        let (id, ty, target) = p.rel;
        doc_rel_entries.push_str(&format!(
            r#"<Relationship Id="{id}" Type="{ty}" Target="{target}"/>"#
        ));
    }
    for r in external_rels {
        doc_rel_entries.push_str(&format!(
            r#"<Relationship Id="{}" Type="{}" Target="{}" TargetMode="External"/>"#,
            r.id, r.rel_type, r.target
        ));
    }
    let doc_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{doc_rel_entries}</Relationships>"#
    );

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
        for p in parts {
            zip.start_file(p.name, opts).unwrap();
            zip.write_all(p.xml.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

/// Wrap a body fragment in a full `w:document`. Declares the namespaces the
/// witnesses use (w, w14 for paraId, r for hyperlinks).
fn document(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    )
}

#[test]
fn sentinel_cell_paragraph_insertion_preserves_native_projections() {
    use stemma::api::Document;
    use stemma::edit::{BlockSpec, InsertPosition, ParagraphBlockSpec};
    for native_list in [false, true] {
        let paragraph = |text: &str| {
            let numbering = if native_list {
                "<w:numPr><w:ilvl w:val=\"0\"/><w:numId w:val=\"1\"/></w:numPr>"
            } else {
                ""
            };
            format!(
                "<w:p><w:pPr>{numbering}<w:ind w:left=\"360\"/></w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>"
            )
        };
        let table = |texts: &[&str]| {
            format!(
                "<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w=\"2400\"/><w:gridCol w:w=\"2400\"/></w:tblGrid><w:tr><w:tc>{}</w:tc><w:tc><w:p><w:r><w:t>Untouched cell.</w:t></w:r><w:fldSimple w:instr=\"PAGE\"><w:r><w:t>1</w:t></w:r></w:fldSimple></w:p></w:tc></w:tr></w:tbl>",
                texts.iter().map(|text| paragraph(text)).collect::<String>()
            )
        };
        let package = |texts: &[&str], nested: bool| {
            let body = table(texts);
            let body = if nested {
                format!(
                    "<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w=\"4800\"/></w:tblGrid><w:tr><w:tc>{body}<w:p/></w:tc></w:tr></w:tbl>"
                )
            } else {
                body
            };
            let xml = document(&body);
            if native_list {
                pack_with_parts(&xml, &[ExtraPart {
                name: "word/numbering.xml",
                content_type: "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml",
                rel: ("rIdNumbering", "http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering", "numbering.xml"),
                xml: "<w:numbering xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:abstractNum w:abstractNumId=\"0\"><w:lvl w:ilvl=\"0\"><w:start w:val=\"1\"/><w:numFmt w:val=\"bullet\"/><w:lvlText w:val=\"•\"/></w:lvl></w:abstractNum><w:num w:numId=\"1\"><w:abstractNumId w:val=\"0\"/></w:num></w:numbering>".into(),
            }])
            } else {
                pack(&xml)
            }
        };
        for nested in [false, true] {
            for (anchor_text, position, expected_texts) in [
                (
                    "First.",
                    InsertPosition::Before,
                    ["Inserted.", "First.", "Last."],
                ),
                (
                    "First.",
                    InsertPosition::After,
                    ["First.", "Inserted.", "Last."],
                ),
                (
                    "Last.",
                    InsertPosition::After,
                    ["First.", "Last.", "Inserted."],
                ),
            ] {
                let source = package(&["First.", "Last."], nested);
                let expected = package(&expected_texts, nested);
                let original = Document::parse(&source).unwrap();
                let paragraphs = common::all_paragraphs(&original.snapshot().canonical);
                let p = paragraphs
                    .into_iter()
                    .find(|p| common::paragraph_text(p).ends_with(anchor_text))
                    .unwrap();
                for mode in [
                    MaterializationMode::TrackedChange,
                    MaterializationMode::Direct,
                ] {
                    let edited = original
                        .apply(&EditTransaction {
                            steps: vec![EditStep::InsertParagraphs {
                                anchor_block_id: p.id.clone(),
                                position,
                                rationale: None,
                                blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                                    tab_stops: None,
                                    format_from: None,
                                    role: None,
                                    content: ParagraphContent {
                                        fragments: vec![ContentFragment::Text("Inserted.".into())],
                                    },
                                    restart_numbering: false,
                                    list: None,
                                })],
                            }],
                            summary: None,
                            materialization_mode: mode,
                            revision: RevisionInfo {
                                revision_id: 1,
                                identity: 0,
                                author: Some("Editor".into()),
                                date: Some("2026-07-01T00:00:00Z".into()),
                                apply_op_id: None,
                            },
                        })
                        .expect("cell paragraph insertion is supported");
                    let options = stemma::ExportOptions::default();
                    let bytes = edited.serialize(&options).unwrap();
                    if mode == MaterializationMode::TrackedChange
                        && position == InsertPosition::After
                        && anchor_text == "Last."
                    {
                        // A cell-tail append introduces the preceding break;
                        // its final paragraph retains the original cell mark.
                        let inserted = common::all_paragraphs(&edited.snapshot().canonical)
                            .into_iter()
                            .find(|paragraph| {
                                common::paragraph_text(paragraph).ends_with("Inserted.")
                            })
                            .unwrap();
                        assert_eq!(
                            inserted.para_mark_status,
                            Some(stemma::TrackingStatus::Normal)
                        );
                    }
                    assert!(
                        stemma::api::validate(&bytes).ok,
                        "serialized insertion must validate"
                    );
                    let reopened = Document::parse(&bytes).unwrap();
                    for (projection, reference) in [
                        (reopened.read_accepted().unwrap(), expected.as_slice()),
                        (
                            reopened.read_rejected().unwrap(),
                            if mode == MaterializationMode::TrackedChange {
                                source.as_slice()
                            } else {
                                expected.as_slice()
                            },
                        ),
                    ] {
                        let actual_bytes = projection.serialize(&options).unwrap();
                        assert!(stemma::api::validate(&actual_bytes).ok);
                        let actual = Document::parse(&actual_bytes).unwrap();
                        let gold = Document::parse(reference).unwrap();
                        let differences = stemma::roundtrip_compare::compare_canon_docs(
                            &gold.snapshot().canonical,
                            &actual.snapshot().canonical,
                        );
                        assert!(differences.is_empty(), "{mode:?}: {differences:?}");
                        // Terminal projections are fixed points, including
                        // after serialization. Comparing an unchanged terminal
                        // document to itself must not invent an edit either.
                        let actual_doc = &actual.snapshot().canonical;
                        assert!(
                            diff_documents(actual_doc, &gold.snapshot().canonical)
                                .unwrap()
                                .changes
                                .is_empty()
                        );
                        assert!(
                            diff_documents(actual_doc, actual_doc)
                                .unwrap()
                                .changes
                                .is_empty()
                        );
                    }
                    if mode == MaterializationMode::TrackedChange {
                        let report = edited.review().unwrap();
                        assert!(report.direct_changes.is_empty(), "{report:?}");
                        assert!(report.untouched.violations.is_empty(), "{report:?}");
                    }
                }
            }
        }
    }
}

#[test]
fn sentinel_explicit_template_preserves_complete_run_context() {
    use stemma::api::Document;
    use stemma::edit::{BlockSpec, InsertPosition, ParagraphBlockSpec};
    // An explicitly selected text template supplies complete source run
    // context, not merely its rPr. Repeated rsid values are session context,
    // not unique node/revision identities. Word's glyph placement can depend
    // on the physical run context even when visible character properties agree.
    for in_cell in [false, true] {
        let content = "<w:p><w:r w:rsidR=\"00112233\"><w:rPr><w:rFonts w:ascii=\"Arial\"/><w:sz w:val=\"24\"/></w:rPr><w:t xml:space=\"preserve\">North </w:t></w:r><w:r w:rsidR=\"00223344\" w:rsidRPr=\"00334455\"><w:rPr><w:rFonts w:ascii=\"Arial\"/><w:sz w:val=\"24\"/></w:rPr><w:t>field:</w:t></w:r></w:p><w:p><w:r><w:t>Tail.</w:t></w:r></w:p>";
        let body = if in_cell {
            format!(
                "<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w=\"4800\"/></w:tblGrid><w:tr><w:tc>{content}</w:tc></w:tr></w:tbl>"
            )
        } else {
            content.into()
        };
        let source = pack(&document(&body));
        let original = Document::parse(&source).unwrap();
        let paragraphs = common::all_paragraphs(&original.snapshot().canonical);
        let template = paragraphs
            .iter()
            .find(|p| common::paragraph_text(p) == "North field:")
            .unwrap();
        let anchor = paragraphs
            .iter()
            .find(|p| common::paragraph_text(p) == "Tail.")
            .unwrap();
        let run_context = |p: &stemma::ParagraphNode| {
            p.all_inlines()
                .map(|inline| match inline {
                    InlineNode::Text(t) => (t.text.to_string(), t.source_run_attrs.clone()),
                    _ => panic!("text-only witness"),
                })
                .collect::<Vec<_>>()
        };
        let expected = run_context(template);
        assert_eq!(expected.len(), 2);
        for mode in [
            MaterializationMode::TrackedChange,
            MaterializationMode::Direct,
        ] {
            let edited = original
                .apply(&EditTransaction {
                    steps: vec![EditStep::InsertParagraphs {
                        anchor_block_id: anchor.id.clone(),
                        position: InsertPosition::Before,
                        rationale: None,
                        blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                            tab_stops: None,
                            format_from: Some(template.id.clone()),
                            role: None,
                            content: ParagraphContent {
                                fragments: vec![ContentFragment::Text("North field:".into())],
                            },
                            restart_numbering: false,
                            list: None,
                        })],
                    }],
                    summary: None,
                    materialization_mode: mode,
                    revision: RevisionInfo {
                        revision_id: 1,
                        identity: 0,
                        author: Some("Editor".into()),
                        date: None,
                        apply_op_id: None,
                    },
                })
                .unwrap();
            let options = stemma::ExportOptions::default();
            let bytes = edited.serialize(&options).unwrap();
            assert!(stemma::api::validate(&bytes).ok);
            let reopened = Document::parse(&bytes).unwrap();
            let accepted = reopened.read_accepted().unwrap();
            let paragraphs = common::all_paragraphs(&accepted.snapshot().canonical);
            let copies: Vec<_> = paragraphs
                .iter()
                .filter(|p| common::paragraph_text(p) == "North field:")
                .collect();
            assert_eq!(copies.len(), 2);
            assert_ne!(
                copies[0].id, copies[1].id,
                "the copy must have a fresh node identity"
            );
            for copy in copies {
                assert_eq!(
                    run_context(copy),
                    expected,
                    "{mode:?}, in_cell={in_cell}: complete template context lost"
                );
            }
            let terminal = accepted.serialize(&options).unwrap();
            assert!(stemma::api::validate(&terminal).ok);
            let saved = Document::parse(&terminal).unwrap();
            assert!(
                stemma::roundtrip_compare::compare_canon_docs(
                    &accepted.snapshot().canonical,
                    &saved.snapshot().canonical
                )
                .is_empty()
            );
            if mode == MaterializationMode::TrackedChange {
                let rejected = reopened.read_rejected().unwrap();
                assert!(
                    stemma::roundtrip_compare::compare_canon_docs(
                        &original.snapshot().canonical,
                        &rejected.snapshot().canonical
                    )
                    .is_empty()
                );
                let review = edited.review().unwrap();
                assert!(
                    review.direct_changes.is_empty() && review.untouched.violations.is_empty(),
                    "{review:?}"
                );
            }
        }
    }
}

/// Native cellDel absorbs its physical slot into the preceding survivor.
/// A reversible partition of that survivor slot preserves the intended width.
#[test]
fn sentinel_column_delete_partition_restores_native_geometry() {
    use stemma::api::Document;
    let pending = pack(&document(
        r#"
<w:tbl><w:tblPr><w:tblW w:w="6200" w:type="dxa"/><w:tblLayout w:type="fixed"/>
<w:tblPrChange w:id="1" w:author="Editor"><w:tblPr><w:tblW w:w="8400" w:type="dxa"/><w:tblLayout w:type="fixed"/></w:tblPr></w:tblPrChange></w:tblPr>
<w:tblGrid><w:gridCol w:w="1800"/><w:gridCol w:w="1000"/><w:gridCol w:w="1000"/><w:gridCol w:w="2400"/>
<w:tblGridChange w:id="2"><w:tblGrid><w:gridCol w:w="1800"/><w:gridCol w:w="2000"/><w:gridCol w:w="2200"/><w:gridCol w:w="2400"/></w:tblGrid></w:tblGridChange></w:tblGrid>
<w:tr><w:tc><w:tcPr><w:tcW w:w="1800" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>A.</w:t></w:r></w:p></w:tc>
<w:tc><w:tcPr><w:tcW w:w="1000" w:type="dxa"/><w:tcPrChange w:id="3" w:author="Editor"><w:tcPr><w:tcW w:w="2000" w:type="dxa"/></w:tcPr></w:tcPrChange></w:tcPr><w:p><w:r><w:t>B.</w:t></w:r></w:p></w:tc>
<w:tc><w:tcPr><w:tcW w:w="1000" w:type="dxa"/><w:cellDel w:id="4" w:author="Editor"/><w:tcPrChange w:id="5" w:author="Editor"><w:tcPr><w:tcW w:w="2200" w:type="dxa"/></w:tcPr></w:tcPrChange></w:tcPr><w:p><w:del w:id="6" w:author="Editor"><w:r><w:delText>C.</w:delText></w:r></w:del></w:p></w:tc>
<w:tc><w:tcPr><w:tcW w:w="2400" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>D.</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/>
"#,
    ));
    let original = Document::parse(&pending).unwrap();
    for (resolution, grid) in [
        (stemma::Resolution::AcceptAll, vec![1800, 2000, 2400]),
        (stemma::Resolution::RejectAll, vec![1800, 2000, 2200, 2400]),
    ] {
        let accept = matches!(resolution, stemma::Resolution::AcceptAll);
        let projected = original.project(resolution).unwrap();
        let BlockNode::Table(table) = &projected.snapshot().canonical.blocks[0].block else {
            panic!("table")
        };
        assert_eq!(table.formatting.grid_cols, grid);
        assert_eq!(table.rows[0].cells.len(), grid.len());
        for (cell, width) in table.rows[0].cells.iter().zip(&grid) {
            assert_eq!(cell.formatting.width.as_ref().unwrap().w, *width);
        }
        let bytes = projected
            .serialize(&stemma::ExportOptions::default())
            .unwrap();
        assert!(stemma::api::validate(&bytes).ok);
        let reopened = Document::parse(&bytes).unwrap();
        assert!(reopened.revisions().is_empty());
        let archive = stemma::docx::DocxArchive::read(&pending).unwrap();
        let terminal = if accept {
            stemma::normalize::normalize_docx(&archive).unwrap().0
        } else {
            stemma::normalize::reject_all_docx(&archive).unwrap().0
        };
        let terminal = Document::parse(&terminal.write().unwrap()).unwrap();
        let BlockNode::Table(table) = &terminal.snapshot().canonical.blocks[0].block else {
            panic!("table")
        };
        assert_eq!(table.formatting.grid_cols, grid);
        for (cell, width) in table.rows[0].cells.iter().zip(&grid) {
            assert_eq!(cell.formatting.width.as_ref().unwrap().w, *width);
        }
    }
}

#[test]
fn sentinel_column_partition_resolves_each_boundary_in_both_paths() {
    use stemma::api::Document;
    let source = [1800u32, 2000, 2200, 2400];
    let grid_xml = |widths: &[u32]| {
        widths
            .iter()
            .map(|width| format!("<w:gridCol w:w=\"{width}\"/>"))
            .collect::<String>()
    };
    for deleted in 0..4 {
        let neighbor = if deleted == 0 { 1 } else { deleted - 1 };
        let mut current = source;
        current[deleted] = source[neighbor] / 2;
        current[neighbor] -= current[deleted];
        let mut rows = String::new();
        for row in 0..2 {
            rows.push_str("<w:tr>");
            for column in 0..4 {
                let id = 3 + row * 12 + column * 3;
                let previous = if column == neighbor || column == deleted {
                    format!(
                        "<w:tcPrChange w:id=\"{id}\" w:author=\"Editor\"><w:tcPr><w:tcW w:w=\"{}\" w:type=\"dxa\"/></w:tcPr></w:tcPrChange>",
                        source[column]
                    )
                } else {
                    String::new()
                };
                let mark = if column == deleted {
                    format!("<w:cellDel w:id=\"{}\" w:author=\"Editor\"/>", id + 1)
                } else {
                    String::new()
                };
                let text = format!("Row {row} column {column}.");
                let content = if column == deleted {
                    format!(
                        "<w:del w:id=\"{}\" w:author=\"Editor\"><w:r><w:delText>{text}</w:delText></w:r></w:del>",
                        id + 2
                    )
                } else {
                    format!("<w:r><w:t>{text}</w:t></w:r>")
                };
                rows.push_str(&format!("<w:tc><w:tcPr><w:tcW w:w=\"{}\" w:type=\"dxa\"/>{mark}{previous}</w:tcPr><w:p>{content}</w:p></w:tc>", current[column]));
            }
            rows.push_str("</w:tr>");
        }
        let body = format!(
            "<w:tbl><w:tblPr><w:tblW w:w=\"{}\" w:type=\"dxa\"/><w:tblLayout w:type=\"fixed\"/><w:tblPrChange w:id=\"1\" w:author=\"Editor\"><w:tblPr><w:tblW w:w=\"8400\" w:type=\"dxa\"/><w:tblLayout w:type=\"fixed\"/></w:tblPr></w:tblPrChange></w:tblPr><w:tblGrid>{}<w:tblGridChange w:id=\"2\"><w:tblGrid>{}</w:tblGrid></w:tblGridChange></w:tblGrid>{rows}</w:tbl><w:p/>",
            current.iter().sum::<u32>(),
            grid_xml(&current),
            grid_xml(&source)
        );
        let bytes = pack(&document(&body));
        let pending = Document::parse(&bytes).unwrap();
        let archive = stemma::docx::DocxArchive::read(&bytes).unwrap();
        for accept in [true, false] {
            let expected: Vec<_> = source
                .iter()
                .copied()
                .enumerate()
                .filter(|(column, _)| !accept || *column != deleted)
                .collect();
            let resolution = if accept {
                stemma::Resolution::AcceptAll
            } else {
                stemma::Resolution::RejectAll
            };
            let model = pending.project(resolution).unwrap();
            let normalized = if accept {
                stemma::normalize::normalize_docx(&archive).unwrap().0
            } else {
                stemma::normalize::reject_all_docx(&archive).unwrap().0
            };
            let raw = Document::parse(&normalized.write().unwrap()).unwrap();
            for projected in [model, raw] {
                let terminal = projected
                    .serialize(&stemma::ExportOptions::default())
                    .unwrap();
                assert!(stemma::api::validate(&terminal).ok);
                let reopened = Document::parse(&terminal).unwrap();
                assert!(reopened.revisions().is_empty());
                let BlockNode::Table(table) = &reopened.snapshot().canonical.blocks[0].block else {
                    panic!("table")
                };
                assert_eq!(
                    table.formatting.grid_cols,
                    expected.iter().map(|(_, width)| *width).collect::<Vec<_>>()
                );
                assert_eq!(table.rows.len(), 2);
                for (row_index, row) in table.rows.iter().enumerate() {
                    assert_eq!(row.cells.len(), expected.len());
                    for (cell, &(column, width)) in row.cells.iter().zip(&expected) {
                        assert_eq!(cell.formatting.width.as_ref().unwrap().w, width);
                        let BlockNode::Paragraph(paragraph) = &cell.blocks[0] else {
                            panic!("cell paragraph")
                        };
                        assert_eq!(
                            common::paragraph_text(paragraph),
                            format!("Row {row_index} column {column}.")
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn sentinel_authored_column_deletion_is_reversible_and_one_intention() {
    use stemma::ResolveSelectionAction;
    use stemma::api::Document;
    use stemma::edit::TableOp;
    let widths = [1800, 2000, 2200, 2400];
    let grid = widths
        .iter()
        .map(|width| format!("<w:gridCol w:w=\"{width}\"/>"))
        .collect::<String>();
    let mut rows = String::new();
    for row in 0..2 {
        rows.push_str("<w:tr>");
        for (column, width) in widths.iter().enumerate() {
            let preferred = width + 100;
            rows.push_str(&format!("<w:tc><w:tcPr><w:tcW w:w=\"{preferred}\" w:type=\"dxa\"/><w:shd w:fill=\"ABCDEF\"/></w:tcPr><w:p><w:r><w:t>Row {row} column {column}.</w:t></w:r></w:p></w:tc>"));
        }
        rows.push_str("</w:tr>");
    }
    let borders = ["top", "bottom", "left", "right", "insideH", "insideV"]
        .iter()
        .map(|side| format!("<w:{side} w:val=\"single\" w:sz=\"4\"/>"))
        .collect::<String>();
    let bytes = pack(&document(&format!(
        "<w:tbl><w:tblPr><w:tblW w:w=\"0\" w:type=\"auto\"/><w:tblBorders>{borders}</w:tblBorders></w:tblPr><w:tblGrid>{grid}</w:tblGrid>{rows}</w:tbl><w:p/>"
    )));
    let original = Document::parse(&bytes).unwrap();
    let BlockNode::Table(source) = &original.snapshot().canonical.blocks[0].block else {
        panic!("table")
    };
    for column in 0..4 {
        let edited = original
            .apply(&EditTransaction {
                steps: vec![EditStep::TableStructureOp {
                    block_id: source.id.clone(),
                    semantic_hash: None,
                    op: TableOp::DeleteColumn {
                        col_index: column,
                        expect_header: None,
                    },
                    rationale: None,
                }],
                summary: None,
                materialization_mode: MaterializationMode::TrackedChange,
                revision: revision(),
            })
            .unwrap();
        let review = edited.review().unwrap();
        assert!(
            review.direct_changes.is_empty() && review.untouched.violations.is_empty(),
            "{review:?}"
        );
        let saved = edited.serialize(&stemma::ExportOptions::default()).unwrap();
        assert!(stemma::api::validate(&saved).ok);
        let reopened = Document::parse(&saved).unwrap();
        let proposals: std::collections::HashSet<_> = reopened
            .revisions()
            .iter()
            .map(|record| record.revision_id)
            .collect();
        assert_eq!(
            proposals.len(),
            1,
            "column geometry and deletion form one intention"
        );
        let id = *proposals.iter().next().unwrap();
        for action in [
            ResolveSelectionAction::Accept,
            ResolveSelectionAction::Reject,
        ] {
            let accept = matches!(action, ResolveSelectionAction::Accept);
            let selected = reopened
                .project(stemma::Resolution::Selective {
                    ids: [id].into_iter().collect(),
                    action,
                })
                .unwrap();
            assert!(selected.revisions().is_empty());
            let projected_canonical = selected.snapshot().canonical.clone();
            let terminal = selected
                .serialize(&stemma::ExportOptions::default())
                .unwrap();
            assert!(stemma::api::validate(&terminal).ok);
            let selected = Document::parse(&terminal).unwrap();
            assert!(
                stemma::roundtrip_compare::compare_canon_docs(
                    &projected_canonical,
                    &selected.snapshot().canonical
                )
                .is_empty(),
                "selection must not leave inherited border views stale"
            );
            let BlockNode::Table(table) = &selected.snapshot().canonical.blocks[0].block else {
                panic!("table")
            };
            let expected: Vec<_> = widths
                .iter()
                .copied()
                .enumerate()
                .filter(|(index, _)| !accept || *index != column)
                .collect();
            assert_eq!(
                table.formatting.grid_cols,
                expected.iter().map(|(_, width)| *width).collect::<Vec<_>>()
            );
            for (row_index, row) in table.rows.iter().enumerate() {
                assert_eq!(row.cells.len(), expected.len());
                for (cell, &(index, width)) in row.cells.iter().zip(&expected) {
                    assert_eq!(
                        cell.formatting.width.as_ref().unwrap().w,
                        if accept { width } else { width + 100 }
                    );
                    assert_eq!(
                        cell.formatting.shading,
                        source.rows[row_index].cells[index].formatting.shading
                    );
                    let BlockNode::Paragraph(paragraph) = &cell.blocks[0] else {
                        panic!("paragraph")
                    };
                    assert_eq!(
                        common::paragraph_text(paragraph),
                        format!("Row {row_index} column {index}.")
                    );
                }
            }
            if !accept {
                assert!(
                    stemma::roundtrip_compare::compare_canon_docs(
                        &original.snapshot().canonical,
                        &selected.snapshot().canonical
                    )
                    .is_empty()
                );
            }
        }
    }
}

#[test]
fn sentinel_cell_tail_replacement_preserves_both_property_readings() {
    use stemma::api::Document;
    use stemma::edit::{BlockSpec, InsertPosition, ParagraphBlockSpec};
    let paragraph = |text: &str, left: u32, bold: bool| {
        // The replacement inherits the original physical final mark, not the
        // template paragraph's editing-session context.
        let session = if text == "Template." {
            "22222222"
        } else {
            "11111111"
        };
        // The copied run retains its effective template session independently
        // of the destination's retained physical mark (§17.3.1.22).
        let run_session = if text == "Replacement." {
            " w:rsidR=\"22222222\""
        } else {
            ""
        };
        format!(
            "<w:p w:rsidRDefault=\"{session}\" w:rsidRPr=\"{session}\"><w:pPr><w:ind w:left=\"{left}\"/><w:rPr>{}</w:rPr></w:pPr><w:r{run_session}><w:rPr>{}</w:rPr><w:t>{text}</w:t></w:r></w:p>",
            if bold { "<w:b/>" } else { "<w:i/>" },
            if bold { "<w:b/>" } else { "<w:i/>" },
        )
    };
    let package = |replaced: bool| {
        let cell = if replaced {
            paragraph("Replacement.", 720, false)
        } else {
            paragraph("Original.", 360, true)
        };
        pack(&document(&format!(
            "<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w=\"2400\"/></w:tblGrid><w:tr><w:tc>{cell}</w:tc></w:tr></w:tbl>{}",
            paragraph("Template.", 720, false),
        )))
    };
    let source = package(false);
    let target = package(true);
    let original = Document::parse(&source).unwrap();
    let paragraphs = common::all_paragraphs(&original.snapshot().canonical);
    let anchor = paragraphs
        .iter()
        .find(|p| common::paragraph_text(p) == "Original.")
        .unwrap();
    let template = paragraphs
        .iter()
        .find(|p| common::paragraph_text(p) == "Template.")
        .unwrap();
    let edited = original
        .apply(&EditTransaction {
            steps: vec![
                EditStep::InsertParagraphs {
                    anchor_block_id: anchor.id.clone(),
                    position: InsertPosition::After,
                    rationale: None,
                    blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                        tab_stops: None,
                        format_from: Some(template.id.clone()),
                        role: None,
                        content: ParagraphContent {
                            fragments: vec![ContentFragment::Text("Replacement.".into())],
                        },
                        restart_numbering: false,
                        list: None,
                    })],
                },
                EditStep::DeleteBlockRange {
                    story: stemma::edit::StoryRef::Body,
                    from_block_id: anchor.id.clone(),
                    to_block_id: anchor.id.clone(),
                    rationale: None,
                    expect: "Original.".into(),
                    semantic_hash: None,
                },
            ],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: revision(),
        })
        .unwrap();
    let options = stemma::ExportOptions::default();
    let bytes = edited.serialize(&options).unwrap();
    assert!(stemma::api::validate(&bytes).ok);
    let reopened = Document::parse(&bytes).unwrap();
    assert_eq!(
        reopened
            .revisions()
            .iter()
            .map(|r| r.revision_id)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        2
    );
    for document in [&edited, &reopened] {
        let ids: std::collections::HashSet<_> =
            document.revisions().iter().map(|r| r.revision_id).collect();
        for (resolution, expected) in [
            (stemma::Resolution::AcceptAll, &target),
            (stemma::Resolution::RejectAll, &source),
            (
                stemma::Resolution::Selective {
                    ids: ids.clone(),
                    action: stemma::ResolveSelectionAction::Accept,
                },
                &target,
            ),
            (
                stemma::Resolution::Selective {
                    ids,
                    action: stemma::ResolveSelectionAction::Reject,
                },
                &source,
            ),
        ] {
            let projected = document.project(resolution).unwrap();
            let serialized = projected.serialize(&options).unwrap();
            assert!(stemma::api::validate(&serialized).ok);
            let actual = Document::parse(&serialized).unwrap();
            let expected = Document::parse(expected).unwrap();
            let differences = stemma::roundtrip_compare::compare_canon_docs(
                &expected.snapshot().canonical,
                &actual.snapshot().canonical,
            );
            assert!(
                differences.is_empty(),
                "full terminal property reading: {differences:?}"
            );
            assert!(actual.revisions().is_empty());
        }
    }
}

#[test]
fn sentinel_cell_interior_paragraph_deletion_preserves_native_projections() {
    use stemma::api::Document;

    // CT_Tc retains its terminating paragraph. Removing an interior paragraph
    // deletes its content and paragraph mark, not the containing cell or the
    // following paragraph's formatting (ISO/IEC 29500-1 §§17.4.65, 17.13.5).
    let package = |include_first: bool, nested: bool| {
        let first = if include_first {
            "<w:p><w:pPr><w:ind w:left=\"360\"/></w:pPr><w:r><w:rPr><w:b/></w:rPr><w:t>First.</w:t></w:r></w:p>"
        } else {
            ""
        };
        let table = format!(
            "<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w=\"2400\"/><w:gridCol w:w=\"2400\"/></w:tblGrid><w:tr><w:tc>{first}<w:p><w:pPr><w:ind w:left=\"720\"/></w:pPr><w:r><w:rPr><w:i/></w:rPr><w:t>Last.</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Untouched.</w:t></w:r><w:fldSimple w:instr=\"PAGE\"><w:r><w:t>1</w:t></w:r></w:fldSimple></w:p></w:tc></w:tr></w:tbl>"
        );
        let body = if nested {
            format!(
                "<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w=\"4800\"/></w:tblGrid><w:tr><w:tc>{table}<w:p/></w:tc></w:tr></w:tbl>"
            )
        } else {
            table
        };
        pack(&document(&body))
    };
    for nested in [false, true] {
        let source = package(true, nested);
        let target = package(false, nested);
        let original = Document::parse(&source).unwrap();
        let paragraph = common::all_paragraphs(&original.snapshot().canonical)
            .into_iter()
            .find(|p| common::paragraph_text(p) == "First.")
            .unwrap();
        for mode in [
            MaterializationMode::TrackedChange,
            MaterializationMode::Direct,
        ] {
            let edited = original
                .apply(&EditTransaction {
                    steps: vec![EditStep::DeleteBlockRange {
                        story: stemma::edit::StoryRef::Body,
                        from_block_id: paragraph.id.clone(),
                        to_block_id: paragraph.id.clone(),
                        rationale: None,
                        expect: "First.".into(),
                        semantic_hash: None,
                    }],
                    summary: None,
                    materialization_mode: mode,
                    revision: RevisionInfo {
                        revision_id: 1,
                        identity: 0,
                        author: Some("Editor".into()),
                        date: Some("2026-07-01T00:00:00Z".into()),
                        apply_op_id: None,
                    },
                })
                .expect("interior cell paragraph deletion must preserve the terminating paragraph");
            let options = stemma::ExportOptions::default();
            let bytes = edited.serialize(&options).unwrap();
            assert!(stemma::api::validate(&bytes).ok);
            let reopened = Document::parse(&bytes).unwrap();
            for (projection, reference) in [
                (reopened.read_accepted().unwrap(), target.as_slice()),
                (
                    reopened.read_rejected().unwrap(),
                    if mode == MaterializationMode::TrackedChange {
                        source.as_slice()
                    } else {
                        target.as_slice()
                    },
                ),
            ] {
                let bytes = projection.serialize(&options).unwrap();
                assert!(stemma::api::validate(&bytes).ok);
                let actual = Document::parse(&bytes).unwrap();
                let expected = Document::parse(reference).unwrap();
                let actual = &actual.snapshot().canonical;
                let expected = &expected.snapshot().canonical;
                let differences = stemma::roundtrip_compare::compare_canon_docs(expected, actual);
                assert!(
                    differences.is_empty(),
                    "{mode:?}, nested={nested}: {differences:?}"
                );
                assert!(diff_documents(actual, expected).unwrap().changes.is_empty());
                assert!(diff_documents(actual, actual).unwrap().changes.is_empty());
            }
            if mode == MaterializationMode::TrackedChange {
                let review = edited.review().unwrap();
                assert!(review.direct_changes.is_empty(), "{review:?}");
                assert!(review.untouched.violations.is_empty(), "{review:?}");
                let ids: std::collections::HashSet<_> =
                    reopened.revisions().iter().map(|r| r.revision_id).collect();
                assert_eq!(
                    ids.len(),
                    1,
                    "whole-paragraph content and pilcrow form one enumerable proposal"
                );
                for id in &ids {
                    for action in [
                        stemma::ResolveSelectionAction::Accept,
                        stemma::ResolveSelectionAction::Reject,
                    ] {
                        let partial = reopened
                            .project(stemma::Resolution::Selective {
                                ids: [*id].into(),
                                action,
                            })
                            .unwrap();
                        let bytes = partial.serialize(&options).unwrap();
                        assert!(stemma::api::validate(&bytes).ok);
                        let saved = Document::parse(&bytes).unwrap();
                        let remaining: std::collections::HashSet<_> =
                            saved.revisions().iter().map(|r| r.revision_id).collect();
                        assert_eq!(
                            remaining,
                            ids.iter().copied().filter(|other| other != id).collect()
                        );
                        for (before, after) in [
                            (stemma::Resolution::AcceptAll, stemma::Resolution::AcceptAll),
                            (stemma::Resolution::RejectAll, stemma::Resolution::RejectAll),
                        ] {
                            let before_save = partial.project(before).unwrap();
                            let after_save = saved.project(after).unwrap();
                            let differences = stemma::roundtrip_compare::compare_canon_docs(
                                &before_save.snapshot().canonical,
                                &after_save.snapshot().canonical,
                            );
                            assert!(
                                differences.is_empty(),
                                "selected {id:?}, {action:?}: {differences:?}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn sentinel_cell_paragraph_deletion_refuses_unsafe_boundaries_and_stale_guards() {
    use stemma::api::Document;
    let source = pack(&document(
        "<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w=\"2400\"/><w:gridCol w:w=\"2400\"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>First.</w:t></w:r></w:p><w:p><w:r><w:t>Last.</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Other.</w:t></w:r></w:p></w:tc></w:tr></w:tbl>",
    ));
    let original = Document::parse(&source).unwrap();
    let paragraphs = common::all_paragraphs(&original.snapshot().canonical);
    let id = |text| {
        paragraphs
            .iter()
            .find(|p| common::paragraph_text(p) == text)
            .unwrap()
            .id
            .clone()
    };
    for mode in [
        MaterializationMode::TrackedChange,
        MaterializationMode::Direct,
    ] {
        for (from, to, expect, guard, message) in [
            (
                "Last.",
                "Last.",
                "Last.",
                None,
                "last paragraph cannot be deleted",
            ),
            ("First.", "Other.", "First.", None, "same table cell"),
            ("First.", "First.", "Wrong.", None, "expect"),
            (
                "First.",
                "First.",
                "First.",
                Some("0000000000000000"),
                "semantic hash",
            ),
        ] {
            let result = original.apply(&EditTransaction {
                steps: vec![EditStep::DeleteBlockRange {
                    story: stemma::edit::StoryRef::Body,
                    from_block_id: id(from),
                    to_block_id: id(to),
                    rationale: None,
                    expect: expect.into(),
                    semantic_hash: guard.map(str::to_string),
                }],
                summary: None,
                materialization_mode: mode,
                revision: RevisionInfo {
                    revision_id: 1,
                    identity: 0,
                    author: Some("Editor".into()),
                    date: None,
                    apply_op_id: None,
                },
            });
            let error = result.err().expect("unsafe or stale deletion must refuse");
            assert!(
                error.to_string().to_lowercase().contains(message),
                "{error}"
            );
            assert!(
                original.revisions().is_empty(),
                "failed transactions cannot mutate their input"
            );
        }
    }
}

#[test]
fn sentinel_cell_paragraph_deletions_resolve_independently() {
    use stemma::api::Document;
    let package = |keep_left: bool, keep_right: bool| {
        let cell = |first: &str, last: &str, keep: bool| {
            format!(
                "<w:tc>{}<w:p><w:r><w:rPr><w:i/></w:rPr><w:t>{last}</w:t></w:r></w:p></w:tc>",
                if keep {
                    format!("<w:p><w:r><w:rPr><w:b/></w:rPr><w:t>{first}</w:t></w:r></w:p>")
                } else {
                    String::new()
                },
            )
        };
        pack(&document(&format!(
            "<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w=\"2400\"/><w:gridCol w:w=\"2400\"/></w:tblGrid><w:tr>{}{}</w:tr></w:tbl>",
            cell("Left first.", "Left last.", keep_left),
            cell("Right first.", "Right last.", keep_right)
        )))
    };
    let original = Document::parse(&package(true, true)).unwrap();
    let paragraphs = common::all_paragraphs(&original.snapshot().canonical);
    let steps = ["Left first.", "Right first."]
        .into_iter()
        .map(|text| {
            let p = paragraphs
                .iter()
                .find(|p| common::paragraph_text(p) == text)
                .unwrap();
            EditStep::DeleteBlockRange {
                story: stemma::edit::StoryRef::Body,
                from_block_id: p.id.clone(),
                to_block_id: p.id.clone(),
                rationale: None,
                expect: text.into(),
                semantic_hash: None,
            }
        })
        .collect();
    let edited = original
        .apply(&EditTransaction {
            steps,
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: RevisionInfo {
                revision_id: 1,
                identity: 0,
                author: Some("Editor".into()),
                date: None,
                apply_op_id: None,
            },
        })
        .unwrap();
    let options = stemma::ExportOptions::default();
    let saved = Document::parse(&edited.serialize(&options).unwrap()).unwrap();
    let records = saved.revisions();
    let ids: std::collections::HashSet<_> = records.iter().map(|r| r.revision_id).collect();
    assert_eq!(
        ids.len(),
        2,
        "deletions in distinct cells must remain independent"
    );
    // The census can report content and pilcrow separately with the same
    // identity. Selection addresses proposals, not physical carrier rows.
    let mut seen = std::collections::HashSet::new();
    let proposals: Vec<_> = records
        .iter()
        .filter(|r| seen.insert(r.revision_id))
        .collect();
    for (index, record) in proposals.iter().enumerate() {
        for accept in [false, true] {
            let partial = saved
                .project(stemma::Resolution::Selective {
                    ids: [record.revision_id].into(),
                    action: if accept {
                        stemma::ResolveSelectionAction::Accept
                    } else {
                        stemma::ResolveSelectionAction::Reject
                    },
                })
                .unwrap();
            let bytes = partial.serialize(&options).unwrap();
            assert!(stemma::api::validate(&bytes).ok);
            let reopened = Document::parse(&bytes).unwrap();
            assert_eq!(
                reopened
                    .revisions()
                    .iter()
                    .map(|r| r.revision_id)
                    .collect::<std::collections::HashSet<_>>(),
                ids.iter()
                    .copied()
                    .filter(|id| *id != record.revision_id)
                    .collect()
            );
            // Revision enumeration follows document order. Once one deletion is
            // settled, full resolution affects only the other cell.
            for accept_rest in [false, true] {
                let keeps = if index == 0 {
                    (!accept, !accept_rest)
                } else {
                    (!accept_rest, !accept)
                };
                let target = Document::parse(&package(keeps.0, keeps.1)).unwrap();
                let terminal = reopened
                    .project(if accept_rest {
                        stemma::Resolution::AcceptAll
                    } else {
                        stemma::Resolution::RejectAll
                    })
                    .unwrap();
                let actual = Document::parse(&terminal.serialize(&options).unwrap()).unwrap();
                let differences = stemma::roundtrip_compare::compare_canon_docs(
                    &target.snapshot().canonical,
                    &actual.snapshot().canonical,
                );
                assert!(
                    differences.is_empty(),
                    "selected={index}, accept={accept}, rest={accept_rest}: {differences:?}"
                );
            }
        }
    }
}

#[test]
fn sentinel_cell_paragraph_deletion_refuses_pending_carriers() {
    use stemma::api::Document;
    for (row_change, mark_change, content) in [
        (
            "<w:trPr><w:ins w:id=\"7\" w:author=\"Prior\"/></w:trPr>",
            "",
            "<w:r><w:t>First.</w:t></w:r>",
        ),
        (
            "",
            "",
            "<w:ins w:id=\"7\" w:author=\"Prior\"><w:r><w:t>First.</w:t></w:r></w:ins>",
        ),
    ] {
        let source = pack(&document(&format!(
            "<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w=\"2400\"/></w:tblGrid><w:tr>{row_change}<w:tc><w:p>{mark_change}{content}</w:p><w:p><w:r><w:t>Last.</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"
        )));
        let original = Document::parse(&source).unwrap();
        let before = original.snapshot().canonical.clone();
        let p = common::all_paragraphs(&before)
            .into_iter()
            .find(|p| common::paragraph_text(p) == "First.")
            .unwrap();
        for mode in [
            MaterializationMode::TrackedChange,
            MaterializationMode::Direct,
        ] {
            let result = original.apply(&EditTransaction {
                steps: vec![EditStep::DeleteBlockRange {
                    story: stemma::edit::StoryRef::Body,
                    from_block_id: p.id.clone(),
                    to_block_id: p.id.clone(),
                    rationale: None,
                    expect: "First.".into(),
                    semantic_hash: None,
                }],
                summary: None,
                materialization_mode: mode,
                revision: RevisionInfo {
                    revision_id: 1,
                    identity: 0,
                    author: Some("Editor".into()),
                    date: None,
                    apply_op_id: None,
                },
            });
            let error = result
                .err()
                .expect("pending ancestors/content require explicit resolution before deletion");
            assert!(error.to_string().contains("tracked"), "{error}");
            assert_eq!(
                original.snapshot().canonical,
                before,
                "refusal cannot flatten existing history"
            );
        }
    }
}

#[test]
fn sentinel_cell_insertion_refuses_tracked_ancestor() {
    use stemma::api::Document;
    use stemma::edit::{BlockSpec, InsertPosition, ParagraphBlockSpec};
    let source = pack(&document(
        "<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w=\"2400\"/></w:tblGrid><w:tr><w:trPr><w:ins w:id=\"7\" w:author=\"Prior editor\"/></w:trPr><w:tc><w:p><w:r><w:t>Anchor.</w:t></w:r></w:p></w:tc></w:tr></w:tbl>",
    ));
    let doc = Document::parse(&source).unwrap();
    let paragraphs = common::all_paragraphs(&doc.snapshot().canonical);
    let transaction = EditTransaction {
        steps: vec![EditStep::InsertParagraphs {
            anchor_block_id: paragraphs[0].id.clone(),
            position: InsertPosition::After,
            rationale: None,
            blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                tab_stops: None,
                format_from: None,
                role: None,
                restart_numbering: false,
                list: None,
                content: ParagraphContent {
                    fragments: vec![ContentFragment::Text("New.".into())],
                },
            })],
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("Editor".into()),
            date: None,
            apply_op_id: None,
        },
    };
    let Err(error) = doc.apply(&transaction) else {
        panic!("resolve the enclosing row insertion first");
    };
    assert!(error.message.contains("has tracked status"), "{error:?}");
    assert_eq!(all_text(&doc.snapshot().canonical), "Anchor.");
}

#[test]
fn sentinel_continuation_notice_survives_serialized_projections() {
    use stemma::api::Document;
    for (kind, part, content_type, relationship, target) in [
        (
            "footnote",
            "word/footnotes.xml",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes",
            "footnotes.xml",
        ),
        (
            "endnote",
            "word/endnotes.xml",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.endnotes+xml",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/endnotes",
            "endnotes.xml",
        ),
    ] {
        let xml = document(&format!(
            "<w:p><w:r><w:t>Alpha.</w:t><w:{kind}Reference w:id=\"8\"/></w:r><w:ins w:id=\"3\" w:author=\"Editor\"><w:r><w:t> Added.</w:t></w:r></w:ins></w:p>"
        ));
        let source = pack_with_parts(
            &xml,
            &[ExtraPart {
                name: part,
                content_type,
                rel: ("rIdNotes", relationship, target),
                xml: format!(
                    "<w:{kind}s xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:{kind} w:id=\"-1\" w:type=\"separator\"><w:p><w:r><w:separator/></w:r></w:p></w:{kind}><w:{kind} w:id=\"0\" w:type=\"continuationSeparator\"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:{kind}><w:{kind} w:id=\"7\" w:type=\"continuationNotice\"><w:p><w:r><w:t>Notes continue.</w:t></w:r></w:p></w:{kind}><w:{kind} w:id=\"8\"><w:p><w:r><w:t>Existing note.</w:t></w:r></w:p></w:{kind}></w:{kind}s>"
                ),
            }],
        );
        let doc = Document::parse(&source).unwrap();
        for projection in [doc.read_accepted().unwrap(), doc.read_rejected().unwrap()] {
            let mut current = projection;
            for _ in 0..2 {
                let bytes = current
                    .serialize(&stemma::ExportOptions::default())
                    .unwrap();
                assert!(stemma::api::validate(&bytes).ok);
                let archive = stemma::docx::DocxArchive::read(&bytes).unwrap();
                let raw = archive.get(part).unwrap();
                let root = xmltree::Element::parse(raw).unwrap();
                let ids: Vec<_> = root
                    .children
                    .iter()
                    .filter_map(|child| child.as_element())
                    .map(|note| {
                        note.attributes
                            .iter()
                            .find(|(name, _)| name.local_name == "id")
                            .unwrap()
                            .1
                            .as_str()
                    })
                    .collect();
                assert_eq!(
                    ids,
                    ["-1", "0", "7", "8"],
                    "structural order survives {kind} projection and serialization"
                );
                assert!(
                    std::str::from_utf8(raw)
                        .unwrap()
                        .contains("Notes continue.")
                );
                assert!(std::str::from_utf8(raw).unwrap().contains("Existing note."));
                current = Document::parse(&bytes).unwrap().read_accepted().unwrap();
            }
        }
    }
}

// ── text extraction in canonical space ─────────────────────────────────────

#[test]
fn sentinel_paragraph_session_attrs_survive_body_cell_and_note_projections() {
    // Word's save/revision-resolution behavior can use paragraph session
    // attributes when combining runs. Untouched source paragraphs retain them;
    // paragraphs without source attributes must not acquire invented ones.
    let attrs = r#"w:rsidR="00112233" w:rsidRDefault="00445566" w:rsidRPr="00778899" w:rsidP="00AABBCC" w:rsidDel="00DDEEFF""#;
    let source = pack_with_parts(
        &document(&format!(
            r#"<w:p {attrs}><w:r><w:t>Body.</w:t><w:footnoteReference w:id="8"/></w:r><w:ins w:id="3" w:author="Editor"><w:r><w:t> Added.</w:t></w:r></w:ins></w:p><w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="2400"/></w:tblGrid><w:tr><w:tc><w:tcPr/><w:p {attrs}><w:r><w:t>Cell.</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#
        )),
        &[ExtraPart {
            name: "word/footnotes.xml",
            content_type: "application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml",
            rel: (
                "rNotes",
                "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes",
                "footnotes.xml",
            ),
            xml: format!(
                r#"<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:footnote w:id="8"><w:p {attrs}><w:r><w:t>Note.</w:t></w:r></w:p></w:footnote></w:footnotes>"#
            ),
        }],
    );
    let doc = stemma::api::Document::parse(&source).unwrap();
    for mut projection in [doc.read_accepted().unwrap(), doc.read_rejected().unwrap()] {
        for _ in 0..2 {
            let bytes = projection
                .serialize(&stemma::ExportOptions::default())
                .unwrap();
            assert!(stemma::api::validate(&bytes).ok);
            let archive = stemma::docx::DocxArchive::read(&bytes).unwrap();
            for (part, count) in [("word/document.xml", 2), ("word/footnotes.xml", 1)] {
                let xml = std::str::from_utf8(archive.get(part).unwrap()).unwrap();
                for attr in attrs.split_whitespace() {
                    assert_eq!(
                        xml.matches(attr).count(),
                        count,
                        "missing source attribute {attr} in {part}"
                    );
                }
            }
            projection = stemma::api::Document::parse(&bytes)
                .unwrap()
                .read_accepted()
                .unwrap();
        }
    }
    let plain = pack(&document(
        "<w:p><w:r><w:t>No source session.</w:t></w:r></w:p>",
    ));
    let output = stemma::api::Document::parse(&plain)
        .unwrap()
        .serialize(&stemma::ExportOptions::default())
        .unwrap();
    let archive = stemma::docx::DocxArchive::read(&output).unwrap();
    assert!(
        !std::str::from_utf8(archive.get("word/document.xml").unwrap())
            .unwrap()
            .contains("w:rsid")
    );
}

/// All paragraph text across the document (recursing into tables), space-joined.
/// Matches the `all_text` helper used by the accept/reject spec tests.
fn all_text(doc: &CanonDoc) -> String {
    common::all_paragraphs(doc)
        .iter()
        .map(|p| common::paragraph_text(p))
        .collect::<Vec<_>>()
        .join(" ")
}

fn revision() -> RevisionInfo {
    RevisionInfo {
        revision_id: 1,
        identity: 0,
        author: Some("sentinel".to_string()),
        date: Some("2026-06-01T00:00:00Z".to_string()),
        apply_op_id: None,
    }
}

fn redline_meta() -> TransactionMeta {
    TransactionMeta {
        author: "sentinel".to_string(),
        reason: Some("sentinel invariant".to_string()),
        timestamp_utc: Some("2026-06-01T00:00:00Z".to_string()),
    }
}

// ── the canonical-space relational harness (#6, #7, #10, #18) ──────────────

/// Import a before/after pair through the diff -> merge_diff path and assert the
/// relational invariants in canonical space. `expected_accept` / `expected_reject`
/// are the domain-rule texts (B and A normalized through the same extractor),
/// computed by the caller from the imported docs.
struct Pair {
    /// Human description of the structural class this witness covers.
    class: &'static str,
    a: CanonDoc,
    b: CanonDoc,
}

impl Pair {
    fn build(class: &'static str, a_bytes: &[u8], b_bytes: &[u8]) -> Self {
        let rt = SimpleRuntime::new();
        let a = std::sync::Arc::unwrap_or_clone(
            rt.import_docx(a_bytes)
                .unwrap_or_else(|e| panic!("[{class}] import A: {e:?}"))
                .canonical,
        );
        let b = std::sync::Arc::unwrap_or_clone(
            rt.import_docx(b_bytes)
                .unwrap_or_else(|e| panic!("[{class}] import B: {e:?}"))
                .canonical,
        );
        Pair { class, a, b }
    }

    /// Merge A→B into a tracked-change document via the diff/merge path.
    fn merged(&self) -> CanonDoc {
        let diff = diff_documents(&self.a, &self.b)
            .unwrap_or_else(|e| panic!("[{}] diff_documents A→B: {e}", self.class));
        merge_diff(&self.a, &self.b, &diff, &revision())
            .unwrap_or_else(|e| panic!("[{}] merge_diff: {e:?}", self.class))
            .doc
    }

    /// #6: accept_all(merge) == text(B); reject_all(merge) == text(A).
    fn assert_accept_reject(&self) {
        let class = self.class;
        let want_b = all_text(&self.b);
        let want_a = all_text(&self.a);

        let mut accepted = self.merged();
        accept_all(&mut accepted);
        assert_eq!(
            all_text(&accepted),
            want_b,
            "[{class}] #6 accept_all must equal target B text"
        );

        let mut rejected = self.merged();
        stemma::reject_all_with_styles(&mut rejected, None);
        assert_eq!(
            all_text(&rejected),
            want_a,
            "[{class}] #6 reject_all must equal base A text"
        );
    }

    /// #7: diff -> merge -> accept_all -> re-diff(merged, B) is empty.
    fn assert_fixpoint(&self) {
        let class = self.class;
        let mut merged = self.merged();
        accept_all(&mut merged);
        let fixpoint = diff_documents(&merged, &self.b)
            .unwrap_or_else(|e| panic!("[{class}] fixpoint re-diff: {e}"));
        assert!(
            fixpoint.changes.is_empty(),
            "[{class}] #7 fixpoint violated: accept_all(merge(A,B)) still differs from B by {} change(s): {}",
            fixpoint.changes.len(),
            describe_changes(&fixpoint.changes),
        );
    }

    /// #10 identity: diff(A,A) empty AND diff(B,B) empty.
    fn assert_identity(&self) {
        let class = self.class;
        for (label, doc) in [("A", &self.a), ("B", &self.b)] {
            let diff = diff_documents(doc, doc)
                .unwrap_or_else(|e| panic!("[{class}] identity diff({label},{label}): {e}"));
            assert!(
                diff.changes.is_empty(),
                "[{class}] #10 identity diff({label},{label}) produced {} change(s): {}",
                diff.changes.len(),
                describe_changes(&diff.changes),
            );
        }
    }

    fn assert_core(&self) {
        // Guard against a vacuous witness: a diff pair whose A and B already
        // collapse to the same text would make accept/reject/fixpoint pass
        // trivially. Every diff-pair witness must encode a real A→B change.
        assert_ne!(
            all_text(&self.a),
            all_text(&self.b),
            "[{}] witness is vacuous — A and B have identical text",
            self.class
        );
        self.assert_identity();
        self.assert_accept_reject();
        self.assert_fixpoint();
    }
}

fn describe_changes(changes: &[DiffChange]) -> String {
    changes
        .iter()
        .take(8)
        .map(|c| format!("{c:?}").chars().take(140).collect::<String>())
        .collect::<Vec<_>>()
        .join(" | ")
}

// ── runtime redline harness (#13 validator-clean + #10 redline-no-markup) ──

/// Run the full runtime redline pipeline (import A, import B, diff_and_redline,
/// export Redline) and return the exported bytes. This is the path that covers
/// non-body stories (headers / footers / footnotes), which the canonical-space
/// `diff_documents` harness does not.
fn redline_export(class: &str, a_bytes: &[u8], b_bytes: &[u8]) -> Vec<u8> {
    let rt = SimpleRuntime::new();
    let ia = rt
        .import_docx(a_bytes)
        .unwrap_or_else(|e| panic!("[{class}] import A: {e:?}"));
    let ib = rt
        .import_docx(b_bytes)
        .unwrap_or_else(|e| panic!("[{class}] import B: {e:?}"));
    let apply = rt
        .diff_and_redline(&ia.doc_handle, &ib.doc_handle, redline_meta())
        .unwrap_or_else(|e| panic!("[{class}] diff_and_redline: {e:?}"));
    assert!(apply.applied, "[{class}] redline must be applied");
    rt.export_docx(&ia.doc_handle, ExportMode::Redline)
        .unwrap_or_else(|e| panic!("[{class}] export_docx Redline: {e:?}"))
}

/// #13 hermetic proxy: the serialized redline output must carry no
/// Error-severity validator findings (the in-process stand-in for "opens clean
/// in Word"). A finding here means we emitted structurally invalid OOXML.
fn assert_validator_clean(class: &str, bytes: &[u8]) {
    let validation = stemma::docx_validate::validate_docx(bytes);
    let errors: Vec<String> = validation.errors().map(|f| format!("{f}")).collect();
    assert!(
        errors.is_empty(),
        "[{class}] #13 serialize(redline) is not validator-clean ({} error finding(s)):\n  {}",
        errors.len(),
        errors.join("\n  "),
    );
}

/// #10 (redline path): `redline(A,A)` must emit zero tracked-change spans.
fn assert_redline_identity(class: &str, a_bytes: &[u8]) {
    let exported = redline_export(class, a_bytes, a_bytes);
    let extract =
        extract_redline(&exported).unwrap_or_else(|e| panic!("[{class}] extract_redline: {e}"));
    let phantom = extract.body.iter().any(|p| {
        p.spans
            .iter()
            .any(|s| matches!(s, RedlineSpan::Inserted(_) | RedlineSpan::Deleted(_)))
    });
    assert!(
        !phantom,
        "[{class}] #10 redline(A,A) emitted phantom tracked-change markup"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  WITNESSES
//  Each builds a minimal (before, after) pair for one structural class and
//  states why the expected accept/reject text is correct per the domain rule.
// ═══════════════════════════════════════════════════════════════════════════

// (a) Tracked MOVE across a paragraph boundary — modelled as a content diff:
//     a sentence that exists in paragraph 1 of A is relocated to paragraph 2 in
//     B. The diff/merge path must reach a state where accept == B (sentence in
//     P2) and reject == A (sentence in P1). Fixpoint must hold.
fn move_across_paragraph() -> (Vec<u8>, Vec<u8>) {
    let a = pack(&document(
        r#"<w:p><w:r><w:t xml:space="preserve">Alpha MOVEME tail</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">Beta body</w:t></w:r></w:p>"#,
    ));
    let b = pack(&document(
        r#"<w:p><w:r><w:t xml:space="preserve">Alpha tail</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">Beta MOVEME body</w:t></w:r></w:p>"#,
    ));
    (a, b)
}

// (a) Tracked MOVE across a table-cell boundary — a token moves out of cell
//     (r1,c1) into cell (r1,c2). accept == B, reject == A.
fn move_across_table_cell() -> (Vec<u8>, Vec<u8>) {
    let cell = |t: &str| {
        format!(
            r#"<w:tc><w:tcPr><w:tcW w:w="2400" w:type="dxa"/></w:tcPr><w:p><w:r><w:t xml:space="preserve">{t}</w:t></w:r></w:p></w:tc>"#
        )
    };
    let table = |c1: &str, c2: &str| {
        format!(
            r#"<w:tbl><w:tblPr><w:tblW w:w="4800" w:type="dxa"/></w:tblPr><w:tblGrid><w:gridCol w:w="2400"/><w:gridCol w:w="2400"/></w:tblGrid><w:tr>{}{}</w:tr></w:tbl><w:p/>"#,
            cell(c1),
            cell(c2)
        )
    };
    let a = pack(&document(&table("Left TOKEN", "Right")));
    let b = pack(&document(&table("Left", "Right TOKEN")));
    (a, b)
}

// (b) Pre-existing ins/del spanning a paragraph mark. A already carries a
//     deleted paragraph mark (w:p[0]/pPr/rPr/del) plus a tracked insertion in
//     the following paragraph. Domain rule (§17.13.5.15): accepting the deleted
//     para-mark MERGES the two paragraphs; accepting the w:ins keeps inserted
//     text. We assert accept/reject text over the IMPORT-preserved canonical of
//     A itself (this witness is a static tracked-change doc, not a diff pair).
fn preexisting_change_across_paragraph_mark() -> Vec<u8> {
    pack(&document(
        r#"<w:p><w:pPr><w:rPr><w:del w:id="1" w:author="x" w:date="2026-01-01T00:00:00Z"/></w:rPr></w:pPr><w:r><w:t xml:space="preserve">First</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">Second </w:t></w:r><w:ins w:id="2" w:author="x" w:date="2026-01-01T00:00:00Z"><w:r><w:t xml:space="preserve">added</w:t></w:r></w:ins></w:p>"#,
    ))
}

// (b) Pre-existing ins/del at a table-cell boundary: a tracked insertion inside
//     a cell and a tracked deletion inside the neighbouring cell.
fn preexisting_change_across_table_cell() -> Vec<u8> {
    pack(&document(
        r#"<w:tbl><w:tblPr><w:tblW w:w="4800" w:type="dxa"/></w:tblPr><w:tblGrid><w:gridCol w:w="2400"/><w:gridCol w:w="2400"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="2400" w:type="dxa"/></w:tcPr><w:p><w:r><w:t xml:space="preserve">Keep </w:t></w:r><w:ins w:id="1" w:author="x" w:date="2026-01-01T00:00:00Z"><w:r><w:t xml:space="preserve">INS</w:t></w:r></w:ins></w:p></w:tc><w:tc><w:tcPr><w:tcW w:w="2400" w:type="dxa"/></w:tcPr><w:p><w:del w:id="2" w:author="x" w:date="2026-01-01T00:00:00Z"><w:r><w:delText xml:space="preserve">DEL</w:delText></w:r></w:del><w:r><w:t xml:space="preserve">stay</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/>"#,
    ))
}

// (c) A hyperlink inside a CHANGED run: A has body text "see link here", B
//     changes the surrounding words but the run carries a w:hyperlink whose
//     visible text changes. The hyperlink opaque must survive accept; accept==B.
fn hyperlink_in_changed_run() -> (Vec<u8>, Vec<u8>) {
    let body = |lead: &str, link: &str| {
        format!(
            r#"<w:p><w:r><w:t xml:space="preserve">{lead} </w:t></w:r><w:hyperlink r:id="rIdLink"><w:r><w:t xml:space="preserve">{link}</w:t></w:r></w:hyperlink><w:r><w:t xml:space="preserve"> end</w:t></w:r></w:p>"#
        )
    };
    // The hyperlink references an EXTERNAL target relationship; without it the
    // package is invalid (a dangling r:id), so the witness ships the rel.
    let link_rel = ExternalRel {
        id: "rIdLink",
        rel_type: "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink",
        target: "https://example.com/anchor",
    };
    let a = pack_full(
        &document(&body("Please review the", "old anchor")),
        &[],
        &[link_rel],
    );
    let b = pack_full(
        &document(&body("Kindly examine the", "new anchor")),
        &[],
        &[link_rel],
    );
    (a, b)
}

// (c) A field (w:fldSimple) inside a CHANGED run: surrounding text changes while
//     the PAGE field stays. The field is opaque and must survive accept.
fn field_in_changed_run() -> (Vec<u8>, Vec<u8>) {
    let body = |lead: &str| {
        format!(
            r#"<w:p><w:r><w:t xml:space="preserve">{lead} page </w:t></w:r><w:fldSimple w:instr=" PAGE "><w:r><w:t>1</w:t></w:r></w:fldSimple></w:p>"#
        )
    };
    let a = pack(&document(&body("See")));
    let b = pack(&document(&body("Refer to")));
    (a, b)
}

// (c) A block-level SDT (content control) next to a CHANGED paragraph. A
//     body-level w:sdt imports as a whole OpaqueBlock(Sdt) (import.rs §1237 —
//     the inner text is referenced by body index, never parsed into the
//     canonical model), so the structural class this witnesses is "an opaque
//     content control survives the diff/redline path while an adjacent run
//     changes": the SDT opaque inventory must NOT shrink across accept, and the
//     body text change must satisfy accept==B / reject==A.
fn sdt_adjacent_to_changed_run() -> (Vec<u8>, Vec<u8>) {
    let sdt = r#"<w:sdt><w:sdtPr><w:alias w:val="field"/><w:id w:val="101"/></w:sdtPr><w:sdtContent><w:p><w:r><w:t xml:space="preserve">control value</w:t></w:r></w:p></w:sdtContent></w:sdt>"#;
    let body =
        |t: &str| format!(r#"<w:p><w:r><w:t xml:space="preserve">{t}</w:t></w:r></w:p>{sdt}"#);
    let a = pack(&document(&body("draft text")));
    let b = pack(&document(&body("final text")));
    (a, b)
}

/// Count the body's block content controls: open ones (a wrapper over
/// ordinary blocks) and sealed ones (one preserved opaque block).
fn count_block_controls(doc: &CanonDoc) -> usize {
    doc.blocks
        .iter()
        .filter(|tb| {
            tb.block_sdt_wrap.is_some()
                || matches!(
                    &tb.block,
                    BlockNode::OpaqueBlock(o) if matches!(o.kind, stemma::OpaqueKind::Sdt)
                )
        })
        .count()
}

// (d) Footnote-body edit: the footnote story text changes between A and B. The
//     redline path must carry tracked changes in word/footnotes.xml and stay
//     validator-clean.
fn footnote_body_edit() -> (Vec<u8>, Vec<u8>) {
    let footnotes = |note: &str| ExtraPart {
        name: "word/footnotes.xml",
        content_type: "application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml",
        rel: (
            "rIdFn",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes",
            "footnotes.xml",
        ),
        xml: format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote><w:footnote w:id="1"><w:p><w:r><w:rPr><w:rStyle w:val="FootnoteReference"/></w:rPr><w:footnoteRef/></w:r><w:r><w:t xml:space="preserve">{note}</w:t></w:r></w:p></w:footnote></w:footnotes>"#
        ),
    };
    let doc = r#"<w:p><w:r><w:t xml:space="preserve">Body text</w:t></w:r><w:r><w:rPr><w:rStyle w:val="FootnoteReference"/></w:rPr><w:footnoteReference w:id="1"/></w:r></w:p>"#;
    let a = pack_with_parts(&document(doc), &[footnotes("original note")]);
    let b = pack_with_parts(&document(doc), &[footnotes("revised note")]);
    (a, b)
}

// (e) Header/footer redline: the header story text changes between A and B. The
//     redline path must carry tracked changes in word/header1.xml.
fn header_redline() -> (Vec<u8>, Vec<u8>) {
    let header = |t: &str| ExtraPart {
        name: "word/header1.xml",
        content_type: "application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml",
        rel: (
            "rIdHdr",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header",
            "header1.xml",
        ),
        xml: format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:r><w:t xml:space="preserve">{t}</w:t></w:r></w:p></w:hdr>"#
        ),
    };
    // Reference the header from the section so it is a live story.
    let body = r#"<w:p><w:r><w:t xml:space="preserve">Body</w:t></w:r></w:p><w:sectPr><w:headerReference w:type="default" r:id="rIdHdr"/></w:sectPr>"#;
    let doc = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>{body}</w:body></w:document>"#
    );
    let a = pack_with_parts(&doc, &[header("Original Header")]);
    let b = pack_with_parts(&doc, &[header("Modified Header")]);
    (a, b)
}

// (f) Nested-table cell change: an inner table sits inside an outer cell; the
//     inner cell's text changes A→B. accept==B, reject==A, fixpoint holds.
fn nested_table_cell_change() -> (Vec<u8>, Vec<u8>) {
    let inner = |t: &str| {
        format!(
            r#"<w:tbl><w:tblPr><w:tblW w:w="2000" w:type="dxa"/></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t xml:space="preserve">{t}</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/>"#
        )
    };
    let outer = |inner_xml: &str| {
        format!(
            r#"<w:tbl><w:tblPr><w:tblW w:w="4800" w:type="dxa"/></w:tblPr><w:tblGrid><w:gridCol w:w="4800"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="4800" w:type="dxa"/></w:tcPr>{inner_xml}</w:tc></w:tr></w:tbl><w:p/>"#
        )
    };
    let a = pack(&document(&outer(&inner("inner OLD"))));
    let b = pack(&document(&outer(&inner("inner NEW"))));
    (a, b)
}

// (g) Heading-style + numbering + bold runs (the Tier-2 formatting bundle).
//     A→B edits the body text of a Heading1, numbered (numId/ilvl), bold-run
//     paragraph. The accept projection must preserve style_id="Heading1",
//     heading_level, numbering(num_id,ilvl), and the Bold mark — these are
//     exactly the Tier-2 attributes that #18 only covers via Word today.
fn formatting_tier_bundle() -> (Vec<u8>, Vec<u8>) {
    let para = |t: &str| {
        format!(
            r#"<w:p><w:pPr><w:pStyle w:val="Heading1"/><w:numPr><w:ilvl w:val="0"/><w:numId w:val="3"/></w:numPr></w:pPr><w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">{t}</w:t></w:r></w:p>"#
        )
    };
    // numId=3 must be defined by word/numbering.xml or the importer drops the
    // NumberingInfo (numbering is Some only when definitions resolve, per
    // import.rs synthesize). Heading1 is a built-in style; heading_level is
    // derived from the "HeadingN" style id, so no styles.xml part is required.
    let numbering = ExtraPart {
        name: "word/numbering.xml",
        content_type:
            "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml",
        rel: (
            "rIdNum",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering",
            "numbering.xml",
        ),
        xml: r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum><w:num w:numId="3"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#.to_string(),
    };
    let numbering_b = numbering.clone();
    let a = pack_with_parts(
        &document(&para("Heading old text")),
        std::slice::from_ref(&numbering),
    );
    let b = pack_with_parts(
        &document(&para("Heading new text")),
        std::slice::from_ref(&numbering_b),
    );
    (a, b)
}

// ── tests ──────────────────────────────────────────────────────────────────

#[test]
fn sentinel_empty_cell_typing_preserves_mark_formatting_and_rejects_cleanly() {
    fn property_shape(element: &xmltree::Element) -> serde_json::Value {
        // Compare expanded XML names and all values, not incidental in-scope
        // namespace declarations inherited at different tree locations.
        let mut attrs: Vec<_> = element
            .attributes
            .iter()
            .map(|(name, value)| {
                (
                    name.namespace.as_deref(),
                    name.local_name.as_str(),
                    value.as_str(),
                )
            })
            .collect();
        attrs.sort();
        let children: Vec<_> = element
            .children
            .iter()
            .map(|node| match node {
                xmltree::XMLNode::Element(child) => property_shape(child),
                other => serde_json::json!({"non_element": format!("{other:?}")}),
            })
            .collect();
        serde_json::json!([element.namespace, element.name, attrs, children])
    }
    // §17.3.1.29: the empty paragraph's mark owns its direct formatting.
    // Typing copies it to the new run, not to neighboring text or a style.
    let source = pack(&document(concat!(
        r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="2400"/></w:tblGrid><w:tr><w:tc><w:p><w:pPr><w:rPr>"#,
        r#"<w:rFonts w:ascii="Arial" w:hAnsi="Courier New" w:cs="Times New Roman"/><w:b w:val="0"/><w:bCs/><w:sz w:val="26"/><w:szCs w:val="30"/>"#,
        r#"</w:rPr></w:pPr></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:t>Untouched</w:t></w:r></w:p>"#,
    )));
    let original = stemma::api::Document::parse(&source).unwrap();
    let BlockNode::Table(table) = &original.snapshot().canonical.blocks[0].block else {
        panic!()
    };
    let BlockNode::Paragraph(paragraph) = &table.rows[0].cells[0].blocks[0] else {
        panic!()
    };
    let transaction = EditTransaction {
        steps: vec![EditStep::ReplaceParagraphText {
            block_id: paragraph.id.clone(),
            expect: String::new(),
            content: ParagraphContent {
                fragments: vec![ContentFragment::Text("Typed".into())],
            },
            rationale: None,
            replacement_role: None,
            semantic_hash: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: revision(),
    };
    let edited = original.apply(&transaction).unwrap();
    let pending_bytes = edited.serialize(&stemma::ExportOptions::default()).unwrap();
    assert_validator_clean("empty-cell-typing-pending", &pending_bytes);
    let pending = stemma::api::Document::parse(&pending_bytes).unwrap();
    assert_eq!(pending.revisions().len(), 1);
    for (resolution, expected_text) in [
        (stemma::Resolution::AcceptAll, "Typed"),
        (stemma::Resolution::RejectAll, ""),
    ] {
        let terminal = pending.project(resolution).unwrap();
        let bytes = terminal
            .serialize(&stemma::ExportOptions::default())
            .unwrap();
        assert_validator_clean("empty-cell-typing-terminal", &bytes);
        let reopened = stemma::api::Document::parse(&bytes).unwrap();
        assert!(reopened.revisions().is_empty());
        let xml = document_xml(&bytes);
        let root = xmltree::Element::parse(xml.as_bytes()).unwrap();
        let body = root.get_child("body").unwrap();
        let cell = body
            .get_child("tbl")
            .unwrap()
            .get_child("tr")
            .unwrap()
            .get_child("tc")
            .unwrap();
        let p = cell.get_child("p").unwrap();
        let mark = p.get_child("pPr").unwrap().get_child("rPr").unwrap();
        assert_eq!(
            mark.get_child("sz")
                .unwrap()
                .attributes
                .iter()
                .find(|(name, _)| name.local_name == "val")
                .map(|(_, value)| value.as_str()),
            Some("26")
        );
        if expected_text.is_empty() {
            assert!(p.get_child("r").is_none());
        } else {
            let run = p.get_child("r").unwrap();
            assert_eq!(
                run.get_child("t").unwrap().get_text().as_deref(),
                Some(expected_text)
            );
            let rpr = run.get_child("rPr").unwrap();
            assert_eq!(
                property_shape(rpr),
                property_shape(mark),
                "all direct properties, including exact font slots and off forms, travel together"
            );
        }
        assert_eq!(
            body.get_child("p")
                .unwrap()
                .get_child("r")
                .unwrap()
                .get_child("t")
                .unwrap()
                .get_text()
                .as_deref(),
            Some("Untouched")
        );
    }
}

#[test]
fn sentinel_untouched_equal_format_run_boundaries_survive_text_edit() {
    let source = pack(&document(
        r#"<w:p><w:pPr><w:jc w:val="both"/></w:pPr><w:r><w:t xml:space="preserve">Alpha </w:t></w:r><w:r><w:t xml:space="preserve">Beta </w:t></w:r><w:r><w:t>Gamma</w:t></w:r></w:p>"#,
    ));
    let runtime = SimpleRuntime::new();
    let canon = std::sync::Arc::unwrap_or_clone(
        runtime
            .import_docx(&source)
            .expect("import run-boundary sentinel")
            .canonical,
    );
    let block_id = canon
        .blocks
        .iter()
        .find_map(|tracked| match &tracked.block {
            BlockNode::Paragraph(paragraph) => Some(paragraph.id.clone()),
            _ => None,
        })
        .expect("sentinel must contain a paragraph");
    let transaction = EditTransaction {
        steps: vec![EditStep::ReplaceParagraphText {
            block_id: block_id.clone(),
            expect: "Alpha Beta Gamma".to_string(),
            content: ParagraphContent {
                fragments: vec![ContentFragment::Text("Alpha Beta Delta".to_string())],
            },
            rationale: None,
            replacement_role: None,
            semantic_hash: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: revision(),
    };
    let (edited, _) = apply_transaction(&canon, &transaction).expect("apply sentinel edit");
    let paragraph = edited
        .blocks
        .iter()
        .find_map(|tracked| match &tracked.block {
            BlockNode::Paragraph(paragraph) if paragraph.id == block_id => Some(paragraph),
            _ => None,
        })
        .expect("edited sentinel paragraph");
    let untouched_runs: Vec<&str> = paragraph
        .segments
        .iter()
        .filter(|segment| matches!(segment.status, stemma::TrackingStatus::Normal))
        .flat_map(|segment| segment.inlines.iter())
        .filter_map(|inline| match inline {
            InlineNode::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        untouched_runs,
        vec!["Alpha ", "Beta "],
        "an edit to Gamma must retain both untouched source runs"
    );

    let mut accepted = edited.clone();
    accept_all(&mut accepted);
    assert_eq!(all_text(&accepted), "Alpha Beta Delta");
    let mut rejected = edited;
    // This minimal witness has no styles part, so `None` is exact here.
    stemma::reject_all_with_styles(&mut rejected, None);
    assert_eq!(all_text(&rejected), "Alpha Beta Gamma");
}

#[test]
fn sentinel_split_literal_prefix_runs_survive_rebuild() {
    let source = pack(&document(
        r#"<w:p><w:pPr><w:jc w:val="both"/></w:pPr><w:r><w:t>1</w:t></w:r><w:r><w:t>.</w:t></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:t>Body text here.</w:t></w:r></w:p>"#,
    ));
    let rebuilt = stemma::api::Document::parse(&source)
        .expect("parse split-prefix sentinel")
        .serialize(&stemma::ExportOptions::default())
        .expect("serialize split-prefix sentinel");
    let xml = extract_part(&rebuilt, "word/document.xml");
    let paragraph = &xml
        [xml.find("<w:p>").expect("paragraph start")..xml.find("</w:p>").expect("paragraph end")];
    let run_count = paragraph.matches("<w:r>").count() + paragraph.matches("<w:r ").count();
    assert_eq!(
        run_count, 4,
        "identity rebuild must retain both label runs, the separator run, and the body run: {paragraph}"
    );
    assert!(
        paragraph.contains(">1</w:t>") && paragraph.contains(">.</w:t>"),
        "the split label must not be consolidated into one run: {paragraph}"
    );
}

#[test]
fn sentinel_move_across_paragraph_boundary() {
    let (a, b) = move_across_paragraph();
    Pair::build("move-across-paragraph", &a, &b).assert_core();
    assert_validator_clean(
        "move-across-paragraph",
        &redline_export("move-across-paragraph", &a, &b),
    );
}

#[test]
fn sentinel_move_across_table_cell_boundary() {
    let (a, b) = move_across_table_cell();
    Pair::build("move-across-table-cell", &a, &b).assert_core();
    assert_validator_clean(
        "move-across-table-cell",
        &redline_export("move-across-table-cell", &a, &b),
    );
}

#[test]
fn sentinel_preexisting_change_across_paragraph_mark() {
    // Static tracked-change doc: import-preserved canonical, then accept/reject.
    // Domain rule (§17.13.5.15): accepting the deleted para mark merges P1 into
    // P2 and keeps the w:ins text → "First Second added". Rejecting restores the
    // two separate paragraphs and drops the insertion → "First Second ".
    let bytes = preexisting_change_across_paragraph_mark();
    let rt = SimpleRuntime::new();
    let canon = std::sync::Arc::unwrap_or_clone(rt.import_docx(&bytes).expect("import").canonical);

    let mut accepted = canon.clone();
    accept_all(&mut accepted);
    let accepted_text = all_text(&accepted);
    assert!(
        accepted_text.contains("First")
            && accepted_text.contains("Second")
            && accepted_text.contains("added"),
        "#6 accept of pre-existing para-mark del + ins must keep 'First', 'Second', 'added'; got {accepted_text:?}"
    );
    // Accepting the deleted para mark merges into ONE top-level paragraph.
    let accepted_top = accepted
        .blocks
        .iter()
        .filter(|tb| matches!(&tb.block, BlockNode::Paragraph(_)))
        .count();
    assert_eq!(
        accepted_top, 1,
        "#6 accepting a deleted paragraph mark must merge two paragraphs into one"
    );

    let mut rejected = canon.clone();
    stemma::reject_all_with_styles(&mut rejected, None);
    let rejected_text = all_text(&rejected);
    assert!(
        rejected_text.contains("First")
            && rejected_text.contains("Second")
            && !rejected_text.contains("added"),
        "#6 reject must keep base text and drop the insertion 'added'; got {rejected_text:?}"
    );
    let rejected_top = rejected
        .blocks
        .iter()
        .filter(|tb| matches!(&tb.block, BlockNode::Paragraph(_)))
        .count();
    assert_eq!(
        rejected_top, 2,
        "#6 rejecting a deleted paragraph mark must keep the two paragraphs separate"
    );
}

#[test]
fn sentinel_preexisting_change_across_table_cell() {
    let bytes = preexisting_change_across_table_cell();
    let rt = SimpleRuntime::new();
    let canon = std::sync::Arc::unwrap_or_clone(rt.import_docx(&bytes).expect("import").canonical);

    // accept: insertion stays, deletion removed → "Keep INS" + "stay"
    let mut accepted = canon.clone();
    accept_all(&mut accepted);
    let acc = all_text(&accepted);
    assert!(
        acc.contains("Keep") && acc.contains("INS") && acc.contains("stay") && !acc.contains("DEL"),
        "#6 accept of cell ins/del: keep INS+stay, drop DEL; got {acc:?}"
    );

    // reject: insertion removed, deletion restored → "Keep" + "DELstay"
    let mut rejected = canon.clone();
    stemma::reject_all_with_styles(&mut rejected, None);
    let rej = all_text(&rejected);
    assert!(
        rej.contains("Keep") && rej.contains("DEL") && rej.contains("stay") && !rej.contains("INS"),
        "#6 reject of cell ins/del: keep DEL+stay, drop INS; got {rej:?}"
    );
}

#[test]
fn sentinel_hyperlink_in_changed_run() {
    let (a, b) = hyperlink_in_changed_run();
    let pair = Pair::build("hyperlink-in-changed-run", &a, &b);
    pair.assert_core();

    // The hyperlink imports as an OpaqueInline(Hyperlink). It must survive the
    // accept projection (opaque preservation) even though the surrounding run
    // text changes A→B.
    let is_link = |k: &stemma::OpaqueKind| matches!(k, stemma::OpaqueKind::Hyperlink(_));
    assert_eq!(
        count_opaque_inline(&pair.a, is_link),
        1,
        "fixture A should import exactly one hyperlink opaque inline"
    );
    let mut accepted = pair.merged();
    accept_all(&mut accepted);
    assert_eq!(
        count_opaque_inline(&accepted, is_link),
        1,
        "#18/I1: hyperlink opaque must survive the accept projection"
    );

    assert_redline_identity("hyperlink-in-changed-run", &a);
    assert_validator_clean(
        "hyperlink-in-changed-run",
        &redline_export("hyperlink-in-changed-run", &a, &b),
    );
}

#[test]
fn sentinel_field_in_changed_run() {
    let (a, b) = field_in_changed_run();
    let pair = Pair::build("field-in-changed-run", &a, &b);
    pair.assert_core();

    // The PAGE field imports as an OpaqueInline(Field). It must survive accept
    // even though the lead run text changes A→B.
    let is_field = |k: &stemma::OpaqueKind| matches!(k, stemma::OpaqueKind::Field(_));
    assert_eq!(
        count_opaque_inline(&pair.a, is_field),
        1,
        "fixture A should import exactly one field opaque inline"
    );
    let mut accepted = pair.merged();
    accept_all(&mut accepted);
    assert_eq!(
        count_opaque_inline(&accepted, is_field),
        1,
        "#18/I1: field opaque must survive the accept projection"
    );

    assert_validator_clean(
        "field-in-changed-run",
        &redline_export("field-in-changed-run", &a, &b),
    );
}

#[test]
fn sentinel_sdt_adjacent_to_changed_run() {
    let (a, b) = sdt_adjacent_to_changed_run();
    let pair = Pair::build("sdt-adjacent-to-changed-run", &a, &b);
    pair.assert_core();

    // Content-control preservation: the control must still be present after
    // the accept projection — a diff/merge must never silently drop a content
    // control while editing an adjacent run.
    assert_eq!(
        count_block_controls(&pair.a),
        1,
        "fixture A should import exactly one block content control"
    );
    let mut accepted = pair.merged();
    accept_all(&mut accepted);
    assert_eq!(
        count_block_controls(&accepted),
        1,
        "#18/I1 opaque preservation: SDT content control must survive the accept projection"
    );

    assert_validator_clean(
        "sdt-adjacent-to-changed-run",
        &redline_export("sdt-adjacent-to-changed-run", &a, &b),
    );
}

#[test]
fn sentinel_footnote_body_edit() {
    let (a, b) = footnote_body_edit();
    // Footnote story lives outside the body, so use the runtime redline path.
    let exported = redline_export("footnote-body-edit", &a, &b);
    assert_validator_clean("footnote-body-edit", &exported);
    let extract = extract_redline(&exported).expect("extract_redline");
    let (deleted, inserted) = extract
        .tracked_changes_in("word/footnotes.xml")
        .expect("footnotes part should carry tracked changes");
    assert!(
        deleted.join(" ").contains("original"),
        "#6 footnote edit: deleted text should contain 'original', got {deleted:?}"
    );
    assert!(
        inserted.join(" ").contains("revised"),
        "#6 footnote edit: inserted text should contain 'revised', got {inserted:?}"
    );
    // #10: redline(A,A) over the footnote story emits no markup.
    assert_redline_identity("footnote-body-edit", &a);
}

#[test]
fn sentinel_header_redline() {
    let (a, b) = header_redline();
    let exported = redline_export("header-redline", &a, &b);
    assert_validator_clean("header-redline", &exported);
    let extract = extract_redline(&exported).expect("extract_redline");
    let (deleted, inserted) = extract
        .tracked_changes_in("word/header1.xml")
        .expect("header part should carry tracked changes");
    assert!(
        deleted.join(" ").contains("Original"),
        "#6 header redline: deleted should contain 'Original', got {deleted:?}"
    );
    assert!(
        inserted.join(" ").contains("Modified"),
        "#6 header redline: inserted should contain 'Modified', got {inserted:?}"
    );
    assert_redline_identity("header-redline", &a);
}

#[test]
fn sentinel_nested_table_cell_change() {
    let (a, b) = nested_table_cell_change();
    let pair = Pair::build("nested-table-cell-change", &a, &b);
    pair.assert_core();
    assert_validator_clean(
        "nested-table-cell-change",
        &redline_export("nested-table-cell-change", &a, &b),
    );
}

#[test]
fn sentinel_formatting_tier_bundle() {
    let (a, b) = formatting_tier_bundle();
    let pair = Pair::build("formatting-tier-bundle", &a, &b);
    // Core relational invariants first.
    pair.assert_core();

    // #18 Tier-2: the accept projection must preserve the formatting bundle.
    let mut accepted = pair.merged();
    accept_all(&mut accepted);
    let para = first_paragraph(&accepted);

    assert_eq!(
        para.style_id.as_deref(),
        Some("Heading1"),
        "#18 Tier-2: style_id must survive accept on the diff path"
    );
    assert!(
        para.heading_level.is_some(),
        "#18 Tier-2: heading_level must survive accept (style is Heading1)"
    );
    let numbering = para
        .numbering
        .as_ref()
        .expect("#18 Tier-2: numbering must survive accept");
    assert_eq!(
        (numbering.num_id, numbering.ilvl),
        (3, 0),
        "#18 Tier-2: num_id/ilvl must survive accept on the diff path"
    );
    assert!(
        paragraph_has_bold(para),
        "#18 Tier-2: Bold mark must survive accept on the diff path"
    );

    assert_validator_clean(
        "formatting-tier-bundle",
        &redline_export("formatting-tier-bundle", &a, &b),
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tracked-content serializer content-model witnesses
// ═══════════════════════════════════════════════════════════════════════════

/// The `word/document.xml` string of a packaged DOCX.
fn document_xml(bytes: &[u8]) -> String {
    common::read_zip_entry(bytes, "word/document.xml")
        .expect("exported DOCX must contain word/document.xml")
}

/// Canonical (normalized) text of a DOCX, via a fresh import.
fn imported_text(bytes: &[u8]) -> String {
    let rt = SimpleRuntime::new();
    let canon =
        std::sync::Arc::unwrap_or_clone(rt.import_docx(bytes).expect("import for text").canonical);
    all_text(&canon)
}

/// #14/#12 through serialize→reparse: re-import the exported redline bytes and
/// assert `accept_all` yields the target text and `reject_all` the base text.
fn assert_reparse_accept_reject(class: &str, redline: &[u8], want_accept: &str, want_reject: &str) {
    let rt = SimpleRuntime::new();
    let canon = std::sync::Arc::unwrap_or_clone(
        rt.import_docx(redline)
            .unwrap_or_else(|e| panic!("[{class}] re-import redline: {e:?}"))
            .canonical,
    );
    let mut accepted = canon.clone();
    accept_all(&mut accepted);
    assert_eq!(
        all_text(&accepted),
        want_accept,
        "[{class}] accept_all through serialize→reparse must equal target text"
    );
    let mut rejected = canon;
    stemma::reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        all_text(&rejected),
        want_reject,
        "[{class}] reject_all through serialize→reparse must equal base text"
    );
}

/// Extract each `<w:del …>…</w:del>` region. `<w:del ` (with the trailing space
/// before the `w:id` attribute) never matches `<w:delText>`/`<w:delInstrText>`.
/// The witnesses below produce non-nested deletions, so an open→next-close scan
/// is exact.
fn del_spans(xml: &str) -> Vec<String> {
    let mut spans = Vec::new();
    let mut rest = xml;
    while let Some(open) = rest.find("<w:del ") {
        let after = &rest[open..];
        let Some(close_rel) = after.find("</w:del>") else {
            break;
        };
        let end = close_rel + "</w:del>".len();
        spans.push(after[..end].to_string());
        rest = &after[end..];
    }
    spans
}

// Run-level widget (w:pgNum) emitted BARE at paragraph level. A paragraph
//   carries hidden page-number placeholder runs
//   (`<w:r><w:rPr><w:vanish/><w:sz w:val="24"/></w:rPr><w:pgNum/></w:r>`).
//   `w:pgNum` is EG_RunInnerContent (§17.3.3.22, CT_Empty) and is only legal
//   inside `w:r`; when the serializer dropped the run wrapper it emitted
//   `<w:pgNum/>` as a direct child of `<w:p>` and Word refused the file. The
//   bare-emission path is the whole-document rebuild, forced here by a tracked
//   edit to a DIFFERENT paragraph (the plain unedited roundtrip is verbatim and
//   would not reproduce it).
fn pgnum_widget_whole_doc_rebuild() -> (Vec<u8>, Vec<u8>) {
    let widget = r#"<w:r><w:rPr><w:vanish/><w:sz w:val="24"/></w:rPr><w:pgNum/></w:r>"#;
    let pgnum_para = format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Page </w:t></w:r>{widget}<w:r><w:t xml:space="preserve"> of </w:t></w:r>{widget}</w:p>"#
    );
    let other = |t: &str| format!(r#"<w:p><w:r><w:t xml:space="preserve">{t}</w:t></w:r></w:p>"#);
    let a = pack(&document(&format!("{pgnum_para}{}", other("draft body"))));
    let b = pack(&document(&format!("{pgnum_para}{}", other("final body"))));
    (a, b)
}

#[test]
fn sentinel_pgnum_widget_survives_whole_doc_rebuild() {
    let class = "pgnum-widget-rebuild";
    let (a, b) = pgnum_widget_whole_doc_rebuild();

    let exported = redline_export(class, &a, &b);
    let xml = document_xml(&exported);

    // (a) every <w:pgNum> is inside a <w:r> (rPr immediately precedes it, </w:r>
    //     immediately follows) — none emitted bare at paragraph level.
    let pgnum_total = xml.matches("<w:pgNum").count();
    assert_eq!(
        pgnum_total, 2,
        "[{class}] both page-number widgets must survive the rebuild"
    );
    // (a) every <w:pgNum> is preceded by a </w:rPr> — i.e. it sits inside a
    //     <w:r> after that run's rPr, never bare under <w:p> (a bare widget
    //     would follow </w:r> or <w:p>, not </w:rPr>). Equality with the total
    //     pgNum count proves NONE is emitted bare.
    assert_eq!(
        xml.matches("</w:rPr><w:pgNum").count(),
        pgnum_total,
        "[{class}] (a) every <w:pgNum> must sit inside a <w:r> after its <w:rPr>, not bare under <w:p>"
    );
    // (b) each wrapping run's rPr keeps w:vanish — the widgets stay hidden.
    assert!(
        xml.matches("<w:rPr><w:vanish").count() >= pgnum_total,
        "[{class}] (b) each wrapping run's rPr must preserve w:vanish (hidden)"
    );

    // (c) validator-clean (opens clean in Word — hermetic proxy).
    assert_validator_clean(class, &exported);

    // (d) accept/reject text identity through serialize→reparse.
    assert_reparse_accept_reject(class, &exported, &imported_text(&b), &imported_text(&a));
}

// Tracked DELETE of an inline content control (w:sdt). The SDT is preserved
//   as opaque raw XML; when the deletion wraps it in `w:del`, the SDT's own
//   descendant runs must switch to the deleted-text content model — `w:t` →
//   `w:delText` (§17.4.20, the I-TC-001 rule). Left as `w:t`, Word opens the file
//   only after repair and a programmatic accept-all crashes Word.
fn tracked_delete_of_inline_sdt() -> (Vec<u8>, Vec<u8>) {
    let sdt = r#"<w:sdt><w:sdtPr><w:alias w:val="Clause"/><w:tag w:val="ctl"/><w:id w:val="55"/></w:sdtPr><w:sdtContent><w:r><w:t xml:space="preserve">secret clause</w:t></w:r></w:sdtContent></w:sdt>"#;
    // A has the inline content control; B removes it (a tracked deletion).
    let a = pack(&document(&format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Keep </w:t></w:r>{sdt}<w:r><w:t xml:space="preserve"> tail</w:t></w:r></w:p>"#
    )));
    let b = pack(&document(
        r#"<w:p><w:r><w:t xml:space="preserve">Keep </w:t></w:r><w:r><w:t xml:space="preserve"> tail</w:t></w:r></w:p>"#,
    ));
    (a, b)
}

fn tracked_delete_of_locked_inline_sdt() -> (Vec<u8>, Vec<u8>) {
    let sdt = r#"<w:sdt><w:sdtPr><w:alias w:val="Clause"/><w:tag w:val="ctl"/><w:id w:val="55"/><w:lock w:val="sdtContentLocked"/></w:sdtPr><w:sdtContent><w:r><w:t xml:space="preserve">secret clause</w:t></w:r></w:sdtContent></w:sdt>"#;
    let a = pack(&document(&format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Keep </w:t></w:r>{sdt}<w:r><w:t xml:space="preserve"> tail</w:t></w:r></w:p>"#
    )));
    let b = pack(&document(
        r#"<w:p><w:r><w:t xml:space="preserve">Keep </w:t></w:r><w:r><w:t xml:space="preserve"> tail</w:t></w:r></w:p>"#,
    ));
    (a, b)
}

// A whole inline content-control replacement requires an inserted SDT envelope.
// `w:ins > w:sdt` is schema-valid, but native Word Reject All can leave the
// inserted control's content and revision pending. The exact comparison must
// refuse instead of silently degrading the target to inserted plain text.
fn tracked_replace_of_inline_sdt() -> (Vec<u8>, Vec<u8>) {
    let sdt = |txt: &str| {
        format!(
            r#"<w:sdt><w:sdtPr><w:alias w:val="Clause"/><w:tag w:val="ctl"/><w:id w:val="55"/></w:sdtPr><w:sdtContent><w:r><w:t xml:space="preserve">{txt}</w:t></w:r></w:sdtContent></w:sdt>"#
        )
    };
    let body = |inner: &str| {
        format!(
            r#"<w:p><w:r><w:t xml:space="preserve">Keep </w:t></w:r>{inner}<w:r><w:t xml:space="preserve"> tail</w:t></w:r></w:p>"#
        )
    };
    let a = pack(&document(&body(&sdt("secret clause"))));
    let b = pack(&document(&body(&sdt("public clause"))));
    (a, b)
}

/// Every `w:sdtPr>w:id` value in the document, in order. `<w:id w:val="` is the
/// CT_DecimalNumber element form used by the SDT id (§17.5.2.18) — distinct from
/// the `w:id="…"` attribute the tracked-change containers carry, so this never
/// matches a `w:del`/`w:ins` id.
fn sdt_id_vals(xml: &str) -> Vec<String> {
    let needle = "<w:id w:val=\"";
    let mut ids = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find(needle) {
        let after = &rest[i + needle.len()..];
        let end = after.find('"').expect("w:val attribute must close");
        ids.push(after[..end].to_string());
        rest = &after[end..];
    }
    ids
}

#[test]
fn sentinel_tracked_replace_of_inline_sdt_refuses_unqualified_insertion() {
    let class = "tracked-replace-inline-sdt";
    let (a, b) = tracked_replace_of_inline_sdt();
    let rt = SimpleRuntime::new();
    let ia = rt
        .import_docx(&a)
        .unwrap_or_else(|error| panic!("[{class}] import A: {error:?}"));
    let ib = rt
        .import_docx(&b)
        .unwrap_or_else(|error| panic!("[{class}] import B: {error:?}"));
    let error = rt
        .diff_and_redline(&ia.doc_handle, &ib.doc_handle, redline_meta())
        .expect_err("a whole inserted SDT envelope must refuse before output");

    assert_eq!(error.code, stemma::ErrorCode::UnsupportedEdit);
    assert!(
        error.message.contains("inline content-control envelope"),
        "unexpected refusal: {error:?}"
    );
    assert!(
        error
            .details
            .context
            .as_deref()
            .is_some_and(|context| context.contains("w:sdt")),
        "unexpected refusal: {error:?}"
    );
}

#[test]
fn sentinel_tracked_delete_of_locked_inline_sdt_refuses() {
    let class = "tracked-delete-locked-inline-sdt";
    let (a, b) = tracked_delete_of_locked_inline_sdt();
    let rt = SimpleRuntime::new();
    let ia = rt
        .import_docx(&a)
        .unwrap_or_else(|error| panic!("[{class}] import A: {error:?}"));
    let ib = rt
        .import_docx(&b)
        .unwrap_or_else(|error| panic!("[{class}] import B: {error:?}"));
    let error = rt
        .diff_and_redline(&ia.doc_handle, &ib.doc_handle, redline_meta())
        .expect_err("a locked inline content-control deletion must refuse before output");

    assert_eq!(error.code, stemma::ErrorCode::UnsupportedEdit);
    assert!(
        error.message.contains("locked inline content-control"),
        "unexpected refusal: {error:?}"
    );
    assert!(
        error.details.context.as_deref().is_some_and(|context| {
            context.contains("lock=sdtContentLocked") && context.contains("native Word Accept All")
        }),
        "unexpected refusal context: {error:?}"
    );
}

#[test]
fn sentinel_word_saved_inline_sdt_deletion_remains_resolvable() {
    let class = "word-saved-inline-sdt-deletion";
    let body = r#"<w:p><w:r><w:t xml:space="preserve">LEFT </w:t></w:r><w:customXmlDelRangeStart w:id="0" w:author="Stemma"/><w:sdt><w:sdtPr><w:tag w:val="control"/><w:id w:val="701"/></w:sdtPr><w:sdtEndPr/><w:sdtContent><w:customXmlDelRangeEnd w:id="0"/><w:del w:id="1" w:author="Stemma"><w:r><w:delText>CONTROL</w:delText></w:r></w:del><w:customXmlDelRangeStart w:id="2" w:author="Stemma"/></w:sdtContent></w:sdt><w:customXmlDelRangeEnd w:id="2"/><w:r><w:t xml:space="preserve"> RIGHT</w:t></w:r></w:p>"#;
    let bytes = pack(&document(body));
    let rt = SimpleRuntime::new();
    let canonical = std::sync::Arc::unwrap_or_clone(
        rt.import_docx(&bytes)
            .expect("import Word-normalized SDT deletion")
            .canonical,
    );
    let surviving_sdt_ids = |doc: &CanonDoc| -> Vec<String> {
        common::all_paragraphs(doc)
            .iter()
            .flat_map(|paragraph| paragraph.all_inlines_owned())
            .filter_map(|inline| match inline {
                InlineNode::OpaqueInline(opaque) => opaque.raw_xml.as_ref().and_then(|raw| {
                    sdt_id_vals(&String::from_utf8_lossy(raw))
                        .into_iter()
                        .next()
                }),
                _ => None,
            })
            .collect()
    };
    let has_deletion_range = |doc: &CanonDoc| {
        common::all_paragraphs(doc).iter().any(|paragraph| {
            paragraph.all_inlines_owned().iter().any(|inline| {
                matches!(inline, InlineNode::Decoration(decoration)
                if decoration.raw_xml.as_deref().is_some_and(|raw| {
                    String::from_utf8_lossy(raw).contains("customXmlDelRange")
                }))
            })
        })
    };
    let deletion_id = stemma::enumerate_revisions(&canonical)
        .into_iter()
        .find(|revision| revision.kind == stemma::RevisionKind::OpaqueInterior)
        .map(|revision| revision.revision_id)
        .filter(|identity| *identity != 0)
        .expect("Word-normalized SDT deletion remains selectable");

    for (label, action, expected_ids) in [
        (
            "accept",
            stemma::ResolveSelectionAction::Accept,
            Vec::<String>::new(),
        ),
        (
            "reject",
            stemma::ResolveSelectionAction::Reject,
            vec!["701".to_string()],
        ),
    ] {
        let mut projected = canonical.clone();
        stemma::resolve_selected_revisions_with_styles(
            &mut projected,
            &std::collections::HashSet::from([deletion_id]),
            action,
            None,
        )
        .unwrap_or_else(|ids| panic!("[{class}] selective {label} refused ids {ids:?}"));
        assert_eq!(surviving_sdt_ids(&projected), expected_ids, "{label}");
        assert!(!has_deletion_range(&projected), "{label}");
        assert!(
            stemma::enumerate_revisions(&projected)
                .iter()
                .all(|revision| revision.revision_id != deletion_id),
            "{label}: the selected logical deletion must be fully settled"
        );
    }

    let mut accepted = canonical.clone();
    accept_all(&mut accepted);
    assert!(surviving_sdt_ids(&accepted).is_empty());
    assert!(!has_deletion_range(&accepted));
    let mut rejected = canonical;
    stemma::reject_all_with_styles(&mut rejected, None);
    assert_eq!(surviving_sdt_ids(&rejected), vec!["701".to_string()]);
    assert!(!has_deletion_range(&rejected));
}

#[test]
fn sentinel_tracked_delete_of_inline_sdt_uses_deltext() {
    let class = "tracked-delete-inline-sdt";
    let (a, b) = tracked_delete_of_inline_sdt();

    let exported = redline_export(class, &a, &b);
    let xml = document_xml(&exported);

    // (a) no <w:t>/<w:instrText> anywhere inside any <w:del> (this witness has no
    //     w:txbxContent, so the textbox exemption is not exercised here).
    let dels = del_spans(&xml);
    assert!(
        !dels.is_empty(),
        "[{class}] the removed content control must serialize inside a <w:del>"
    );
    for span in &dels {
        assert!(
            !span.contains("<w:t>") && !span.contains("<w:t ") && !span.contains("<w:instrText"),
            "[{class}] (a) run text inside <w:del> must be <w:delText>/<w:delInstrText>, found plain w:t/w:instrText:\n{span}"
        );
    }
    // The deleted SDT's inner text must have become delText.
    assert!(
        xml.contains("<w:delText"),
        "[{class}] the deleted content-control text must serialize as <w:delText>"
    );

    // (b) validator-clean — after the I-TC-001 sdt-descendant fix this proves the
    //     deleted-text content model over sdtContent hermetically.
    assert_validator_clean(class, &exported);

    // (c)/(d) accept/reject text identity through serialize→reparse.
    assert_reparse_accept_reject(class, &exported, &imported_text(&b), &imported_text(&a));
}

/// Count opaque INLINE nodes across all paragraphs whose kind matches `pred`.
fn count_opaque_inline(doc: &CanonDoc, pred: impl Fn(&stemma::OpaqueKind) -> bool) -> usize {
    common::all_paragraphs(doc)
        .iter()
        .flat_map(|p| p.all_inlines_owned())
        .filter(|inline| match inline {
            InlineNode::OpaqueInline(o) => pred(&o.kind),
            _ => false,
        })
        .count()
}

// ── small accessors for the formatting-tier assertions ─────────────────────

fn first_paragraph(doc: &CanonDoc) -> &stemma::ParagraphNode {
    doc.blocks
        .iter()
        .find_map(|tb| match &tb.block {
            BlockNode::Paragraph(p) => Some(p),
            _ => None,
        })
        .expect("document should have a top-level paragraph")
}

/// True if any text inline in the paragraph carries the Bold mark.
fn paragraph_has_bold(p: &stemma::ParagraphNode) -> bool {
    p.all_inlines_owned().iter().any(|inline| match inline {
        InlineNode::Text(t) => t.marks.contains(&Mark::Bold),
        _ => false,
    })
}

// (h) numbering.xml with a picture bullet (CT_Numbering root sequence).
//     ECMA-376 §17.9 CT_Numbering is an xsd:sequence:
//         numPicBullet*, abstractNum*, num*, numIdMacAtCleanup?
//     The base numbering part carries its numPicBullet FIRST (correct source
//     order). The redline export path (merge_target_numbering) re-parses and
//     re-emits numbering.xml, and must preserve that sequence — if numPicBullet
//     lands after abstractNum/num, Word repairs the file and can drop the
//     picture bullets. A→B edits only the body text of the numbered paragraph,
//     so the export runs the numbering-merge rewrite while every numId still
//     resolves. numId=10 is referenced by the paragraph; the second
//     abstractNum/num pair (id 1 / numId 11) is unreferenced but present so the
//     witness proves *all* abstractNum precede *all* num, both after the single
//     numPicBullet.
fn numbering_with_pic_bullet() -> (Vec<u8>, Vec<u8>) {
    let para = |t: &str| {
        format!(
            r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="10"/></w:numPr></w:pPr><w:r><w:t xml:space="preserve">{t}</w:t></w:r></w:p>"#
        )
    };
    // Source order: numPicBullet, then two abstractNum, then two num.
    let numbering = ExtraPart {
        name: "word/numbering.xml",
        content_type:
            "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml",
        rel: (
            "rIdNum",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering",
            "numbering.xml",
        ),
        xml: r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:numPicBullet w:numPicBulletId="0"><w:pict/></w:numPicBullet><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum><w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="o"/></w:lvl></w:abstractNum><w:num w:numId="10"><w:abstractNumId w:val="0"/></w:num><w:num w:numId="11"><w:abstractNumId w:val="1"/></w:num></w:numbering>"#.to_string(),
    };
    let numbering_b = numbering.clone();
    let a = pack_with_parts(
        &document(&para("Item old")),
        std::slice::from_ref(&numbering),
    );
    let b = pack_with_parts(
        &document(&para("Item new")),
        std::slice::from_ref(&numbering_b),
    );
    (a, b)
}

#[test]
fn sentinel_numbering_pic_bullet_root_order() {
    let class = "numbering-pic-bullet-root-order";
    let (a, b) = numbering_with_pic_bullet();

    // Core relational invariants over the diff/merge path.
    Pair::build(class, &a, &b).assert_core();

    // Drive the redline export — this is the path that re-emits numbering.xml
    // via merge_target_numbering.
    let exported = redline_export(class, &a, &b);

    // (b) #13 hermetic proxy: the emitted package opens clean.
    assert_validator_clean(class, &exported);

    // (a) CT_Numbering root sequence: numPicBullet* precede abstractNum*,
    //     which precede num*. Assert on the actually-emitted part, not the IR.
    let numbering_xml = extract_part(&exported, "word/numbering.xml");
    let pic = numbering_xml
        .find("<w:numPicBullet")
        .expect("emitted numbering.xml must retain the numPicBullet element");
    let first_abstract = numbering_xml
        .find("<w:abstractNum ")
        .expect("emitted numbering.xml must retain abstractNum elements");
    let first_num = numbering_xml
        .find("<w:num ")
        .expect("emitted numbering.xml must retain num elements");
    // All three groups present with the fixture's cardinality (nothing dropped).
    assert_eq!(
        numbering_xml.matches("<w:numPicBullet").count(),
        1,
        "[{class}] numPicBullet must survive the rewrite:\n{numbering_xml}"
    );
    assert_eq!(
        numbering_xml.matches("<w:abstractNum ").count(),
        2,
        "[{class}] both abstractNum must survive the rewrite:\n{numbering_xml}"
    );
    assert_eq!(
        numbering_xml.matches("<w:num ").count(),
        2,
        "[{class}] both num must survive the rewrite:\n{numbering_xml}"
    );
    // Schema order: numPicBullet before every abstractNum and num. Because the
    // groups are contiguous, numPicBullet < first abstractNum < first num proves
    // the full sequence. Pre-fix, numPicBullet sorted last (after num) and this
    // fails.
    assert!(
        pic < first_abstract,
        "[{class}] (a) CT_Numbering sequence violated: numPicBullet must precede abstractNum:\n{numbering_xml}"
    );
    assert!(
        first_abstract < first_num,
        "[{class}] (a) CT_Numbering sequence violated: abstractNum must precede num:\n{numbering_xml}"
    );

    // (c) Numbering survives: the numbered paragraph still resolves its numPr
    //     (numId=10) after export + reparse.
    let rt = SimpleRuntime::new();
    let reimported = std::sync::Arc::unwrap_or_clone(
        rt.import_docx(&exported)
            .unwrap_or_else(|e| panic!("[{class}] reimport exported redline: {e:?}"))
            .canonical,
    );
    let resolves = common::all_paragraphs(&reimported)
        .iter()
        .any(|p| p.numbering.as_ref().is_some_and(|n| n.num_id == 10));
    assert!(
        resolves,
        "[{class}] (c) numbered paragraph must still resolve numPr numId=10 after redline export + reparse"
    );
}

/// Read a single archive part out of an exported DOCX as a UTF-8 string.
fn extract_part(docx_bytes: &[u8], part: &str) -> String {
    use std::io::Read;
    let mut zip =
        zip::ZipArchive::new(std::io::Cursor::new(docx_bytes)).expect("open exported zip");
    let mut file = zip
        .by_name(part)
        .unwrap_or_else(|_| panic!("exported package must contain {part}"));
    let mut s = String::new();
    file.read_to_string(&mut s).expect("read part as UTF-8");
    s
}

// A row deletion must compose with revisions already present inside that row.
// The superscript "7" is a pending insertion in the source, so rejecting ALL
// changes must still remove it; rejecting the later row deletion cannot erase
// the insertion's independent provenance.
fn table_row_with_preexisting_inline_insertion() -> Vec<u8> {
    pack(&document(
        r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="2400"/></w:tblGrid><w:tr><w:tc><w:tcPr/><w:p><w:r><w:t>s. 12(1)</w:t></w:r><w:ins w:id="195" w:author="source" w:date="2015-12-11T06:20:00Z"><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:t>7</w:t></w:r></w:ins></w:p></w:tc></w:tr><w:tr><w:tc><w:tcPr/><w:p><w:r><w:t>keep</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/>"#,
    ))
}

fn archive_reject_text(bytes: &[u8]) -> String {
    let archive = stemma::docx::DocxArchive::read(bytes).expect("read sentinel archive");
    let (rejected, _) =
        stemma::normalize::reject_all_docx(&archive).expect("archive reject sentinel");
    let rejected_bytes = rejected.write().expect("write rejected sentinel");
    let rejected_doc =
        stemma::api::Document::parse(&rejected_bytes).expect("parse rejected sentinel");
    rejected_doc.to_text()
}

#[test]
fn sentinel_nested_form_value_refuses_to_drop_cached_result_bookmark() {
    let source = pack(&document(
        // A customXml element directly inside the control keeps it a
        // preserved opaque block, the host `set_block_form_text` addresses.
        r#"<w:sdt><w:sdtPr><w:id w:val="124"/></w:sdtPr><w:sdtContent><w:customXml w:uri="urn:x" w:element="sealed"><w:p><w:r><w:fldChar w:fldCharType="begin"><w:ffData><w:textInput/></w:ffData></w:fldChar></w:r><w:r><w:instrText> FORMTEXT </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:bookmarkStart w:id="3" w:name="KeepResultAnchor"/><w:r><w:t>Old value</w:t></w:r><w:bookmarkEnd w:id="3"/><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p></w:customXml></w:sdtContent></w:sdt><w:p/>"#,
    ));
    assert_validator_clean("field-result-bookmark-input", &source);
    let original = stemma::api::Document::parse(&source).unwrap();
    let fields = original.block_form_field_targets().unwrap();
    let t = &fields[0];
    let json = format!(
        r#"{{"ops":[{{"op":"set_block_form_text","body_index":{},"paragraph_index":{},"field_index":{},"semantic_hash":"{}","text":"New value"}}],"revision":{{"author":"Reviewer"}}}}"#,
        t.body_index, t.paragraph_index, t.field_index, t.semantic_hash
    );
    let txn = stemma::edit_v4::parse_transaction(&json)
        .unwrap()
        .into_edit_transaction()
        .unwrap();
    let before = original
        .serialize(&stemma::ExportOptions::default())
        .unwrap();
    assert!(
        original.apply(&txn).is_err(),
        "a field-value update cannot erase an unrelated bookmark"
    );
    let after = original
        .serialize(&stemma::ExportOptions::default())
        .unwrap();
    assert_eq!(
        before, after,
        "refused update must preserve the entire package"
    );
    assert_validator_clean("field-result-bookmark-preserved", &after);
    assert_eq!(
        stemma::api::Document::parse(&after)
            .unwrap()
            .block_form_field_targets()
            .unwrap(),
        fields
    );
}

#[test]
fn sentinel_nested_form_text_update_preserves_other_field_and_default() {
    let field = |value: &str| {
        format!(
            r#"<w:p><w:r><w:fldChar w:fldCharType="begin"><w:ffData><w:name w:val="Repeated"/><w:textInput><w:default w:val="Initial default"/></w:textInput></w:ffData></w:fldChar></w:r><w:r><w:instrText> FORMTEXT </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:rPr><w:rFonts w:ascii="Georgia" w:hAnsi="Georgia"/><w:i/></w:rPr><w:t>{value}</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#
        )
    };
    let source = pack(&document(&format!(
        // A customXml element directly inside the control keeps it a
        // preserved opaque block, the host `set_block_form_text` addresses.
        r#"<w:sdt><w:sdtPr><w:id w:val="123"/></w:sdtPr><w:sdtContent><w:customXml w:uri="urn:x" w:element="sealed"><w:p><w:r><w:t>Keep this label</w:t></w:r></w:p>{}{}</w:customXml></w:sdtContent></w:sdt><w:p/>"#,
        field("Keep this field"),
        field("Replace this value")
    )));
    let original = stemma::api::Document::parse(&source).unwrap();
    let fields = original.block_form_field_targets().unwrap();
    let t = &fields[1];
    let json = format!(
        r#"{{"ops":[{{"op":"set_block_form_text","body_index":{},"paragraph_index":{},"field_index":{},"semantic_hash":"{}","text":"New value"}}],"revision":{{"author":"Reviewer"}}}}"#,
        t.body_index, t.paragraph_index, t.field_index, t.semantic_hash
    );
    let txn = stemma::edit_v4::parse_transaction(&json)
        .unwrap()
        .into_edit_transaction()
        .unwrap();
    let edited = original.apply(&txn).unwrap();
    let bytes = edited.serialize(&stemma::ExportOptions::default()).unwrap();
    assert_validator_clean("nested-form-text-update", &bytes);
    let parsed = stemma::api::Document::parse(&bytes).unwrap();
    let result = parsed.block_form_field_targets().unwrap();
    assert_eq!(result[0].cached_result, "Keep this field");
    assert_eq!(result[1].cached_result, "New value");
    assert_eq!(original.block_form_field_targets().unwrap(), fields);
    assert!(
        parsed.apply(&txn).is_err(),
        "stale field address must refuse"
    );
    // A field-value update is direct: resolving unrelated revisions must not
    // undo it or consume its field boundaries.
    for resolution in [stemma::Resolution::AcceptAll, stemma::Resolution::RejectAll] {
        let projected = parsed.project(resolution).unwrap();
        let output = projected
            .serialize(&stemma::ExportOptions::default())
            .unwrap();
        assert_validator_clean("nested-form-text-projection", &output);
        let p = stemma::api::Document::parse(&output).unwrap();
        assert_eq!(
            p.block_form_field_targets().unwrap()[1].cached_result,
            "New value"
        );
    }
}

#[test]
fn sentinel_complex_field_controls_preserve_explicit_formatting_off() {
    // Field controls have character formatting even though they are not text.
    // An explicit OFF must remain distinct from inheritance when another
    // paragraph is edited, including after Accept and Reject.
    let props =
        r#"<w:rPr><w:b w:val="0"/><w:i w:val="0"/><w:sz w:val="18"/><w:u w:val="none"/></w:rPr>"#;
    let source = pack(&document(&format!(
        r#"<w:p><w:r>{props}<w:fldChar w:fldCharType="begin"/></w:r><w:r>{props}<w:instrText> PAGE </w:instrText></w:r><w:r>{props}<w:fldChar w:fldCharType="separate"/></w:r><w:r>{props}<w:t>3</w:t></w:r><w:r>{props}<w:fldChar w:fldCharType="end"/></w:r></w:p><w:p><w:r><w:t>Append after this paragraph.</w:t></w:r></w:p>"#
    )));
    assert_validator_clean("field-control-off-input", &source);
    let original = stemma::api::Document::parse(&source).unwrap();
    let BlockNode::Paragraph(anchor) = &original.snapshot().canonical.blocks[1].block else {
        panic!("expected paragraph anchor")
    };
    let anchor = &anchor.id;
    let json = serde_json::json!({"ops":[{"op":"insert","target":{"anchor":anchor.0,"position":"after"},"content":[{"type":"paragraph","content":[{"type":"text","text":"Additional paragraph."}]}]}],"revision":{"author":"Reviewer"}}).to_string();
    let txn = stemma::edit_v4::parse_transaction(&json)
        .unwrap()
        .into_edit_transaction()
        .unwrap();
    let edited = original.apply(&txn).unwrap();
    for doc in [
        edited.project(stemma::Resolution::AcceptAll).unwrap(),
        edited.project(stemma::Resolution::RejectAll).unwrap(),
        edited,
    ] {
        let bytes = doc.serialize(&stemma::ExportOptions::default()).unwrap();
        assert_validator_clean("field-control-off-output", &bytes);
        let xml = extract_part(&bytes, "word/document.xml");
        let root = xmltree::Element::parse(xml.as_bytes()).unwrap();
        let para = root.get_child("body").unwrap().get_child("p").unwrap();
        let mut controls = 0;
        for run in para
            .children
            .iter()
            .filter_map(xmltree::XMLNode::as_element)
        {
            if run.name != "r"
                || (run.get_child("fldChar").is_none() && run.get_child("instrText").is_none())
            {
                continue;
            }
            controls += 1;
            let rpr = run.get_child("rPr").expect("field control run properties");
            for (name, value) in [("b", "0"), ("i", "0"), ("u", "none"), ("sz", "18")] {
                let property = rpr.get_child(name).unwrap_or_else(|| {
                    panic!("field control {controls} lost direct {name}: {xml}")
                });
                assert_eq!(
                    property
                        .attributes
                        .iter()
                        .find(|(name, _)| name.local_name == "val")
                        .map(|(_, value)| value.as_str()),
                    Some(value)
                );
            }
        }
        assert_eq!(controls, 4, "all field controls must remain present");
    }
}

#[test]
fn sentinel_format_from_kept_mark_override_and_new_hyperlink() {
    let source = pack(&document(
        r#"<w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Alpha</w:t></w:r><w:r><w:rPr><w:sz w:val="22"/></w:rPr><w:t xml:space="preserve"> beta.</w:t></w:r></w:p>"#,
    ));
    let original = stemma::api::Document::parse(&source).unwrap();
    let BlockNode::Paragraph(reference) = &original.snapshot().canonical.blocks[0].block else {
        panic!()
    };
    let id = &reference.id;
    let json=serde_json::json!({"ops":[{"op":"insert","target":{"anchor":id.0,"position":"after"},"content":[{"type":"paragraph","attrs":{"format_from":id.0},"content":[{"type":"text","text":"Alpha "},{"type":"text","text":"beta.","marks":[{"type":"italic"}]},{"type":"text","text":" "},{"type":"hyperlink","attrs":{"href":"https://example.test/resource"},"content":[{"type":"text","text":"Resource"}]}]}]}],"revision":{"author":"Reviewer"}}).to_string();
    let txn = stemma::edit_v4::parse_transaction(&json)
        .unwrap()
        .into_edit_transaction()
        .unwrap();
    let edited = original.apply(&txn).unwrap();
    let bytes = edited.serialize(&stemma::ExportOptions::default()).unwrap();
    assert_validator_clean("template-kept-format-and-link", &bytes);
    let reopened = stemma::api::Document::parse(&bytes).unwrap();
    let accepted = reopened.project(stemma::Resolution::AcceptAll).unwrap();
    let BlockNode::Paragraph(inserted) = &accepted.snapshot().canonical.blocks[1].block else {
        panic!()
    };
    let runs: Vec<_> = inserted
        .all_inlines()
        .filter_map(|n| {
            if let stemma::domain::InlineNode::Text(t) = n {
                Some(t)
            } else {
                None
            }
        })
        .collect();
    assert!(
        runs.iter()
            .any(|t| t.text == "Alpha" && t.marks.contains(&stemma::domain::Mark::Bold))
    );
    assert!(runs.iter().any(|t| t.text == "beta."
        && t.marks.contains(&stemma::domain::Mark::Italic)
        && t.style_props.font_size == Some(22)));
    assert_eq!(
        hyperlinks(&accepted),
        vec![(
            Some("https://example.test/resource".to_string()),
            "Resource".to_string()
        )]
    );
    assert!(document_xml(&bytes).contains("https://example.test/resource"));
    assert_eq!(
        reopened
            .project(stemma::Resolution::RejectAll)
            .unwrap()
            .to_text(),
        original.to_text()
    );
}

#[test]
fn sentinel_format_from_preserves_source_revisions_without_copying_them() {
    let source = pack(&document(
        r#"<w:p><w:r><w:t>“</w:t></w:r><w:del w:id="31" w:author="Earlier"><w:r><w:rPr><w:b/></w:rPr><w:delText>Record</w:delText></w:r></w:del><w:ins w:id="32" w:author="Earlier"><w:r><w:rPr><w:b/></w:rPr><w:t>Register</w:t></w:r></w:ins><w:r><w:t>” means the current collection.</w:t></w:r></w:p>"#,
    ));
    assert_validator_clean("template-source-revisions", &source);
    let original = stemma::api::Document::parse(&source).unwrap();
    let BlockNode::Paragraph(reference) = &original.snapshot().canonical.blocks[0].block else {
        panic!("expected template paragraph")
    };
    let expected = "“Registry” means the current collection.";
    let json = serde_json::json!({"ops":[{"op":"insert","target":{"anchor":reference.id.0,"position":"after"},"content":[{"type":"paragraph","attrs":{"format_from":reference.id.0},"content":[{"type":"text","text":expected}]}]}],"revision":{"author":"Current"}}).to_string();
    let txn = stemma::edit_v4::parse_transaction(&json)
        .unwrap()
        .into_edit_transaction()
        .unwrap();
    let edited = original.apply(&txn).unwrap();
    let BlockNode::Paragraph(retained) = &edited.snapshot().canonical.blocks[0].block else {
        panic!("expected retained template paragraph")
    };
    // A tracked paragraph insertion necessarily authors its preceding paragraph
    // mark. That new boundary is not a mutation of the source inline history.
    assert_eq!(
        retained.segments, reference.segments,
        "source text, formatting, anchors, and pending inline revisions must remain untouched"
    );
    let bytes = edited.serialize(&stemma::ExportOptions::default()).unwrap();
    assert_validator_clean("template-revisions-output", &bytes);
    let reopened = stemma::api::Document::parse(&bytes).unwrap();
    let accepted = reopened.project(stemma::Resolution::AcceptAll).unwrap();
    let BlockNode::Paragraph(inserted) = &accepted.snapshot().canonical.blocks[1].block else {
        panic!("expected inserted paragraph")
    };
    let text: String = inserted
        .all_inlines()
        .filter_map(|n| match n {
            InlineNode::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect();
    let bold: String = inserted
        .all_inlines()
        .filter_map(|n| match n {
            InlineNode::Text(t) if t.marks.contains(&Mark::Bold) => Some(t.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, expected);
    assert_eq!(bold, "Registry");
    let xml = extract_part(&bytes, "word/document.xml");
    let root = xmltree::Element::parse(xml.as_bytes()).unwrap();
    let inserted_xml = root
        .get_child("body")
        .unwrap()
        .children
        .iter()
        .filter_map(xmltree::XMLNode::as_element)
        .filter(|e| e.name == "p")
        .nth(1)
        .unwrap();
    fn assert_no_source_revision(element: &xmltree::Element) {
        assert!(
            !element
                .attributes
                .iter()
                .any(|(name, value)| name.local_name == "author" && value == "Earlier"),
            "source revision history must not be duplicated"
        );
        for child in element
            .children
            .iter()
            .filter_map(xmltree::XMLNode::as_element)
        {
            assert_no_source_revision(child);
        }
    }
    assert_no_source_revision(inserted_xml);
    let rejected = reopened.project(stemma::Resolution::RejectAll).unwrap();
    assert_eq!(
        rejected.to_text(),
        original
            .project(stemma::Resolution::RejectAll)
            .unwrap()
            .to_text()
    );
    assert_validator_clean(
        "template-revisions-rejected",
        &rejected
            .serialize(&stemma::ExportOptions::default())
            .unwrap(),
    );
}

/// A template that holds an anchor (here a bookmark) lends its formatting, not
/// its anchor: the new paragraph is built from the caller's content, so the
/// bookmark is neither duplicated into it nor removed from the template.
#[test]
fn sentinel_format_from_template_anchor_is_neither_duplicated_nor_dropped() {
    let source = pack(&document(
        r#"<w:p><w:bookmarkStart w:id="7" w:name="KeepAnchor"/><w:r><w:rPr><w:b/></w:rPr><w:t>Template text</w:t></w:r><w:bookmarkEnd w:id="7"/></w:p>"#,
    ));
    assert_validator_clean("template-bookmark-input", &source);
    let original = stemma::api::Document::parse(&source).unwrap();
    let BlockNode::Paragraph(reference) = &original.snapshot().canonical.blocks[0].block else {
        panic!("expected template paragraph")
    };
    let json = serde_json::json!({"ops":[{"op":"insert","target":{"anchor":reference.id.0,"position":"after"},"content":[{"type":"paragraph","attrs":{"format_from":reference.id.0},"content":[{"type":"text","text":"New text"}]}]}],"revision":{"author":"Reviewer"}}).to_string();
    let txn = stemma::edit_v4::parse_transaction(&json)
        .unwrap()
        .into_edit_transaction()
        .unwrap();
    let edited = original
        .apply(&txn)
        .expect("a template with an anchor lends its formatting");
    let bytes = edited.serialize(&stemma::ExportOptions::default()).unwrap();
    assert_validator_clean("template-bookmark-output", &bytes);
    let xml = String::from_utf8(
        stemma::docx::DocxArchive::read(&bytes)
            .unwrap()
            .get("word/document.xml")
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert_eq!(
        xml.matches(r#"w:name="KeepAnchor""#).count(),
        1,
        "the bookmark stays once: {xml}"
    );
    let new_paragraph = &xml[xml.find("New text").unwrap()..];
    let new_paragraph = &xml[xml[..xml.find("New text").unwrap()].rfind("<w:p").unwrap()
        ..xml.find("New text").unwrap() + new_paragraph.find("</w:p>").unwrap()];
    assert!(
        !new_paragraph.contains("bookmark"),
        "the new paragraph has no anchor: {new_paragraph}"
    );
    assert!(
        new_paragraph.contains("<w:b"),
        "it takes the template's bold: {new_paragraph}"
    );
}

#[test]
fn sentinel_format_from_preserves_mixed_character_formatting() {
    assert_mixed_template_insert("“Register” means the current collection.", "Register");
}

#[test]
fn sentinel_format_from_heavy_rewrite_keeps_formatting_boundaries() {
    assert_mixed_template_insert(
        "“Inventory” describes reports prepared for review.",
        "Inventory",
    );
}

/// Text that replaces template text takes the formatting of most of the text it
/// replaces, not of whichever template character happens to precede it. A
/// panel order item ending "by the 21st of June 2027" carries a superscript
/// ordinal suffix; a new item in the same format ("Match the surrounding
/// items") must not set an unrelated word in superscript because its diff
/// hunk began just after that suffix.
#[test]
fn sentinel_format_from_does_not_spread_an_ordinal_superscript() {
    let source = pack(&document(
        r#"<w:p><w:pPr><w:ind w:left="720"/></w:pPr><w:r><w:t xml:space="preserve">The contractor and the client have each proposed one inspector, who are Ms. Ada Morgan Price and Mr. Owen Hale respectively, and the Board has selected Ms Ruth Collins as the third inspector to make up the panel. The panel shall appoint a Chair and they shall regulate their own internal proceedings, and their costs shall be jointly borne by the parties. The panel shall make available a report to this Board and both parties by the 21</w:t></w:r><w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:t>st</w:t></w:r><w:r><w:t xml:space="preserve"> of June 2027.</w:t></w:r></w:p>"#,
    ));
    let original = stemma::api::Document::parse(&source).unwrap();
    let BlockNode::Paragraph(reference) = &original.snapshot().canonical.blocks[0].block else {
        panic!()
    };
    let id = reference.id.clone();
    let expected = "Each inspector shall disclose any conflict of interest to the Board and both parties before commencing the inspection.";
    let json = serde_json::json!({"ops":[{"op":"insert","target":{"anchor":id.0,"position":"after"},"content":[{"type":"paragraph","attrs":{"format_from":id.0},"content":[{"type":"text","text":expected}]}]}],"revision":{"author":"Reviewer"}}).to_string();
    let txn = stemma::edit_v4::parse_transaction(&json)
        .unwrap()
        .into_edit_transaction()
        .unwrap();
    let accepted = original
        .apply(&txn)
        .unwrap()
        .project(stemma::Resolution::AcceptAll)
        .unwrap();
    let BlockNode::Paragraph(inserted) = &accepted.snapshot().canonical.blocks[1].block else {
        panic!()
    };
    let raised: String = inserted
        .all_inlines()
        .filter_map(|n| match n {
            stemma::domain::InlineNode::Text(t)
                if t.marks.contains(&stemma::domain::Mark::Superscript) =>
            {
                Some(t.text.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(raised, "", "no word of the new item is raised");
    assert_eq!(inserted.indent, reference.indent);
}

/// The spaces between replaced words are part of the replacement, not kept
/// text that splits it word by word. A notice item "returned to the office
/// *ex ante* for a review *ab initio* before a different reviewer" used as the
/// template for "... for review within 30 days of the date of this notice"
/// must not italicise "within 30 days" because those words line up with "ab
/// initio": the tail they replace is mostly roman, so it stays roman. The
/// caller's own italic on "ex ante" stands.
#[test]
fn sentinel_format_from_replaces_a_phrase_as_a_whole() {
    let source = pack(&document(
        r#"<w:p><w:r><w:t xml:space="preserve">The file be and is hereby returned to the office </w:t></w:r><w:r><w:rPr><w:i/></w:rPr><w:t>ex ante</w:t></w:r><w:r><w:t xml:space="preserve"> for a review </w:t></w:r><w:r><w:rPr><w:i/></w:rPr><w:t>ab initio</w:t></w:r><w:r><w:t xml:space="preserve"> before a different reviewer.</w:t></w:r></w:p>"#,
    ));
    let original = stemma::api::Document::parse(&source).unwrap();
    let BlockNode::Paragraph(reference) = &original.snapshot().canonical.blocks[0].block else {
        panic!()
    };
    let id = reference.id.clone();
    let json = serde_json::json!({"ops":[{"op":"insert","target":{"anchor":id.0,"position":"after"},"content":[{"type":"paragraph","attrs":{"format_from":id.0},"content":[
        {"type":"text","text":"The clerk of the office "},
        {"type":"text","text":"ex ante","marks":[{"type":"italic"}]},
        {"type":"text","text":" shall set the file down for review within 30 days of the date of this notice."}
    ]}]}],"revision":{"author":"Reviewer"}}).to_string();
    let txn = stemma::edit_v4::parse_transaction(&json)
        .unwrap()
        .into_edit_transaction()
        .unwrap();
    let accepted = original
        .apply(&txn)
        .unwrap()
        .project(stemma::Resolution::AcceptAll)
        .unwrap();
    let BlockNode::Paragraph(inserted) = &accepted.snapshot().canonical.blocks[1].block else {
        panic!()
    };
    let italic: Vec<&str> = inserted
        .all_inlines()
        .filter_map(|n| match n {
            stemma::domain::InlineNode::Text(t)
                if t.marks.contains(&stemma::domain::Mark::Italic) =>
            {
                Some(t.text.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(italic, vec!["ex ante"], "only the caller's italic");
}

fn assert_mixed_template_insert(expected: &str, expected_bold: &str) {
    let source = pack(&document(
        r#"<w:p><w:pPr><w:ind w:left="600"/></w:pPr><w:r><w:t>“</w:t></w:r><w:r><w:rPr><w:b/><w:sz w:val="26"/></w:rPr><w:t>Record</w:t></w:r><w:r><w:t>” </w:t></w:r><w:r><w:rPr><w:sz w:val="22"/><w:rFonts w:ascii="Georgia" w:hAnsi="Georgia"/></w:rPr><w:t>means the original collection.</w:t></w:r></w:p>"#,
    ));
    let original = stemma::api::Document::parse(&source).unwrap();
    let BlockNode::Paragraph(reference) = &original.snapshot().canonical.blocks[0].block else {
        panic!()
    };
    let id = &reference.id;
    let json=serde_json::json!({"ops":[{"op":"insert","target":{"anchor":id.0,"position":"after"},"content":[{"type":"paragraph","attrs":{"format_from":id.0},"content":[{"type":"text","text":expected}]}]}],"revision":{"author":"Reviewer"}}).to_string();
    let txn = stemma::edit_v4::parse_transaction(&json)
        .unwrap()
        .into_edit_transaction()
        .unwrap();
    let edited = original.apply(&txn).unwrap();
    let bytes = edited.serialize(&stemma::ExportOptions::default()).unwrap();
    assert_validator_clean("mixed-format-template", &bytes);
    let reopened = stemma::api::Document::parse(&bytes).unwrap();
    let accepted = reopened.project(stemma::Resolution::AcceptAll).unwrap();
    let BlockNode::Paragraph(inserted) = &accepted.snapshot().canonical.blocks[1].block else {
        panic!()
    };
    let runs: Vec<_> = inserted
        .all_inlines()
        .filter_map(|n| {
            if let stemma::domain::InlineNode::Text(t) = n {
                Some(t)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        runs.iter().map(|t| t.text.as_str()).collect::<String>(),
        expected
    );
    let bold = runs
        .iter()
        .filter(|t| t.marks.contains(&stemma::domain::Mark::Bold))
        .map(|t| t.text.as_str())
        .collect::<String>();
    assert_eq!(
        bold, expected_bold,
        "a replacement term inherits its template term's emphasis, not the leading quote's formatting"
    );
    for t in &runs {
        if t.text.contains(expected_bold) {
            assert_eq!(t.style_props.font_size, Some(26));
        }
        if t.text.contains("current") || t.text.contains("describes") {
            assert_eq!(t.style_props.font_size, Some(22));
            assert_eq!(t.style_props.font_family.as_deref(), Some("Georgia"));
        }
    }
    assert_eq!(inserted.indent, reference.indent);
    assert_ne!(inserted.id, reference.id);
    let rejected = reopened.project(stemma::Resolution::RejectAll).unwrap();
    assert_eq!(rejected.to_text(), original.to_text());
    let rejected_bytes = rejected
        .serialize(&stemma::ExportOptions::default())
        .unwrap();
    assert_validator_clean("mixed-format-template-rejected", &rejected_bytes);
    assert_eq!(original.snapshot().canonical.blocks.len(), 1);
}

#[test]
fn sentinel_insert_format_from_copies_exact_paragraph_not_role_exemplar() {
    let class = "explicit-paragraph-format-source";
    let source = pack_with_parts(&document(
        r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr><w:ind w:left="600"/><w:spacing w:after="120"/></w:pPr><w:r><w:rPr><w:rFonts w:ascii="Georgia" w:hAnsi="Georgia"/><w:sz w:val="22"/></w:rPr><w:t>Representative</w:t></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr><w:ind w:left="597"/><w:spacing w:after="120"/></w:pPr><w:r><w:rPr><w:rFonts w:ascii="Georgia" w:hAnsi="Georgia"/><w:sz w:val="22"/></w:rPr><w:t>Exact neighbor</w:t></w:r></w:p><w:p><w:r><w:t>Unrelated following paragraph</w:t></w:r></w:p>"#,
    ), &[ExtraPart {
        name: "word/numbering.xml",
        content_type: "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml",
        rel: ("rNumbering", "http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering", "numbering.xml"),
        xml: r#"<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="•"/><w:lvlJc w:val="left"/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#.into(),
    }]);
    let original = stemma::api::Document::parse(&source).unwrap();
    let BlockNode::Paragraph(reference) = &original.snapshot().canonical.blocks[1].block else {
        panic!("synthetic reference paragraph exists");
    };
    let id = &reference.id;
    let json = format!(
        r#"{{"ops":[{{"op":"insert","target":{{"anchor":"{id}","position":"after"}},"content":[{{"type":"paragraph","attrs":{{"format_from":"{id}"}},"content":[{{"type":"text","text":"New paragraph"}}]}}]}}],"revision":{{"author":"Reviewer"}}}}"#
    );
    let txn = stemma::edit_v4::parse_transaction(&json)
        .unwrap()
        .into_edit_transaction()
        .unwrap();
    let edited = original.apply(&txn).unwrap();
    let redline = edited.serialize(&stemma::ExportOptions::default()).unwrap();
    assert_validator_clean(class, &redline);
    let reparsed = stemma::api::Document::parse(&redline).unwrap();
    let accepted = reparsed.project(stemma::Resolution::AcceptAll).unwrap();
    let BlockNode::Paragraph(inserted) = &accepted.snapshot().canonical.blocks[2].block else {
        panic!("new paragraph exists at requested position");
    };
    assert_eq!(inserted.indent, reference.indent);
    assert_eq!(inserted.authored_indent, reference.authored_indent);
    assert_eq!(inserted.spacing, reference.spacing);
    assert_eq!(
        inserted.numbering.as_ref().unwrap().num_id,
        reference.numbering.as_ref().unwrap().num_id
    );
    assert!(inserted.numbering.as_ref().unwrap().is_bullet);
    assert_eq!(
        inserted.first_content_text_node().unwrap().text,
        "New paragraph"
    );
    assert_eq!(
        inserted
            .first_content_text_node()
            .unwrap()
            .style_props
            .font_family
            .as_deref(),
        Some("Georgia")
    );
    assert_ne!(inserted.id, reference.id);
    let bad_json = json.replace(
        &format!(r#""format_from":"{id}""#),
        r#""format_from":"missing_paragraph""#,
    );
    let bad_txn = stemma::edit_v4::parse_transaction(&bad_json)
        .unwrap()
        .into_edit_transaction()
        .unwrap();
    assert!(
        original.apply(&bad_txn).is_err(),
        "missing source must not fall back to host or role"
    );
    let rejected = reparsed.project(stemma::Resolution::RejectAll).unwrap();
    assert_eq!(rejected.to_text(), original.to_text());
    assert_validator_clean(
        class,
        &rejected
            .serialize(&stemma::ExportOptions::default())
            .unwrap(),
    );
}

#[test]
fn sentinel_multi_paragraph_move_has_one_range_per_side() {
    use stemma::TrackingStatus;

    // ECMA-376 17.13.5: a move range start/end pair encloses the moved
    // region; one identifier cannot denote repeated independent starts.
    let source = pack(&document(
        "<w:p><w:r><w:t>Alpha</w:t></w:r></w:p><w:p><w:r><w:t>Beta</w:t></w:r></w:p><w:p><w:r><w:t>Gamma</w:t></w:r></w:p><w:p><w:r><w:t>Delta</w:t></w:r></w:p>",
    ));
    let original = stemma::api::Document::parse(&source).unwrap();
    let ids: Vec<_> = original
        .snapshot()
        .canonical
        .blocks
        .iter()
        .map(|b| match &b.block {
            BlockNode::Paragraph(p) => p.id.0.clone(),
            _ => panic!(),
        })
        .collect();
    let json = serde_json::json!({"ops":[{"op":"move","target":{"from":ids[0],"to":ids[1]},"destination":{"anchor":ids[3],"position":"after"}}],"revision":{"author":"Reviewer"}}).to_string();
    let txn = stemma::edit_v4::parse_transaction(&json)
        .unwrap()
        .into_edit_transaction()
        .unwrap();
    let edited = original.apply(&txn).unwrap();
    let bytes = edited.serialize(&stemma::ExportOptions::default()).unwrap();
    let xml = document_xml(&bytes);
    for tag in [
        "moveFromRangeStart",
        "moveFromRangeEnd",
        "moveToRangeStart",
        "moveToRangeEnd",
    ] {
        assert_eq!(
            xml.matches(&format!("<w:{tag} ")).count(),
            1,
            "a multi-paragraph move needs exactly one {tag}"
        );
    }
    assert_validator_clean("multi-paragraph-move-ranges", &bytes);
    let reopened = stemma::api::Document::parse(&bytes).unwrap();
    // Saving a whole-block move must retain the ordered proposal carriers,
    // not add duplicate run/mark revisions that invalidate persisted identity.
    let carriers = |doc: &stemma::api::Document| {
        let mut result = Vec::new();
        let mut record = |status: &TrackingStatus| {
            let (kind, revision) = match status {
                TrackingStatus::Normal => return,
                TrackingStatus::Inserted(revision) => ("insert", revision),
                TrackingStatus::Deleted(revision) => ("delete", revision),
                TrackingStatus::InsertedThenDeleted(_) => panic!("no stacked edits in witness"),
            };
            result.push((
                kind,
                revision.identity,
                revision.author.clone(),
                revision.date.clone(),
            ));
        };
        for block in &doc.snapshot().canonical.blocks {
            record(&block.status);
            let BlockNode::Paragraph(paragraph) = &block.block else {
                panic!("paragraph-only witness");
            };
            for segment in &paragraph.segments {
                record(&segment.status);
            }
            if let Some(status) = &paragraph.para_mark_status {
                record(status);
            }
        }
        result
    };
    assert_eq!(carriers(&reopened), carriers(&edited));
    let rejected = reopened.project(stemma::Resolution::RejectAll).unwrap();
    assert_eq!(rejected.to_text(), original.to_text());
    let accepted = reopened.project(stemma::Resolution::AcceptAll).unwrap();
    let text = accepted.to_text();
    assert!(text.find("Gamma").unwrap() < text.find("Delta").unwrap());
    assert!(text.find("Delta").unwrap() < text.find("Alpha").unwrap());
    assert!(text.find("Alpha").unwrap() < text.find("Beta").unwrap());
    assert_validator_clean(
        "accepted-multi-paragraph-move",
        &accepted
            .serialize(&stemma::ExportOptions::default())
            .unwrap(),
    );
    assert_validator_clean(
        "rejected-multi-paragraph-move",
        &rejected
            .serialize(&stemma::ExportOptions::default())
            .unwrap(),
    );
}

#[test]
fn sentinel_row_relocation_allocates_distinct_physical_note_resources() {
    use std::collections::HashSet;
    for (kind, part, content_type, rel_type) in [
        (
            "footnote",
            "word/footnotes.xml",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes",
        ),
        (
            "endnote",
            "word/endnotes.xml",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.endnotes+xml",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/endnotes",
        ),
    ] {
        // Every row has a note: either valid alignment of a permutation must
        // allocate resources for the rows it physically inserts, not just the
        // row index in the caller's request. Shared deletion/insertion note
        // identities are not Word-compatible for this native row carrier.
        let mut rows = String::new();
        let mut notes = String::new();
        for (id, label) in [(1, "Alpha"), (2, "Beta"), (3, "Gamma")] {
            let row_id = id + 16;
            rows.push_str(&format!(r#"<w:tr xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" w14:paraId="{row_id:08X}"><w:tc><w:tcPr><w:tcW w:w="2400" w:type="dxa"/></w:tcPr><w:p w14:paraId="{id:08X}" w14:textId="00001000"><w:r><w:t>{label}</w:t></w:r><w:r><w:{kind}Reference w:id="{id}"/></w:r></w:p></w:tc></w:tr>"#));
            notes.push_str(&format!(r#"<w:{kind} w:id="{id}" w:type="normal"><w:p xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" w14:paraId="{id:08X}" w14:textId="00001000"><w:pPr><w:spacing w:after="140"/></w:pPr><w:r w:rsidR="00112233" w:rsidRPr="00445566"><w:{kind}Ref/></w:r><w:r><w:rPr><w:b/><w:color w:val="345678"/></w:rPr><w:t>{label} note</w:t></w:r></w:p></w:{kind}>"#));
        }
        let source = pack_with_parts(
            &document(&format!(
                r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="2400"/></w:tblGrid>{rows}</w:tbl>"#
            )),
            &[ExtraPart {
                name: part,
                content_type,
                rel: ("notes", rel_type, part.strip_prefix("word/").unwrap()),
                xml: format!(
                    r#"<w:{kind}s xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{notes}</w:{kind}s>"#
                ),
            }],
        );
        let original = stemma::api::Document::parse(&source).unwrap();
        let BlockNode::Table(table) = &original.snapshot().canonical.blocks[0].block else {
            panic!("table")
        };
        let plan = format!(
            r#"{{"ops":[{{"op":"table_op","target":"{}","table_op":{{"kind":"move_row","row_index":0,"ref_row":2,"position":"after"}}}}],"revision":{{"author":"Editor"}}}}"#,
            table.id.0
        );
        let transaction = stemma::edit_v4::parse_transaction(&plan)
            .unwrap()
            .into_edit_transaction()
            .unwrap();
        for guard in ["bookmark", "pending revision"] {
            let guarded_notes = [(1, "Alpha"), (2, "Beta"), (3, "Gamma")].map(|(id, label)| {
                let payload = if guard == "bookmark" {
                    format!(r#"<w:bookmarkStart w:id="{}" w:name="Note{label}"/><w:r><w:t>{label} note</w:t></w:r><w:bookmarkEnd w:id="{}"/>"#, 100 + id, 100 + id)
                } else {
                    format!(r#"<w:ins w:id="{}" w:author="Other"><w:r><w:t>{label} note</w:t></w:r></w:ins>"#, 100 + id)
                };
                format!(r#"<w:{kind} w:id="{id}"><w:p><w:r><w:{kind}Ref/></w:r>{payload}</w:p></w:{kind}>"#)
            }).join("");
            let guarded_source = pack_with_parts(
                &document(&format!(
                    r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="2400"/></w:tblGrid>{rows}</w:tbl>"#
                )),
                &[ExtraPart {
                    name: part,
                    content_type,
                    rel: ("notes", rel_type, part.strip_prefix("word/").unwrap()),
                    xml: format!(
                        r#"<w:{kind}s xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{guarded_notes}</w:{kind}s>"#
                    ),
                }],
            );
            let guarded = stemma::api::Document::parse(&guarded_source).unwrap();
            let before = guarded.snapshot().canonical.clone();
            let error = match guarded.apply(&transaction) {
                Ok(_) => panic!("unmapped note identities must be refused"),
                Err(error) => error,
            };
            assert!(error.to_string().contains(guard), "{error}");
            assert_eq!(
                guarded.snapshot().canonical,
                before,
                "refusal leaves source intact"
            );
        }
        let edited = original.apply(&transaction).unwrap();
        let pending = edited.serialize(&stemma::ExportOptions::default()).unwrap();
        let xml = document_xml(&pending);
        let tree = xmltree::Element::parse(xml.as_bytes()).unwrap();
        fn physical_ids(node: &xmltree::Element, ids: &mut Vec<u32>) {
            for (name, value) in &node.attributes {
                if name.local_name == "paraId" {
                    ids.push(u32::from_str_radix(value, 16).unwrap());
                }
            }
            for child in &node.children {
                if let xmltree::XMLNode::Element(child) = child {
                    physical_ids(child, ids);
                }
            }
        }
        let mut main_ids = Vec::new();
        physical_ids(&tree, &mut main_ids);
        assert_eq!(
            main_ids.iter().collect::<HashSet<_>>().len(),
            main_ids.len(),
            "inserted and deleted row copies need distinct physical paragraph/row identities"
        );
        fn reference_ids(node: &xmltree::Element, tag: &str, ids: &mut Vec<String>) {
            if node.name == tag {
                let (_, id) = node
                    .attributes
                    .iter()
                    .find(|(key, _)| key.local_name == "id")
                    .unwrap();
                ids.push(id.clone());
            }
            for child in &node.children {
                if let xmltree::XMLNode::Element(child) = child {
                    reference_ids(child, tag, ids);
                }
            }
        }
        let mut references = Vec::new();
        reference_ids(&tree, &format!("{kind}Reference"), &mut references);
        assert!(
            references.len() > 3,
            "pending relocation contains both physical row copies"
        );
        assert_eq!(
            references.iter().collect::<HashSet<_>>().len(),
            references.len(),
            "each physical {kind} reference needs its own resource: {references:?}"
        );
        assert_validator_clean("row-note-pending", &pending);
        // ECMA-376 §17.3.2.25: the marker's host run owns its source
        // editing-session attributes, just like a text or opaque run.
        let pending_notes = extract_part(&pending, part);
        // MS-DOCX §2.6.2.3: a physical clone is a new paragraph within
        // the same note part, not another reference to the original identity.
        let note_tree = xmltree::Element::parse(pending_notes.as_bytes()).unwrap();
        let para_ids: Vec<_> = note_tree
            .children
            .iter()
            .filter_map(|node| {
                let xmltree::XMLNode::Element(note) = node else {
                    return None;
                };
                let paragraph = note.get_child("p").unwrap();
                Some(
                    paragraph
                        .attributes
                        .iter()
                        .find(|(name, _)| name.local_name == "paraId")
                        .map(|(_, value)| u32::from_str_radix(value, 16).unwrap())
                        .unwrap(),
                )
            })
            .collect();
        assert_eq!(para_ids.len(), references.len());
        assert!(para_ids.iter().all(|id| *id > 0 && *id < 0x8000_0000));
        assert_eq!(
            para_ids.iter().collect::<HashSet<_>>().len(),
            para_ids.len(),
            "physical note paragraphs require distinct persisted identities"
        );
        for attribute in [r#"w:rsidR="00112233""#, r#"w:rsidRPr="00445566""#] {
            assert_eq!(
                pending_notes.matches(attribute).count(),
                references.len(),
                "every physical note marker preserves its host-run provenance"
            );
        }
        let reopened = stemma::api::Document::parse(&pending).unwrap();
        for candidate in [&edited, &reopened] {
            for (resolution, labels) in [
                (stemma::Resolution::AcceptAll, ["Beta", "Gamma", "Alpha"]),
                (stemma::Resolution::RejectAll, ["Alpha", "Beta", "Gamma"]),
            ] {
                let resolved = candidate.project(resolution).unwrap();
                let snapshot = resolved.snapshot();
                let count = if kind == "footnote" {
                    snapshot.canonical.footnotes.len()
                } else {
                    snapshot.canonical.endnotes.len()
                };
                assert_eq!(count, 3, "resolved move retains exactly three note bodies");
                let terminal = resolved
                    .serialize(&stemma::ExportOptions::default())
                    .unwrap();
                assert_validator_clean("row-note-terminal", &terminal);
                let main = document_xml(&terminal);
                let mut terminal_ids = Vec::new();
                physical_ids(
                    &xmltree::Element::parse(main.as_bytes()).unwrap(),
                    &mut terminal_ids,
                );
                assert_eq!(terminal_ids.len(), 6);
                assert_eq!(terminal_ids.iter().collect::<HashSet<_>>().len(), 6);
                if labels[0] == "Alpha" {
                    let mut source_ids = Vec::new();
                    physical_ids(
                        &xmltree::Element::parse(document_xml(&source).as_bytes()).unwrap(),
                        &mut source_ids,
                    );
                    assert_eq!(
                        terminal_ids, source_ids,
                        "Reject restores original physical identities"
                    );
                }
                let positions = labels.map(|label| main.find(&format!(">{label}<")).unwrap());
                assert!(positions[0] < positions[1] && positions[1] < positions[2]);
                let notes = extract_part(&terminal, part);
                for label in ["Alpha note", "Beta note", "Gamma note"] {
                    assert_eq!(notes.matches(label).count(), 1);
                }
                assert_eq!(notes.matches(r#"w:val="345678""#).count(), 3);
                assert_eq!(notes.matches(r#"w:after="140""#).count(), 3);
                assert_eq!(notes.matches(r#"w:rsidR="00112233""#).count(), 3);
                assert_eq!(notes.matches(r#"w:rsidRPr="00445566""#).count(), 3);
                assert_eq!(
                    notes.matches(r#"w:type="normal""#).count(),
                    3,
                    "cloned root envelope preserved"
                );
            }
        }
    }
}

#[test]
fn sentinel_resolution_removes_discarded_note_definitions() {
    // Reference deletion owns the backing resource lifetime. Native Word
    // removes a normal note when its final tracked reference is discarded;
    // the surviving note's body and an unrelated live note must stay intact.
    for (kind, part, content_type, rel_type) in [
        (
            "footnote",
            "word/footnotes.xml",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes",
        ),
        (
            "endnote",
            "word/endnotes.xml",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.endnotes+xml",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/endnotes",
        ),
    ] {
        let body = format!(
            r#"<w:p><w:del w:id="1" w:author="Editor"><w:r><w:{kind}Reference w:id="1"/></w:r></w:del><w:ins w:id="2" w:author="Editor"><w:r><w:{kind}Reference w:id="2"/></w:r></w:ins><w:r><w:t>Unrelated</w:t><w:{kind}Reference w:id="3"/></w:r></w:p>"#
        );
        let notes = format!(
            r#"<w:{kind}s xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:{kind} w:id="1"><w:p><w:r><w:{kind}Ref/><w:t>Original note</w:t></w:r></w:p></w:{kind}><w:{kind} w:id="2"><w:p><w:r><w:{kind}Ref/><w:t>Relocated note</w:t></w:r></w:p></w:{kind}><w:{kind} w:id="3"><w:p><w:r><w:{kind}Ref/><w:t>Untouched note</w:t></w:r></w:p></w:{kind}></w:{kind}s>"#
        );
        let bytes = pack_with_parts(
            &document(&body),
            &[ExtraPart {
                name: part,
                content_type,
                rel: ("notes", rel_type, part.strip_prefix("word/").unwrap()),
                xml: notes,
            }],
        );
        let original = stemma::api::Document::parse(&bytes).unwrap();
        for (wire_id, action, retained, discarded) in [
            (1, stemma::ResolveSelectionAction::Accept, "2", "1"),
            (2, stemma::ResolveSelectionAction::Reject, "1", "2"),
        ] {
            let identity = original
                .revisions()
                .iter()
                .find(|r| r.wire_id == wire_id)
                .unwrap()
                .revision_id;
            let partial = original
                .project(stemma::Resolution::Selective {
                    ids: [identity].into(),
                    action,
                })
                .unwrap();
            assert_eq!(
                partial.revisions().len(),
                1,
                "the other proposal remains pending"
            );
            let snapshot = partial.snapshot();
            let ids: Vec<_> = if kind == "footnote" {
                snapshot
                    .canonical
                    .footnotes
                    .iter()
                    .map(|s| s.id.as_str())
                    .collect()
            } else {
                snapshot
                    .canonical
                    .endnotes
                    .iter()
                    .map(|s| s.id.as_str())
                    .collect()
            };
            assert!(ids.contains(&retained) && ids.contains(&"3") && !ids.contains(&discarded));
            let bytes = partial
                .serialize(&stemma::ExportOptions::default())
                .unwrap();
            assert_validator_clean("selective-note-resources", &bytes);
            assert_eq!(
                stemma::api::Document::parse(&bytes)
                    .unwrap()
                    .revisions()
                    .len(),
                1
            );
        }
        for (resolution, retained, discarded) in [
            (stemma::Resolution::AcceptAll, "2", "1"),
            (stemma::Resolution::RejectAll, "1", "2"),
            (
                stemma::Resolution::Selective {
                    ids: original.revisions().iter().map(|r| r.revision_id).collect(),
                    action: stemma::ResolveSelectionAction::Accept,
                },
                "2",
                "1",
            ),
            (
                stemma::Resolution::Selective {
                    ids: original.revisions().iter().map(|r| r.revision_id).collect(),
                    action: stemma::ResolveSelectionAction::Reject,
                },
                "1",
                "2",
            ),
        ] {
            let projected = original.project(resolution).unwrap();
            let snapshot = projected.snapshot();
            let ids: Vec<_> = if kind == "footnote" {
                snapshot
                    .canonical
                    .footnotes
                    .iter()
                    .map(|s| s.id.as_str())
                    .collect()
            } else {
                snapshot
                    .canonical
                    .endnotes
                    .iter()
                    .map(|s| s.id.as_str())
                    .collect()
            };
            assert!(ids.contains(&retained) && ids.contains(&"3"));
            assert!(
                !ids.contains(&discarded),
                "{kind}: discarded reference left its body behind: {ids:?}"
            );
            let output = projected
                .serialize(&stemma::ExportOptions::default())
                .unwrap();
            assert_validator_clean("resolved-note-resources", &output);
            let xml = extract_part(&output, part);
            assert!(xml.contains("Untouched note"));
            assert_eq!(xml.contains("Original note"), retained == "1");
            assert_eq!(xml.contains("Relocated note"), retained == "2");
            assert!(
                stemma::api::Document::parse(&output)
                    .unwrap()
                    .revisions()
                    .is_empty()
            );
        }
    }
}

#[test]
fn sentinel_explicit_underline_off_survives_history_and_reopen_once() {
    // §17.3.2.40: none is a direct override, including the previous state
    // retained by rPrChange. One source property must remain one property.
    let source = pack(&document(
        r#"<w:p><w:pPr><w:rPr><w:b/><w:u w:val="none"/><w:rPrChange w:id="1" w:author="Editor"><w:rPr><w:i/><w:u w:val="none"/></w:rPr></w:rPrChange></w:rPr></w:pPr><w:r><w:rPr><w:b/><w:u w:val="none"/><w:rPrChange w:id="2" w:author="Editor"><w:rPr><w:i/><w:u w:val="none"/></w:rPr></w:rPrChange></w:rPr><w:t>Sample</w:t></w:r></w:p>"#,
    ));
    let original = stemma::api::Document::parse(&source).unwrap();
    let bytes = original
        .serialize(&stemma::ExportOptions::default())
        .unwrap();
    assert_validator_clean("underline-off-pending", &bytes);
    let xml = document_xml(&bytes);
    assert_eq!(xml.matches(r#"<w:u w:val="none""#).count(), 4, "{xml}");
    let reopened = stemma::api::Document::parse(&bytes).unwrap();
    for candidate in [&original, &reopened] {
        for resolution in [stemma::Resolution::AcceptAll, stemma::Resolution::RejectAll] {
            let terminal = candidate.project(resolution).unwrap();
            assert!(terminal.revisions().is_empty());
            let bytes = terminal
                .serialize(&stemma::ExportOptions::default())
                .unwrap();
            assert_validator_clean("underline-off-terminal", &bytes);
            let xml = document_xml(&bytes);
            assert_eq!(xml.matches(r#"<w:u w:val="none""#).count(), 2, "{xml}");
            let reparsed = stemma::api::Document::parse(&bytes).unwrap();
            assert!(reparsed.revisions().is_empty());
        }
    }
}

#[test]
fn sentinel_move_row_preserves_field_and_properties_after_reopen() {
    let source = pack(&document(
        r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="2400"/></w:tblGrid><w:tr><w:trPr><w:trHeight w:val="400" w:hRule="atLeast"/></w:trPr><w:tc><w:tcPr><w:tcW w:w="2400" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>First</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:trPr><w:trHeight w:val="600" w:hRule="exact"/><w:cantSplit/></w:trPr><w:tc><w:tcPr><w:tcW w:w="2400" w:type="dxa"/><w:noWrap/><w:vAlign w:val="center"/></w:tcPr><w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Second</w:t></w:r><w:fldSimple w:instr=" DATE "><w:r><w:t>2026</w:t></w:r></w:fldSimple></w:p></w:tc></w:tr></w:tbl>"#,
    ));
    let original = stemma::api::Document::parse(&source).unwrap();
    let BlockNode::Table(base) = &original.snapshot().canonical.blocks[0].block else {
        panic!("table")
    };
    let rows = base.rows.clone();
    let json = format!(
        r#"{{"ops":[{{"op":"table_op","target":"{}","table_op":{{"kind":"move_row","row_index":1,"ref_row":0,"position":"before"}}}}],"revision":{{"author":"Reviewer"}}}}"#,
        base.id.0
    );
    let transaction = stemma::edit_v4::parse_transaction(&json)
        .unwrap()
        .into_edit_transaction()
        .unwrap();
    let edited = original.apply(&transaction).unwrap();
    let bytes = edited.serialize(&stemma::ExportOptions::default()).unwrap();
    assert_validator_clean("move-row-pending", &bytes);
    let reopened = stemma::api::Document::parse(&bytes).unwrap();
    assert!(!reopened.revisions().is_empty());
    for candidate in [&edited, &reopened] {
        for (resolution, order) in [
            (stemma::Resolution::AcceptAll, [1, 0]),
            (stemma::Resolution::RejectAll, [0, 1]),
        ] {
            let resolved = candidate.project(resolution).unwrap();
            assert!(resolved.revisions().is_empty());
            let BlockNode::Table(table) = &resolved.snapshot().canonical.blocks[0].block else {
                panic!("table")
            };
            assert_eq!(table.rows.len(), 2);
            for (actual, index) in table.rows.iter().zip(order) {
                assert_eq!(actual.height, rows[index].height);
                assert_eq!(actual.height_rule, rows[index].height_rule);
                assert_eq!(actual.cant_split, rows[index].cant_split);
                assert_eq!(actual.cells[0].formatting, rows[index].cells[0].formatting);
            }
            let terminal = resolved
                .serialize(&stemma::ExportOptions::default())
                .unwrap();
            assert_validator_clean("move-row-terminal", &terminal);
            let xml = document_xml(&terminal);
            // Row deletion tracking may expand fldSimple into the equivalent
            // begin/instruction/separate/result/end carrier. Check the parsed
            // instruction/result, not one physical field spelling.
            let roundtrip = stemma::api::Document::parse(&terminal).unwrap();
            let BlockNode::Table(table) = &roundtrip.snapshot().canonical.blocks[0].block else {
                panic!("table")
            };
            let fields: Vec<_> = table
                .rows
                .iter()
                .flat_map(|r| &r.cells)
                .flat_map(|c| &c.blocks)
                .filter_map(|b| match b {
                    BlockNode::Paragraph(p) => Some(p),
                    _ => None,
                })
                .flat_map(|p| &p.segments)
                .flat_map(|s| &s.inlines)
                .filter_map(|inline| match inline {
                    InlineNode::OpaqueInline(o) => match &o.kind {
                        stemma::domain::OpaqueKind::Field(field) => Some(field),
                        _ => None,
                    },
                    _ => None,
                })
                .collect();
            use stemma::domain::FieldKind;
            let instruction = match fields.as_slice() {
                [simple] if simple.field_kind == FieldKind::Simple => {
                    assert_eq!(simple.result_text.as_deref(), Some("2026"));
                    simple.instruction_text.as_deref().unwrap()
                }
                [begin, instruction, separate, end] => {
                    assert_eq!(begin.field_kind, FieldKind::Begin);
                    assert_eq!(instruction.field_kind, FieldKind::Instruction);
                    assert_eq!(separate.field_kind, FieldKind::Separate);
                    assert_eq!(end.field_kind, FieldKind::End);
                    let result_runs = table
                        .rows
                        .iter()
                        .flat_map(|r| &r.cells)
                        .flat_map(|c| &c.blocks)
                        .filter_map(|b| match b {
                            BlockNode::Paragraph(p) => Some(p),
                            _ => None,
                        })
                        .flat_map(|p| &p.segments)
                        .flat_map(|s| &s.inlines)
                        .filter(|inline| matches!(inline, InlineNode::Text(t) if t.text == "2026"))
                        .count();
                    assert_eq!(result_runs, 1);
                    instruction.instruction_text.as_deref().unwrap()
                }
                _ => panic!("expected exactly one complete field, got {fields:?}"),
            };
            assert_eq!(instruction.trim(), "DATE");
            let first = xml.find(">First<").unwrap();
            let second = xml.find(">Second<").unwrap();
            assert_eq!(second < first, order == [1, 0]);
            assert!(xml.contains("<w:b"), "bold text survives");
        }
    }
}

#[test]
fn sentinel_complete_row_permutation_survives_docx_reopen_and_resolution() {
    let mut rows_xml = String::new();
    for index in 0..4 {
        let field = if index == 2 {
            r#"<w:fldSimple w:instr=" DATE "><w:r><w:t>2026</w:t></w:r></w:fldSimple>"#
        } else {
            ""
        };
        rows_xml.push_str(&format!(
            r#"<w:tr><w:trPr><w:trHeight w:val="{}" w:hRule="atLeast"/><w:cantSplit w:val="{}"/></w:trPr><w:tc><w:tcPr><w:tcW w:w="2400" w:type="dxa"/><w:noWrap w:val="{}"/></w:tcPr><w:p><w:r><w:rPr><w:b w:val="{}"/></w:rPr><w:t>Row {index}</w:t></w:r>{field}</w:p></w:tc></w:tr>"#,
            300 + index * 71, index % 2, index % 2, index % 2
        ));
    }
    let source = pack(&document(&format!(
        r#"<w:p><w:r><w:t>Unchanged before</w:t></w:r></w:p><w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="2400"/></w:tblGrid>{rows_xml}</w:tbl><w:p><w:r><w:t>Unchanged after</w:t></w:r></w:p>"#
    )));
    let original = stemma::api::Document::parse(&source).unwrap();
    let BlockNode::Table(base) = &original.snapshot().canonical.blocks[1].block else {
        panic!("table witness")
    };
    let source_rows = base.rows.clone();
    let json = format!(
        r#"{{"ops":[{{"op":"table_op","target":"{}","table_op":{{"kind":"reorder_rows","row_order":[3,1,0,2]}}}}],"revision":{{"author":"Reviewer"}}}}"#,
        base.id.0
    );
    let transaction = stemma::edit_v4::parse_transaction(&json)
        .unwrap()
        .into_edit_transaction()
        .unwrap();
    let edited = original.apply(&transaction).unwrap();
    let bytes = edited.serialize(&stemma::ExportOptions::default()).unwrap();
    assert_validator_clean("row-permutation-pending", &bytes);
    let reopened = stemma::api::Document::parse(&bytes).unwrap();
    assert!(!reopened.revisions().is_empty());
    let proposal_ids: std::collections::HashSet<_> = edited
        .revisions()
        .iter()
        .map(|record| record.revision_id)
        .collect();
    assert_eq!(proposal_ids.len(), 1, "One permutation is one intention");
    assert_eq!(
        reopened
            .revisions()
            .iter()
            .map(|record| record.revision_id)
            .collect::<std::collections::HashSet<_>>(),
        proposal_ids,
        "The proposal must remain addressable after serialization"
    );
    for candidate in [&edited, &reopened] {
        for (resolution, order) in [
            (stemma::Resolution::AcceptAll, [3, 1, 0, 2]),
            (stemma::Resolution::RejectAll, [0, 1, 2, 3]),
            (
                stemma::Resolution::Selective {
                    ids: proposal_ids.clone(),
                    action: stemma::ResolveSelectionAction::Accept,
                },
                [3, 1, 0, 2],
            ),
            (
                stemma::Resolution::Selective {
                    ids: proposal_ids.clone(),
                    action: stemma::ResolveSelectionAction::Reject,
                },
                [0, 1, 2, 3],
            ),
        ] {
            let terminal = candidate.project(resolution).unwrap();
            assert!(terminal.revisions().is_empty());
            let bytes = terminal
                .serialize(&stemma::ExportOptions::default())
                .unwrap();
            assert_validator_clean("row-permutation-terminal", &bytes);
            let roundtrip = stemma::api::Document::parse(&bytes).unwrap();
            assert!(roundtrip.revisions().is_empty());
            let BlockNode::Table(table) = &roundtrip.snapshot().canonical.blocks[1].block else {
                panic!("terminal table")
            };
            let BlockNode::Table(before_save) = &terminal.snapshot().canonical.blocks[1].block
            else {
                panic!("projected table")
            };
            assert_eq!(
                before_save.structure_hash, table.structure_hash,
                "Resolution must invalidate the prior physical-grid digest before save"
            );
            assert_eq!(table.rows.len(), 4);
            let mut field_instructions = Vec::new();
            for (row, index) in table.rows.iter().zip(order) {
                assert_eq!(row.height, source_rows[index].height);
                assert_eq!(row.height_rule, source_rows[index].height_rule);
                assert_eq!(row.cant_split, source_rows[index].cant_split);
                assert_eq!(
                    row.cells[0].formatting,
                    source_rows[index].cells[0].formatting
                );
                let BlockNode::Paragraph(paragraph) = &row.cells[0].blocks[0] else {
                    panic!("row paragraph")
                };
                let inlines: Vec<_> = paragraph.segments.iter().flat_map(|s| &s.inlines).collect();
                let label = inlines
                    .iter()
                    .find_map(|inline| match inline {
                        InlineNode::Text(t) if t.text.starts_with("Row ") => Some(t),
                        _ => None,
                    })
                    .unwrap();
                assert_eq!(label.text, format!("Row {index}"));
                assert_eq!(label.marks.contains(&Mark::Bold), index % 2 == 1);
                for inline in inlines {
                    if let InlineNode::OpaqueInline(opaque) = inline
                        && let stemma::domain::OpaqueKind::Field(field) = &opaque.kind
                        && let Some(instruction) = &field.instruction_text
                    {
                        assert_eq!(
                            index, 2,
                            "Field must stay attached to its complete source row"
                        );
                        field_instructions.push(instruction.trim().to_owned());
                    }
                }
            }
            assert_eq!(field_instructions, ["DATE"]);
            let xml = document_xml(&bytes);
            assert_eq!(
                xml.matches("2026").count(),
                1,
                "Field result must survive exactly once"
            );
            assert!(xml.contains("Unchanged before") && xml.contains("Unchanged after"));
        }
    }
}

#[test]
fn sentinel_inserted_row_retains_reference_presentation() {
    super::table_ops_formatted::check_inserted_row_presentation();
    let source = pack(&document(
        r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="2400"/></w:tblGrid><w:tr><w:trPr><w:cantSplit/><w:trHeight w:val="480" w:hRule="atLeast"/><w:tblHeader/></w:trPr><w:tc><w:tcPr><w:tcW w:w="2400" w:type="dxa"/><w:noWrap w:val="0"/><w:vAlign w:val="center"/></w:tcPr><w:p><w:r><w:t>Reference</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
    ));
    let original = stemma::api::Document::parse(&source).unwrap();
    let BlockNode::Table(table) = &original.snapshot().canonical.blocks[0].block else {
        panic!("table fixture");
    };
    let reference = table.rows[0].clone();
    let json = format!(
        r#"{{"ops":[{{"op":"table_op","target":"{}","table_op":{{"kind":"insert_row","ref_row":0,"position":"after","cells":["Added"]}}}}],"revision":{{"author":"Reviewer"}}}}"#,
        table.id.0
    );
    let transaction = stemma::edit_v4::parse_transaction(&json)
        .unwrap()
        .into_edit_transaction()
        .unwrap();
    let edited = original.apply(&transaction).unwrap();
    let bytes = edited.serialize(&stemma::ExportOptions::default()).unwrap();
    assert_validator_clean("inserted-row-pending", &bytes);
    let reopened = stemma::api::Document::parse(&bytes).unwrap();
    for candidate in [&edited, &reopened] {
        for (resolution, expected_rows) in [
            (stemma::Resolution::AcceptAll, 2),
            (stemma::Resolution::RejectAll, 1),
        ] {
            let resolved = candidate.project(resolution).unwrap();
            let BlockNode::Table(table) = &resolved.snapshot().canonical.blocks[0].block else {
                panic!("resolved table");
            };
            assert_eq!(table.rows.len(), expected_rows);
            for row in &table.rows {
                assert_eq!(row.height, reference.height);
                assert_eq!(row.height_rule, reference.height_rule);
                assert_eq!(row.cant_split, reference.cant_split);
                assert!(row.is_header);
                assert_eq!(row.cells[0].formatting, reference.cells[0].formatting);
            }
            assert_validator_clean(
                "inserted-row-terminal",
                &resolved
                    .serialize(&stemma::ExportOptions::default())
                    .unwrap(),
            );
        }
    }
}

#[test]
fn sentinel_story_row_retains_editing_session_provenance() {
    super::spec_story_sdt_preservation::check_story_row_session_attributes();
}

#[test]
fn sentinel_proofing_cleanup_retains_distinct_run_provenance() {
    super::spec_mixed_move_resolution::check_proofing_run_session_provenance();
}

#[test]
fn sentinel_row_revision_retains_complete_previous_properties() {
    // ECMA-376 17.13.5.37: trPrChange holds the COMPLETE previous row
    // property set. It is not a delta limited to the properties we can author.
    let source = pack(&document(
        r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="1800"/><w:gridCol w:w="2200"/></w:tblGrid><w:tr><w:trPr><w:trHeight w:val="700" w:hRule="atLeast"/><w:trPrChange w:id="17" w:author="Reviewer"><w:trPr><w:gridBefore w:val="1"/><w:wBefore w:w="1800" w:type="dxa"/><w:cantSplit/><w:trHeight w:val="400" w:hRule="exact"/><w:tblHeader/><w:jc w:val="right"/><w:hidden/></w:trPr></w:trPrChange></w:trPr><w:tc><w:tcPr><w:tcW w:w="2200" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>Entry</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
    ));
    let original = stemma::api::Document::parse(&source).unwrap();
    let bytes = original
        .serialize(&stemma::ExportOptions::default())
        .unwrap();
    let xml = document_xml(&bytes);
    assert!(
        xml.contains("<w:cantSplit"),
        "previous pagination property was discarded"
    );
    assert!(
        xml.contains("<w:tblHeader"),
        "previous header-row property was discarded"
    );
    assert!(
        xml.contains("<w:hidden"),
        "previous property remainder was discarded"
    );
    let reopened = stemma::api::Document::parse(&bytes).unwrap();
    for document in [&original, &reopened] {
        let rejected = document.project(stemma::Resolution::RejectAll).unwrap();
        let BlockNode::Table(table) = &rejected.snapshot().canonical.blocks[0].block else {
            panic!()
        };
        let row = &table.rows[0];
        assert!(row.cant_split == Some(true) && row.is_header);
        assert_eq!(row.height, Some(400));
        assert_eq!(row.height_rule, Some(stemma::domain::HeightRule::Exact));
        assert_eq!(row.grid_before, 1);
        assert_eq!(row.jc, Some(stemma::domain::Alignment::Right));
        assert_eq!(row.w_before.as_ref().unwrap().w, 1800);
        assert_eq!(row.preserved.len(), 1);
        assert_validator_clean(
            "previous-row-reject",
            &rejected
                .serialize(&stemma::ExportOptions::default())
                .unwrap(),
        );
        let accepted = document.project(stemma::Resolution::AcceptAll).unwrap();
        let BlockNode::Table(table) = &accepted.snapshot().canonical.blocks[0].block else {
            panic!()
        };
        assert!(table.rows[0].cant_split.is_none() && !table.rows[0].is_header);
        assert_eq!(table.rows[0].height, Some(700));
    }
}

#[test]
fn sentinel_cell_revision_restores_previous_structure() {
    // ECMA-376 17.13.5.36: tcPrChange contains the complete old tcPr,
    // including structural properties, not only visible formatting.
    let source = pack(&document(
        r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="1800"/><w:gridCol w:w="2200"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="1800" w:type="dxa"/><w:tcPrChange w:id="18" w:author="Reviewer"><w:tcPr><w:tcW w:w="4000" w:type="dxa"/><w:gridSpan w:val="2"/><w:vMerge w:val="restart"/><w:hideMark/></w:tcPr></w:tcPrChange></w:tcPr><w:p><w:r><w:t>Entry</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
    ));
    let original = stemma::api::Document::parse(&source).unwrap();
    let bytes = original
        .serialize(&stemma::ExportOptions::default())
        .unwrap();
    let reopened = stemma::api::Document::parse(&bytes).unwrap();
    for document in [&original, &reopened] {
        let rejected = document.project(stemma::Resolution::RejectAll).unwrap();
        let BlockNode::Table(table) = &rejected.snapshot().canonical.blocks[0].block else {
            panic!()
        };
        let cell = &table.rows[0].cells[0];
        assert_eq!(
            cell.grid_span, 2,
            "Reject must restore the old occupied columns"
        );
        assert_eq!(cell.v_merge, stemma::domain::VerticalMerge::Restart);
        assert!(cell.hide_mark);
        assert_eq!(cell.formatting.width.as_ref().unwrap().w, 4000);
        assert_validator_clean(
            "previous-cell-reject",
            &rejected
                .serialize(&stemma::ExportOptions::default())
                .unwrap(),
        );
        let accepted = document.project(stemma::Resolution::AcceptAll).unwrap();
        let BlockNode::Table(table) = &accepted.snapshot().canonical.blocks[0].block else {
            panic!()
        };
        let cell = &table.rows[0].cells[0];
        assert_eq!(cell.grid_span, 1);
        assert_eq!(cell.v_merge, stemma::domain::VerticalMerge::None);
        assert!(!cell.hide_mark);
    }
}

#[test]
fn sentinel_cell_revision_reject_restores_authored_property_provenance() {
    // Reject restores the old authored tcPr, not the current cell's border
    // emission cache or direct-shading presence (ECMA-376 17.13.5.36).
    let source = pack(&document(
        r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcBorders><w:top w:val="single" w:sz="8" w:color="FF0000"/></w:tcBorders><w:tcPrChange w:id="19" w:author="Reviewer"><w:tcPr><w:tcBorders><w:top w:val="single" w:sz="8" w:color="00FF00"/></w:tcBorders><w:shd w:val="clear" w:fill="00FFFF"/></w:tcPr></w:tcPrChange></w:tcPr><w:p><w:r><w:t>Entry</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
    ));
    let original = stemma::api::Document::parse(&source).unwrap();
    let bytes = original
        .serialize(&stemma::ExportOptions::default())
        .unwrap();
    let reopened = stemma::api::Document::parse(&bytes).unwrap();
    for document in [&original, &reopened] {
        let rejected = document.project(stemma::Resolution::RejectAll).unwrap();
        let bytes = rejected
            .serialize(&stemma::ExportOptions::default())
            .unwrap();
        assert_validator_clean("previous-cell-provenance-reject", &bytes);
        let xml = document_xml(&bytes);
        assert!(
            xml.contains("00FF00"),
            "Reject did not emit the previous authored border: {xml}"
        );
        assert!(
            !xml.contains("FF0000"),
            "current authored border leaked into Reject"
        );
        assert!(
            xml.contains("00FFFF"),
            "previous direct shading was suppressed"
        );
    }
}

#[test]
fn sentinel_standalone_grid_history_does_not_create_a_revision() {
    // Native Word does not expose standalone tblGridChange as a user revision.
    // Without property owners, terminal resolution keeps current geometry.
    let source = pack(&document(
        r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="1548"/><w:gridCol w:w="8028"/><w:tblGridChange w:id="21"><w:tblGrid><w:gridCol w:w="4788"/><w:gridCol w:w="4788"/></w:tblGrid></w:tblGridChange></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>First</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Second</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
    ));
    let original = stemma::api::Document::parse(&source).unwrap();
    assert!(original.revisions().is_empty());
    let bytes = original
        .serialize(&stemma::ExportOptions::default())
        .unwrap();
    let reopened = stemma::api::Document::parse(&bytes).unwrap();
    for document in [&original, &reopened] {
        let rejected = document.project(stemma::Resolution::RejectAll).unwrap();
        let BlockNode::Table(table) = &rejected.snapshot().canonical.blocks[0].block else {
            panic!()
        };
        assert_eq!(table.formatting.grid_cols, [1548, 8028]);
        assert_validator_clean(
            "previous-grid-reject",
            &rejected
                .serialize(&stemma::ExportOptions::default())
                .unwrap(),
        );
        let accepted = document.project(stemma::Resolution::AcceptAll).unwrap();
        let BlockNode::Table(table) = &accepted.snapshot().canonical.blocks[0].block else {
            panic!()
        };
        assert_eq!(table.formatting.grid_cols, [1548, 8028]);
    }
}

#[test]
fn sentinel_table_geometry_history_survives_unrelated_edit() {
    // The old grid accompanies the prior cell properties (17.13.5.33/.36).
    // It is auxiliary state of the table change, not another user revision.
    let source = pack(&document(
        r#"<w:p><w:ins w:id="22" w:author="Reviewer"><w:r><w:t>Added</w:t></w:r></w:ins></w:p><w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="3000"/><w:tblGridChange w:id="21"><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid></w:tblGridChange></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="3000" w:type="dxa"/><w:tcPrChange w:id="23" w:author="Reviewer"><w:tcPr><w:tcW w:w="4000" w:type="dxa"/></w:tcPr></w:tcPrChange></w:tcPr><w:p><w:r><w:t>Entry</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
    ));
    let original = stemma::api::Document::parse(&source).unwrap();
    assert_eq!(original.revisions().len(), 2);
    let insertion = original
        .revisions()
        .into_iter()
        .find(|r| r.wire_id == 22)
        .unwrap();
    let resolved = original
        .project(stemma::Resolution::Selective {
            ids: [insertion.revision_id].into_iter().collect(),
            action: stemma::tracked_model::ResolveSelectionAction::Accept,
        })
        .unwrap();
    let bytes = resolved
        .serialize(&stemma::ExportOptions::default())
        .unwrap();
    assert!(document_xml(&bytes).contains("tblGridChange"));
    let reopened = stemma::api::Document::parse(&bytes).unwrap();
    assert_eq!(reopened.revisions().len(), 1);
    // The archive kernel and the typed facade must obey the same terminal
    // law. In particular, raw Reject must not combine current grid widths
    // with previous cell widths, or leave pending auxiliary markup behind.
    let archive = stemma::docx::DocxArchive::read(&bytes).unwrap();
    for (terminal, expected) in [
        (stemma::normalize::normalize_docx(&archive).unwrap().0, 3000),
        (
            stemma::normalize::reject_all_docx(&archive).unwrap().0,
            4000,
        ),
    ] {
        let xml = terminal.get("word/document.xml").unwrap();
        assert!(!std::str::from_utf8(xml).unwrap().contains("tblGridChange"));
        let root = xmltree::Element::parse(xml).unwrap();
        let table = root.get_child("body").unwrap().get_child("tbl").unwrap();
        let grid = table
            .get_child("tblGrid")
            .unwrap()
            .get_child("gridCol")
            .unwrap();
        let cell = table
            .get_child("tr")
            .unwrap()
            .get_child("tc")
            .unwrap()
            .get_child("tcPr")
            .unwrap()
            .get_child("tcW")
            .unwrap();
        for element in [grid, cell] {
            assert_eq!(
                element
                    .attributes
                    .iter()
                    .find(|(key, _)| key.local_name == "w")
                    .unwrap()
                    .1,
                &expected.to_string()
            );
        }
    }
    for document in [&resolved, &reopened] {
        let selection = document
            .project(stemma::Resolution::Selective {
                ids: document
                    .revisions()
                    .iter()
                    .map(|revision| revision.revision_id)
                    .collect(),
                action: stemma::tracked_model::ResolveSelectionAction::Reject,
            })
            .unwrap();
        let BlockNode::Table(selected_table) = &selection.snapshot().canonical.blocks[1].block
        else {
            panic!()
        };
        assert_eq!(
            selected_table.formatting.grid_cols,
            [4000],
            "selective Reject of the owning property must resolve its auxiliary grid"
        );
        assert!(
            !document_xml(
                &selection
                    .serialize(&stemma::ExportOptions::default())
                    .unwrap()
            )
            .contains("tblGridChange")
        );
        let rejected = document.project(stemma::Resolution::RejectAll).unwrap();
        let BlockNode::Table(table) = &rejected.snapshot().canonical.blocks[1].block else {
            panic!()
        };
        assert_eq!(table.formatting.grid_cols, [4000]);
        assert_eq!(
            table.rows[0].cells[0].formatting.width.as_ref().unwrap().w,
            4000
        );
        let bytes = rejected
            .serialize(&stemma::ExportOptions::default())
            .unwrap();
        assert!(!document_xml(&bytes).contains("tblGridChange"));
        assert_validator_clean("coupled-grid-history-reject", &bytes);
    }
}

#[test]
fn sentinel_footer_bookmark_keeps_table_boundaries() {
    // ISO29500-1 17.13.6: a bookmark denotes the region between its markers,
    // including cross-structure content. Re-anchoring both halves after a
    // table preserves pairing but destroys the bookmarked region.
    let footer = ExtraPart {
        name: "word/footer1.xml",
        content_type: "application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml",
        rel: ("rIdFooter", "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer", "footer1.xml"),
        xml: r#"<w:ftr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:r><w:t>Before</w:t></w:r></w:p><w:bookmarkStart w:id="7" w:name="TableRegion"/><w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="2400"/></w:tblGrid><w:tr><w:tc><w:tcPr/><w:p><w:r><w:t>Inside</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:bookmarkEnd w:id="7"/><w:p><w:r><w:t>After</w:t></w:r></w:p></w:ftr>"#.to_owned(),
    };
    let source = pack_with_parts(
        &document(
            r#"<w:p><w:r><w:t>Body</w:t></w:r></w:p><w:sectPr><w:footerReference w:type="default" r:id="rIdFooter"/></w:sectPr>"#,
        ),
        &[footer],
    );
    let document = stemma::api::Document::parse(&source).unwrap();
    for resolution in [
        None,
        Some(stemma::Resolution::AcceptAll),
        Some(stemma::Resolution::RejectAll),
    ] {
        let projected = match resolution {
            Some(resolution) => document.project(resolution).unwrap(),
            None => stemma::api::Document::parse(&source).unwrap(),
        };
        let output = projected
            .serialize(&stemma::ExportOptions::default())
            .unwrap();
        let archive = stemma::docx::DocxArchive::read(&output).unwrap();
        let root = xmltree::Element::parse(archive.get("word/footer1.xml").unwrap()).unwrap();
        let children: Vec<_> = root
            .children
            .iter()
            .filter_map(|child| child.as_element())
            .map(|child| child.name.as_str())
            .collect();
        assert_eq!(
            children,
            ["p", "bookmarkStart", "tbl", "bookmarkEnd", "p"],
            "a root range must not move into a paragraph"
        );
        assert_validator_clean("footer-table-bookmark", &output);
    }
}

#[test]
fn sentinel_table_bookmark_keeps_table_level_endpoint() {
    // ISO29500-1 17.13.6 permits a table-level bookmark endpoint. Moving
    // it inside the final paragraph is not a structural identity: Word can
    // interpret the original as a table-row range including cell terminators.
    // Include a real pending edit so Accept/Reject are not no-op projections.
    let source = pack(&document(concat!(
        r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="2400"/><w:gridCol w:w="2400"/></w:tblGrid>"#,
        r#"<w:tr><w:tc><w:p><w:r><w:t>First</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Row</w:t></w:r></w:p></w:tc></w:tr>"#,
        r#"<w:tr><w:tc><w:p><w:r><w:t>Second</w:t></w:r><w:ins w:id="31" w:author="Reviewer"><w:r><w:t xml:space="preserve"> pending</w:t></w:r></w:ins></w:p></w:tc>"#,
        r#"<w:tc><w:p><w:r><w:t>Row</w:t></w:r><w:bookmarkStart w:id="17" w:name="RowRegion"/></w:p></w:tc></w:tr><w:bookmarkEnd w:id="17"/></w:tbl>"#,
        r#"<w:p><w:r><w:t>After table</w:t></w:r></w:p>"#,
    )));
    for resolution in [
        None,
        Some(stemma::Resolution::AcceptAll),
        Some(stemma::Resolution::RejectAll),
    ] {
        let rejecting = matches!(&resolution, Some(stemma::Resolution::RejectAll));
        let input = stemma::api::Document::parse(&source).unwrap();
        assert!(!input.revisions().is_empty());
        let candidate = match resolution {
            Some(resolution) => input.project(resolution).unwrap(),
            None => input,
        };
        let output = candidate
            .serialize(&stemma::ExportOptions::default())
            .unwrap();
        let xml = document_xml(&output);
        let root = xmltree::Element::parse(xml.as_bytes()).unwrap();
        let table = root.get_child("body").unwrap().get_child("tbl").unwrap();
        let end = table
            .get_child("bookmarkEnd")
            .expect("table-level endpoint must remain a direct table child");
        assert_eq!(end.attributes.len(), 1);
        let (name, value) = end.attributes.first().unwrap();
        assert_eq!(name.local_name, "id");
        assert_eq!(
            name.namespace.as_deref(),
            Some("http://schemas.openxmlformats.org/wordprocessingml/2006/main")
        );
        assert_eq!(value, "17");
        assert_eq!(xml.contains(" pending"), !rejecting);
        assert_validator_clean("table-bookmark-endpoint", &output);
    }
}

#[test]
fn sentinel_table_boundary_repairs_a_removed_inline_partner() {
    let source = pack(&document(concat!(
        r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="2400"/></w:tblGrid><w:tr><w:tc><w:p>"#,
        r#"<w:ins w:id="31" w:author="Reviewer"><w:bookmarkStart w:id="17" w:name="Region"/><w:r><w:t>Added</w:t></w:r></w:ins>"#,
        r#"</w:p></w:tc></w:tr><w:bookmarkEnd w:id="17"/></w:tbl><w:p/>"#,
    )));
    let rejected = stemma::api::Document::parse(&source)
        .unwrap()
        .project(stemma::Resolution::RejectAll)
        .unwrap();
    let output = rejected
        .serialize(&stemma::ExportOptions::default())
        .unwrap();
    let xml = document_xml(&output);
    let root = xmltree::Element::parse(xml.as_bytes()).unwrap();
    let table = root.get_child("body").unwrap().get_child("tbl").unwrap();
    let tags: Vec<_> = table
        .children
        .iter()
        .filter_map(|node| node.as_element())
        .map(|node| node.name.as_str())
        .collect();
    // Rejecting removes the inserted text, not the start position
    // (ECMA-376 §17.13.6): the start stays at the front of the cell
    // paragraph, so the range still runs to the table-level end.
    assert_eq!(tags, ["tblPr", "tblGrid", "tr", "bookmarkEnd"]);
    let cell_paragraph = table
        .get_child("tr")
        .and_then(|row| row.get_child("tc"))
        .and_then(|cell| cell.get_child("p"))
        .unwrap();
    assert!(
        cell_paragraph.get_child("bookmarkStart").is_some(),
        "the start stays where the inserted text was: {xml}"
    );
    assert!(!xml.contains("Added"));
    assert_validator_clean("table-boundary-inline-partner", &output);
}

#[test]
fn sentinel_table_boundary_repairs_a_partner_removed_with_its_row() {
    let source = pack(&document(concat!(
        r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="2400"/></w:tblGrid>"#,
        r#"<w:tr><w:tc><w:p><w:r><w:t>Kept</w:t></w:r></w:p></w:tc></w:tr>"#,
        r#"<w:tr><w:trPr><w:ins w:id="32" w:author="Reviewer"/></w:trPr><w:tc><w:p>"#,
        r#"<w:ins w:id="31" w:author="Reviewer"><w:bookmarkStart w:id="17" w:name="Region"/><w:r><w:t>Added</w:t></w:r></w:ins>"#,
        r#"</w:p></w:tc></w:tr><w:bookmarkEnd w:id="17"/></w:tbl><w:p/>"#,
    )));
    let rejected = stemma::api::Document::parse(&source)
        .unwrap()
        .project(stemma::Resolution::RejectAll)
        .unwrap();
    let output = rejected
        .serialize(&stemma::ExportOptions::default())
        .unwrap();
    let xml = document_xml(&output);
    let root = xmltree::Element::parse(xml.as_bytes()).unwrap();
    let table = root.get_child("body").unwrap().get_child("tbl").unwrap();
    let tags: Vec<_> = table
        .children
        .iter()
        .filter_map(|node| node.as_element())
        .map(|node| node.name.as_str())
        .collect();
    // The inserted row went with the start; the removal point is the end of
    // the remaining rows, where the table-level end sits.
    assert!(
        tags.ends_with(&["tr", "bookmarkStart", "bookmarkEnd"]),
        "the torn pair must collapse at its surviving table boundary: {tags:?}"
    );
    assert!(!xml.contains("Added"));
    assert_validator_clean("table-boundary-row-partner", &output);
}

#[test]
fn sentinel_removed_table_restores_its_endpoint_beside_inline_start() {
    let source = pack(&document(concat!(
        r#"<w:p><w:r><w:t>Before</w:t></w:r><w:bookmarkStart w:id="17" w:name="Region"/></w:p>"#,
        r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="2400"/></w:tblGrid><w:tr><w:trPr><w:del w:id="31" w:author="Reviewer"/></w:trPr>"#,
        r#"<w:tc><w:p><w:r><w:t>Removed row</w:t></w:r></w:p></w:tc></w:tr><w:bookmarkEnd w:id="17"/></w:tbl><w:p/>"#,
    )));
    let accepted = stemma::api::Document::parse(&source)
        .unwrap()
        .project(stemma::Resolution::AcceptAll)
        .unwrap();
    let output = accepted
        .serialize(&stemma::ExportOptions::default())
        .unwrap();
    let xml = document_xml(&output);
    let root = xmltree::Element::parse(xml.as_bytes()).unwrap();
    let body = root.get_child("body").unwrap();
    assert!(body.get_child("tbl").is_none());
    let paragraph = body.get_child("p").unwrap();
    let tags: Vec<_> = paragraph
        .children
        .iter()
        .filter_map(|node| node.as_element())
        .map(|node| node.name.as_str())
        .collect();
    assert!(
        tags.ends_with(&["bookmarkStart", "bookmarkEnd"]),
        "the deleted table's endpoint must rejoin its surviving inline start: {tags:?}"
    );
    assert!(!xml.contains("Removed row"));
    assert_validator_clean("removed-table-range-endpoint", &output);
}

#[test]
fn sentinel_table_boundary_follows_removed_row_to_next_survivor() {
    let source = pack(&document(concat!(
        r#"<w:tbl><w:bookmarkStart w:id="18" w:name="Whole"/><w:tblPr/><w:tblGrid><w:gridCol w:w="2400"/></w:tblGrid>"#,
        r#"<w:tr><w:tc><w:p><w:r><w:t>First</w:t></w:r></w:p></w:tc></w:tr><w:bookmarkStart w:id="17" w:name="Tail"/>"#,
        r#"<w:tr><w:trPr><w:del w:id="31" w:author="Reviewer"/></w:trPr><w:tc><w:p><w:r><w:t>Removed</w:t></w:r></w:p></w:tc></w:tr>"#,
        r#"<w:tr><w:tc><w:p><w:r><w:t>Last</w:t></w:r></w:p></w:tc></w:tr><w:bookmarkEnd w:id="17"/><w:bookmarkEnd w:id="18"/></w:tbl><w:p/>"#,
    )));
    let accepted = stemma::api::Document::parse(&source)
        .unwrap()
        .project(stemma::Resolution::AcceptAll)
        .unwrap();
    let output = accepted
        .serialize(&stemma::ExportOptions::default())
        .unwrap();
    let xml = document_xml(&output);
    let root = xmltree::Element::parse(xml.as_bytes()).unwrap();
    let table = root.get_child("body").unwrap().get_child("tbl").unwrap();
    let tags: Vec<_> = table
        .children
        .iter()
        .filter_map(|node| node.as_element())
        .map(|node| node.name.as_str())
        .collect();
    assert_eq!(
        tags,
        [
            "bookmarkStart",
            "tblPr",
            "tblGrid",
            "tr",
            "bookmarkStart",
            "tr",
            "bookmarkEnd",
            "bookmarkEnd"
        ]
    );
    assert!(!xml.contains("Removed"));
    assert_validator_clean("removed-row-table-boundary", &output);
}

fn column_insertion_with_property_history() -> Vec<u8> {
    let inserted = r#"<w:tc><w:tcPr><w:tcW w:w="1200" w:type="dxa"/><w:cellIns w:id="33" w:author="Reviewer"/></w:tcPr><w:p><w:r><w:t>Added</w:t></w:r></w:p></w:tc>"#;
    let retained = r#"<w:tc><w:tcPr><w:tcW w:w="2800" w:type="dxa"/><w:shd w:fill="0000FF"/><w:tcPrChange w:id="23" w:author="Reviewer"><w:tcPr><w:tcW w:w="2800" w:type="dxa"/><w:shd w:fill="FF0000"/></w:tcPr></w:tcPrChange></w:tcPr><w:p><w:r><w:t>Retained</w:t></w:r></w:p></w:tc>"#;
    let widths = [1200, 2800];
    let cells = format!("{inserted}{retained}");
    pack(&document(&format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/><w:tblLayout w:type="fixed"/></w:tblPr><w:tblGrid><w:gridCol w:w="{}"/><w:gridCol w:w="{}"/><w:tblGridChange w:id="21"><w:tblGrid><w:gridCol w:w="2800"/></w:tblGrid></w:tblGridChange></w:tblGrid><w:tr>{cells}</w:tr></w:tbl>"#,
        widths[0], widths[1]
    )))
}

#[test]
fn sentinel_reject_column_does_not_remove_from_restored_previous_grid() {
    // A previous grid is already the pre-insertion coordinate frame
    // (17.13.5.33). Resolving cellIns must not remove its column a second time
    // from that restored frame. A leftmost insertion exposes the double
    // removal directly: the one surviving prior column would disappear.
    let source = column_insertion_with_property_history();
    let original = stemma::api::Document::parse(&source).unwrap();
    let rejected = original.project(stemma::Resolution::RejectAll).unwrap();
    let BlockNode::Table(table) = &rejected.snapshot().canonical.blocks[0].block else {
        panic!()
    };
    assert_eq!(
        table.formatting.grid_cols,
        [2800],
        "current column removal must not run against the prior coordinate frame"
    );
    assert_eq!(table.rows[0].cells.len(), 1);
    let bytes = rejected
        .serialize(&stemma::ExportOptions::default())
        .unwrap();
    assert_validator_clean("reject-column-with-prior-grid", &bytes);
    let reopened = stemma::api::Document::parse(&bytes).unwrap();
    let BlockNode::Table(table) = &reopened.snapshot().canonical.blocks[0].block else {
        panic!()
    };
    assert_eq!(table.formatting.grid_cols, [2800]);
    assert!(reopened.revisions().is_empty());
}

#[test]
fn sentinel_shared_grid_selection_includes_structural_carriers() {
    let source = column_insertion_with_property_history();
    let original = stemma::api::Document::parse(&source).unwrap();
    assert_eq!(original.revisions().len(), 2);
    // Resolving either half alone needs mixed-frame rebasing. A property-only
    // owner census must not restore a one-column grid under two pending cells.
    for revision in original.revisions() {
        for action in [
            stemma::tracked_model::ResolveSelectionAction::Accept,
            stemma::tracked_model::ResolveSelectionAction::Reject,
        ] {
            match original.project(stemma::Resolution::Selective {
                ids: [revision.revision_id].into_iter().collect(),
                action,
            }) {
                Ok(_) => panic!("partial structural/property grid selection returned success"),
                Err(error) => assert_eq!(error.code, stemma::runtime::ErrorCode::UnsupportedEdit),
            }
        }
    }
    let rejected = original
        .project(stemma::Resolution::Selective {
            ids: original
                .revisions()
                .iter()
                .map(|revision| revision.revision_id)
                .collect(),
            action: stemma::tracked_model::ResolveSelectionAction::Reject,
        })
        .unwrap();
    let BlockNode::Table(table) = &rejected.snapshot().canonical.blocks[0].block else {
        panic!()
    };
    assert_eq!(table.formatting.grid_cols, [2800]);
    assert_eq!(table.rows[0].cells.len(), 1);
    assert!(rejected.revisions().is_empty());
    assert_validator_clean(
        "complete-structural-property-grid-selection",
        &rejected
            .serialize(&stemma::ExportOptions::default())
            .unwrap(),
    );
    assert_eq!(original.revisions().len(), 2);
}

#[test]
fn sentinel_partial_shared_grid_resolution_is_explicitly_refused() {
    // One prior coordinate frame describes both property owners. Until the
    // model can rebase mixed readings, selecting only half is not a valid
    // transition; it must not return a successful document with mixed frames.
    let source = pack(&document(
        r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="1000"/><w:gridCol w:w="3000"/><w:tblGridChange w:id="21"><w:tblGrid><w:gridCol w:w="1000"/><w:gridCol w:w="1000"/><w:gridCol w:w="2000"/></w:tblGrid></w:tblGridChange></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="1000" w:type="dxa"/><w:tcPrChange w:id="23" w:author="Reviewer"><w:tcPr><w:tcW w:w="2000" w:type="dxa"/><w:gridSpan w:val="2"/></w:tcPr></w:tcPrChange></w:tcPr><w:p><w:r><w:t>First</w:t></w:r></w:p></w:tc><w:tc><w:tcPr><w:tcW w:w="3000" w:type="dxa"/><w:tcPrChange w:id="24" w:author="Reviewer"><w:tcPr><w:tcW w:w="2000" w:type="dxa"/></w:tcPr></w:tcPrChange></w:tcPr><w:p><w:r><w:t>Second</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
    ));
    let original = stemma::api::Document::parse(&source).unwrap();
    let first = original
        .revisions()
        .into_iter()
        .find(|r| r.wire_id == 23)
        .unwrap();
    for action in [
        stemma::tracked_model::ResolveSelectionAction::Accept,
        stemma::tracked_model::ResolveSelectionAction::Reject,
    ] {
        match original.project(stemma::Resolution::Selective {
            ids: [first.revision_id].into_iter().collect(),
            action,
        }) {
            Ok(_) => panic!("partial coupled-grid resolution returned success"),
            Err(error) => {
                assert_eq!(error.code, stemma::runtime::ErrorCode::UnsupportedEdit);
                assert!(error.message.contains("shared table grid"));
            }
        }
    }
    assert_eq!(original.revisions().len(), 2);
}

#[test]
fn sentinel_untouched_section_session_provenance_survives_resolution() {
    // ISO 29500-1 §17.6.18 defines four section editing-session attributes.
    // Resolving an unrelated text insertion must not erase that source state.
    // These are provenance, not a claim about visible section formatting.
    let source = pack(&document(
        r#"<w:p><w:pPr><w:sectPr w:rsidR="00112233" w:rsidDel="00445566" w:rsidRPr="00778899" w:rsidSect="00AABBCC"><w:type w:val="continuous"/></w:sectPr></w:pPr><w:r><w:t>First section</w:t></w:r></w:p><w:p><w:r><w:t>Second</w:t></w:r><w:ins w:id="1" w:author="Reviewer"><w:r><w:t> added</w:t></w:r></w:ins></w:p>"#,
    ));
    let original = stemma::api::Document::parse(&source).unwrap();
    let pending = original
        .serialize(&stemma::ExportOptions::default())
        .unwrap();
    let reopened = stemma::api::Document::parse(&pending).unwrap();
    for input in [&original, &reopened] {
        for resolution in [stemma::Resolution::AcceptAll, stemma::Resolution::RejectAll] {
            let resolved = input.project(resolution).unwrap();
            let bytes = resolved
                .serialize(&stemma::ExportOptions::default())
                .unwrap();
            assert_validator_clean("untouched-section-session", &bytes);
            let xml = document_xml(&bytes);
            let root = xmltree::Element::parse(xml.as_bytes()).unwrap();
            let section = root
                .get_child("body")
                .unwrap()
                .get_child("p")
                .unwrap()
                .get_child("pPr")
                .unwrap()
                .get_child("sectPr")
                .unwrap();
            for (name, value) in [
                ("rsidR", "00112233"),
                ("rsidDel", "00445566"),
                ("rsidRPr", "00778899"),
                ("rsidSect", "00AABBCC"),
            ] {
                let actual = section
                    .attributes
                    .iter()
                    .find(|(key, _)| {
                        key.local_name == name
                            && key.namespace.as_deref()
                                == Some(
                                    "http://schemas.openxmlformats.org/wordprocessingml/2006/main",
                                )
                    })
                    .map(|(_, value)| value.as_str());
                assert_eq!(actual, Some(value), "{name}");
            }
        }
    }
}

#[test]
fn sentinel_whole_hyperlink_insertion_tracks_the_destination() {
    // ECMA-376 17.16.5.25 + 17.13.5: the HYPERLINK instruction and
    // controls belong to a whole-link insertion, not just its display text.
    // Tracking only a w:hyperlink's inner runs leaves a destination on Reject.
    let source = pack(&document("<w:p><w:r><w:t>Portal</w:t></w:r></w:p>"));
    let original = stemma::api::Document::parse(&source).unwrap();
    let BlockNode::Paragraph(p) = &original.snapshot().canonical.blocks[0].block else {
        panic!()
    };
    let json = serde_json::json!({"ops":[{"op":"replace","target":p.id.0,"expect":"Portal","content":{"type":"paragraph","content":[{"type":"hyperlink","attrs":{"href":"https://example.test/portal"},"content":[{"type":"text","text":"Portal"}]}]}}],"revision":{"author":"Reviewer"}}).to_string();
    let txn = stemma::edit_v4::parse_transaction(&json)
        .unwrap()
        .into_edit_transaction()
        .unwrap();
    let edited = original.apply(&txn).unwrap();
    let bytes = edited.serialize(&stemma::ExportOptions::default()).unwrap();
    assert_validator_clean("whole-link-insert", &bytes);
    let xml = document_xml(&bytes);
    assert!(
        !xml.contains("<w:hyperlink"),
        "destination is outside the whole-link insertion"
    );
    let root = xmltree::Element::parse(xml.as_bytes()).unwrap();
    let insertion = root
        .get_child("body")
        .unwrap()
        .get_child("p")
        .unwrap()
        .get_child("ins")
        .unwrap();
    let runs: Vec<_> = insertion
        .children
        .iter()
        .filter_map(xmltree::XMLNode::as_element)
        .collect();
    assert_eq!(
        runs.iter()
            .filter(|r| r.get_child("fldChar").is_some())
            .count(),
        3
    );
    assert_eq!(
        runs.iter()
            .filter(|r| r.get_child("instrText").is_some())
            .count(),
        1
    );
    assert!(xml.contains("HYPERLINK"));
    // §17.16.22: an omitted hyperlink history attribute means false, while
    // a HYPERLINK field without Word's \\h switch enables visited history.
    assert!(
        xml.contains("\\h"),
        "whole-link carrier changed history: {xml}"
    );
    let reopened = stemma::api::Document::parse(&bytes).unwrap();
    let roundtrip = reopened
        .serialize(&stemma::ExportOptions::default())
        .unwrap();
    assert_validator_clean("whole-link-history-reopen", &roundtrip);
    assert!(document_xml(&roundtrip).contains("\\h"));
    let rejected = reopened.project(stemma::Resolution::RejectAll).unwrap();
    assert_eq!(rejected.to_text(), original.to_text());
    let rejected_bytes = rejected
        .serialize(&stemma::ExportOptions::default())
        .unwrap();
    assert_validator_clean("whole-link-reject", &rejected_bytes);
    let rejected_xml = document_xml(&rejected_bytes);
    assert!(!rejected_xml.contains("HYPERLINK") && !rejected_xml.contains("<w:hyperlink"));
    let accepted = reopened.project(stemma::Resolution::AcceptAll).unwrap();
    assert_eq!(
        hyperlinks(&accepted),
        vec![(
            Some("https://example.test/portal".to_string()),
            "Portal".to_string()
        )]
    );
    assert_validator_clean(
        "whole-link-accept",
        &accepted
            .serialize(&stemma::ExportOptions::default())
            .unwrap(),
    );
}

#[test]
fn sentinel_unlink_retains_formatted_editable_text_and_reversible_destination() {
    super::edit_hyperlink_synthesis::check_unlink_formatted_text();
}

#[test]
fn sentinel_resolution_preserves_unrelated_run_deletion_session() {
    // §17.3.2.25: rsidDel is editing-session provenance, not a pending
    // deletion by itself. Word retains it on normal runs and accepted
    // insertions; rejecting a real deletion clears that deletion's session.
    for payload in [r#"<w:t xml:space="preserve"> </w:t>"#, "<w:br/>"] {
        for state in ["normal", "ins", "del"] {
            let content = if state == "del" {
                payload.replace("w:t", "w:delText")
            } else {
                payload.to_string()
            };
            let run = format!(r#"<w:r w:rsidDel="00112233">{content}</w:r>"#);
            let target = if state == "normal" {
                run
            } else {
                format!(r#"<w:{state} w:id="1" w:author="Reviewer">{run}</w:{state}>"#)
            };
            let source = pack(&document(&format!(
                r#"<w:p>{target}</w:p><w:p><w:ins w:id="2" w:author="Other"><w:r><w:t>Pending</w:t></w:r></w:ins></w:p>"#
            )));
            let original = stemma::api::Document::parse(&source).unwrap();
            let selected_author = if state == "normal" {
                "Other"
            } else {
                "Reviewer"
            };
            let ids: std::collections::HashSet<_> = original
                .revisions()
                .iter()
                .filter(|r| r.author.as_deref() == Some(selected_author))
                .map(|r| r.revision_id)
                .collect();
            let remaining = usize::from(state != "normal");
            for (accept, action, pending) in [
                (true, stemma::Resolution::AcceptAll, 0),
                (false, stemma::Resolution::RejectAll, 0),
                (
                    true,
                    stemma::Resolution::Selective {
                        ids: ids.clone(),
                        action: stemma::ResolveSelectionAction::Accept,
                    },
                    remaining,
                ),
                (
                    false,
                    stemma::Resolution::Selective {
                        ids,
                        action: stemma::ResolveSelectionAction::Reject,
                    },
                    remaining,
                ),
            ] {
                let resolved = original.project(action).unwrap();
                let bytes = resolved
                    .serialize(&stemma::ExportOptions::default())
                    .unwrap();
                assert_validator_clean("run-deletion-session-projection", &bytes);
                let retained = state == "normal" || state == "ins" && accept;
                let xml = document_xml(&bytes);
                assert_eq!(
                    xml.contains("rsidDel=\"00112233\""),
                    retained,
                    "state={state}, accept={accept}, payload={payload}: {xml}"
                );
                let reopened = stemma::api::Document::parse(&bytes).unwrap();
                assert_eq!(reopened.revisions().len(), pending);
                let saved = reopened
                    .serialize(&stemma::ExportOptions::default())
                    .unwrap();
                assert_eq!(
                    document_xml(&saved).contains("rsidDel=\"00112233\""),
                    retained
                );
            }
        }
    }
}

#[test]
fn sentinel_reject_nested_deletion_clears_only_its_deletion_session() {
    // §17.3.2.25: run sessions describe editing provenance, not pending
    // revision identity. Word clears the rejected deletion's session even
    // when the restored run remains owned by a pending insertion.
    let source = pack(&document(
        r#"<w:p><w:ins w:id="11" w:author="Origin"><w:r w:rsidR="00112233"><w:t>Before</w:t></w:r><w:del w:id="12" w:author="Later"><w:r w:rsidR="00112233" w:rsidDel="00445566"><w:rPr><w:i/></w:rPr><w:delText>Restored</w:delText></w:r></w:del><w:r w:rsidR="00112233"><w:t>After</w:t></w:r></w:ins></w:p>"#,
    ));
    let original = stemma::api::Document::parse(&source).unwrap();
    let ids = original
        .revisions()
        .iter()
        .filter(|revision| revision.author.as_deref() == Some("Later"))
        .map(|revision| revision.revision_id)
        .collect();
    let restored = original
        .project(stemma::Resolution::Selective {
            ids,
            action: stemma::ResolveSelectionAction::Reject,
        })
        .unwrap();
    let bytes = restored
        .serialize(&stemma::ExportOptions::default())
        .unwrap();
    assert_validator_clean("nested-deletion-session-reject", &bytes);
    let xml = document_xml(&bytes);
    assert!(!xml.contains("rsidDel=\"00445566\""), "{xml}");
    assert!(xml.contains("rsidR=\"00112233\""), "{xml}");
    assert!(xml.contains("Restored"), "{xml}");
    let tree = xmltree::Element::parse(xml.as_bytes()).unwrap();
    let paragraph = tree.get_child("body").unwrap().get_child("p").unwrap();
    let runs: Vec<_> = paragraph
        .children
        .iter()
        .filter_map(xmltree::XMLNode::as_element)
        .filter(|child| child.name == "ins")
        .flat_map(|insertion| {
            insertion
                .children
                .iter()
                .filter_map(xmltree::XMLNode::as_element)
        })
        .filter(|run| run.name == "r")
        .filter(|run| {
            run.get_child("t")
                .and_then(|text| text.get_text())
                .is_some_and(|text| text == "Restored")
        })
        .collect();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].attributes.len(), 1);
    assert!(
        runs[0]
            .attributes
            .iter()
            .any(|(name, value)| name.local_name == "rsidR" && value == "00112233")
    );
    assert!(runs[0].get_child("rPr").unwrap().get_child("i").is_some());
    let reopened = stemma::api::Document::parse(&bytes).unwrap();
    assert!(!reopened.revisions().is_empty());
    assert!(
        reopened
            .revisions()
            .iter()
            .all(|revision| revision.author.as_deref() == Some("Origin"))
    );
    let original_outer: std::collections::HashSet<_> = original
        .revisions()
        .iter()
        .filter(|revision| revision.author.as_deref() == Some("Origin"))
        .map(|revision| revision.revision_id)
        .collect();
    let surviving: std::collections::HashSet<_> = reopened
        .revisions()
        .iter()
        .map(|revision| revision.revision_id)
        .collect();
    assert_eq!(surviving, original_outer);
    for resolution in [stemma::Resolution::AcceptAll, stemma::Resolution::RejectAll] {
        let settled = reopened.project(resolution).unwrap();
        let output = settled
            .serialize(&stemma::ExportOptions::default())
            .unwrap();
        assert_validator_clean("nested-deletion-session-terminal", &output);
        assert!(!document_xml(&output).contains("rsidDel=\"00445566\""));
        assert!(
            stemma::api::Document::parse(&output)
                .unwrap()
                .revisions()
                .is_empty()
        );
    }
}

#[test]
fn sentinel_new_hyperlink_retains_local_run_formatting() {
    // ECMA-376 17.16.22: a hyperlink owns runs, not an implicit font reset.
    // Authoring a link at a formatted insertion point must retain the same
    // direct run properties as ordinary text inserted at that point.
    let source = pack(&document(
        r#"<w:p><w:r><w:rPr><w:rFonts w:ascii="Arial" w:hAnsi="Arial"/><w:b w:val="0"/><w:u w:val="double"/><w:sz w:val="28"/></w:rPr><w:t>Resource</w:t></w:r></w:p>"#,
    ));
    let original = stemma::api::Document::parse(&source).unwrap();
    let BlockNode::Paragraph(p) = &original.snapshot().canonical.blocks[0].block else {
        panic!()
    };
    let json = serde_json::json!({"ops":[{"op":"replace","target":p.id.0,"expect":"Resource","content":{"type":"paragraph","content":[{"type":"hyperlink","attrs":{"href":"https://example.test/resource"},"content":[{"type":"text","text":"Resource"}]}]}}],"revision":{"author":"Reviewer"}}).to_string();
    let txn = stemma::edit_v4::parse_transaction(&json)
        .unwrap()
        .into_edit_transaction()
        .unwrap();
    let edited = original.apply(&txn).unwrap();
    let bytes = edited.serialize(&stemma::ExportOptions::default()).unwrap();
    assert_validator_clean("new-link-local-format", &bytes);
    let reopened = stemma::api::Document::parse(&bytes).unwrap();
    for doc in [
        &edited,
        &reopened.project(stemma::Resolution::AcceptAll).unwrap(),
    ] {
        let bytes = doc.serialize(&stemma::ExportOptions::default()).unwrap();
        assert_validator_clean("new-link-local-format-projection", &bytes);
        let xml = document_xml(&bytes);
        let root = xmltree::Element::parse(xml.as_bytes()).unwrap();
        fn link_rpr(element: &xmltree::Element) -> Option<&xmltree::Element> {
            if element.name == "r"
                && element.get_child("t").and_then(|t| t.get_text()).as_deref() == Some("Resource")
            {
                return element.get_child("rPr");
            }
            element
                .children
                .iter()
                .filter_map(xmltree::XMLNode::as_element)
                .find_map(link_rpr)
        }
        let rpr = link_rpr(&root)
            .unwrap_or_else(|| panic!("new hyperlink lost its run formatting: {xml}"));
        for (element, attribute, expected) in [
            ("rFonts", "ascii", "Arial"),
            ("rFonts", "hAnsi", "Arial"),
            ("b", "val", "0"),
            ("u", "val", "double"),
            ("sz", "val", "28"),
        ] {
            let property = rpr
                .get_child(element)
                .unwrap_or_else(|| panic!("link lost {element}"));
            assert_eq!(
                property
                    .attributes
                    .iter()
                    .find(|(key, _)| key.local_name == attribute)
                    .map(|(_, value)| value.as_str()),
                Some(expected)
            );
        }
        assert_eq!(
            hyperlinks(doc),
            vec![(
                Some("https://example.test/resource".to_string()),
                "Resource".to_string()
            )]
        );
    }
    let rejected = reopened.project(stemma::Resolution::RejectAll).unwrap();
    assert_eq!(rejected.to_text(), original.to_text());
    let rejected_bytes = rejected
        .serialize(&stemma::ExportOptions::default())
        .unwrap();
    assert_validator_clean("new-link-reject", &rejected_bytes);
    assert!(!document_xml(&rejected_bytes).contains("<w:hyperlink"));
}

#[test]
fn sentinel_run_border_keeps_rendering_effects() {
    // ECMA-376 17.3.2.4 and 17.3.4: the full border attribute set
    // determines rendering and whether adjacent runs share a border group.
    let source = pack(&document(
        r#"<w:p><w:r><w:rPr><w:bdr w:val="single" w:sz="8" w:space="1" w:color="auto" w:frame="1" w:shadow="true" w:themeColor="accent1" w:themeTint="99"/></w:rPr><w:t>Bordered text</w:t></w:r></w:p>"#,
    ));
    let original = stemma::api::Document::parse(&source).unwrap();
    let BlockNode::Paragraph(p) = &original.snapshot().canonical.blocks[0].block else {
        panic!()
    };
    let json = serde_json::json!({"ops":[{"op":"set_format","target":p.id.0,"expect":"Bordered text","marks":{"bold":true}}],"revision":{"author":"Reviewer"}}).to_string();
    let txn = stemma::edit_v4::parse_transaction(&json)
        .unwrap()
        .into_edit_transaction()
        .unwrap();
    let edited = original.apply(&txn).unwrap();
    let bytes = edited.serialize(&stemma::ExportOptions::default()).unwrap();
    let reopened = stemma::api::Document::parse(&bytes).unwrap();
    let accepted = reopened.project(stemma::Resolution::AcceptAll).unwrap();
    let rejected = reopened.project(stemma::Resolution::RejectAll).unwrap();
    assert_eq!(rejected.to_text(), original.to_text());
    for doc in [original, edited, accepted, rejected] {
        let bytes = doc.serialize(&stemma::ExportOptions::default()).unwrap();
        assert_validator_clean("run-border-effects", &bytes);
        let xml = document_xml(&bytes);
        for attribute in [
            r#"w:frame="1""#,
            r#"w:shadow="true""#,
            r#"w:themeColor="accent1""#,
            r#"w:themeTint="99""#,
        ] {
            assert!(xml.contains(attribute), "lost run border {attribute}");
        }
    }
}

#[test]
fn sentinel_run_shading_keeps_theme_attributes() {
    // ECMA-376 17.3.2.32 / 17.3.5: themeFill and its shade modifier
    // determine shading even when a literal fill is also present.
    let source = pack(&document(
        r#"<w:p><w:r><w:rPr><w:shd w:val="clear" w:fill="D9E2F3" w:themeFill="accent1" w:themeFillShade="BF"/></w:rPr><w:t>Shaded text</w:t></w:r></w:p>"#,
    ));
    let original = stemma::api::Document::parse(&source).unwrap();
    fn assert_theme(doc: &stemma::api::Document) {
        let BlockNode::Paragraph(p) = &doc.snapshot().canonical.blocks[0].block else {
            panic!()
        };
        let shading = p
            .segments
            .iter()
            .flat_map(|s| &s.inlines)
            .find_map(|inline| match inline {
                InlineNode::Text(t) => t.style_props.run_shading.as_ref(),
                _ => None,
            })
            .unwrap();
        for (name, value) in [("themeFill", "accent1"), ("themeFillShade", "BF")] {
            assert!(
                shading.extra_attrs.iter().any(|a| a.local_name == name
                    && a.value == value
                    && a.namespace.as_deref()
                        == Some("http://schemas.openxmlformats.org/wordprocessingml/2006/main")),
                "lost run shading {name}"
            );
        }
    }
    assert_theme(&original);
    let BlockNode::Paragraph(p) = &original.snapshot().canonical.blocks[0].block else {
        panic!()
    };
    let json = serde_json::json!({"ops":[{"op":"set_format","target":p.id.0,"expect":"Shaded text","marks":{"bold":true}}],"revision":{"author":"Reviewer"}}).to_string();
    let txn = stemma::edit_v4::parse_transaction(&json)
        .unwrap()
        .into_edit_transaction()
        .unwrap();
    let edited = original.apply(&txn).unwrap();
    let bytes = edited.serialize(&stemma::ExportOptions::default()).unwrap();
    assert_validator_clean("run-shading-theme", &bytes);
    let reopened = stemma::api::Document::parse(&bytes).unwrap();
    assert_theme(&reopened);
    for resolution in [stemma::Resolution::AcceptAll, stemma::Resolution::RejectAll] {
        let projected = reopened.project(resolution).unwrap();
        let bytes = projected
            .serialize(&stemma::ExportOptions::default())
            .unwrap();
        assert_theme(&stemma::api::Document::parse(&bytes).unwrap());
    }
}

#[test]
fn sentinel_paragraph_border_effects_survive_edit_and_projection() {
    // ECMA-376 17.3.4: omitted frame/shadow means no effect. They are
    // rendering properties, not disposable metadata of an unchanged border.
    let effects = r#"<w:top w:val="single" w:sz="8" w:color="auto" w:frame="1" w:shadow="true" w:themeColor="accent1" w:themeTint="99"/>"#;
    for inherited in [false, true] {
        let properties = if inherited {
            r#"<w:pStyle w:val="Framed"/>"#.to_string()
        } else {
            format!("<w:pBdr>{effects}</w:pBdr>")
        };
        let styles = ExtraPart {
            name: "word/styles.xml",
            content_type: "application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml",
            rel: (
                "styles",
                "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles",
                "styles.xml",
            ),
            xml: format!(
                r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style w:type="paragraph" w:styleId="Framed"><w:name w:val="Framed"/><w:pPr><w:pBdr>{effects}</w:pBdr></w:pPr></w:style></w:styles>"#
            ),
        };
        let source = pack_with_parts(
            &document(&format!(
                "<w:p><w:pPr>{properties}</w:pPr><w:r><w:t>Framed text</w:t></w:r></w:p>"
            )),
            &[styles],
        );
        let original = stemma::api::Document::parse(&source).unwrap();
        fn assert_effects(doc: &stemma::api::Document) {
            let BlockNode::Paragraph(p) = &doc.snapshot().canonical.blocks[0].block else {
                panic!()
            };
            let border = p.borders.as_ref().unwrap().top.as_ref().unwrap();
            for (name, value) in [
                ("w:frame", "1"),
                ("w:shadow", "true"),
                ("w:themeColor", "accent1"),
                ("w:themeTint", "99"),
            ] {
                assert!(
                    border.extra_attrs.contains(&(name.into(), value.into())),
                    "lost border property {name}={value}"
                );
            }
        }
        assert_effects(&original);
        let BlockNode::Paragraph(p) = &original.snapshot().canonical.blocks[0].block else {
            panic!()
        };
        let json = serde_json::json!({"ops":[{"op":"insert","target":{"anchor":p.id.0,"position":"after"},"content":[{"type":"paragraph","content":[{"type":"text","text":"Added paragraph"}]}]}],"revision":{"author":"Reviewer"}}).to_string();
        let txn = stemma::edit_v4::parse_transaction(&json)
            .unwrap()
            .into_edit_transaction()
            .unwrap();
        let edited = original.apply(&txn).unwrap();
        for doc in [original, edited] {
            let bytes = doc.serialize(&stemma::ExportOptions::default()).unwrap();
            assert_validator_clean("paragraph-border-effects", &bytes);
            let reopened = stemma::api::Document::parse(&bytes).unwrap();
            assert_effects(&reopened);
            for resolution in [stemma::Resolution::AcceptAll, stemma::Resolution::RejectAll] {
                let projected = reopened.project(resolution).unwrap();
                let bytes = projected
                    .serialize(&stemma::ExportOptions::default())
                    .unwrap();
                assert_effects(&stemma::api::Document::parse(&bytes).unwrap());
            }
        }
    }
}

#[test]
fn sentinel_inserted_table_cells_do_not_duplicate_paragraph_identity() {
    // MS-DOCX 2.6.2.3: paraId is unique within a document part, except
    // alternative representations. 2.6.2.4: textId requires paraId.
    // A fresh cell inherits formatting, not the exemplar's identity/version.
    let source = pack(&document(
        r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="2200"/><w:gridCol w:w="2200"/></w:tblGrid><w:tr><w:tc><w:tcPr/><w:p w14:paraId="00000121" w14:textId="00000231"><w:r><w:t>First template</w:t></w:r></w:p></w:tc><w:tc><w:tcPr/><w:p w14:paraId="00000122" w14:textId="00000232"><w:r><w:t>Second template</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/>"#,
    ));
    fn table(doc: &CanonDoc) -> &stemma::domain::TableNode {
        doc.blocks
            .iter()
            .find_map(|b| match &b.block {
                BlockNode::Table(t) => Some(t.as_ref()),
                _ => None,
            })
            .unwrap()
    }
    fn assert_identities(doc: &CanonDoc, expected_cells: usize) {
        let paragraphs: Vec<_> = table(doc)
            .rows
            .iter()
            .flat_map(|r| &r.cells)
            .flat_map(|c| &c.blocks)
            .map(|b| match b {
                BlockNode::Paragraph(p) => p.as_ref(),
                _ => panic!("synthetic cell must contain a paragraph"),
            })
            .collect();
        assert_eq!(paragraphs.len(), expected_cells);
        let mut identities = std::collections::BTreeSet::new();
        for p in &paragraphs {
            if let Some(id) = &p.para_id {
                assert!(
                    identities.insert(id.clone()),
                    "duplicate active paragraph identity: {id}"
                );
            } else {
                assert!(p.text_id.is_none(), "textId requires a paragraph identity");
            }
        }
        for (id, version) in [("00000121", "00000231"), ("00000122", "00000232")] {
            let original = paragraphs
                .iter()
                .find(|p| p.para_id.as_deref() == Some(id))
                .unwrap();
            assert_eq!(original.text_id.as_deref(), Some(version));
        }
    }
    for (operation, count) in [
        (
            r#"{"kind":"insert_row","ref_row":0,"position":"after","cells":["Added A","Added B"]}"#,
            4,
        ),
        (
            r#"{"kind":"insert_column","ref_col":0,"position":"after"}"#,
            3,
        ),
    ] {
        let original = stemma::api::Document::parse(&source).unwrap();
        let id = &table(&original.snapshot().canonical).id;
        let json = format!(
            r#"{{"ops":[{{"op":"table_op","target":"{id}","table_op":{operation}}}],"revision":{{"author":"Reviewer"}}}}"#
        );
        let txn = stemma::edit_v4::parse_transaction(&json)
            .unwrap()
            .into_edit_transaction()
            .unwrap();
        let edited = original.apply(&txn).unwrap();
        assert_identities(&edited.snapshot().canonical, count);
        let bytes = edited.serialize(&stemma::ExportOptions::default()).unwrap();
        assert_validator_clean("fresh-table-paragraph-identity", &bytes);
        let reparsed = stemma::api::Document::parse(&bytes).unwrap();
        assert_identities(&reparsed.snapshot().canonical, count);
        let accepted = reparsed.project(stemma::Resolution::AcceptAll).unwrap();
        assert_identities(&accepted.snapshot().canonical, count);
        let rejected = reparsed.project(stemma::Resolution::RejectAll).unwrap();
        assert_identities(&rejected.snapshot().canonical, 2);
        assert_eq!(rejected.to_text(), original.to_text());
    }
}

#[test]
fn sentinel_inserted_row_inherits_text_formatting_and_rejects_to_baseline() {
    let class = "insert-row-inherits-reference-text-formatting";
    let source = pack(&document(
        r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="2400"/></w:tblGrid><w:tr><w:tc><w:tcPr/><w:p><w:r><w:rPr><w:rFonts w:ascii="Georgia" w:hAnsi="Georgia"/><w:i/><w:sz w:val="22"/></w:rPr><w:t>Existing requirement</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/>"#,
    ));
    fn table(doc: &CanonDoc) -> &stemma::domain::TableNode {
        doc.blocks
            .iter()
            .find_map(|b| match &b.block {
                BlockNode::Table(t) => Some(t.as_ref()),
                _ => None,
            })
            .expect("synthetic table exists")
    }
    fn cell_text(table: &stemma::domain::TableNode, row: usize) -> &stemma::domain::TextNode {
        let BlockNode::Paragraph(p) = &table.rows[row].cells[0].blocks[0] else {
            panic!("synthetic cell paragraph exists");
        };
        p.first_content_text_node()
            .expect("synthetic cell text exists")
    }
    let original = stemma::api::Document::parse(&source).unwrap();
    let id = &table(&original.snapshot().canonical).id;
    let json = format!(
        r#"{{"ops":[{{"op":"table_op","target":"{id}","table_op":{{"kind":"insert_row","ref_row":0,"position":"after","cells":["Additional requirement"]}}}}],"revision":{{"author":"Reviewer"}}}}"#
    );
    let txn = stemma::edit_v4::parse_transaction(&json)
        .unwrap()
        .into_edit_transaction()
        .unwrap();
    let edited = original.apply(&txn).unwrap();
    let redline = edited.serialize(&stemma::ExportOptions::default()).unwrap();
    assert_validator_clean(class, &redline);
    let reparsed = stemma::api::Document::parse(&redline).unwrap();
    let accepted = reparsed.project(stemma::Resolution::AcceptAll).unwrap();
    let accepted_bytes = accepted
        .serialize(&stemma::ExportOptions::default())
        .unwrap();
    assert_validator_clean(class, &accepted_bytes);
    let accepted_again = stemma::api::Document::parse(&accepted_bytes).unwrap();
    let a = table(&accepted_again.snapshot().canonical);
    assert_eq!(a.rows.len(), 2);
    let inserted = cell_text(a, 1);
    assert_eq!(inserted.text, "Additional requirement");
    assert_eq!(inserted.style_props.font_family.as_deref(), Some("Georgia"));
    assert_eq!(inserted.style_props.font_size, Some(22));
    assert!(inserted.marks.contains(&Mark::Italic));
    assert_eq!(cell_text(a, 0).text, "Existing requirement");
    assert_eq!(
        cell_text(a, 0).style_props,
        cell_text(table(&original.snapshot().canonical), 0).style_props
    );
    assert_ne!(cell_text(a, 0).id, inserted.id);
    let rejected = reparsed.project(stemma::Resolution::RejectAll).unwrap();
    let rejected_bytes = rejected
        .serialize(&stemma::ExportOptions::default())
        .unwrap();
    assert_validator_clean(class, &rejected_bytes);
    let rejected_again = stemma::api::Document::parse(&rejected_bytes).unwrap();
    assert_eq!(table(&rejected_again.snapshot().canonical).rows.len(), 1);
    assert_eq!(rejected_again.to_text(), original.to_text());
    assert_eq!(
        cell_text(table(&rejected_again.snapshot().canonical), 0).style_props,
        cell_text(table(&original.snapshot().canonical), 0).style_props
    );
}

#[test]
fn sentinel_partial_paragraph_insert_does_not_capture_normal_literal_prefix() {
    let class = "partial-paragraph-insert-with-normal-literal-prefix";
    let source = pack(&document(
        r#"<w:p><w:pPr><w:rPr><w:ins w:id="1" w:author="source" w:date="2026-01-01T00:00:00Z"/></w:rPr></w:pPr><w:r><w:tab/><w:t>(b)</w:t></w:r><w:r><w:tab/></w:r><w:ins w:id="2" w:author="source" w:date="2026-01-01T00:00:00Z"><w:r><w:t>replacement branch</w:t></w:r></w:ins></w:p><w:p><w:r><w:t>baseline branch</w:t></w:r></w:p>"#,
    ));
    let expected = archive_reject_text(&source);
    assert!(
        expected.contains("(b)"),
        "[{class}] the source label is outside w:ins and belongs to the reject baseline"
    );

    let rebuilt = stemma::api::Document::parse(&source)
        .expect("parse partial-insert sentinel")
        .serialize(&stemma::ExportOptions::default())
        .expect("serialize partial-insert sentinel");
    assert_validator_clean(class, &rebuilt);
    assert_eq!(
        archive_reject_text(&rebuilt),
        expected,
        "[{class}] identity rebuild must not pull an untracked literal prefix into the paragraph insertion"
    );
}

#[test]
fn sentinel_row_delete_preserves_nested_revision_reject_fixpoint() {
    let class = "row-delete-over-preexisting-inline-insertion";
    let source = table_row_with_preexisting_inline_insertion();
    let doc = stemma::api::Document::parse(&source).expect("parse row-delete sentinel");
    let table_id = doc
        .snapshot()
        .canonical
        .blocks
        .iter()
        .find_map(|tracked| match &tracked.block {
            BlockNode::Table(table) => Some(table.id.to_string()),
            _ => None,
        })
        .expect("sentinel table id");
    let json = format!(
        r#"{{"ops":[{{"op":"table_op","target":"{table_id}","table_op":{{"kind":"delete_row","row_index":0}}}}],"revision":{{"author":"later"}}}}"#
    );
    let transaction = stemma::edit_v4::parse_transaction(&json)
        .expect("parse row-delete transaction")
        .into_edit_transaction()
        .expect("translate row-delete transaction");
    let baseline_text = all_text(
        &doc.project(stemma::Resolution::RejectAll)
            .expect("resolve source baseline")
            .snapshot()
            .canonical,
    );
    let edited = doc.apply(&transaction).expect("apply row deletion");

    let redline = edited
        .serialize(&stemma::ExportOptions::default())
        .expect("serialize composed row deletion");
    assert_validator_clean(class, &redline);

    // Reparse the emitted OOXML before resolving: this catches a serializer
    // that flattened the inner insertion even if the in-memory model was sound.
    let reparsed = stemma::api::Document::parse(&redline).expect("reparse composed redline");
    let rejected = reparsed
        .project(stemma::Resolution::RejectAll)
        .expect("reject all composed revisions");
    assert_eq!(
        all_text(&rejected.snapshot().canonical),
        baseline_text,
        "[{class}] reject-all must equal the source baseline; the pending inserted 7 must not survive"
    );

    let accepted = reparsed
        .project(stemma::Resolution::AcceptAll)
        .expect("accept all composed revisions");
    assert_eq!(
        all_text(&accepted.snapshot().canonical).trim(),
        "keep",
        "[{class}] accept-all must remove the deleted row"
    );
}

#[test]
fn sentinel_unmodified_drawing_keeps_alternate_representations() {
    fn shape(node: &xmltree::Element) -> serde_json::Value {
        let mut attributes: Vec<_> = node
            .attributes
            .iter()
            .map(|(name, value)| {
                (
                    name.namespace.as_deref(),
                    name.local_name.as_str(),
                    value.as_str(),
                )
            })
            .collect();
        attributes.sort();
        let children: Vec<_> = node
            .children
            .iter()
            .map(|child| match child {
                xmltree::XMLNode::Element(element) => shape(element),
                other => serde_json::json!(["non_element", format!("{other:?}")]),
            })
            .collect();
        serde_json::json!([node.namespace, node.name, attributes, children])
    }
    fn find_envelope(node: &xmltree::Element) -> Option<serde_json::Value> {
        if node.name == "AlternateContent"
            && node.namespace.as_deref()
                == Some("http://schemas.openxmlformats.org/markup-compatibility/2006")
        {
            return Some(shape(node));
        }
        node.children.iter().find_map(|child| match child {
            xmltree::XMLNode::Element(child) => find_envelope(child),
            _ => None,
        })
    }
    fn envelope(bytes: &[u8]) -> Option<serde_json::Value> {
        let xml = document_xml(bytes);
        let parsed = xmltree::Element::parse(xml.as_bytes()).unwrap();
        find_envelope(&parsed)
    }
    // MCE chooses one active drawing, while the unmodified legacy drawing
    // remains owned by its compatibility envelope (MS-OI29500 §3.1.4).
    let alternate = concat!(
        r#"<mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:v="urn:schemas-microsoft-com:vml">"#,
        r#"<mc:Choice Requires="wps"><w:drawing><wp:inline><wp:extent cx="914400" cy="457200"/><wp:docPr id="1" name="Rectangle"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:cNvSpPr/><wps:spPr><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></wps:spPr><wps:bodyPr/></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></mc:Choice>"#,
        r##"<mc:Fallback><w:pict><v:rect id="LegacyRectangle" style="width:72pt;height:36pt" fillcolor="#ffffff"/></w:pict></mc:Fallback></mc:AlternateContent>"##,
    );
    for inserted_drawing in [false, true] {
        let drawing = format!("<w:r>{alternate}</w:r>");
        let drawing = if inserted_drawing {
            format!(r#"<w:ins w:id="7" w:author="Earlier">{drawing}</w:ins>"#)
        } else {
            drawing
        };
        let source = pack(&document(&format!(
            "<w:p><w:r><w:t>Before</w:t></w:r></w:p><w:p>{drawing}</w:p>"
        )));
        let original = stemma::api::Document::parse(&source).unwrap();
        let BlockNode::Paragraph(first) = &original.snapshot().canonical.blocks[0].block else {
            panic!("first block is the editable paragraph");
        };
        let transaction = EditTransaction {
            steps: vec![EditStep::ReplaceParagraphText {
                block_id: first.id.clone(),
                expect: "Before".into(),
                content: ParagraphContent {
                    fragments: vec![ContentFragment::Text("After".into())],
                },
                rationale: None,
                replacement_role: None,
                semantic_hash: None,
            }],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: revision(),
        };
        let pending = original.apply(&transaction).unwrap();
        let bytes = pending
            .serialize(&stemma::ExportOptions::default())
            .unwrap();
        assert_validator_clean("preserved-alternate-drawing", &bytes);
        assert_eq!(envelope(&bytes), envelope(&source));
        let reopened = stemma::api::Document::parse(&bytes).unwrap();
        for action in [stemma::Resolution::AcceptAll, stemma::Resolution::RejectAll] {
            let rejecting = matches!(action, stemma::Resolution::RejectAll);
            let resolved = reopened.project(action).unwrap();
            let bytes = resolved
                .serialize(&stemma::ExportOptions::default())
                .unwrap();
            assert_validator_clean("resolved-alternate-drawing", &bytes);
            let terminal = stemma::api::Document::parse(&bytes).unwrap();
            assert!(terminal.revisions().is_empty());
            assert_eq!(
                envelope(&bytes),
                if rejecting && inserted_drawing {
                    None
                } else {
                    envelope(&source)
                }
            );
            assert!(
                terminal
                    .to_text()
                    .contains(if rejecting { "Before" } else { "After" })
            );
        }
    }
}

#[test]
fn sentinel_nested_story_controls_keep_exact_ownership() {
    super::spec_story_sdt_preservation::check_nested_footer_control_lifecycle();
}

#[test]
fn sentinel_hyperlink_window_and_history_switches_are_independent() {
    super::spec_fields_parser::check_hyperlink_switch_independence();
    for (switches, new_window, no_history) in [
        ("", false, false),
        (r" \n", true, false),
        (r" \h", false, true),
        (r" \n \h", true, true),
    ] {
        let body = format!(
            r#"<w:p><w:fldSimple w:instr='HYPERLINK "https://example.test/resource"{switches}'><w:r><w:rPr><w:b/></w:rPr><w:t>Resource</w:t></w:r></w:fldSimple></w:p>"#
        );
        let mut bytes = pack(&document(&body));
        for _ in 0..2 {
            let doc = stemma::api::Document::parse(&bytes).unwrap();
            let BlockNode::Paragraph(paragraph) = &doc.snapshot().canonical.blocks[0].block else {
                panic!("expected paragraph");
            };
            let fields: Vec<_> = paragraph
                .all_inlines()
                .filter_map(|inline| {
                    let InlineNode::OpaqueInline(opaque) = inline else {
                        return None;
                    };
                    let stemma::OpaqueKind::Field(field) = &opaque.kind else {
                        return None;
                    };
                    let Some(stemma::FieldSemantic::Hyperlink(link)) = &field.semantic else {
                        return None;
                    };
                    Some(link)
                })
                .collect();
            assert_eq!(fields.len(), 1);
            assert_eq!(fields[0].open_in_new_window, new_window);
            assert_eq!(fields[0].no_history, no_history);
            assert_eq!(doc.to_text().trim(), "Resource");
            assert!(doc.revisions().is_empty());
            bytes = doc.serialize(&stemma::ExportOptions::default()).unwrap();
            assert_validator_clean("hyperlink-switches", &bytes);
        }
    }
}
// The standalone in-memory witnesses also run in the corpus-free sentinel
// tier: prefix text and its live ranges must share one source-position law.
#[path = "../literal_prefix_range_ownership.rs"]
mod prefix_range_ownership_sentinels;

#[path = "../paragraph_text_versions.rs"]
mod paragraph_text_version_sentinels;

/// (url, display text) of every hyperlink in the body's paragraphs, in order.
fn hyperlinks(doc: &stemma::api::Document) -> Vec<(Option<String>, String)> {
    doc.snapshot()
        .canonical
        .blocks
        .iter()
        .filter_map(|tracked| match &tracked.block {
            BlockNode::Paragraph(paragraph) => Some(paragraph),
            _ => None,
        })
        .flat_map(|paragraph| paragraph.all_inlines_owned())
        .filter_map(|inline| match inline {
            stemma::domain::InlineNode::OpaqueInline(opaque) => match opaque.kind {
                stemma::domain::OpaqueKind::Hyperlink(link) => Some((link.url, link.text)),
                _ => None,
            },
            _ => None,
        })
        .collect()
}
