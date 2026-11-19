//! A revision's explicit UTC date (`w16du:dateUtc`, MS-DOCX §2.12.2.1) is
//! content the document carries: a save writes it back exactly as it was.
//!
//! Word writes the author's local clock in `w:date` and the true UTC time in
//! `w16du:dateUtc`, so the two differ whenever the author was not on UTC. A
//! save must not rewrite one from the other, must not drop it, and must not
//! invent one for a revision that had none. The engine's own new revisions
//! record their date as their UTC date as well (its clock is UTC).
//!
//! Daily tier: synthetic in-memory DOCX, invented text.

use std::collections::BTreeSet;
use std::io::{Cursor, Write};

use stemma::ExportOptions;
use stemma::api::Document;
use stemma::domain::{BlockNode, NodeId, RevisionInfo};
use stemma::edit::{
    ContentFragment, EditStep, EditTransaction, MaterializationMode, ParagraphContent,
};
use zip::ZipWriter;
use zip::write::FileOptions;

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const W16DU: &str = "http://schemas.microsoft.com/office/word/2023/wordml/word16du";
const MC: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";

fn docx(body: &str) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = FileOptions::default();
    zip.start_file("[Content_Types].xml", options).unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#).unwrap();
    zip.start_file("_rels/.rels", options).unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#).unwrap();
    zip.start_file("word/_rels/document.xml.rels", options)
        .unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#).unwrap();
    zip.start_file("word/document.xml", options).unwrap();
    zip.write_all(
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="{W}" xmlns:w16du="{W16DU}" xmlns:mc="{MC}" mc:Ignorable="w16du"><w:body>{body}</w:body></w:document>"#
        )
        .as_bytes(),
    )
    .unwrap();
    zip.finish().unwrap().into_inner()
}

/// `w:author`, `w:date` and `w16du:dateUtc` for a revision element. The
/// local clock is an hour ahead of UTC.
fn stamp(author: &str, minute: u32) -> String {
    format!(
        r#"w:author="{author}" w:date="2024-03-01T11:{minute:02}:00Z" w16du:dateUtc="2024-03-01T10:{minute:02}:00Z""#
    )
}

/// Every revision element of the saved document.xml as (element, author,
/// w:date, w16du:dateUtc).
type Attribution = (String, String, Option<String>, Option<String>);

fn attributions(bytes: &[u8]) -> BTreeSet<Attribution> {
    let archive = stemma::docx::DocxArchive::read(bytes).expect("the package reads");
    let root = xmltree::Element::parse(archive.get("word/document.xml").expect("document part"))
        .expect("document.xml parses");
    fn walk(element: &xmltree::Element, out: &mut BTreeSet<Attribution>) {
        let revision = [
            "ins",
            "del",
            "moveFrom",
            "moveTo",
            "rPrChange",
            "pPrChange",
            "sectPrChange",
            "tblPrChange",
            "trPrChange",
            "tcPrChange",
            "tblPrExChange",
            "cellIns",
            "cellDel",
        ];
        if element.namespace.as_deref() == Some(W) && revision.contains(&element.name.as_str()) {
            let attr = |namespace: &str, local: &str| {
                element
                    .attributes
                    .iter()
                    .find(|(name, _)| {
                        name.local_name == local && name.namespace.as_deref() == Some(namespace)
                    })
                    .map(|(_, value)| value.clone())
            };
            out.insert((
                element.name.clone(),
                attr(W, "author").unwrap_or_default(),
                attr(W, "date"),
                attr(W16DU, "dateUtc"),
            ));
        }
        for child in element.children.iter().filter_map(|c| c.as_element()) {
            walk(child, out);
        }
    }
    let mut out = BTreeSet::new();
    walk(&root, &mut out);
    out
}

fn save(doc: &Document) -> Vec<u8> {
    doc.serialize(&ExportOptions::default())
        .expect("the document saves")
}

fn paragraph_id(doc: &Document, index: usize) -> NodeId {
    doc.snapshot()
        .canonical
        .blocks
        .iter()
        .filter_map(|tb| match &tb.block {
            BlockNode::Paragraph(p) => Some(p.id.clone()),
            _ => None,
        })
        .nth(index)
        .expect("the paragraph exists")
}

