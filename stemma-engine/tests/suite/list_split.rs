//! Integration tests for the list-SPLIT op (`verbs::numbering`'s `Split`),
//! which authors a BRAND-NEW list definition in `word/numbering.xml` and
//! re-points the split tail at it so the tail renumbers from 1 independently.
//!
//! Domain rules under test (the invariants the breadth task `ex-list-split`
//! needs):
//!
//! 1. `accept_all` == TWO lists. The head (items before the split) keeps the
//!    original `numId`; the tail (the split item onward) points at a fresh
//!    `numId` that did not exist in the base. Word renumbers the tail from 1
//!    because it is a distinct `w:num` with its own counter.
//! 2. `reject_all` == the ORIGINAL single list: every item back on the base
//!    `numId`.
//! 3. The authored `word/numbering.xml` is well-formed: the new `numId` has a
//!    `<w:num>` pointing at a fresh `<w:abstractNum>` whose level formats CLONE
//!    the source list's (decimal here) — no dangling `abstractNumId`.
//! 4. Validator-clean on the accepted output.
//! 5. Opaque preservation: the paragraph text survives the split unchanged.
//!
//! The fixture carries a real `word/numbering.xml` with a decimal list
//! (`numId=1`, `abstractNumId=0`) so the split has a resolvable source to clone.

use std::io::Read;

use stemma::api::Document;
use stemma::domain::{BlockNode, CanonDoc, NodeId, RevisionInfo};
use stemma::edit::{EditStep, EditTransaction, MaterializationMode, NumberingChange};
use stemma::{ExportOptions, Resolution};

// ─── Fixture: a single decimal list (numId=1) of N items ─────────────────────

