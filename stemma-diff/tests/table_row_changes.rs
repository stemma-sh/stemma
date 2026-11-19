//! Table rows in a comparison, as Word's Compare reads them.
//!
//! Word's Compare keeps a row that changed in place (same position between
//! matching rows, same cells) as the same row and marks the changed words
//! inside its cells: a row whose second cell went from "within thirty days of
//! invoice" to "within forty-five days of invoice" is one deletion
//! ("thirty") and one insertion ("forty-five") in that cell. A row whose
//! words all changed comes out as each cell's content deleted and inserted.
//! A row that is added or removed is a tracked row insertion or deletion.
//! Word opens such redlines, with a later changed paragraph in the same
//! story, without repair, and its Reject All and Accept All leave the base
//! and the target.

use std::io::Write;

use stemma::ExportOptions;
use stemma::Resolution;
use stemma::api::Document;
use stemma::domain::{BlockNode, TrackingStatus};

fn docx(body: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = zip::write::FileOptions::default();
        for (name, part) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/document.xml", document_xml.as_str()),
        ] {
            zip.start_file(name, opts).expect("start part");
            zip.write_all(part.as_bytes()).expect("write part");
        }
        zip.finish().expect("finish package");
    }
    buf
}

fn paragraph(text: &str) -> String {
    format!(r#"<w:p><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p>"#)
}

fn row(cells: &[&str]) -> String {
    let cells: String = cells
        .iter()
        .map(|text| {
            format!(
                r#"<w:tc><w:tcPr><w:tcW w:w="2400" w:type="dxa"/></w:tcPr>{}</w:tc>"#,
                paragraph(text)
            )
        })
        .collect();
    format!("<w:tr>{cells}</w:tr>")
}

fn table(rows: &[&[&str]]) -> String {
    let rows: String = rows.iter().map(|cells| row(cells)).collect();
    format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="4800" w:type="dxa"/></w:tblPr><w:tblGrid><w:gridCol w:w="2400"/><w:gridCol w:w="2400"/></w:tblGrid>{rows}</w:tbl>"#
    )
}

fn reopened(document: &Document) -> Document {
    Document::parse(
        &document
            .serialize(&ExportOptions::default())
            .expect("serialize"),
    )
    .expect("reopen")
}

/// The redline's text revisions, in document order, as (kind, text).
fn revisions(document: &Document) -> Vec<(String, String)> {
    document
        .revisions()
        .into_iter()
        .map(|r| (format!("{:?}", r.kind), r.excerpt))
        .collect()
}

/// Rows of the redline's tables carrying a tracked row insertion and a
/// tracked row deletion.
fn tracked_rows(document: &Document) -> (usize, usize) {
    let mut inserted = 0;
    let mut deleted = 0;
    for tracked in &document.snapshot().canonical.blocks {
        if let BlockNode::Table(table) = &tracked.block {
            for row in &table.rows {
                match &row.tracking_status {
                    Some(TrackingStatus::Inserted(_)) => inserted += 1,
                    Some(TrackingStatus::Deleted(_)) => deleted += 1,
                    _ => {}
                }
            }
        }
    }
    (inserted, deleted)
}

/// Accept All reads as the target and Reject All as the base, by text and
/// by a comparison back to zero changes.
fn assert_round_trip(redline: &Document, base: &Document, target: &Document) {
    let accepted = reopened(&redline.project(Resolution::AcceptAll).expect("accept"));
    let rejected = reopened(&redline.project(Resolution::RejectAll).expect("reject"));
    assert_eq!(
        accepted.to_text(),
        target.to_text(),
        "Accept All reads as the target"
    );
    assert_eq!(
        rejected.to_text(),
        base.to_text(),
        "Reject All reads as the base"
    );
    for (label, reading, input) in [
        ("Accept All", &accepted, target),
        ("Reject All", &rejected, base),
    ] {
        let remaining = stemma_diff::diff_detailed(reading, input).expect("re-compare");
        assert_eq!(
            remaining.semantic_change_count, 0,
            "{label} compares to zero changes against its input"
        );
    }
}

const HEADER: &[&str] = &["Term", "Value"];
const NOTICE: &[&str] = &["Notice period", "ninety days in writing"];
const LAW: &[&str] = &["Governing law", "the laws of England"];

fn agreement(rows: &[&[&str]], closing: &str) -> Document {
    Document::parse(&docx(&format!("{}{}", table(rows), paragraph(closing)))).expect("parse")
}

