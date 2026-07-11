//! SENTINEL — adding structural numbering to a paragraph that already carries a
//! hoisted literal prefix must NOT drop the prefix text from the exported bytes.
//!
//! # The bug this pins
//!
//! A non-numbered paragraph whose text begins with a label-shaped token (e.g.
//! `"1) first item"`) hoists `"1) "` into `literal_prefix` at import (rendered
//! inline, stripped from the body runs). The serializer used to SUPPRESS a
//! materialized `literal_prefix` whenever the paragraph carried structural
//! numbering, on the theory that Word regenerates the label from numbering.xml.
//!
//! Since 93c9ae4 the importer never hoists a label on a numbered paragraph, so
//! import cannot produce the `literal_prefix + numbering` combination. But an
//! EDIT can: `SetBlockRangeAttr` promotes a paragraph to a numbered role and
//! `copy_paragraph_formatting_from_exemplar` copies the exemplar's `numbering`
//! while deliberately leaving `literal_prefix` (text content) intact. The
//! serializer's numbering-gated suppression then silently dropped the `"1) "`
//! bytes from the redline — lawyer-visible on BOTH accept and reject.
//!
//! The fix: the prefix is real body text Word shows IN ADDITION to the structural
//! number (93c9ae4's model rule), so it is re-materialized into the text stream
//! unconditionally; the numbering renders its own label separately.

use stemma::api::Document;
use stemma::edit::{EditStep, EditTransaction, MaterializationMode};
use stemma::vocabulary::{NumberingSource, extract_vocabulary};
use stemma::{DocxRuntime, ExportMode, ExportOptions, RevisionInfo, SimpleRuntime};

use crate::common;

const NUMBERING_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#;

fn pack(document_xml: &str) -> Vec<u8> {
    use std::io::Write;
    use zip::write::FileOptions;

    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId10" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/></Relationships>"#;

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
        zip.start_file("word/numbering.xml", opts).unwrap();
        zip.write_all(NUMBERING_XML.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

fn document(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    )
}

fn revision() -> RevisionInfo {
    RevisionInfo {
        revision_id: 5,
        author: Some("prefix-numbering".to_string()),
        date: Some("2026-07-07T00:00:00Z".to_string()),
        apply_op_id: None,
    }
}

/// Concatenate every `<w:t>` text node in `word/document.xml`, in document
/// order — the visible text the bytes carry (so a prefix that survives as a run,
/// separated across runs, is still asserted as a contiguous string).
fn visible_text(docx: &[u8]) -> String {
    use std::io::Read;
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(docx.to_vec())).expect("zip");
    let mut xml = String::new();
    z.by_name("word/document.xml")
        .expect("document.xml")
        .read_to_string(&mut xml)
        .expect("read");
    let mut out = String::new();
    let mut rest = xml.as_str();
    while let Some(open) = rest.find("<w:t") {
        rest = &rest[open..];
        let Some(gt) = rest.find('>') else { break };
        // Skip self-closing <w:tab/> etc. — only <w:t> and <w:t ...> carry text.
        if !rest.starts_with("<w:t>") && !rest.starts_with("<w:t ") {
            rest = &rest[gt + 1..];
            continue;
        }
        let body = &rest[gt + 1..];
        let Some(close) = body.find("</w:t>") else {
            break;
        };
        out.push_str(&body[..close]);
        rest = &body[close + "</w:t>".len()..];
    }
    out
}

/// A numbered paragraph (numId=1) supplying the "numbered role" for the promotion
/// edit, followed by a plain paragraph whose text begins with a literal "1) "
/// label (hoisted at import).
const BODY: &str = concat!(
    "<w:p><w:pPr><w:numPr><w:ilvl w:val=\"0\"/><w:numId w:val=\"1\"/></w:numPr></w:pPr>",
    "<w:r><w:t>existing numbered item</w:t></w:r></w:p>",
    "<w:p><w:r><w:t>1) first item body</w:t></w:r></w:p>"
);

const PREFIX_TEXT: &str = "1) first item body";

#[test]
fn adding_numbering_to_a_hoisted_prefix_paragraph_keeps_the_prefix_bytes() {
    let rt = SimpleRuntime::new();
    let import = rt.import_docx(&pack(&document(BODY))).expect("import");
    let canonical = &*import.canonical;

    // Precondition: the second paragraph hoisted its "1) " label and is NOT yet
    // numbered.
    let target = common::all_paragraphs(canonical)[1];
    assert_eq!(
        target.literal_prefix.as_deref(),
        Some("1)"),
        "precondition: the plain paragraph hoisted its literal prefix"
    );
    assert!(
        target.numbering.is_none(),
        "precondition: the target paragraph is not yet numbered"
    );
    let target_id = target.id.clone();

    // The numbered role the first paragraph exposes.
    let vocab = extract_vocabulary(canonical);
    let numbered_role_id = vocab
        .paragraph_roles
        .iter()
        .find(|r| r.has_numbering && r.numbering_source == Some(NumberingSource::Auto))
        .expect("doc must expose a numbered role")
        .id
        .clone();

    // Promote the "1) " paragraph to the numbered role, tracked. This is the
    // exemplar-copy path that copies `numbering` while keeping `literal_prefix`.
    let tx = EditTransaction {
        steps: vec![EditStep::SetBlockRangeAttr {
            from_block_id: target_id.clone(),
            to_block_id: target_id.clone(),
            role: numbered_role_id,
            rationale: Some("make it a numbered item".to_string()),
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: revision(),
    };
    let applied = rt.apply_edit(&import.doc_handle, &tx).expect("apply_edit");
    assert!(applied.applied, "the promotion edit must apply");

    // The reachable combo now exists in the live model.
    let live_target = common::all_paragraphs(&applied.canonical)[1];
    assert!(
        live_target.numbering.is_some() && live_target.literal_prefix.is_some(),
        "the edit must produce a paragraph carrying BOTH numbering and the hoisted \
         literal prefix (numbering={:?}, literal_prefix={:?})",
        live_target.numbering.is_some(),
        live_target.literal_prefix,
    );

    // PENDING bytes: the "1) first item body" text must NOT be suppressed.
    let redline = rt
        .export_docx(&import.doc_handle, ExportMode::Redline)
        .expect("export redline");
    assert!(
        visible_text(&redline).contains(PREFIX_TEXT),
        "the redline export dropped the '1) ' literal prefix text: {:?}",
        visible_text(&redline)
    );

    // BOTH resolutions keep the prefix text in the serialized bytes.
    let redlined_doc = Document::parse(&redline).expect("reparse redline");
    let accepted = redlined_doc
        .read_accepted()
        .expect("accept")
        .serialize(&ExportOptions::default())
        .expect("serialize accepted");
    assert!(
        visible_text(&accepted).contains(PREFIX_TEXT),
        "accept: the '1) ' prefix text must survive into the bytes: {:?}",
        visible_text(&accepted)
    );
    let rejected = redlined_doc
        .read_rejected()
        .expect("reject")
        .serialize(&ExportOptions::default())
        .expect("serialize rejected");
    assert!(
        visible_text(&rejected).contains(PREFIX_TEXT),
        "reject: the '1) ' prefix text must survive into the bytes: {:?}",
        visible_text(&rejected)
    );
}
