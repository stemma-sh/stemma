//! Integration tests for the content-control DATA-BINDING extension of the
//! inline `WrapInContentControl` verb (`w:dataBinding`, ECMA-376 §17.5.2.6).
//!
//! Domain rule (CLAUDE.md "no silent fallbacks"; domain-model §11): a data-bound
//! control binds its displayed value to a node in a custom-XML datastore part.
//! Authoring one is a two-part transformation, mirroring the styles.xml /
//! numbering.xml part-bootstrap:
//!   1. the `sdtPr` gains a `<w:dataBinding w:xpath=… w:storeItemID=…>`; and
//!   2. the save path authors (or reuses) a `customXml/item*.xml` datastore part
//!      whose `itemProps` carries that `storeItemID` as its `ds:itemID`, plus its
//!      content-type Overrides and a `customXml` relationship from document.xml.
//!
//! Invariants under test:
//!   - the wrapped span gets a `w:sdt` whose `sdtPr` carries the `w:dataBinding`
//!     (xpath + storeItemID, in spec order: after `w:id`, before the control kind);
//!   - the backing `customXml/item*.xml` + `itemProps*.xml` + `_rels` are authored,
//!     content-typed, and linked by a `customXml` document relationship;
//!   - the package opens validator-clean (Blocking);
//!   - UNTRACKED: accept-all == reject-all == the bound document (no `w:sdtChange`);
//!   - the opaque/content inventory is non-shrinking (the wrap adds an envelope,
//!     never drops the inner run text);
//!   - fail-loud: an empty xpath or empty storeItemID is `MalformedDataBinding`.
//!
//! Daily tier, corpus-free (synthesized in-memory DOCX).

use stemma::api::Document;
use stemma::domain::{
    BlockNode, CanonDoc, InlineNode, NodeId, OpaqueKind, RevisionInfo, SdtControl,
};
use stemma::edit::{
    DataBinding, EditError, EditStep, EditTransaction, MaterializationMode, SdtSpec,
    apply_transaction,
};
use stemma::{ExportMode, ExportOptions, Resolution, ValidatorLevel};

/// A minimal single-paragraph DOCX with the w14/w15 namespaces declared (so a
/// content control round-trips).
fn make_docx(text: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" xmlns:w15="http://schemas.microsoft.com/office/word/2012/wordml"><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#
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

fn first_block_id(canon: &CanonDoc) -> NodeId {
    match &canon.blocks[0].block {
        BlockNode::Paragraph(p) => p.id.clone(),
        _ => panic!("expected a paragraph"),
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
            author: Some("CCDB".to_string()),
            date: Some("2026-06-05T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

/// Count opaque inlines (SDTs, drawings, …) in a doc — the inventory must never
/// shrink across an authoring edit.
fn opaque_inline_count(canon: &CanonDoc) -> usize {
    let mut n = 0;
    for tb in &canon.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if matches!(inline, InlineNode::OpaqueInline(_)) {
                        n += 1;
                    }
                }
            }
        }
    }
    n
}

fn has_sdt(canon: &CanonDoc) -> bool {
    canon.blocks.iter().any(|tb| {
        matches!(&tb.block, BlockNode::Paragraph(p) if p.segments.iter().any(|s| {
            s.inlines.iter().any(|i| matches!(i, InlineNode::OpaqueInline(o) if matches!(o.kind, OpaqueKind::Sdt)))
        }))
    })
}

fn serialize_clean(doc: &Document) -> Vec<u8> {
    doc.serialize(&ExportOptions {
        mode: ExportMode::Redline,
        validator_level: ValidatorLevel::Blocking,
        validator: None,
    })
    .expect("serialize+validate Blocking-clean")
}

const STORE_ID: &str = "{4D2A5B3C-1E6F-4A8B-9C0D-2E3F4A5B6C7D}";

fn bind_step(block_id: NodeId, expect: &str, binding: Option<DataBinding>) -> EditStep {
    EditStep::WrapInContentControl {
        block_id,
        expect: expect.to_string(),
        semantic_hash: None,
        spec: SdtSpec {
            tag: Some("party".to_string()),
            alias: Some("Counterparty".to_string()),
            control: SdtControl::PlainText,
            binding,
        },
        rationale: None,
    }
}

// ─── happy path ───────────────────────────────────────────────────────────────