/// Word's Compare: the changed word inside the cell, then the changed word
/// in the paragraph after the table; the row is kept.
#[test]
fn a_row_changed_in_place_keeps_the_row_and_marks_the_changed_word() {
    let base = agreement(
        &[
            HEADER,
            &["Payment due", "within thirty days of invoice"],
            NOTICE,
            LAW,
        ],
        "Deliveries are made to the north gate.",
    );
    let target = agreement(
        &[
            HEADER,
            &["Payment due", "within forty-five days of invoice"],
            NOTICE,
            LAW,
        ],
        "Deliveries are made to the south gate.",
    );

    let redline = reopened(&stemma_diff::diff_as(&base, &target, "compare").expect("compare"));

    assert_eq!(
        tracked_rows(&redline),
        (0, 0),
        "no row is inserted or deleted"
    );
    let texts: Vec<String> = revisions(&redline)
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    assert_eq!(
        texts,
        ["thirty", "forty-five", "north", "south"],
        "{texts:?}"
    );
    assert_round_trip(&redline, &base, &target);
}

/// Word's Compare: a row whose words all changed is kept, and each cell's
/// content is deleted and inserted.
#[test]
fn a_row_whose_words_all_changed_replaces_each_cells_content() {
    let base = agreement(
        &[
            HEADER,
            &["Payment due", "within thirty days of invoice"],
            NOTICE,
            LAW,
        ],
        "Deliveries are made to the north gate.",
    );
    let target = agreement(
        &[
            HEADER,
            &["Liability cap", "twice the annual fees"],
            NOTICE,
            LAW,
        ],
        "Deliveries are made to the south gate.",
    );

    let redline = reopened(&stemma_diff::diff_as(&base, &target, "compare").expect("compare"));

    assert_eq!(
        tracked_rows(&redline),
        (0, 0),
        "no row is inserted or deleted"
    );
    let texts: Vec<String> = revisions(&redline)
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    assert_eq!(
        texts,
        [
            "Payment due",
            "Liability cap",
            "within thirty days of invoice",
            "twice the annual fees",
            "north",
            "south"
        ],
        "{texts:?}"
    );
    assert_round_trip(&redline, &base, &target);
}

/// A row added next to a changed row: position no longer says which rows
/// correspond, so no row lineage is inferred. The added row is a tracked row
/// insertion and the round trip holds.
#[test]
fn an_added_row_is_a_tracked_row_insertion() {
    let base = agreement(
        &[HEADER, NOTICE, LAW],
        "Deliveries are made to the north gate.",
    );
    let target = agreement(
        &[
            HEADER,
            NOTICE,
            &["Payment due", "within thirty days of invoice"],
            LAW,
        ],
        "Deliveries are made to the north gate.",
    );

    let redline = reopened(&stemma_diff::diff_as(&base, &target, "compare").expect("compare"));

    assert_eq!(tracked_rows(&redline), (1, 0));
    assert_round_trip(&redline, &base, &target);
}

/// A removed row is a tracked row deletion.
#[test]
fn a_removed_row_is_a_tracked_row_deletion() {
    let base = agreement(
        &[
            HEADER,
            NOTICE,
            &["Payment due", "within thirty days of invoice"],
            LAW,
        ],
        "Deliveries are made to the north gate.",
    );
    let target = agreement(
        &[HEADER, NOTICE, LAW],
        "Deliveries are made to the north gate.",
    );

    let redline = reopened(&stemma_diff::diff_as(&base, &target, "compare").expect("compare"));

    assert_eq!(tracked_rows(&redline), (0, 1));
    assert_round_trip(&redline, &base, &target);
}

/// A changed row next to an added row: the region between matching rows has
/// one source row and two target rows, so position does not say which target
/// row the source row became, and no lineage is inferred from text
/// similarity. The first pair shares only its row shell (each cell's content
/// deleted and inserted whole), the other target row is a tracked row
/// insertion, and the round trip holds.
#[test]
fn a_changed_row_next_to_an_added_row_gets_no_inferred_lineage() {
    let base = agreement(
        &[
            HEADER,
            &["Payment due", "within thirty days of invoice"],
            LAW,
        ],
        "Deliveries are made to the north gate.",
    );
    let target = agreement(
        &[
            HEADER,
            &["Payment due", "within forty-five days of invoice"],
            NOTICE,
            LAW,
        ],
        "Deliveries are made to the north gate.",
    );

    let redline = reopened(&stemma_diff::diff_as(&base, &target, "compare").expect("compare"));

    assert_eq!(tracked_rows(&redline), (1, 0));
    let texts: Vec<String> = revisions(&redline)
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    assert_eq!(
        texts.iter().filter(|text| *text == "Payment due").count(),
        2,
        "the paired row keeps none of its text, even its unchanged cell's: {texts:?}"
    );
    assert_round_trip(&redline, &base, &target);
}
