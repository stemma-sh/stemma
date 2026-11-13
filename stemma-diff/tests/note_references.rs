//! Domain rule: a footnote or endnote belongs to one reference (ISO 29500-1
//! §17.11.14 / §17.11.7); Word repairs a package where two references name
//! one note. Content holding a reference that a comparison reads as deleted
//! in one place and inserted in another keeps the base note at the deleted
//! copy and gives the inserted copy a note of its own with the target's
//! content, so both readings hold and every note is referenced once.

use std::collections::{BTreeSet, HashMap};

use stemma::api::{Document, Resolution, ResolveSelectionAction};
use stemma::domain::{BlockNode, InlineNode, OpaqueKind, TrackedBlock};
use stemma::runtime::ExportOptions;

/// A package with `body`, footnotes `footnotes` and endnotes
/// `endnotes` (id, text).
fn package(body: &str, footnotes: &[(u32, &str)], endnotes: &[(u32, &str)]) -> Vec<u8> {
    use std::io::Write;
    use zip::write::FileOptions;

    let notes = |tag: &str, reference: &str, notes: &[(u32, &str)]| {
        let user: String = notes
            .iter()
            .map(|(id, text)| {
                format!(
                    r#"<w:{tag} w:id="{id}"><w:p><w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:{reference}/></w:r><w:r><w:t xml:space="preserve"> {text}</w:t></w:r></w:p></w:{tag}>"#
                )
            })
            .collect();
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:{tag}s xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:{tag} w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:{tag}><w:{tag} w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:{tag}>{user}</w:{tag}s>"#
        )
    };
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let footnotes_part = notes("footnote", "footnoteRef", footnotes);
    let endnotes_part = notes("endnote", "endnoteRef", endnotes);
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/footnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"/><Override PartName="/word/endnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.endnotes+xml"/></Types>"#;
    let package_relationships = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let document_relationships = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes" Target="footnotes.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/endnotes" Target="endnotes.xml"/></Relationships>"#;
    let mut bytes = Vec::new();
    {
        let mut archive = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
        let options: FileOptions = FileOptions::default();
        for (path, content) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", package_relationships),
            ("word/document.xml", document.as_str()),
            ("word/_rels/document.xml.rels", document_relationships),
            ("word/footnotes.xml", footnotes_part.as_str()),
            ("word/endnotes.xml", endnotes_part.as_str()),
        ] {
            archive.start_file(path, options).expect("start part");
            archive.write_all(content.as_bytes()).expect("write part");
        }
        archive.finish().expect("finish package");
    }
    bytes
}

fn run(text: &str) -> String {
    format!(r#"<w:r><w:t xml:space="preserve">{text}</w:t></w:r>"#)
}

fn footnote_ref(id: u32) -> String {
    format!(
        r#"<w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:footnoteReference w:id="{id}"/></w:r>"#
    )
}

fn endnote_ref(id: u32) -> String {
    format!(
        r#"<w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:endnoteReference w:id="{id}"/></w:r>"#
    )
}

fn paragraph(content: &str) -> String {
    format!("<w:p>{content}</w:p>")
}

/// A three-column table of `rows`; a cell is plain text, or run XML when it
/// starts with `<`.
fn table(rows: &[[&str; 3]]) -> String {
    let rows: String = rows
        .iter()
        .map(|cells| {
            let cells: String = cells
                .iter()
                .map(|content| {
                    let content = if content.starts_with('<') {
                        content.to_string()
                    } else {
                        run(content)
                    };
                    format!(
                        r#"<w:tc><w:tcPr><w:tcW w:w="3000" w:type="dxa"/></w:tcPr>{}</w:tc>"#,
                        paragraph(&content)
                    )
                })
                .collect();
            format!("<w:tr>{cells}</w:tr>")
        })
        .collect();
    format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="3000"/><w:gridCol w:w="3000"/><w:gridCol w:w="3000"/></w:tblGrid>{rows}</w:tbl>{}"#,
        paragraph(&run("After the table."))
    )
}

