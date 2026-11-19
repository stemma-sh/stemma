//! The audit's two readings of "untouched": the same markup (fidelity), or
//! the same text and resolved formatting (appearance).
//!
//! A program that saves a document rewrites its markup everywhere: it drops
//! revision-save ids and proofing marks, joins or splits runs, writes a
//! default out, renumbers the relationships. None of that changes what the
//! document says or how it looks. Under fidelity equality every such block
//! is a violation; under appearance equality none is. A change of text or of
//! a formatting value outside any tracked change is a finding under both.
//!
//! Every case is one before/after pair that differs in exactly the named
//! way, with no tracked change in it.

use std::io::{Cursor, Write};

use stemma::api::audit_with_equality;
use stemma::audit::{AuditReport, UntouchedEquality};
use zip::{ZipWriter, write::FileOptions};

const HEAD: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#;
const NAMESPACES: &str = concat!(
    r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" "#,
    r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" "#,
    r#"xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" "#,
    r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" "#,
    r#"xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture""#,
);
const STYLES: &str = concat!(
    r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
    r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>"#,
    r#"</w:styles>"#,
);

/// A package with a styles part and, when `image` is given, one image
/// relationship: (relationship id, image bytes).
fn docx(body: &str, image: Option<(&str, &[u8])>) -> Vec<u8> {
    let image_relationship = image
        .map(|(id, _)| {
            format!(
                r#"<Relationship Id="{id}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/chart.png"/>"#
            )
        })
        .unwrap_or_default();
    let files: Vec<(&str, Vec<u8>)> = vec![
        (
            "[Content_Types].xml",
            format!(
                r#"{HEAD}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#
            )
            .into_bytes(),
        ),
        (
            "_rels/.rels",
            format!(
                r#"{HEAD}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#
            )
            .into_bytes(),
        ),
        (
            "word/_rels/document.xml.rels",
            format!(
                r#"{HEAD}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdStyles" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>{image_relationship}</Relationships>"#
            )
            .into_bytes(),
        ),
        (
            "word/document.xml",
            format!(
                r#"{HEAD}<w:document {NAMESPACES}><w:body>{body}<w:sectPr/></w:body></w:document>"#
            )
            .into_bytes(),
        ),
        ("word/styles.xml", format!("{HEAD}{STYLES}").into_bytes()),
    ];
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, content) in files {
        zip.start_file(name, FileOptions::default()).expect("start");
        zip.write_all(&content).expect("write");
    }
    if let Some((_, bytes)) = image {
        zip.start_file("word/media/chart.png", FileOptions::default())
            .expect("start");
        zip.write_all(bytes).expect("write");
    }
    zip.finish().expect("finish").into_inner()
}

fn audit(before: &[u8], after: &[u8], equality: UntouchedEquality) -> AuditReport {
    audit_with_equality(before, after, equality).expect("the pair audits")
}

/// Nothing found: no direct change, no untouched violation.
fn is_clean(report: &AuditReport) -> bool {
    report.direct_changes.is_empty() && report.untouched.violations.is_empty()
}

/// Bookkeeping only: a finding under fidelity equality, none under
/// appearance equality.
fn assert_bookkeeping_only(before: &[u8], after: &[u8]) {
    let fidelity = audit(before, after, UntouchedEquality::Fidelity);
    assert!(
        !is_clean(&fidelity),
        "the markup differs, so fidelity equality reports it"
    );
    let appearance = audit(before, after, UntouchedEquality::Appearance);
    assert!(
        is_clean(&appearance),
        "nothing reads or looks different: {:?} {:?}",
        appearance.direct_changes,
        appearance.untouched.violations
    );
}

/// A change to the document: a finding under both.
fn assert_reported_by_both(before: &[u8], after: &[u8]) {
    for equality in [UntouchedEquality::Fidelity, UntouchedEquality::Appearance] {
        assert!(
            !is_clean(&audit(before, after, equality)),
            "{equality:?} reports a change of text or formatting"
        );
    }
}

fn plain(body: &str) -> Vec<u8> {
    docx(body, None)
}

const OPENING: &str =
    r#"<w:p><w:r><w:t>The lighthouse keeper logs the tide twice a day.</w:t></w:r></w:p>"#;

#[test]
fn an_identical_pair_is_clean_under_both() {
    let document = plain(OPENING);
    for equality in [UntouchedEquality::Fidelity, UntouchedEquality::Appearance] {
        assert!(is_clean(&audit(&document, &document, equality)));
    }
}