#[test]
fn data_bound_wrap_emits_databinding_and_authors_datastore_part() {
    let base = Document::parse(&make_docx("The Counterparty shall sign.")).expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);
    let binding = DataBinding {
        xpath: "/ns0:root[1]/ns0:party[1]".to_string(),
        store_item_id: STORE_ID.to_string(),
        prefix_mappings: Some("xmlns:ns0='urn:contract'".to_string()),
    };
    let edited = base
        .apply(&txn(vec![bind_step(
            block_id,
            "Counterparty",
            Some(binding),
        )]))
        .expect("apply");

    let bytes = serialize_clean(&edited);
    let archive = stemma::docx::DocxArchive::read(&bytes).expect("read out");
    let doc_xml = String::from_utf8(
        archive
            .get("word/document.xml")
            .expect("document.xml")
            .to_vec(),
    )
    .unwrap();

    // (1) the sdtPr carries the w:dataBinding with xpath + storeItemID.
    assert!(doc_xml.contains("<w:sdt"), "a w:sdt must be present");
    assert!(
        doc_xml.contains("<w:dataBinding "),
        "the sdtPr must carry a w:dataBinding; xml={doc_xml}"
    );
    assert!(
        doc_xml.contains(r#"w:xpath="/ns0:root[1]/ns0:party[1]""#),
        "the bound xpath must be on the dataBinding; xml={doc_xml}"
    );
    assert!(
        doc_xml.contains(&format!(r#"w:storeItemID="{STORE_ID}""#)),
        "the storeItemID must be on the dataBinding; xml={doc_xml}"
    );
    // spec order: w:id precedes w:dataBinding precedes the control kind (w:text).
    let id = doc_xml.find("<w:id ").expect("w:id");
    let db = doc_xml.find("<w:dataBinding ").expect("w:dataBinding");
    let text = doc_xml.find("<w:text").expect("w:text control kind");
    assert!(
        id < db && db < text,
        "sdtPr order: id < dataBinding < control kind"
    );
    // The wrapped run text survives inside the sdtContent.
    assert!(
        doc_xml.contains(">Counterparty<"),
        "wrapped text must survive"
    );

    // (2) the backing datastore part triad is authored.
    let names: Vec<String> = archive.list().map(|s| s.to_string()).collect();
    let item_path = names
        .iter()
        .find(|n| {
            n.starts_with("customXml/item")
                && n.ends_with(".xml")
                && !n.contains("itemProps")
                && !n.contains("_rels")
        })
        .unwrap_or_else(|| {
            panic!("a customXml/item*.xml data part must be authored; parts={names:?}")
        })
        .clone();
    let n = item_path
        .strip_prefix("customXml/item")
        .and_then(|s| s.strip_suffix(".xml"))
        .expect("item index");
    let props_path = format!("customXml/itemProps{n}.xml");
    let rels_path = format!("customXml/_rels/item{n}.xml.rels");
    assert!(
        names.contains(&props_path),
        "itemProps must be authored; parts={names:?}"
    );
    assert!(
        names.contains(&rels_path),
        "item rels must be authored; parts={names:?}"
    );

    // (3) the itemProps carries the storeItemID as its ds:itemID — this is what
    //     Word matches the binding against.
    let props = String::from_utf8(archive.get(&props_path).unwrap().to_vec()).unwrap();
    assert!(
        props.contains("datastoreItem"),
        "itemProps must be a ds:datastoreItem; props={props}"
    );
    assert!(
        props.contains(&format!(r#"ds:itemID="{STORE_ID}""#)),
        "itemProps ds:itemID must equal the binding's storeItemID; props={props}"
    );

    // (4) the data part is a well-formed root the xpath addresses.
    let data = String::from_utf8(archive.get(&item_path).unwrap().to_vec()).unwrap();
    assert!(
        data.contains("<root"),
        "datastore root derived from the xpath first step; data={data}"
    );

    // (5) content-types declare the itemProps part; (6) document.xml.rels links
    //     the datastore via a customXml relationship.
    let ct = String::from_utf8(archive.get("[Content_Types].xml").unwrap().to_vec()).unwrap();
    assert!(
        ct.contains("customXmlProperties+xml") && ct.contains(&format!("/{props_path}")),
        "itemProps content-type Override must be declared; ct={ct}"
    );
    let doc_rels = String::from_utf8(
        archive
            .get("word/_rels/document.xml.rels")
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(
        doc_rels.contains("relationships/customXml")
            && doc_rels.contains(&format!("../customXml/item{n}.xml")),
        "a customXml relationship to the datastore must be registered; rels={doc_rels}"
    );

    // The item's own rels links it to its itemProps.
    let item_rels = String::from_utf8(archive.get(&rels_path).unwrap().to_vec()).unwrap();
    assert!(
        item_rels.contains("customXmlProps") && item_rels.contains(&format!("itemProps{n}.xml")),
        "item rels must link item -> itemProps; rels={item_rels}"
    );
}

#[test]
fn data_bound_wrap_is_untracked_accept_equals_reject_equals_bound() {
    let base = Document::parse(&make_docx("The Counterparty shall sign.")).expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);
    let binding = DataBinding {
        xpath: "/contract/party".to_string(),
        store_item_id: STORE_ID.to_string(),
        prefix_mappings: None,
    };
    let edited = base
        .apply(&txn(vec![bind_step(
            block_id,
            "Counterparty",
            Some(binding),
        )]))
        .expect("apply");

    // IR-level: the bound control persists under both resolutions (no w:sdtChange).
    assert!(
        has_sdt(&edited.snapshot().canonical),
        "the edit produced a bound sdt"
    );
    let accepted = edited.project(Resolution::AcceptAll).expect("accept");
    let rejected = edited.project(Resolution::RejectAll).expect("reject");
    assert!(
        has_sdt(&accepted.snapshot().canonical),
        "accept-all keeps the bound control"
    );
    assert!(
        has_sdt(&rejected.snapshot().canonical),
        "reject-all keeps the bound control"
    );

    // Serialized-markup level (what Word reads): accept-all == reject-all == bound.
    let bound_xml = doc_xml(&serialize_clean(&edited));
    let accept_xml = doc_xml(&serialize_clean(&accepted));
    let reject_xml = doc_xml(&serialize_clean(&rejected));
    assert_eq!(
        accept_xml, bound_xml,
        "serialized accept-all == bound (untracked)"
    );
    assert_eq!(
        reject_xml, bound_xml,
        "serialized reject-all == bound (untracked)"
    );
    for (label, xml) in [
        ("bound", &bound_xml),
        ("accept", &accept_xml),
        ("reject", &reject_xml),
    ] {
        assert!(
            xml.contains("<w:dataBinding "),
            "[{label}] dataBinding survives"
        );
        assert!(
            !xml.contains("w:sdtChange"),
            "[{label}] no w:sdtChange envelope"
        );
        assert!(
            !xml.contains("<w:ins") && !xml.contains("<w:del"),
            "[{label}] not a tracked change"
        );
    }
}

#[test]
fn data_bound_wrap_does_not_shrink_opaque_inventory() {
    let base = Document::parse(&make_docx("The Counterparty shall sign.")).expect("parse");
    let canon = base.snapshot().canonical.clone();
    let before = opaque_inline_count(&canon);
    let block_id = first_block_id(&canon);
    let binding = DataBinding {
        xpath: "/contract/party".to_string(),
        store_item_id: STORE_ID.to_string(),
        prefix_mappings: None,
    };
    let edited = apply_transaction(
        &canon,
        &txn(vec![bind_step(block_id, "Counterparty", Some(binding))]),
    )
    .expect("apply")
    .0;
    let after = opaque_inline_count(&edited);
    assert!(
        after > before,
        "the wrap adds an sdt opaque inline (before={before}, after={after})"
    );
}

/// Two bindings sharing one storeItemID collapse to a single authored datastore
/// part (reuse / dedup) — the second binding's storeItemID already resolves.
#[test]
fn two_bindings_sharing_store_item_id_author_one_part() {
    let base = Document::parse(&make_docx("Party A and Party B both sign.")).expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);
    let mk = |expect: &str| {
        let binding = DataBinding {
            xpath: "/contract/party".to_string(),
            store_item_id: STORE_ID.to_string(),
            prefix_mappings: None,
        };
        bind_step(block_id.clone(), expect, Some(binding))
    };
    let edited = base
        .apply(&txn(vec![mk("Party A"), mk("Party B")]))
        .expect("apply");

    let archive = stemma::docx::DocxArchive::read(&serialize_clean(&edited)).expect("read");
    let data_parts = archive
        .list()
        .filter(|n| {
            n.starts_with("customXml/item")
                && n.ends_with(".xml")
                && !n.contains("itemProps")
                && !n.contains("_rels")
        })
        .count();
    assert_eq!(
        data_parts, 1,
        "shared storeItemID must author exactly one datastore part"
    );
}

// ─── fail-loud ──────────────────────────────────────────────────────────────

#[test]
fn empty_xpath_binding_fails_loud() {
    let base = Document::parse(&make_docx("The Counterparty shall sign.")).expect("parse");
    let canon = base.snapshot().canonical.clone();
    let block_id = first_block_id(&canon);
    let binding = DataBinding {
        xpath: "   ".to_string(),
        store_item_id: STORE_ID.to_string(),
        prefix_mappings: None,
    };
    let err = apply_transaction(
        &canon,
        &txn(vec![bind_step(block_id, "Counterparty", Some(binding))]),
    )
    .expect_err("empty xpath must fail");
    assert!(
        matches!(err, EditError::MalformedDataBinding { .. }),
        "got {err:?}"
    );
}

#[test]
fn empty_store_item_id_binding_fails_loud() {
    let base = Document::parse(&make_docx("The Counterparty shall sign.")).expect("parse");
    let canon = base.snapshot().canonical.clone();
    let block_id = first_block_id(&canon);
    let binding = DataBinding {
        xpath: "/contract/party".to_string(),
        store_item_id: "".to_string(),
        prefix_mappings: None,
    };
    let err = apply_transaction(
        &canon,
        &txn(vec![bind_step(block_id, "Counterparty", Some(binding))]),
    )
    .expect_err("empty storeItemID must fail");
    assert!(
        matches!(err, EditError::MalformedDataBinding { .. }),
        "got {err:?}"
    );
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
