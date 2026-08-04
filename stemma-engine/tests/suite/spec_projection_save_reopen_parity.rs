//! Spec-compliance: a projection equals its own save/reopen.
//!
//! `project(resolution)` and `parse(serialize(project(resolution)))` describe
//! the SAME document, so their canonical forms must agree — any field where
//! the in-memory projection and the reopened parse spell one rendered state
//! two ways is a latent equality bug (the recurring one-state-two-spellings
//! disease). Each test here pins a field where the two sides drifted on wild
//! documents:
//!
//! - segment partition after a projection-time paragraph JOIN on a
//!   literal-prefix paragraph (the join left donor+target segments split);
//! - a table's `structure_hash` after tracked row changes settle (the cached
//!   import-time digest described the pre-projection grid);
//! - a restored deleted field's `content_hash` (the restore rewrote the raw
//!   transport bytes but cleared the digest a fresh parse recomputes).

use std::io::Write as _;

use stemma::api::Document;
use stemma::domain::{BlockNode, InlineNode};
use stemma::{ExportOptions, Resolution};
use zip::write::FileOptions;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn docx_from_body(body: &str) -> Vec<u8> {
    let doc = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let ct = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let options: FileOptions = FileOptions::default();
        zip.start_file("[Content_Types].xml", options).unwrap();
        zip.write_all(ct.as_bytes()).unwrap();
        zip.start_file("_rels/.rels", options).unwrap();
        zip.write_all(rels.as_bytes()).unwrap();
        zip.start_file("word/_rels/document.xml.rels", options)
            .unwrap();
        zip.write_all(br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#).unwrap();
        zip.start_file("word/document.xml", options).unwrap();
        zip.write_all(doc.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

fn reopened(projection: &Document) -> Document {
    let bytes = projection
        .serialize(&ExportOptions::default())
        .expect("serialize projection");
    Document::parse(&bytes).expect("reopen projection")
}

/// Rejecting an INSERTED paragraph mark joins two paragraphs. The joined
/// paragraph is one revision-free paragraph, and a freshly-parsed document
/// spells that as ONE Normal segment — the in-memory projection must agree,
/// including on the literal-prefix path (a numbered-looking lead like
/// "2.7<tab>" routes the paragraph through prefix extraction, which used to
/// skip the segment merge entirely).
#[test]
fn spec_projected_join_coalesces_settled_segments_on_prefix_paragraph() {
    let body = r#"<w:p><w:pPr><w:rPr><w:ins w:id="7" w:author="A" w:date="2026-01-01T10:00:00Z"/></w:rPr></w:pPr><w:r><w:t xml:space="preserve">2.7</w:t></w:r><w:r><w:tab/><w:t xml:space="preserve">First part of the clause </w:t></w:r></w:p><w:p><w:r><w:t>continues after the joined mark.</w:t></w:r></w:p>"#;
    let doc = Document::parse(&docx_from_body(body)).expect("parse fixture");
    let projected = doc
        .project(Resolution::RejectAll)
        .expect("project reject-all");
    let reparsed = reopened(&projected);

    let projected_snapshot = projected.snapshot();
    let reparsed_snapshot = reparsed.snapshot();
    let partition = |doc: &stemma::domain::CanonDoc| -> Vec<usize> {
        doc.blocks
            .iter()
            .filter_map(|tracked| match &tracked.block {
                BlockNode::Paragraph(paragraph) => Some(paragraph.segments.len()),
                _ => None,
            })
            .collect()
    };
    let in_memory = partition(&projected_snapshot.canonical);
    let persisted = partition(&reparsed_snapshot.canonical);
    // Precondition: the join happened (two paragraphs became one).
    assert_eq!(in_memory.len(), 1, "reject joins the two paragraphs");
    assert_eq!(
        in_memory, persisted,
        "projected segment partition equals its own save/reopen"
    );
    assert_eq!(in_memory[0], 1, "a settled join is ONE Normal segment");
}

/// A tracked-inserted row rejects away; the table's geometry digest must
/// describe the PROJECTED grid, not the import-time one.
#[test]
fn spec_projected_table_structure_hash_matches_reopen() {
    let row = |text: &str, tracked: bool| -> String {
        let tr_pr = if tracked {
            r#"<w:trPr><w:ins w:id="21" w:author="A" w:date="2026-01-01T10:00:00Z"/></w:trPr>"#
        } else {
            ""
        };
        format!(
            r#"<w:tr>{tr_pr}<w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:tc></w:tr>"#
        )
    };
    let body = format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid>{}{}</w:tbl><w:p><w:r><w:t>after table</w:t></w:r></w:p>"#,
        row("kept row", false),
        row("inserted row", true),
    );
    let doc = Document::parse(&docx_from_body(&body)).expect("parse fixture");
    let projected = doc
        .project(Resolution::RejectAll)
        .expect("project reject-all");
    let reparsed = reopened(&projected);

    let projected_snapshot = projected.snapshot();
    let reparsed_snapshot = reparsed.snapshot();
    let table_state = |doc: &stemma::domain::CanonDoc| -> (usize, String) {
        doc.blocks
            .iter()
            .find_map(|tracked| match &tracked.block {
                BlockNode::Table(table) => Some((table.rows.len(), table.structure_hash.clone())),
                _ => None,
            })
            .expect("fixture keeps its table")
    };
    let (rows_in_memory, hash_in_memory) = table_state(&projected_snapshot.canonical);
    let (rows_persisted, hash_persisted) = table_state(&reparsed_snapshot.canonical);
    assert_eq!(rows_in_memory, 1, "reject drops the inserted row");
    assert_eq!(rows_persisted, 1, "the reopened table agrees on the grid");
    assert_eq!(
        hash_in_memory, hash_persisted,
        "structure_hash describes the projected grid, not the import-time one"
    );
}

/// Rejecting a deleted complex field restores it (delInstrText → instrText,
/// delText → t). The restored raw bytes are the opaque's transport
/// representation, and `content_hash` names those bytes — a fresh parse
/// computes it, so the in-memory restore must too.
#[test]
fn spec_restored_field_content_hash_matches_reopen() {
    let body = r#"<w:p><w:del w:id="10" w:author="A" w:date="2026-01-01T10:00:00Z"><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:delInstrText xml:space="preserve"> PAGE </w:delInstrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:delText>4</w:delText></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:del></w:p><w:p><w:r><w:t>Body stays.</w:t></w:r></w:p>"#;
    let doc = Document::parse(&docx_from_body(body)).expect("parse fixture");
    let projected = doc
        .project(Resolution::RejectAll)
        .expect("project reject-all");
    let reparsed = reopened(&projected);

    let projected_snapshot = projected.snapshot();
    let reparsed_snapshot = reparsed.snapshot();
    let hashes = |doc: &stemma::domain::CanonDoc| -> Vec<Option<String>> {
        let BlockNode::Paragraph(paragraph) = &doc.blocks[0].block else {
            panic!("first block is the field paragraph");
        };
        paragraph
            .segments
            .iter()
            .flat_map(|segment| &segment.inlines)
            .filter_map(|inline| match inline {
                InlineNode::OpaqueInline(opaque) => Some(opaque.content_hash.clone()),
                _ => None,
            })
            .collect()
    };
    let in_memory = hashes(&projected_snapshot.canonical);
    let persisted = hashes(&reparsed_snapshot.canonical);
    assert!(
        !in_memory.is_empty(),
        "the restored field is opaque inlines"
    );
    assert_eq!(
        in_memory, persisted,
        "restored raw digests equal a fresh parse of the same bytes"
    );
    assert!(
        in_memory.iter().all(Option::is_some),
        "every restored opaque names its transport bytes"
    );
}

/// §17.7.4.17 in a NOTE story: a footnote paragraph without an explicit
/// pStyle resolves its effective paragraph properties through the DEFAULT
/// paragraph style — the same rule as the body path — and the spelling
/// survives projection and save/reopen. (Story paragraphs used to resolve
/// against a bare None and spelled effective spacing None where the body
/// rule, the projection re-resolvers, and a fresh parse all spell the
/// resolved value.)
#[test]
fn spec_note_paragraph_resolves_default_style_spacing_like_body() {
    let styles = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:pPrDefault/></w:docDefaults><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:pPr><w:spacing w:before="240"/></w:pPr></w:style></w:styles>"#;
    let footnotes = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:footnote w:id="1"><w:p><w:r><w:t>A note without pStyle.</w:t></w:r></w:p></w:footnote></w:footnotes>"#;
    let document = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Body text with a note</w:t><w:footnoteReference w:id="1"/></w:r></w:p><w:sectPr/></w:body></w:document>"#;
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/word/footnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"/></Types>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId6" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes" Target="footnotes.xml"/></Relationships>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let options: FileOptions = FileOptions::default();
        let mut write = |name: &str, content: &str| {
            zip.start_file(name, options).unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        };
        write("[Content_Types].xml", content_types);
        write("_rels/.rels", rels);
        write("word/_rels/document.xml.rels", doc_rels);
        write("word/document.xml", document);
        write("word/styles.xml", styles);
        write("word/footnotes.xml", footnotes);
        zip.finish().unwrap();
    }

    let note_spacing_before = |doc: &Document| -> Option<i64> {
        let snapshot = doc.snapshot();
        let note = snapshot
            .canonical
            .footnotes
            .iter()
            .find(|note| note.id == "1")
            .expect("fixture note");
        let BlockNode::Paragraph(paragraph) = &note.blocks[0].block else {
            panic!("note holds a paragraph");
        };
        paragraph
            .spacing
            .as_ref()
            .and_then(|spacing| spacing.before)
            .map(i64::from)
    };
    let doc = Document::parse(&buf).expect("parse fixture");
    assert_eq!(
        note_spacing_before(&doc),
        Some(240),
        "import resolves the note paragraph's spacing through the default style"
    );
    let projected = doc
        .project(Resolution::AcceptAll)
        .expect("project accept-all");
    assert_eq!(note_spacing_before(&projected), Some(240));
    let reparsed = reopened(&projected);
    assert_eq!(
        note_spacing_before(&reparsed),
        Some(240),
        "and the spelling survives save/reopen"
    );
}

/// A numbered paragraph's rendered label is a view over its position in the
/// list; rejecting an INSERTED earlier item (or accepting a deletion of one)
/// removes that item, and the survivors' labels must re-derive — "1." where
/// the import-time counter said "2." — exactly as a save/reopen would spell
/// them.
#[test]
fn spec_projected_list_labels_rederive_after_item_removal() {
    let numbering = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#;
    let numbered = |tracked: bool, text: &str| -> String {
        let (open, close) = if tracked {
            (
                r#"<w:ins w:id="31" w:author="A" w:date="2026-01-01T10:00:00Z">"#,
                "</w:ins>",
            )
        } else {
            ("", "")
        };
        let mark = if tracked {
            r#"<w:rPr><w:ins w:id="32" w:author="A" w:date="2026-01-01T10:00:00Z"/></w:rPr>"#
        } else {
            ""
        };
        format!(
            r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr>{mark}</w:pPr>{open}<w:r><w:t>{text}</w:t></w:r>{close}</w:p>"#
        )
    };
    let body = format!(
        "{}{}",
        numbered(true, "A pending first item"),
        numbered(false, "The settled second item")
    );
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/></Types>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId7" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/></Relationships>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let options: FileOptions = FileOptions::default();
        let mut write = |name: &str, content: &str| {
            zip.start_file(name, options).unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        };
        write("[Content_Types].xml", content_types);
        write("_rels/.rels", rels);
        write("word/_rels/document.xml.rels", doc_rels);
        write("word/document.xml", &document);
        write("word/numbering.xml", numbering);
        zip.finish().unwrap();
    }

    let labels = |doc: &Document| -> Vec<String> {
        let snapshot = doc.snapshot();
        snapshot
            .canonical
            .blocks
            .iter()
            .filter_map(|tracked| match &tracked.block {
                BlockNode::Paragraph(paragraph) => paragraph
                    .numbering
                    .as_ref()
                    .map(|numbering| numbering.synthesized_text.clone()),
                _ => None,
            })
            .collect()
    };
    let doc = Document::parse(&buf).expect("parse fixture");
    assert_eq!(labels(&doc), vec!["1.".to_string(), "2.".to_string()]);

    let rejected = doc
        .project(Resolution::RejectAll)
        .expect("project reject-all");
    let in_memory = labels(&rejected);
    assert_eq!(
        in_memory,
        vec!["1.".to_string()],
        "the survivor re-derives to the first label once the pending item is gone"
    );
    let reparsed = reopened(&rejected);
    assert_eq!(labels(&reparsed), in_memory, "and save/reopen agrees");
}