#[test]
fn dropped_revision_save_ids_are_bookkeeping() {
    let before = plain(
        r#"<w:p w:rsidR="00A1B2C3" w:rsidRDefault="00A1B2C3"><w:r w:rsidRPr="00D4E5F6"><w:t>The lighthouse keeper logs the tide twice a day.</w:t></w:r></w:p>"#,
    );
    assert_bookkeeping_only(&before, &plain(OPENING));
}

#[test]
fn a_run_boundary_between_equal_formatting_is_bookkeeping() {
    // Two runs from two editing sessions (their revision-save ids differ),
    // identically formatted: one stretch of text.
    let split = plain(
        r#"<w:p><w:r w:rsidR="00A1B2C3"><w:t xml:space="preserve">The lighthouse keeper </w:t></w:r><w:r w:rsidR="00D4E5F6"><w:t>logs the tide twice a day.</w:t></w:r></w:p>"#,
    );
    assert_bookkeeping_only(&split, &plain(OPENING));
    assert_bookkeeping_only(&plain(OPENING), &split);
}

#[test]
fn a_run_boundary_between_different_formatting_is_kept() {
    // Joining these two runs would lose the bold: the same text with the
    // bold on other words is a formatting change.
    let before = plain(
        r#"<w:p><w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">The lighthouse keeper </w:t></w:r><w:r><w:t>logs the tide twice a day.</w:t></w:r></w:p>"#,
    );
    let after = plain(
        r#"<w:p><w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">The lighthouse </w:t></w:r><w:r><w:t>keeper logs the tide twice a day.</w:t></w:r></w:p>"#,
    );
    assert_reported_by_both(&before, &after);
}

#[test]
fn a_removed_proofing_mark_is_bookkeeping() {
    let before = plain(
        r#"<w:p><w:r><w:t xml:space="preserve">The </w:t></w:r><w:proofErr w:type="spellStart"/><w:r><w:t>lighthouse</w:t></w:r><w:proofErr w:type="spellEnd"/><w:r><w:t xml:space="preserve"> keeper logs the tide twice a day.</w:t></w:r></w:p>"#,
    );
    assert_bookkeeping_only(&before, &plain(OPENING));
}

#[test]
fn the_default_style_and_zero_spacing_written_out_are_bookkeeping() {
    let after = plain(
        r#"<w:p><w:pPr><w:pStyle w:val="Normal"/><w:spacing w:before="0" w:after="0"/></w:pPr><w:r><w:t>The lighthouse keeper logs the tide twice a day.</w:t></w:r></w:p>"#,
    );
    assert_bookkeeping_only(&plain(OPENING), &after);
}

#[test]
fn changed_text_is_reported_by_both() {
    let after = plain(
        r#"<w:p><w:r><w:t>The lighthouse keeper logs the tide once a day.</w:t></w:r></w:p>"#,
    );
    assert_reported_by_both(&plain(OPENING), &after);
}

#[test]
fn changed_formatting_is_reported_by_both() {
    let bold = plain(
        r#"<w:p><w:r><w:rPr><w:b/></w:rPr><w:t>The lighthouse keeper logs the tide twice a day.</w:t></w:r></w:p>"#,
    );
    assert_reported_by_both(&plain(OPENING), &bold);
    let spaced = plain(
        r#"<w:p><w:pPr><w:spacing w:before="240"/></w:pPr><w:r><w:t>The lighthouse keeper logs the tide twice a day.</w:t></w:r></w:p>"#,
    );
    assert_reported_by_both(&plain(OPENING), &spaced);
}

fn with_picture(relationship_id: &str) -> String {
    format!(
        concat!(
            r#"<w:p><w:r><w:t xml:space="preserve">Tide chart: </w:t></w:r><w:r><w:drawing>"#,
            r#"<wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="914400" cy="320040"/>"#,
            r#"<wp:effectExtent l="0" t="0" r="0" b="0"/><wp:docPr id="1" name="Tide chart"/><wp:cNvGraphicFramePr/>"#,
            r#"<a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic>"#,
            r#"<pic:nvPicPr><pic:cNvPr id="0" name="Tide chart"/><pic:cNvPicPr/></pic:nvPicPr>"#,
            r#"<pic:blipFill><a:blip r:embed="{id}"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>"#,
            r#"<pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="320040"/></a:xfrm>"#,
            r#"<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic>"#,
            r#"</wp:inline></w:drawing></w:r></w:p>"#,
        ),
        id = relationship_id
    )
}

