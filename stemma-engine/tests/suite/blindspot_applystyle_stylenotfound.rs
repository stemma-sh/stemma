//! Blindspot regression: `ApplyStyle` to a dangling (nonexistent) style id must
//! FAIL, not silently succeed.
//!
//! DOMAIN CONTRACT (the verb's own documentation, source of truth):
//!   - `stemma-engine/src/edit/verbs/styles.rs` lines 26-32: "Style **existence**:
//!     `apply_transaction` holds only a `&CanonDoc` ... This verb CANNOT validate
//!     that `style_id` exists — that validation defers to the package-aware
//!     caller (the runtime, which has the `DocxPackage` and its
//!     `word/styles.xml`). The `StyleNotFound` variant is the error that caller
//!     emits. This verb NEVER silently accepts an unknown style as valid output."
//!   - `stemma-engine/src/edit/mod.rs` lines 1921-1925, `EditError::StyleNotFound`:
//!     "`ApplyStyle` named a style ID that does not exist in the document's style
//!     table (`word/styles.xml`). Emitted by the **package-aware caller** (the
//!     runtime) ... No silent acceptance of a dangling style — the missing id is
//!     surfaced verbatim."
//!
//! So the designed, documented behavior is: when the public, package-aware
//! `Document::apply` (-> `EditSnapshot::apply`, which holds the `DocxPackage`
//! and thus `word/styles.xml`) is asked to apply a pStyle id that the style
//! table does not define, it MUST return `Err`. This is also the prime-directive
//! "no silent fallbacks": authoring a dangling pStyle reference and reporting
//! success is exactly the "continuing in an unknown state" the contract forbids.
//!
//! This test drives the public, package-aware path (`Document::apply`) — the
//! caller the contract names as responsible for the check — and asserts it
//! refuses a dangling style. If it returns `Ok`, the documented validation is
//! not wired up (the `StyleNotFound` variant is never constructed) and we have
//! pinpointed the defect.

use stemma::api::Document;
use stemma::domain::{NodeId, RevisionInfo};
use stemma::edit::{EditStep, EditTransaction, MaterializationMode};

// ─── Fixture: minimal plain-paragraph DOCX (verbatim from
// `edit_fidelity_invariants.rs:29-62`). Note: this minimal package carries NO
// `word/styles.xml`, so EVERY style id is dangling — any `ApplyStyle` target is
// guaranteed not to exist in the (absent) style table. ───────────────────────
fn make_test_docx(paragraphs: &[&str]) -> Vec<u8> {
    let mut document_xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#,
    );
    for para in paragraphs {
        document_xml.push_str(&format!(r#"<w:p><w:r><w:t>{para}</w:t></w:r></w:p>"#));
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

fn txn(steps: Vec<EditStep>, mode: MaterializationMode) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: mode,
        revision: RevisionInfo {
            revision_id: 1,
            author: Some("Blindspot".to_string()),
            date: Some("2026-06-06T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

/// `ApplyStyle` to a style id that does not exist in `word/styles.xml` must be
/// REFUSED by the package-aware public caller (`Document::apply`).
#[test]
fn apply_style_to_dangling_style_id_is_refused() {
    let bytes = make_test_docx(&["Hello"]);
    let doc = Document::parse(&bytes).expect("parse minimal docx");

    // The first (only) top-level paragraph, by its stable block id.
    let block_id = {
        let view = doc.read();
        view.blocks
            .first()
            .expect("one paragraph block")
            .id
            .to_string()
    };

    // A style id that is guaranteed dangling: this package has no
    // `word/styles.xml`, so no style table defines it.
    let dangling = "ThisStyleDoesNotExist123";

    let transaction = txn(
        vec![EditStep::ApplyStyle {
            block_id: NodeId::new(block_id.clone()),
            semantic_hash: None,
            style_id: dangling.to_string(),
            rationale: None,
        }],
        MaterializationMode::TrackedChange,
    );

    let result = doc.apply(&transaction);
    let outcome = match &result {
        Ok(_) => "Ok(Document)".to_string(),
        Err(e) => format!("Err({e:?})"),
    };

    // DOMAIN-CORRECT POSTCONDITION (styles.rs §"style existence" + mod.rs
    // EditError::StyleNotFound doc + prime directive "no silent fallbacks"):
    // applying a pStyle the style table does not define must NOT succeed.
    assert!(
        result.is_err(),
        "ApplyStyle to dangling style '{dangling}' on block '{block_id}' returned Ok — \
         the package-aware caller authored a pStyle reference into a style table that \
         does not define it, a silent fallback. The verb's own contract \
         (verbs/styles.rs §style-existence + EditError::StyleNotFound, mod.rs) \
         requires this to be refused with an error. Got: {outcome}",
    );
}