fn make_decimal_list_docx(items: &[&str]) -> Vec<u8> {
    let mut document_xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#,
    );
    for text in items {
        document_xml.push_str(
            r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr>"#,
        );
        document_xml.push_str(&format!(r#"<w:r><w:t>{text}</w:t></w:r></w:p>"#));
    }
    document_xml.push_str("<w:sectPr/></w:body></w:document>");

    // abstractNum 0 = decimal at ilvl 0; numId=1 references it.
    let numbering_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:nsid w:val="0A0B0C0D"/><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/><w:lvlJc w:val="left"/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#;

    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId10" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/></Relationships>"#;

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
        zip.start_file("word/numbering.xml", opts).unwrap();
        zip.write_all(numbering_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

/// A single decimal list on `numId=1` whose `word/numbering.xml` ALSO defines a
/// SECOND, orphan `numId=2` that no body paragraph references (a real-document
/// shape: Word authors a `<w:num>` per list instance, leaving many defined but
/// unreferenced). `max(body numId) + 1 = 2` therefore collides with the orphan
/// definition — the exact wild-document trap the old body-scan allocator fell
/// into. The correct allocator reads the numbering PART (authoritative for both
/// numIds) and allocates `3`.
fn make_decimal_list_docx_with_orphan_numid2(items: &[&str]) -> Vec<u8> {
    let mut document_xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#,
    );
    for text in items {
        document_xml.push_str(
            r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr>"#,
        );
        document_xml.push_str(&format!(r#"<w:r><w:t>{text}</w:t></w:r></w:p>"#));
    }
    document_xml.push_str("<w:sectPr/></w:body></w:document>");

    // numId=1 -> abstractNum 0 (body list); numId=2 -> abstractNum 1 (ORPHAN:
    // defined, referenced by nothing in the body). Both decimal.
    let numbering_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:nsid w:val="0A0B0C0D"/><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/><w:lvlJc w:val="left"/></w:lvl></w:abstractNum><w:abstractNum w:abstractNumId="1"><w:nsid w:val="1A2B3C4D"/><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/><w:lvlJc w:val="left"/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num><w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num></w:numbering>"#;

    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId10" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/></Relationships>"#;

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
        zip.start_file("word/numbering.xml", opts).unwrap();
        zip.write_all(numbering_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

/// Body decimal list on `numId=1`, plus a FOOTNOTE story whose paragraph is a
/// list item on `numId=2`. `numId=2` is therefore referenced ONLY from the
/// footnote — never from the body — and sits ABOVE the body's max referenced
/// numId (1). The old allocator scanned the body only (`max(body)+1 = 2`) and so
/// collided with the story-referenced list; the fix allocates from the numbering
/// part, which defines both, and picks `3`.
fn make_list_docx_with_footnote_list_numid2(items: &[&str]) -> Vec<u8> {
    let mut body = String::new();
    for (i, text) in items.iter().enumerate() {
        body.push_str(
            r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr>"#,
        );
        body.push_str(&format!(r#"<w:r><w:t>{text}</w:t></w:r>"#));
        // Attach the footnote reference to the first item.
        if i == 0 {
            body.push_str(
                r#"<w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:footnoteReference w:id="1"/></w:r>"#,
            );
        }
        body.push_str("</w:p>");
    }
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );

    // The footnote story: its body paragraph is a list item on numId=2.
    let footnotes_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote><w:footnote w:id="1"><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:footnoteRef/></w:r><w:r><w:t>A cited note item.</w:t></w:r></w:p></w:footnote></w:footnotes>"#;

    // numId=1 -> abstractNum 0 (body); numId=2 -> abstractNum 1 (footnote story).
    let numbering_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:nsid w:val="0A0B0C0D"/><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/><w:lvlJc w:val="left"/></w:lvl></w:abstractNum><w:abstractNum w:abstractNumId="1"><w:nsid w:val="1A2B3C4D"/><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/><w:lvlJc w:val="left"/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num><w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num></w:numbering>"#;

    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/><Override PartName="/word/footnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId10" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/><Relationship Id="rId11" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes" Target="footnotes.xml"/></Relationships>"#;

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
        zip.start_file("word/numbering.xml", opts).unwrap();
        zip.write_all(numbering_xml.as_bytes()).unwrap();
        zip.start_file("word/footnotes.xml", opts).unwrap();
        zip.write_all(footnotes_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

/// A fixture WITHOUT a numbering.xml part (no list can exist). Used to assert
/// the bootstrap path's behavior — see the bootstrap test below.
fn make_no_numbering_docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Plain paragraph</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"></Relationships>"#;
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

fn parse(items: &[&str]) -> (Document, Vec<String>) {
    let doc = Document::parse(&make_decimal_list_docx(items)).expect("parse decimal-list docx");
    let ids = doc.read().blocks.iter().map(|b| b.id.to_string()).collect();
    (doc, ids)
}

fn split_step(block_id: &str) -> EditTransaction {
    EditTransaction {
        steps: vec![EditStep::SetParagraphNumbering {
            block_id: NodeId::from(block_id),
            semantic_hash: None,
            change: NumberingChange::Split,
            rationale: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("Counsel".to_string()),
            date: Some("2026-06-05T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

fn num_id_of(doc: &CanonDoc, block_idx: usize) -> Option<u32> {
    match &doc.blocks[block_idx].block {
        BlockNode::Paragraph(p) => p.numbering.as_ref().map(|n| n.num_id),
        other => panic!("block {block_idx} not a paragraph: {other:?}"),
    }
}

fn num_id_after(doc: &Document, resolution: Resolution, block_idx: usize) -> Option<u32> {
    let resolved = doc.project(resolution).expect("project");
    num_id_of(&resolved.snapshot().canonical, block_idx)
}

/// Extract one part's bytes from a DOCX zip.
fn part_bytes(docx: &[u8], part: &str) -> Option<Vec<u8>> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(docx)).expect("open zip");
    let mut f = zip.by_name(part).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).expect("read part");
    Some(buf)
}

/// A DIRECT split authors no revision: the tail moves to the new list with no
/// tracked pPrChange on any paragraph, so the edited document already reads as
/// accepting the tracked split would.
#[test]
fn direct_split_authors_no_tracked_change() {
    let (doc, ids) = parse(&["One", "Two", "Three", "Four", "Five"]);
    let mut step = split_step(&ids[2]);
    step.materialization_mode = MaterializationMode::Direct;
    let edited = doc.apply(&step).expect("direct split applies");
    let canonical = &edited.snapshot().canonical;
    for (index, block) in canonical.blocks.iter().enumerate() {
        if let BlockNode::Paragraph(p) = &block.block {
            assert!(
                p.formatting_change.is_none(),
                "paragraph {index} carries a tracked change after a direct split"
            );
        }
    }
    assert_eq!(num_id_of(canonical, 1), Some(1), "the head keeps its list");
    assert_ne!(
        num_id_of(canonical, 2),
        Some(1),
        "the tail moves to a new list"
    );
    assert_eq!(num_id_of(canonical, 2), num_id_of(canonical, 4));
}

// ─── 1 + 2: accept == two lists, reject == one list ──────────────────────────

#[test]
fn split_accept_is_two_lists_reject_is_one() {
    // Five-item decimal list; split at item 3 (index 2).
    let (doc, ids) = parse(&["One", "Two", "Three", "Four", "Five"]);
    let edited = doc.apply(&split_step(&ids[2])).expect("split applies");

    // accept-all: head (0,1) keeps numId=1; tail (2,3,4) on a NEW numId.
    let head0 = num_id_after(&edited, Resolution::AcceptAll, 0).expect("item 0 numbered");
    let head1 = num_id_after(&edited, Resolution::AcceptAll, 1).expect("item 1 numbered");
    let tail2 = num_id_after(&edited, Resolution::AcceptAll, 2).expect("item 2 numbered");
    let tail3 = num_id_after(&edited, Resolution::AcceptAll, 3).expect("item 3 numbered");
    let tail4 = num_id_after(&edited, Resolution::AcceptAll, 4).expect("item 4 numbered");

    assert_eq!(head0, 1, "head item 0 keeps the original numId");
    assert_eq!(head1, 1, "head item 1 keeps the original numId");
    assert_eq!(tail2, tail3, "tail items share the new numId");
    assert_eq!(tail3, tail4, "tail items share the new numId");
    assert_ne!(
        tail2, 1,
        "the tail must point at a NEW numId, not the original (that is the split)"
    );
    assert!(
        tail2 > 1,
        "the new numId is allocated as max(existing)+1 = 2"
    );

    // reject-all: the ORIGINAL single list — every item back on numId=1.
    for i in 0..5 {
        assert_eq!(
            num_id_after(&edited, Resolution::RejectAll, i),
            Some(1),
            "reject restores the original single list at item {i}"
        );
    }
}

// ─── 3 + 4 + 5: authored numbering.xml well-formed, validator-clean, opaque ──

#[test]
fn split_authors_a_valid_new_definition_and_preserves_content() {
    let (doc, ids) = parse(&["One", "Two", "Three", "Four", "Five"]);
    let edited = doc.apply(&split_step(&ids[2])).expect("split applies");
    let accepted = edited.project(Resolution::AcceptAll).expect("accept");
    let new_num_id = num_id_of(&accepted.snapshot().canonical, 2).expect("tail numbered");

    let bytes = accepted
        .serialize(&ExportOptions::default())
        .expect("serialize accepted output");

    // (4) Validator-clean.
    let report = stemma::api::validate(&bytes);
    assert!(
        report.ok,
        "accepted split output must be valid DOCX: {report:?}"
    );

    // (3) The authored numbering.xml carries the new num + a fresh abstractNum
    // with the CLONED decimal level — no dangling abstractNumId.
    let numbering = part_bytes(&bytes, "word/numbering.xml").expect("numbering.xml present");
    let nx = String::from_utf8(numbering).expect("numbering.xml utf8");

    // The new <w:num> exists and points at some abstractNumId.
    assert!(
        nx.contains(&format!(r#"w:numId="{new_num_id}""#)),
        "new numId {new_num_id} must have a <w:num> in numbering.xml: {nx}"
    );
    // The original list and definition are still present (head unchanged).
    assert!(
        nx.contains(r#"w:numId="1""#),
        "original numId=1 preserved: {nx}"
    );

    // Re-parse to verify every <w:num>'s abstractNumId resolves (no dangling)
    // and the cloned abstractNum carries a decimal level.
    let defs = stemma::numbering::NumberingDefinitions::parse(nx.as_bytes())
        .expect("authored numbering.xml parses");
    // Every num instance must resolve to a defined abstractNum.
    for (nid, inst) in &defs.num_instances {
        assert!(
            defs.abstract_nums.contains_key(&inst.abstract_num_id),
            "numId {nid} points at abstractNumId {} which is not defined (dangling)",
            inst.abstract_num_id
        );
    }
    // There are now at least two distinct abstractNums (source + clone).
    assert!(
        defs.abstract_nums.len() >= 2,
        "split must have authored a second abstractNum (clone): {:?}",
        defs.abstract_nums.keys().collect::<Vec<_>>()
    );
    // The new list resolves to a decimal level (cloned from the source).
    let new_inst = defs
        .num_instances
        .get(&new_num_id)
        .expect("new numId has a num instance");
    let new_abstract = defs
        .abstract_nums
        .get(&new_inst.abstract_num_id)
        .expect("new abstractNum defined");
    let lvl0 = new_abstract.levels.get(&0).expect("cloned level 0 present");
    assert_eq!(
        lvl0.num_fmt,
        stemma::numbering::NumFormat::Decimal,
        "the cloned tail list keeps the source decimal format"
    );

    // (5) Opaque preservation: the text of every item survives the split.
    let view = accepted.read();
    let texts: Vec<String> = view.blocks.iter().map(|b| b.text.clone()).collect();
    for expected in ["One", "Two", "Three", "Four", "Five"] {
        assert!(
            texts.iter().any(|t| t.contains(expected)),
            "item text {expected:?} must survive the split: {texts:?}"
        );
    }
}

// ─── Run boundary: a non-list paragraph between items ends the tail run ───────

#[test]
fn split_on_unnumbered_is_refused() {
    let doc = Document::parse(&make_no_numbering_docx()).expect("parse no-list docx");
    let bid = doc.read().blocks[0].id.to_string();
    let err = match doc.apply(&split_step(&bid)) {
        Ok(_) => panic!("split on an unnumbered paragraph must be refused"),
        Err(e) => e,
    };
    assert!(
        err.message.contains("no list to split")
            || format!("{err:?}").contains("NumberingSplitOnUnnumbered"),
        "split on a non-list paragraph must fail loud: {err:?}"
    );
}

// ─── Orphan-definition allocation: allocate against the numbering PART ────────
//
// Regression for the body-scan allocator bug: `NumberingChange::Split` used to
// allocate its new `numId` as `max(numId referenced in the body) + 1`. The verb
// core is pure over `&CanonDoc` and cannot see `word/numbering.xml`, so that
// scan misses `numId`s the PART defines but no body paragraph references (orphan
// definitions, style-linked / story-only lists — the common wild shape). When
// `max(body) + 1` lands on such a definition the save path refused the collision
// ("new numId N already exists…"), so a legitimate split failed on a large
// fraction of real documents. The fix moves allocation to the save path, which
// reads the authoritative numbering part.

#[test]
fn split_allocates_above_an_orphan_numid_in_the_numbering_part() {
    // Body list on numId=1 (3 items); numbering.xml ALSO defines an orphan
    // numId=2. The old body-scan allocator picks max(body)+1 = 2, which collides
    // with the orphan and made the save path refuse. The fix allocates from the
    // part: max(part numIds {1,2})+1 = 3.
    let docx = make_decimal_list_docx_with_orphan_numid2(&["One", "Two", "Three"]);
    let doc = Document::parse(&docx).expect("parse orphan-numid docx");
    let ids: Vec<String> = doc.read().blocks.iter().map(|b| b.id.to_string()).collect();

    // Split at item 2 (index 1): the split MUST succeed (this is the bug — it
    // used to fail loud at serialize with an InvalidDocx numId collision).
    let edited = doc.apply(&split_step(&ids[1])).expect("split applies");

    // accept-all: head (0) keeps numId=1; tail (1,2) point at a FRESH numId that
    // is neither the source (1) nor the orphan (2).
    let head0 = num_id_after(&edited, Resolution::AcceptAll, 0).expect("item 0 numbered");
    let tail1 = num_id_after(&edited, Resolution::AcceptAll, 1).expect("item 1 numbered");
    let tail2 = num_id_after(&edited, Resolution::AcceptAll, 2).expect("item 2 numbered");
    assert_eq!(head0, 1, "head keeps the original numId");
    assert_eq!(tail1, tail2, "the tail shares one new numId");
    assert_ne!(tail1, 1, "the tail is a NEW list, not the source");
    assert_ne!(
        tail1, 2,
        "the tail must NOT collide with the orphan numId=2 defined in the part"
    );
    assert_eq!(
        tail1, 3,
        "allocation is max(part numIds {{1,2}})+1 = 3, not max(body)+1 = 2"
    );

    // Serialize the accepted output: it must be valid, and its numbering.xml must
    // carry a resolvable <w:num> for the fresh numId with no dangling abstractNum.
    let accepted = edited.project(Resolution::AcceptAll).expect("accept");
    let bytes = accepted
        .serialize(&ExportOptions::default())
        .expect("serialize accepted output (must not refuse a numId collision)");
    assert!(
        stemma::api::validate(&bytes).ok,
        "accepted split output over an orphan-numid doc must be valid DOCX"
    );

    let nx = String::from_utf8(part_bytes(&bytes, "word/numbering.xml").expect("numbering.xml"))
        .expect("numbering.xml utf8");
    let defs = stemma::numbering::NumberingDefinitions::parse(nx.as_bytes())
        .expect("authored numbering.xml parses");
    assert!(
        defs.num_instances.contains_key(&tail1),
        "fresh numId {tail1} must have a <w:num> in numbering.xml: {nx}"
    );
    for (nid, inst) in &defs.num_instances {
        assert!(
            defs.abstract_nums.contains_key(&inst.abstract_num_id),
            "numId {nid} points at abstractNumId {} which is not defined (dangling)",
            inst.abstract_num_id
        );
    }
    // The pre-existing orphan definition is untouched by the split.
    assert!(
        defs.num_instances.contains_key(&2),
        "orphan numId=2 must survive the split: {nx}"
    );

    // reject-all restores the ORIGINAL single list — every item back on numId=1.
    for i in 0..3 {
        assert_eq!(
            num_id_after(&edited, Resolution::RejectAll, i),
            Some(1),
            "reject restores the original single list at item {i}"
        );
    }

    // Opaque preservation: the item text survives accept and reject.
    let accepted_texts: Vec<String> = accepted
        .read()
        .blocks
        .iter()
        .map(|b| b.text.clone())
        .collect();
    let rejected = edited.project(Resolution::RejectAll).expect("reject");
    let rejected_texts: Vec<String> = rejected
        .read()
        .blocks
        .iter()
        .map(|b| b.text.clone())
        .collect();
    for expected in ["One", "Two", "Three"] {
        assert!(
            accepted_texts.iter().any(|t| t.contains(expected)),
            "accepted text {expected:?} preserved: {accepted_texts:?}"
        );
        assert!(
            rejected_texts.iter().any(|t| t.contains(expected)),
            "rejected text {expected:?} preserved: {rejected_texts:?}"
        );
    }
}

// ─── Story-reference hole: a numId referenced only from a footnote ────────────
//
// The secondary half of the same bug: `max_num_id_in_body` scanned body blocks
// (and table cells) but NOT story parts (headers/footers/footnotes/…). A list
// living only in a footnote could therefore reference a numId ABOVE the body max
// — and `max(body)+1` would collide with it. The fix removes the body scan
// entirely and allocates from the numbering PART, which defines every list
// regardless of which story references it, so the story-reference hole closes by
// construction.

#[test]
fn split_allocates_above_a_footnote_only_numid() {
    // Body list on numId=1; a FOOTNOTE list on numId=2 (referenced only from the
    // footnote story, above the body max). Old body-scan: max(body)+1 = 2 →
    // collides with the footnote's part-defined numId. Fix: part-scan → 3.
    let docx = make_list_docx_with_footnote_list_numid2(&["One", "Two", "Three"]);
    let doc = Document::parse(&docx).expect("parse footnote-list docx");
    let ids: Vec<String> = doc.read().blocks.iter().map(|b| b.id.to_string()).collect();

    let edited = doc.apply(&split_step(&ids[1])).expect("split applies");

    let head0 = num_id_after(&edited, Resolution::AcceptAll, 0).expect("item 0 numbered");
    let tail1 = num_id_after(&edited, Resolution::AcceptAll, 1).expect("item 1 numbered");
    let tail2 = num_id_after(&edited, Resolution::AcceptAll, 2).expect("item 2 numbered");
    assert_eq!(head0, 1, "head keeps the original numId");
    assert_eq!(tail1, tail2, "the tail shares one new numId");
    assert_ne!(
        tail1, 2,
        "the tail must NOT collide with the footnote-only numId=2"
    );
    assert_eq!(
        tail1, 3,
        "allocation is above every part-defined numId (body + story), = 3"
    );

    let accepted = edited.project(Resolution::AcceptAll).expect("accept");
    let bytes = accepted
        .serialize(&ExportOptions::default())
        .expect("serialize accepted output (must not refuse a story-referenced numId collision)");
    assert!(
        stemma::api::validate(&bytes).ok,
        "accepted split output over a footnote-list doc must be valid DOCX"
    );

    let nx = String::from_utf8(part_bytes(&bytes, "word/numbering.xml").expect("numbering.xml"))
        .expect("numbering.xml utf8");
    let defs = stemma::numbering::NumberingDefinitions::parse(nx.as_bytes())
        .expect("authored numbering.xml parses");
    assert!(
        defs.num_instances.contains_key(&tail1),
        "fresh numId {tail1} must have a <w:num> in numbering.xml: {nx}"
    );
    assert!(
        defs.num_instances.contains_key(&2),
        "the footnote's numId=2 must survive the split: {nx}"
    );
}

// ─── Bootstrap note: a list cannot exist without numbering.xml ───────────────
//
// The save path mirrors the styles.xml bootstrap (synthesize a minimal
// <w:numbering> root + content-type Override + relationship when the part is
// absent). But a SPLIT is only reachable when the split point is a list item,
// and a list item can only exist if numbering.xml was present at import. So the
// bootstrap branch is unreachable via the split verb: with no numbering.xml the
// paragraph is unnumbered and the verb refuses BEFORE staging any numbering op
// (asserted by `split_on_unnumbered_is_refused`). There is therefore no
// "bootstrap a numbering part from a split" scenario to test — the model makes
// it impossible, which is the honest outcome. (The styles bootstrap is testable
// because CreateStyle is a package-level op with no body precondition; Split is
// body-anchored.)

// ─── Label format (`set_numbering` kind `format`) ────────────────────────────
//
// Domain rule: changing the label format of an item and the items after it in
// its list (1., 2., 3. → (a), (b), (c)) leaves the earlier items and the list's
// shared definition as they were. The changed items read with the new format
// once accepted, the original labels once rejected, and the saved package
// defines the new format in a definition of its own.

fn format_from(block_id: &str, mode: &str) -> EditTransaction {
    let json = format!(
        r#"{{"ops":[{{"op":"set_numbering","target":"{block_id}","change":{{"kind":"format","num_fmt":"lowerLetter","lvl_text":"(%1)"}}}}],"revision":{{"author":"Counsel"}},"materialization_mode":"{mode}"}}"#
    );
    stemma::edit_v4::parse_transaction(&json)
        .expect("schema-valid format op")
        .into_edit_transaction()
        .expect("adapts")
}

fn labels(doc: &Document) -> Vec<String> {
    let reopened = Document::parse(&doc.serialize(&ExportOptions::default()).expect("serialize"))
        .expect("reopen");
    reopened
        .snapshot()
        .canonical
        .blocks
        .iter()
        .filter_map(|tracked| match &tracked.block {
            BlockNode::Paragraph(p) => p.numbering.as_ref().map(|n| n.synthesized_text.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_label_format_change_reads_new_once_accepted_and_old_once_rejected() {
    let (doc, ids) = parse(&["One", "Two", "Three", "Four", "Five"]);
    let edited = doc
        .apply(&format_from(&ids[2], "tracked_change"))
        .expect("format");
    assert_eq!(
        labels(&edited.project(Resolution::AcceptAll).expect("accept")),
        ["1.", "2.", "(a)", "(b)", "(c)"]
    );
    assert_eq!(
        labels(&edited.project(Resolution::RejectAll).expect("reject")),
        ["1.", "2.", "3.", "4.", "5."]
    );

    let numbering = String::from_utf8(
        part_bytes(
            &edited
                .serialize(&ExportOptions::default())
                .expect("serialize"),
            "word/numbering.xml",
        )
        .expect("numbering part"),
    )
    .expect("utf-8");
    // The source definition is untouched; a second one carries the new format.
    assert_eq!(
        numbering.matches(r#"<w:numFmt w:val="decimal""#).count(),
        1,
        "{numbering}"
    );
    assert!(
        numbering.contains(r#"<w:numFmt w:val="lowerLetter""#),
        "{numbering}"
    );
    assert!(
        numbering.contains(r#"<w:lvlText w:val="(%1)""#),
        "{numbering}"
    );
}

#[test]
fn a_direct_label_format_change_is_no_tracked_change() {
    let (doc, ids) = parse(&["One", "Two", "Three"]);
    let edited = doc.apply(&format_from(&ids[1], "direct")).expect("format");
    assert!(edited.revisions().is_empty(), "{:?}", edited.revisions());
    assert_eq!(labels(&edited), ["1.", "(a)", "(b)"]);
}

#[test]
fn a_label_format_without_a_template_is_refused() {
    let json = r#"{"ops":[{"op":"set_numbering","target":"p_1","change":{"kind":"format","num_fmt":"lowerLetter","lvl_text":""}}],"revision":{"author":"Counsel"}}"#;
    let error =
        stemma::edit_v4::parse_transaction(json).expect_err("an empty template is no format");
    assert!(error.to_string().contains("lvl_text"), "{error}");
}

/// A list label is a view over the list sequence, so an edit that changes the
/// sequence shows the new labels at once, not only after a save and reopen:
/// a split's tail counts from 1 and a format change reads in its new format.
#[test]
fn an_edited_list_reads_its_new_labels_before_saving() {
    let in_session = |doc: &Document| -> Vec<String> {
        doc.snapshot()
            .canonical
            .blocks
            .iter()
            .filter_map(|tracked| match &tracked.block {
                BlockNode::Paragraph(p) => p.numbering.as_ref().map(|n| n.synthesized_text.clone()),
                _ => None,
            })
            .collect()
    };
    let (doc, ids) = parse(&["One", "Two", "Three", "Four"]);
    let split = doc.apply(&split_step(&ids[2])).expect("split");
    assert_eq!(in_session(&split), ["1.", "2.", "1.", "2."]);
    let formatted = doc
        .apply(&format_from(&ids[2], "tracked_change"))
        .expect("format");
    assert_eq!(in_session(&formatted), ["1.", "2.", "(a)", "(b)"]);
}