#[test]
fn a_renumbered_image_relationship_is_bookkeeping() {
    let image: &[u8] = b"not really a picture, but always the same bytes";
    let before = docx(&with_picture("rId6"), Some(("rId6", image)));
    let after = docx(&with_picture("R3"), Some(("R3", image)));
    assert_bookkeeping_only(&before, &after);
}

#[test]
fn a_replaced_image_is_reported_by_appearance_equality() {
    // The reference is unchanged; the picture behind it is another one.
    let before = docx(&with_picture("rId6"), Some(("rId6", b"the first picture")));
    let after = docx(&with_picture("rId6"), Some(("rId6", b"another picture")));
    assert!(!is_clean(&audit(
        &before,
        &after,
        UntouchedEquality::Appearance
    )));
}

#[test]
fn a_property_with_its_attributes_in_another_order_is_bookkeeping() {
    let fonts = |attributes: &str| {
        plain(&format!(
            r#"<w:p><w:r><w:rPr><w:rFonts {attributes}/></w:rPr><w:t>The lighthouse keeper logs the tide twice a day.</w:t></w:r></w:p>"#
        ))
    };
    assert_bookkeeping_only(
        &fonts(r#"w:ascii="Georgia" w:eastAsia="Meiryo" w:hAnsi="Georgia" w:cs="Georgia""#),
        &fonts(r#"w:ascii="Georgia" w:hAnsi="Georgia" w:eastAsia="Meiryo" w:cs="Georgia""#),
    );
    // Another font is another font.
    assert_reported_by_both(
        &fonts(r#"w:ascii="Georgia" w:eastAsia="Meiryo" w:hAnsi="Georgia" w:cs="Georgia""#),
        &fonts(r#"w:ascii="Georgia" w:eastAsia="Yu Mincho" w:hAnsi="Georgia" w:cs="Georgia""#),
    );
}

/// A paragraph with one tracked deletion and one tracked insertion by Ines.
fn revised_paragraph(paragraph_attributes: &str, deleted: &str) -> String {
    format!(
        r#"<w:p {paragraph_attributes}><w:r><w:t xml:space="preserve">The ferry leaves at </w:t></w:r><w:del w:id="1" w:author="Ines" w:date="2024-03-01T09:00:00Z"><w:r><w:delText>{deleted}</w:delText></w:r></w:del><w:ins w:id="2" w:author="Ines" w:date="2024-03-01T09:00:00Z"><w:r><w:t>noon</w:t></w:r></w:ins><w:r><w:t>.</w:t></w:r></w:p>"#
    )
}

const PARAGRAPH_ID: &str =
    r#"xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" w14:paraId="0010A0B1""#;

/// Some programs give every paragraph an id when they save. A pending
/// deletion in such a paragraph is the same deletion it was: same author,
/// date, place and text. It is not one revision resolved and another made.
#[test]
fn paragraph_ids_added_on_save_leave_pending_revisions_as_they_were() {
    use stemma::audit::RevisionDisposition;
    let before = plain(&revised_paragraph("", "dawn"));
    let after = plain(&revised_paragraph(PARAGRAPH_ID, "dawn"));
    for equality in [UntouchedEquality::Fidelity, UntouchedEquality::Appearance] {
        let report = audit(&before, &after, equality);
        assert!(
            report.new_revisions.is_empty(),
            "{equality:?}: no revision was made: {:?}",
            report.new_revisions
        );
        assert_eq!(report.preexisting_revisions.len(), 2);
        for row in &report.preexisting_revisions {
            assert_eq!(
                row.disposition,
                RevisionDisposition::Untouched,
                "{equality:?}: {:?}",
                row.record
            );
        }
    }
}

/// The same save that also rewrote the deleted text did change the revision.
#[test]
fn a_rewritten_deletion_is_not_the_deletion_it_replaced() {
    use stemma::audit::RevisionDisposition;
    let before = plain(&revised_paragraph("", "dawn"));
    let after = plain(&revised_paragraph(PARAGRAPH_ID, "dusk"));
    let report = audit(&before, &after, UntouchedEquality::Appearance);
    assert!(
        report
            .preexisting_revisions
            .iter()
            .any(|row| row.disposition != RevisionDisposition::Untouched),
        "the deletion of 'dawn' is gone: {:?}",
        report.preexisting_revisions
    );
}
