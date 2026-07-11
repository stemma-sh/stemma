//! Spec-compliance tests: wire `w:id="0"` tracked-change revisions.
//!
//! THE DOMAIN RULE: `w:id="0"` is a legal Word revision id — real documents
//! carry `<w:ins w:id="0">`, `<w:del w:id="0">`, `<w:rPrChange w:id="0">`,
//! `<w:pPrChange w:id="0">`. The engine reserves INTERNAL `revision_id == 0`
//! as the legacy sentinel for pre-identity snapshots (reported, never
//! selectable). Conflating the two made a wild `<w:rPrChange w:id="0">`
//! enumerate as id 0 yet be REFUSED by the resolver — a violation of the
//! enumerate↔resolve agreement invariant. And two distinct `w:id="0"` changes
//! would collapse to one unaddressable identity.
//!
//! The fix mints a fresh, document-unique id at import for every carrier that
//! arrives with wire id 0. These tests pin the resulting contract straight
//! from the domain rule: after import every revision is enumerated with a
//! NONZERO, UNIQUE id; each is individually resolvable; accept-all and
//! reject-all followed by export→reimport leave ZERO pending revisions; and a
//! revision carrying a real (nonzero) wire id is left untouched.

use std::collections::HashSet;

use stemma::api::Document;
use stemma::tracked_model::ResolveSelectionAction;
use stemma::tracked_model::enumerate_revisions;
use stemma::{ExportOptions, Resolution};

