//! Comparison reads caps-formatted text as it renders (`w:caps` shows
//! "Definitions" as DEFINITIONS, so the two compare equal), but the redline
//! stores the characters the inputs wrote. Uppercasing is how Word draws the
//! run, not what the document says: rejecting everything must give back the
//! base's letters, accepting everything the target's, and an unchanged caps
//! run must keep its stored text. Kept text whose spellings differ only in
//! letter case under caps keeps the base's spelling (same effective value,
//! different spelling: the redline keeps the base's), unless only the base
//! has caps: then only the target's letters render right once the caps
//! change is accepted.

use std::io::Write;

use stemma::api::Document;
use stemma::{ExportOptions, Resolution};

fn docx(body: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"></Relationships>"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = zip::write::FileOptions::default();
        for (name, part) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", document_xml.as_str()),
        ] {
            zip.start_file(name, opts).expect("start part");
            zip.write_all(part.as_bytes()).expect("write part");
        }
        zip.finish().expect("finish package");
    }
    buf
}

fn reopened(document: &Document) -> Document {
    let bytes = document
        .serialize(&ExportOptions::default())
        .expect("serialize");
    Document::parse(&bytes).expect("reopen")
}

fn text(document: &Document) -> String {
    document.to_text()
}

fn caps_run(text: &str) -> String {
    format!(r#"<w:r><w:rPr><w:caps/></w:rPr><w:t xml:space="preserve">{text}</w:t></w:r>"#)
}

fn plain_run(text: &str) -> String {
    format!(r#"<w:r><w:t xml:space="preserve">{text}</w:t></w:r>"#)
}

/// Readings of the redline, in memory and after a save and reopen.
fn readings(base: &str, target: &str) -> Vec<(String, String, String)> {
    let base = Document::parse(&docx(base)).expect("parse base");
    let target = Document::parse(&docx(target)).expect("parse target");
    let redline = stemma_diff::diff_as(&base, &target, "compare").expect("compare");
    let reopened = reopened(&redline);
    [redline, reopened]
        .iter()
        .map(|document| {
            (
                text(document),
                text(&document.project(Resolution::AcceptAll).expect("accept")),
                text(&document.project(Resolution::RejectAll).expect("reject")),
            )
        })
        .collect()
}

#[test]
fn an_unchanged_caps_run_keeps_its_stored_letters() {
    let base = format!(
        "<w:p>{}{}</w:p>",
        caps_run("Definitions"),
        plain_run(" apply to this agreement.")
    );
    let target = format!(
        "<w:p>{}{}</w:p>",
        caps_run("Definitions"),
        plain_run(" apply to this schedule.")
    );
    for (current, accepted, rejected) in readings(&base, &target) {
        assert!(
            current.starts_with("Definitions apply to this "),
            "{current}"
        );
        assert_eq!(accepted, "Definitions apply to this schedule.");
        assert_eq!(rejected, "Definitions apply to this agreement.");
    }
}

#[test]
fn changed_caps_text_is_deleted_and_inserted_as_written() {
    let base = format!(
        "<w:p>{}{}</w:p>",
        caps_run("Defined terms"),
        plain_run(" apply.")
    );
    let target = format!(
        "<w:p>{}{}</w:p>",
        caps_run("Key terms"),
        plain_run(" apply.")
    );
    let redline = stemma_diff::diff_as(
        &Document::parse(&docx(&base)).expect("parse base"),
        &Document::parse(&docx(&target)).expect("parse target"),
        "compare",
    )
    .expect("compare");
    let mut excerpts: Vec<String> = redline.revisions().into_iter().map(|r| r.excerpt).collect();
    excerpts.sort();
    assert_eq!(
        excerpts,
        ["Defined", "Key"],
        "the census shows the stored letters"
    );
    for (current, accepted, rejected) in readings(&base, &target) {
        assert!(
            !current.contains("TERMS"),
            "no uppercased letters stored: {current}"
        );
        assert_eq!(accepted, "Key terms apply.");
        assert_eq!(rejected, "Defined terms apply.");
    }
}

#[test]
fn caps_text_equal_as_rendered_is_still_no_change() {
    // The 2026-10 contract: caps-aware comparison. "Company Name" with caps
    // and "COMPANY NAME" render identically, so there is nothing to track.
    let base = format!(
        "<w:p>{}{}</w:p>",
        caps_run("Company Name"),
        plain_run(" signs.")
    );
    let target = format!("<w:p>{}</w:p>", plain_run("COMPANY NAME signs."));
    let base = Document::parse(&docx(&base)).expect("parse base");
    let target = Document::parse(&docx(&target)).expect("parse target");
    let comparison = stemma_diff::diff_as_detailed(&base, &target, "compare").expect("compare");
    let texts: Vec<_> = comparison
        .document
        .revisions()
        .iter()
        .filter(|r| {
            matches!(
                r.kind,
                stemma::tracked_model::RevisionKind::Insert
                    | stemma::tracked_model::RevisionKind::Delete
            )
        })
        .map(|r| r.excerpt.clone())
        .collect();
    assert!(texts.is_empty(), "{texts:?}");
}

/// Rendered reading: what Word draws, caps applied.
fn rendered(document: &Document) -> String {
    use stemma::domain::{BlockNode, InlineNode, MarkValue};
    document
        .snapshot()
        .canonical
        .blocks
        .iter()
        .filter_map(|block| match &block.block {
            BlockNode::Paragraph(paragraph) => Some(paragraph),
            _ => None,
        })
        .flat_map(|paragraph| paragraph.segments.iter().flat_map(|s| &s.inlines))
        .filter_map(|inline| match inline {
            InlineNode::Text(t) if t.style_props.caps == MarkValue::On => {
                Some(t.text.to_uppercase())
            }
            InlineNode::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect()
}

/// Both sides write the term under caps, in different letter case, next to
/// an unrelated edit. The kept term is the base's spelling: Reject gives the
/// base's exact stored letters; Accept renders as the target does.
#[test]
fn kept_caps_text_spelled_differently_keeps_the_base_spelling() {
    let base_xml = format!(
        "<w:p>{}{}</w:p>",
        caps_run("Definitions"),
        plain_run(" apply to this agreement.")
    );
    let target_xml = format!(
        "<w:p>{}{}</w:p>",
        caps_run("DEFINITIONS"),
        plain_run(" apply to this schedule.")
    );
    let base = Document::parse(&docx(&base_xml)).expect("parse base");
    let target = Document::parse(&docx(&target_xml)).expect("parse target");
    let redline = stemma_diff::diff_as(&base, &target, "compare").expect("compare");
    let reopened_redline = reopened(&redline);
    for document in [&redline, &reopened_redline] {
        let rejected = document.project(Resolution::RejectAll).expect("reject");
        let accepted = document.project(Resolution::AcceptAll).expect("accept");
        assert_eq!(
            text(&rejected),
            text(&base),
            "Reject gives the base's stored letters"
        );
        assert_eq!(
            rendered(&accepted),
            rendered(&target),
            "Accept renders as the target"
        );
    }
}

/// Caps on the base only, the target writing the capitals out: the kept span
/// carries a tracked caps change, so it keeps the target's letters (the only
/// spelling that renders right once the change is accepted); Reject renders
/// as the base does.
#[test]
fn kept_text_whose_caps_setting_changes_renders_right_both_ways() {
    let base_xml = format!(
        "<w:p>{}{}</w:p>",
        caps_run("Company Name"),
        plain_run(" signs here.")
    );
    let target_xml = format!("<w:p>{}</w:p>", plain_run("COMPANY NAME signs there."));
    let base = Document::parse(&docx(&base_xml)).expect("parse base");
    let target = Document::parse(&docx(&target_xml)).expect("parse target");
    let redline = stemma_diff::diff_as(&base, &target, "compare").expect("compare");
    let reopened_redline = reopened(&redline);
    for document in [&redline, &reopened_redline] {
        let rejected = document.project(Resolution::RejectAll).expect("reject");
        let accepted = document.project(Resolution::AcceptAll).expect("accept");
        assert_eq!(
            rendered(&rejected),
            rendered(&base),
            "Reject renders as the base"
        );
        assert_eq!(
            rendered(&accepted),
            rendered(&target),
            "Accept renders as the target"
        );
        assert_eq!(text(&accepted), text(&target));
    }
}

/// The four ways a kept span's caps setting can stand on the two sides
/// (ECMA-376 §17.3.2.5: caps is display formatting, the run keeps its own
/// letters). Laws: Reject All renders as the base and Accept All as the
/// target; the kept span is a formatting change at most, never a deletion
/// and insertion (the two sides render the same letters). The redline stores
/// one spelling for both readings: when the caps setting changes, only the
/// letters of the side without caps render right on both sides (caps draws
/// any letters in capitals, but no caps draws the stored letters as they
/// are), so that side's reading is also its exact stored letters; when the
/// setting is the same, the base's spelling is kept.
#[test]
fn every_caps_direction_renders_right_and_keeps_the_uncapped_sides_letters() {
    struct Case {
        name: &'static str,
        base: String,
        target: String,
        reject_stores_base: bool,
        accept_stores_target: bool,
    }
    let tail_base = plain_run(" files the annual return.");
    let tail_target = plain_run(" files the quarterly return.");
    let cases = [
        Case {
            name: "caps on the target only",
            base: format!("<w:p>{}{tail_base}</w:p>", plain_run("HARBOUR LEDGER")),
            target: format!("<w:p>{}{tail_target}</w:p>", caps_run("Harbour Ledger")),
            reject_stores_base: true,
            accept_stores_target: false,
        },
        Case {
            name: "caps on the base only",
            base: format!("<w:p>{}{tail_base}</w:p>", caps_run("Harbour Ledger")),
            target: format!("<w:p>{}{tail_target}</w:p>", plain_run("HARBOUR LEDGER")),
            reject_stores_base: false,
            accept_stores_target: true,
        },
        Case {
            name: "caps on both, letters differ",
            base: format!("<w:p>{}{tail_base}</w:p>", caps_run("Harbour Ledger")),
            target: format!("<w:p>{}{tail_target}</w:p>", caps_run("HARBOUR ledger")),
            reject_stores_base: true,
            accept_stores_target: false,
        },
        Case {
            name: "caps on neither",
            base: format!("<w:p>{}{tail_base}</w:p>", plain_run("Harbour Ledger")),
            target: format!("<w:p>{}{tail_target}</w:p>", plain_run("Harbour Ledger")),
            reject_stores_base: true,
            accept_stores_target: true,
        },
    ];
    for case in cases {
        let base = Document::parse(&docx(&case.base)).expect("parse base");
        let target = Document::parse(&docx(&case.target)).expect("parse target");
        let redline = stemma_diff::diff_as(&base, &target, "compare").expect("compare");
        let mut excerpts: Vec<String> = redline
            .revisions()
            .into_iter()
            .filter(|r| {
                matches!(
                    r.kind,
                    stemma::tracked_model::RevisionKind::Insert
                        | stemma::tracked_model::RevisionKind::Delete
                )
            })
            .map(|r| r.excerpt)
            .collect();
        excerpts.sort();
        assert_eq!(
            excerpts,
            ["annual", "quarterly"],
            "{}: only the tail's word is replaced",
            case.name
        );
        let reopened_redline = reopened(&redline);
        for document in [&redline, &reopened_redline] {
            let rejected = document.project(Resolution::RejectAll).expect("reject");
            let accepted = document.project(Resolution::AcceptAll).expect("accept");
            assert_eq!(
                rendered(&rejected),
                rendered(&base),
                "{}: Reject renders as the base",
                case.name
            );
            assert_eq!(
                rendered(&accepted),
                rendered(&target),
                "{}: Accept renders as the target",
                case.name
            );
            if case.reject_stores_base {
                assert_eq!(text(&rejected), text(&base), "{}: Reject stores", case.name);
            }
            if case.accept_stores_target {
                assert_eq!(
                    text(&accepted),
                    text(&target),
                    "{}: Accept stores",
                    case.name
                );
            }
        }
    }
}

fn page_field(result: &str) -> String {
    format!(
        r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> PAGE </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>{result}</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r>"#
    )
}

/// A heavily edited caps paragraph is replaced as one deletion and one
/// insertion. The words both sides keep are spelled differently ("Notice"
/// against "NOTICE", equal as rendered), and once they sit inside the
/// replacement they are no longer kept text: the deletion writes the base's
/// letters and the insertion the target's, so Reject All stores the base and
/// Accept All the target, exactly. With and without a complex field in the
/// paragraph (a field is kept or replaced whole).
#[test]
fn a_collapsed_caps_region_writes_each_sides_letters() {
    let base_words = "Notice of the tenant to the owner is due by the first day of each month";
    let target_words = "NOTICE FROM A TENANT FOR ANY OWNER WAS DUE ON THE LAST DAY IN EACH WEEK";
    for with_field in [true, false] {
        let field = if with_field {
            page_field("4")
        } else {
            String::new()
        };
        let base_xml = format!(
            "<w:p>{}{}{field}</w:p>",
            caps_run(base_words),
            plain_run(" page ")
        );
        let target_xml = format!(
            "<w:p>{}{}{field}</w:p>",
            caps_run(target_words),
            plain_run(" page ")
        );
        let base = Document::parse(&docx(&base_xml)).expect("parse base");
        let target = Document::parse(&docx(&target_xml)).expect("parse target");
        let redline = stemma_diff::diff_as(&base, &target, "compare").expect("compare");
        let excerpts: Vec<String> = redline.revisions().into_iter().map(|r| r.excerpt).collect();
        assert!(
            excerpts.iter().any(|e| e.contains("Notice")),
            "field {with_field}: the region is replaced whole, the base's kept words deleted as written: {excerpts:?}"
        );
        let reopened_redline = reopened(&redline);
        for document in [&redline, &reopened_redline] {
            let rejected = document.project(Resolution::RejectAll).expect("reject");
            let accepted = document.project(Resolution::AcceptAll).expect("accept");
            assert_eq!(text(&rejected), text(&base), "field {with_field}: Reject");
            assert_eq!(text(&accepted), text(&target), "field {with_field}: Accept");
        }
    }
}