/// A parts list whose `flagged` row is the second item, or, `moved`, the
/// fourth: the other rows are kept and the flagged row reads as deleted at
/// its old place and inserted at its new one.
fn moved_rows(flagged: &str, moved: bool) -> Vec<[&str; 3]> {
    let flagged_row = [flagged, "120", "Compression fit"];
    let mut rows = vec![
        ["Item", "Quantity", "Notes"],
        ["Copper pipe", "40", "Half inch bore"],
    ];
    let later = [
        ["Ball valve", "18", "Lever handle"],
        ["Thread seal tape", "25", "PTFE, twelve metre roll"],
    ];
    if moved {
        rows.extend(later);
        rows.push(flagged_row);
    } else {
        rows.push(flagged_row);
        rows.extend(later);
    }
    rows.push(["Pipe clip", "350", "Plastic, grey"]);
    rows
}

fn parse(bytes: Vec<u8>) -> Document {
    Document::parse(&bytes).expect("parse test document")
}

fn reopened(document: &Document) -> Document {
    Document::parse(
        &document
            .serialize(&ExportOptions::default())
            .expect("the redline saves"),
    )
    .expect("the saved redline opens")
}

fn compare(base: &Document, target: &Document) -> Document {
    stemma_diff::diff(base, target).expect("comparison")
}