/// §17.13.5.29 + §17.9.18: a paragraph that EXPLICITLY suppresses its
/// style's numbering (`numId=0`) carries that suppression in its direct
/// pPr — so a tracked formatting change's previous-state record must carry
/// it too. Dropping it makes a reject "restore" a paragraph with no direct
/// numPr, resurrecting the style's list and swapping the rendered label
/// (a literal "64." became an auto-numbered item on a wild document after
/// an alignment-only edit was rejected).
#[test]
fn spec_pprchange_previous_state_preserves_numbering_suppression() {
    use stemma::edit::*;

    let styles = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:pPrDefault/></w:docDefaults><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style><w:style w:type="paragraph" w:styleId="Clause"><w:name w:val="Clause"/><w:basedOn w:val="Normal"/><w:pPr><w:numPr><w:numId w:val="11"/></w:numPr></w:pPr></w:style></w:styles>"#;
    let numbering = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="7"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/><w:pPr><w:ind w:left="1492" w:hanging="706"/></w:pPr></w:lvl></w:abstractNum><w:num w:numId="11"><w:abstractNumId w:val="7"/></w:num></w:numbering>"#;
    let document = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:pPr><w:pStyle w:val="Clause"/><w:numPr><w:ilvl w:val="0"/><w:numId w:val="0"/></w:numPr><w:ind w:firstLine="709"/></w:pPr><w:r><w:t xml:space="preserve">64.</w:t></w:r><w:r><w:tab/><w:t>The clause body with a literal label lives here.</w:t></w:r></w:p><w:p><w:r><w:t>A second untouched paragraph.</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/></Types>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId7" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/></Relationships>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let options: FileOptions = FileOptions::default();
        let mut write = |name: &str, content: &str| {
            zip.start_file(name, options).unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        };
        write("[Content_Types].xml", content_types);
        write("_rels/.rels", rels);
        write("word/_rels/document.xml.rels", doc_rels);
        write("word/document.xml", document);
        write("word/styles.xml", styles);
        write("word/numbering.xml", numbering);
        zip.finish().unwrap();
    }

    let doc = Document::parse(&buf).expect("parse fixture");
    let target_id = {
        let snapshot = doc.snapshot();
        let BlockNode::Paragraph(paragraph) = &snapshot.canonical.blocks[0].block else {
            panic!("first block is the clause paragraph");
        };
        assert!(
            paragraph.numbering.is_none(),
            "explicit numId=0 suppresses the style's list at import"
        );
        paragraph.id.clone()
    };
    let edited = doc
        .apply(&EditTransaction {
            steps: vec![EditStep::SetParagraphFormatting {
                block_id: target_id,
                semantic_hash: None,
                patch: ParagraphFormattingPatch {
                    align: Some(stemma::domain::Alignment::Right),
                    ..Default::default()
                },
                rationale: None,
            }],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: stemma::RevisionInfo {
                revision_id: 900_001,
                identity: 0,
                author: Some("Reviewer".to_string()),
                date: Some("2026-02-01T10:00:00Z".to_string()),
                apply_op_id: None,
            },
        })
        .expect("apply tracked alignment change");

    // The wire record must carry the suppression as part of the previous pPr.
    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize edited");
    let xml = {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).unwrap();
        let mut out = String::new();
        std::io::Read::read_to_string(&mut archive.by_name("word/document.xml").unwrap(), &mut out)
            .unwrap();
        out
    };
    let change_at = xml.find("<w:pPrChange").expect("edit produced a pPrChange");
    let inner = &xml[change_at..xml[change_at..].find("</w:pPrChange>").unwrap() + change_at];
    assert!(
        inner.contains(r#"<w:numId w:val="0""#),
        "the previous-state pPr carries the explicit numbering suppression, got: {inner}"
    );

    // Rejecting the change restores the ORIGINAL spelling — the literal
    // label, not the style's auto number — in memory and through a reopen.
    let label_state = |doc: &Document| -> (Option<String>, bool) {
        let snapshot = doc.snapshot();
        let BlockNode::Paragraph(paragraph) = &snapshot.canonical.blocks[0].block else {
            panic!("first block stays the clause paragraph");
        };
        (
            paragraph.literal_prefix.as_ref().map(|s| s.to_string()),
            paragraph.numbering.is_some(),
        )
    };
    let rejected = edited
        .project(Resolution::RejectAll)
        .expect("project reject-all");
    let in_memory = label_state(&rejected);
    assert!(!in_memory.1, "reject must not resurrect the style's list");
    let reparsed = reopened(&rejected);
    assert_eq!(
        label_state(&reparsed),
        in_memory,
        "and the spelling survives save/reopen"
    );
}