/// Replace paragraph `index`'s text, tracked, as "Reviewer" with `date`.
fn replace(
    doc: &Document,
    index: usize,
    expect: &str,
    new_text: &str,
    date: Option<&str>,
) -> Document {
    let txn = EditTransaction {
        steps: vec![EditStep::ReplaceParagraphText {
            block_id: paragraph_id(doc, index),
            rationale: None,
            replacement_role: None,
            expect: expect.to_string(),
            semantic_hash: None,
            content: ParagraphContent {
                fragments: vec![ContentFragment::Text(new_text.to_string())],
            },
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("Reviewer".to_string()),
            date: date.map(str::to_string),
            apply_op_id: None,
            date_utc: None,
        },
    };
    doc.apply(&txn).expect("the edit applies")
}

fn ines(attributions: &BTreeSet<Attribution>) -> Vec<Attribution> {
    attributions
        .iter()
        .filter(|(_, author, _, _)| author == "Ines")
        .cloned()
        .collect()
}

/// Two paragraphs: the first holds Ines's pending insertion, stamped as
/// `stamp` gives, the second is plain.
fn two_paragraphs(ines_stamp: &str) -> String {
    format!(
        concat!(
            r#"<w:p><w:r><w:t xml:space="preserve">Harbour </w:t></w:r>"#,
            r#"<w:ins w:id="1" {ines}><w:r><w:t>board</w:t></w:r></w:ins></w:p>"#,
            r#"<w:p><w:r><w:t>Quay notes.</w:t></w:r></w:p>"#,
            "<w:sectPr/>",
        ),
        ines = ines_stamp
    )
}

#[test]
fn another_authors_revision_keeps_both_dates_through_an_edit_elsewhere() {
    let doc = Document::parse(&docx(&two_paragraphs(&stamp("Ines", 0)))).expect("loads");
    let edited = replace(&doc, 1, "Quay notes.", "Pier notes.", None);
    assert_eq!(
        ines(&attributions(&save(&edited))),
        [(
            "ins".to_string(),
            "Ines".to_string(),
            Some("2024-03-01T11:00:00Z".to_string()),
            Some("2024-03-01T10:00:00Z".to_string()),
        )]
    );
}

#[test]
fn another_authors_revision_keeps_both_dates_through_an_edit_in_its_paragraph() {
    let doc = Document::parse(&docx(&two_paragraphs(&stamp("Ines", 0)))).expect("loads");
    let edited = replace(&doc, 0, "Harbour board", "New harbour board", None);
    let saved = attributions(&save(&edited));
    assert!(
        saved.iter().any(|(_, author, _, _)| author == "Reviewer"),
        "the edit is tracked: {saved:?}"
    );
    assert_eq!(
        ines(&saved),
        [(
            "ins".to_string(),
            "Ines".to_string(),
            Some("2024-03-01T11:00:00Z".to_string()),
            Some("2024-03-01T10:00:00Z".to_string()),
        )]
    );
}

#[test]
fn a_revision_without_a_utc_date_gains_none_and_a_new_one_records_its_date() {
    let doc = Document::parse(&docx(&two_paragraphs(
        r#"w:author="Ines" w:date="2024-03-01T11:00:00Z""#,
    )))
    .expect("loads");
    let edited = replace(
        &doc,
        1,
        "Quay notes.",
        "Pier notes.",
        Some("2026-02-01T08:30:00Z"),
    );
    let saved = attributions(&save(&edited));
    assert_eq!(
        ines(&saved),
        [(
            "ins".to_string(),
            "Ines".to_string(),
            Some("2024-03-01T11:00:00Z".to_string()),
            None,
        )]
    );
    // The engine's own revisions: its clock is UTC, so the UTC date is the
    // date (what it wrote before, and what Word writes on a UTC clock).
    let reviewer: Vec<_> = saved
        .iter()
        .filter(|(name, author, _, _)| author == "Reviewer" && (name == "ins" || name == "del"))
        .collect();
    assert!(!reviewer.is_empty(), "{saved:?}");
    for (_, _, date, date_utc) in reviewer {
        assert_eq!(date.as_deref(), Some("2026-02-01T08:30:00Z"));
        assert_eq!(date_utc, date);
    }
}

/// One of every revision element that carries a date, each with its own
/// UTC minute.
fn every_kind() -> String {
    let s = stamp;
    format!(
        concat!(
            // Run insertion and deletion.
            r#"<w:p><w:r><w:t xml:space="preserve">Berth </w:t></w:r>"#,
            r#"<w:ins w:id="1" {a}><w:r><w:t xml:space="preserve">one </w:t></w:r></w:ins>"#,
            r#"<w:del w:id="2" {b}><w:r><w:delText>two</w:delText></w:r></w:del></w:p>"#,
            // Paragraph-mark insertion, run and paragraph property changes.
            r#"<w:p><w:pPr><w:jc w:val="center"/><w:rPr><w:ins w:id="3" {c}/></w:rPr>"#,
            r#"<w:pPrChange w:id="4" {d}><w:pPr/></w:pPrChange></w:pPr>"#,
            r#"<w:r><w:rPr><w:b/><w:rPrChange w:id="5" {e}><w:rPr/></w:rPrChange></w:rPr>"#,
            r#"<w:t>Tide table.</w:t></w:r></w:p>"#,
            // Text inserted by one author and deleted by another.
            r#"<w:p><w:r><w:t xml:space="preserve">Pilot </w:t></w:r>"#,
            r#"<w:ins w:id="6" {f}><w:del w:id="7" {g}><w:r><w:delText>boat</w:delText></w:r></w:del></w:ins></w:p>"#,
            // A paragraph-mark deletion.
            r#"<w:p><w:pPr><w:rPr><w:del w:id="8" {h}/></w:rPr></w:pPr><w:r><w:t>Anchor</w:t></w:r></w:p>"#,
            // A section break whose properties changed.
            r#"<w:p><w:pPr><w:sectPr><w:pgSz w:w="12240" w:h="15840"/>"#,
            r#"<w:sectPrChange w:id="9" {i}><w:sectPr/></w:sectPrChange></w:sectPr></w:pPr>"#,
            r#"<w:r><w:t>Chapter end.</w:t></w:r></w:p>"#,
            // A table: table, row and cell property changes, an inserted row,
            // an inserted cell, a row property exception change.
            r#"<w:tbl><w:tblPr><w:tblW w:w="5000" w:type="dxa"/>"#,
            r#"<w:tblPrChange w:id="10" {j}><w:tblPr/></w:tblPrChange></w:tblPr>"#,
            r#"<w:tblGrid><w:gridCol w:w="2500"/><w:gridCol w:w="2500"/></w:tblGrid>"#,
            r#"<w:tr><w:trPr><w:jc w:val="center"/><w:trPrChange w:id="11" {k}><w:trPr/></w:trPrChange></w:trPr>"#,
            r#"<w:tc><w:tcPr><w:tcW w:w="2500" w:type="dxa"/><w:tcPrChange w:id="12" {l}><w:tcPr/></w:tcPrChange></w:tcPr>"#,
            r#"<w:p><w:r><w:t>Dock</w:t></w:r></w:p></w:tc>"#,
            r#"<w:tc><w:tcPr><w:tcW w:w="2500" w:type="dxa"/><w:cellIns w:id="13" {m}/></w:tcPr>"#,
            r#"<w:p><w:r><w:t>Crane</w:t></w:r></w:p></w:tc></w:tr>"#,
            r#"<w:tr><w:tblPrEx><w:jc w:val="center"/><w:tblPrExChange w:id="14" {n}><w:tblPrEx/></w:tblPrExChange></w:tblPrEx>"#,
            r#"<w:trPr><w:ins w:id="15" {o}/></w:trPr>"#,
            r#"<w:tc><w:tcPr><w:tcW w:w="2500" w:type="dxa"/></w:tcPr><w:p><w:pPr><w:rPr><w:ins w:id="16" {o}/></w:rPr></w:pPr>"#,
            r#"<w:ins w:id="17" {o}><w:r><w:t>Hoist</w:t></w:r></w:ins></w:p></w:tc>"#,
            r#"<w:tc><w:tcPr><w:tcW w:w="2500" w:type="dxa"/></w:tcPr><w:p><w:pPr><w:rPr><w:ins w:id="18" {o}/></w:rPr></w:pPr>"#,
            r#"<w:ins w:id="19" {o}><w:r><w:t>Rope</w:t></w:r></w:ins></w:p></w:tc></w:tr></w:tbl>"#,
            // Text moved within the body.
            r#"<w:p><w:r><w:t xml:space="preserve">Load </w:t></w:r>"#,
            r#"<w:moveFromRangeStart w:id="20" w:author="Ines" w:date="2024-03-01T11:16:00Z" w:name="move1"/>"#,
            r#"<w:moveFrom w:id="21" {p}><w:r><w:t xml:space="preserve">fast </w:t></w:r></w:moveFrom>"#,
            r#"<w:moveFromRangeEnd w:id="20"/><w:r><w:t>cargo.</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t xml:space="preserve">Ship </w:t></w:r>"#,
            r#"<w:moveToRangeStart w:id="22" w:author="Ines" w:date="2024-03-01T11:16:00Z" w:name="move1"/>"#,
            r#"<w:moveTo w:id="23" {q}><w:r><w:t xml:space="preserve">fast </w:t></w:r></w:moveTo>"#,
            r#"<w:moveToRangeEnd w:id="22"/><w:r><w:t>now.</w:t></w:r></w:p>"#,
            // A whole paragraph moved.
            r#"<w:moveFromRangeStart w:id="25" w:author="Olaf" w:date="2024-03-01T11:18:00Z" w:name="move2"/>"#,
            r#"<w:p><w:pPr><w:rPr><w:moveFrom w:id="26" {t}/></w:rPr></w:pPr>"#,
            r#"<w:moveFrom w:id="27" {t}><w:r><w:t>Gangway rules.</w:t></w:r></w:moveFrom></w:p>"#,
            r#"<w:moveFromRangeEnd w:id="25"/>"#,
            r#"<w:p><w:r><w:t>Between.</w:t></w:r></w:p>"#,
            r#"<w:moveToRangeStart w:id="28" w:author="Olaf" w:date="2024-03-01T11:18:00Z" w:name="move2"/>"#,
            r#"<w:p><w:pPr><w:rPr><w:moveTo w:id="29" {t}/></w:rPr></w:pPr>"#,
            r#"<w:moveTo w:id="30" {t}><w:r><w:t>Gangway rules.</w:t></w:r></w:moveTo></w:p>"#,
            r#"<w:moveToRangeEnd w:id="28"/>"#,
            r#"<w:p><w:r><w:t>Last line.</w:t></w:r></w:p>"#,
            // The body's own section properties changed.
            r#"<w:sectPr><w:pgSz w:w="12240" w:h="15840"/>"#,
            r#"<w:sectPrChange w:id="24" {r}><w:sectPr/></w:sectPrChange></w:sectPr>"#,
        ),
        a = s("Ines", 1),
        b = s("Ines", 2),
        c = s("Ines", 3),
        d = s("Olaf", 4),
        e = s("Olaf", 5),
        f = s("Ines", 6),
        g = s("Olaf", 7),
        h = s("Olaf", 8),
        i = s("Ines", 9),
        j = s("Ines", 10),
        k = s("Ines", 11),
        l = s("Ines", 12),
        m = s("Olaf", 13),
        n = s("Olaf", 14),
        o = s("Olaf", 15),
        p = s("Ines", 16),
        q = s("Ines", 16),
        r = s("Olaf", 17),
        t = s("Olaf", 18),
    )
}

#[test]
fn every_revision_element_keeps_its_utc_date_through_a_save() {
    let input = docx(&every_kind());
    let before = attributions(&input);
    let doc = Document::parse(&input).expect("loads");
    assert_eq!(attributions(&save(&doc)), before);
}

#[test]
fn every_revision_element_keeps_its_utc_date_through_an_edit_elsewhere() {
    let input = docx(&every_kind());
    let before = attributions(&input);
    let doc = Document::parse(&input).expect("loads");
    let edited = replace(&doc, 4, "Chapter end.", "Chapter close.", None);
    let after: BTreeSet<Attribution> = attributions(&save(&edited))
        .into_iter()
        .filter(|(_, author, _, _)| author != "Reviewer")
        .collect();
    assert_eq!(after, before);
}

#[test]
fn a_saved_document_reopens_as_the_same_model() {
    let doc = Document::parse(&docx(&every_kind())).expect("loads");
    let reopened = Document::parse(&save(&doc)).expect("the saved document loads");
    assert_eq!(
        reopened.snapshot().canonical.blocks,
        doc.snapshot().canonical.blocks
    );
    assert_eq!(
        reopened.snapshot().canonical.body_section_property_change,
        doc.snapshot().canonical.body_section_property_change
    );
}