/// A document's reading: its body text, then the text of each note its
/// references name, in reference order.
fn reading(document: &Document) -> (String, Vec<String>) {
    let canonical = &document.snapshot().canonical;
    let mut texts = Vec::new();
    let footnotes: HashMap<&str, &[TrackedBlock]> = canonical
        .footnotes
        .iter()
        .map(|n| (n.id.as_str(), n.blocks.as_slice()))
        .collect();
    let endnotes: HashMap<&str, &[TrackedBlock]> = canonical
        .endnotes
        .iter()
        .map(|n| (n.id.as_str(), n.blocks.as_slice()))
        .collect();
    fn walk(
        block: &BlockNode,
        footnotes: &HashMap<&str, &[TrackedBlock]>,
        endnotes: &HashMap<&str, &[TrackedBlock]>,
        texts: &mut Vec<String>,
    ) {
        match block {
            BlockNode::Paragraph(p) => {
                for inline in p.all_inlines_owned() {
                    if let InlineNode::OpaqueInline(o) = &inline {
                        let (notes, id) = match &o.kind {
                            OpaqueKind::FootnoteReference(d) => (footnotes, &d.reference_id),
                            OpaqueKind::EndnoteReference(d) => (endnotes, &d.reference_id),
                            _ => continue,
                        };
                        let blocks = notes.get(id.as_str()).expect("a referenced note exists");
                        let text: Vec<String> = blocks
                            .iter()
                            .filter_map(|tb| match &tb.block {
                                BlockNode::Paragraph(p) => {
                                    Some(stemma::local_change::extract_inline_text(
                                        &p.all_inlines_owned(),
                                    ))
                                }
                                _ => None,
                            })
                            .collect();
                        texts.push(text.join("\n"));
                    }
                }
            }
            BlockNode::Table(t) => {
                for row in &t.rows {
                    for cell in &row.cells {
                        for inner in &cell.blocks {
                            walk(inner, footnotes, endnotes, texts);
                        }
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }
    for tracked in &canonical.blocks {
        walk(&tracked.block, &footnotes, &endnotes, &mut texts);
    }
    (document.to_text(), texts)
}

/// Every note reference id of a package's main part, by element.
fn reference_ids(document: &Document) -> Vec<String> {
    let bytes = document
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let archive = stemma::docx::DocxArchive::read(&bytes).expect("read package");
    let xml =
        String::from_utf8(archive.get("word/document.xml").expect("main").to_vec()).expect("UTF-8");
    let mut out = Vec::new();
    for element in ["footnoteReference", "endnoteReference"] {
        let mut rest = xml.as_str();
        while let Some(at) = rest.find(&format!("<w:{element} ")) {
            rest = &rest[at..];
            let id_at = rest.find("w:id=\"").expect("an id") + 6;
            let end = rest[id_at..].find('"').expect("quoted") + id_at;
            out.push(format!("{element}:{}", &rest[id_at..end]));
            rest = &rest[end..];
        }
    }
    out
}

/// The laws, in memory and reopened: Reject All reads as the base and Accept
/// All as the target (body and notes), likewise rejecting or accepting each
/// revision; every note reference id of the redline is distinct, and the
/// redline saves (the package validator refuses a note referenced twice).
fn assert_one_note_per_reference(base: &Document, target: &Document) {
    let redline = compare(base, target);
    let ids = reference_ids(&redline);
    let distinct: BTreeSet<&String> = ids.iter().collect();
    assert_eq!(distinct.len(), ids.len(), "one reference per note: {ids:?}");
    let saved = reopened(&redline);
    for (form, redline) in [("in memory", &redline), ("reopened", &saved)] {
        let project = |resolution| redline.project(resolution).expect("resolve");
        assert_eq!(
            reading(&project(Resolution::RejectAll)),
            reading(base),
            "{form}: Reject All"
        );
        assert_eq!(
            reading(&project(Resolution::AcceptAll)),
            reading(target),
            "{form}: Accept All"
        );
        let revisions: Vec<u32> = redline.revisions().iter().map(|r| r.revision_id).collect();
        for (action, want) in [
            (ResolveSelectionAction::Reject, base),
            (ResolveSelectionAction::Accept, target),
        ] {
            let resolved = project(Resolution::Selective {
                ids: revisions.iter().copied().collect(),
                action,
            });
            assert_eq!(
                reading(&resolved),
                reading(want),
                "{form}: {action:?} each revision"
            );
        }
    }
}

/// A table row holding a footnote reference moved down the table: the
/// row reads as deleted at its old place and inserted at its new one.
#[test]
fn a_moved_table_row_with_a_footnote_gives_the_inserted_copy_its_own_note() {
    let flagged = format!("{}{}", run("Brass elbow"), footnote_ref(1));
    let notes = [(1, "Supplied in packs of ten.")];
    let base = parse(package(&table(&moved_rows(&flagged, false)), &notes, &[]));
    let target = parse(package(&table(&moved_rows(&flagged, true)), &notes, &[]));
    assert_one_note_per_reference(&base, &target);
}

/// The same with an endnote, whose note the target also reworded.
#[test]
fn a_moved_table_row_with_an_endnote_gives_the_inserted_copy_its_own_note() {
    let flagged = format!("{}{}", run("Brass elbow"), endnote_ref(3));
    let base = parse(package(
        &table(&moved_rows(&flagged, false)),
        &[],
        &[(3, "Supplied in packs of ten.")],
    ));
    let target = parse(package(
        &table(&moved_rows(&flagged, true)),
        &[],
        &[(3, "Supplied in packs of twelve.")],
    ));
    assert_one_note_per_reference(&base, &target);
}

/// A paragraph holding a footnote reference moved below two others.
#[test]
fn a_moved_paragraph_with_a_footnote_gives_the_inserted_copy_its_own_note() {
    let moved = paragraph(&format!(
        "{}{}",
        run(
            "Any dispute about the maintenance schedule goes first to the joint committee, which meets within ten working days."
        ),
        footnote_ref(1)
    ));
    let one = paragraph(&run("Section one covers ordering."));
    let two = paragraph(&run("Section two covers seasonal pricing."));
    let three = paragraph(&run("Section three covers returns."));
    let notes = [(1, "The committee's terms are in schedule four.")];
    let base = parse(package(&format!("{one}{moved}{two}{three}"), &notes, &[]));
    let target = parse(package(&format!("{one}{two}{three}{moved}"), &notes, &[]));
    assert_one_note_per_reference(&base, &target);
}

/// A sentence holding a footnote reference moved to another paragraph.
#[test]
fn a_moved_sentence_with_a_footnote_gives_the_inserted_copy_its_own_note() {
    let sentence = "The supplier delivers every consignment to the northern bay before noon.";
    let notes = [(1, "Public holidays excepted.")];
    let base = parse(package(
        &format!(
            "{}{}{}",
            paragraph(&format!(
                "{}{}",
                run(&format!("Section one covers ordering. {sentence}")),
                footnote_ref(1)
            )),
            paragraph(&run("Section two covers seasonal pricing.")),
            paragraph(&run("Section three covers returns."))
        ),
        &notes,
        &[],
    ));
    let target = parse(package(
        &format!(
            "{}{}{}",
            paragraph(&run("Section one covers ordering.")),
            paragraph(&run("Section two covers seasonal pricing.")),
            paragraph(&format!(
                "{}{}",
                run(&format!("Section three covers returns. {sentence}")),
                footnote_ref(1)
            ))
        ),
        &notes,
        &[],
    ));
    assert_one_note_per_reference(&base, &target);
}