/// Pack a minimal single-part DOCX around `body_inner`.
fn make_docx(body_inner: &str) -> Vec<u8> {
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

/// A paragraph whose run text is INSERTED under `<w:ins w:id="0">`.
const INS_ID0: &str = r#"<w:p><w:ins w:id="0" w:author="A" w:date="2026-01-01T00:00:00Z"><w:r><w:t>inserted</w:t></w:r></w:ins></w:p>"#;
/// A paragraph whose run text is DELETED under `<w:del w:id="0">`.
const DEL_ID0: &str = r#"<w:p><w:del w:id="0" w:author="A" w:date="2026-01-01T00:00:00Z"><w:r><w:delText>deleted</w:delText></w:r></w:del></w:p>"#;
/// A run whose bold was tracked via `<w:rPrChange w:id="0">` (previously unbold).
const RPRCHANGE_ID0: &str = r#"<w:p><w:r><w:rPr><w:b/><w:rPrChange w:id="0" w:author="A" w:date="2026-01-01T00:00:00Z"><w:rPr/></w:rPrChange></w:rPr><w:t>bolded</w:t></w:r></w:p>"#;
/// A paragraph whose centering was tracked via `<w:pPrChange w:id="0">` (previously default).
const PPRCHANGE_ID0: &str = r#"<w:p><w:pPr><w:jc w:val="center"/><w:pPrChange w:id="0" w:author="A" w:date="2026-01-01T00:00:00Z"><w:pPr/></w:pPrChange></w:pPr><w:r><w:t>centered</w:t></w:r></w:p>"#;

/// All four wire-id-0 carriers in one document.
fn all_carriers_body() -> String {
    format!("{INS_ID0}{DEL_ID0}{RPRCHANGE_ID0}{PPRCHANGE_ID0}")
}

fn enumerated_ids(doc: &Document) -> Vec<u32> {
    enumerate_revisions(&doc.snapshot().canonical)
        .into_iter()
        .map(|r| r.revision_id)
        .collect()
}

/// Every wire-id-0 carrier is enumerated with a NONZERO, document-UNIQUE id.
/// Before the fix the four carriers would surface internal id 0 (rPrChange /
/// pPrChange refused by the resolver, ins/del colliding on a single identity).
#[test]
fn wire_id_zero_carriers_enumerate_with_nonzero_unique_ids() {
    let doc = Document::parse(&make_docx(&all_carriers_body())).expect("parse");
    let ids = enumerated_ids(&doc);

    assert_eq!(
        ids.len(),
        4,
        "one revision per carrier (ins, del, rPrChange, pPrChange); got {ids:?}"
    );
    assert!(
        ids.iter().all(|&id| id != 0),
        "no carrier may keep the legacy sentinel id 0; got {ids:?}"
    );
    let unique: HashSet<u32> = ids.iter().copied().collect();
    assert_eq!(
        unique.len(),
        ids.len(),
        "each wire-0 carrier must get its OWN identity — no collision; got {ids:?}"
    );
}

/// THE agreement invariant, exercised through import: every id
/// `enumerate_revisions` surfaces is individually RESOLVABLE. The wire-id-0
/// class is the one this pins — a `<w:rPrChange w:id="0">` that enumerates but
/// refuses to resolve is exactly the bug (`InvalidRange` on selecting id 0).
#[test]
fn each_wire_id_zero_revision_is_individually_resolvable() {
    let doc = Document::parse(&make_docx(&all_carriers_body())).expect("parse");
    let ids = enumerated_ids(&doc);
    assert_eq!(ids.len(), 4, "fixture sanity: four revisions");

    for id in ids {
        for action in [
            ResolveSelectionAction::Accept,
            ResolveSelectionAction::Reject,
        ] {
            let resolved = doc.project(Resolution::Selective {
                ids: HashSet::from([id]),
                action,
            });
            assert!(
                resolved.is_ok(),
                "revision id {id} enumerated but could not be resolved ({action:?}) — \
                 the enumerate↔resolve agreement is broken for wire-id-0 markup"
            );
        }
    }
}

/// Accept-all → export → reimport leaves ZERO pending revisions. A wire-0
/// revision that survived export (because it was never resolvable) would
/// re-enumerate here.
#[test]
fn wire_id_zero_accept_all_export_reimport_enumerates_zero() {
    let doc = Document::parse(&make_docx(&all_carriers_body())).expect("parse");
    let accepted = doc
        .project(Resolution::AcceptAll)
        .expect("accept-all must succeed");
    let bytes = accepted
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let reimported = Document::parse(&bytes).expect("reimport");
    assert_eq!(
        enumerated_ids(&reimported),
        Vec::<u32>::new(),
        "after accept-all, export, and reimport no revision may remain"
    );
}

/// Reject-all → export → reimport leaves ZERO pending revisions.
#[test]
fn wire_id_zero_reject_all_export_reimport_enumerates_zero() {
    let doc = Document::parse(&make_docx(&all_carriers_body())).expect("parse");
    let rejected = doc
        .project(Resolution::RejectAll)
        .expect("reject-all must succeed");
    let bytes = rejected
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let reimported = Document::parse(&bytes).expect("reimport");
    assert_eq!(
        enumerated_ids(&reimported),
        Vec::<u32>::new(),
        "after reject-all, export, and reimport no revision may remain"
    );
}

/// A wire id 0 alongside a REAL (nonzero) wire id: the real id is left
/// untouched, the wire-0 carrier is minted ABOVE it (unique), and both resolve.
/// This guards against the fix accidentally renumbering already-identified
/// revisions or re-minting into a collision.
#[test]
fn mixed_wire_zero_and_real_id_revisions_coexist_uniquely() {
    // `<w:ins w:id="5">` (real id) precedes the four wire-0 carriers.
    let real_id_ins = r#"<w:p><w:ins w:id="5" w:author="B" w:date="2026-01-02T00:00:00Z"><w:r><w:t>realid</w:t></w:r></w:ins></w:p>"#;
    let body = format!("{real_id_ins}{}", all_carriers_body());
    let doc = Document::parse(&make_docx(&body)).expect("parse");
    let ids = enumerated_ids(&doc);

    assert_eq!(
        ids.len(),
        5,
        "real-id insertion + four wire-0 carriers; got {ids:?}"
    );
    assert!(
        ids.contains(&5),
        "the real wire id 5 must be preserved, not renumbered; got {ids:?}"
    );
    assert!(
        ids.iter().filter(|&&id| id != 5).all(|&id| id != 0),
        "the wire-0 carriers must be minted to nonzero ids; got {ids:?}"
    );
    let unique: HashSet<u32> = ids.iter().copied().collect();
    assert_eq!(
        unique.len(),
        ids.len(),
        "all five ids distinct; got {ids:?}"
    );

    for id in ids {
        assert!(
            doc.project(Resolution::Selective {
                ids: HashSet::from([id]),
                action: ResolveSelectionAction::Accept,
            })
            .is_ok(),
            "revision id {id} must be resolvable"
        );
    }
}
