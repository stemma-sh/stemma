//! A note of several paragraphs is edited paragraph by paragraph.
//!
//! Domain rule: an `edit_note` body with line breaks gives one line per note
//! paragraph, in order, and each paragraph is edited in place (a word-diff,
//! as a body paragraph's `replace` is), so its properties survive: a
//! footnote's list items stay list items when one item's words change. A
//! body whose line count is not the note's paragraph count is refused, never
//! flattened into one paragraph; a one-line body cannot tracked-edit a note of
//! several paragraphs.

use stemma::ExportOptions;
use stemma::api::Document;

const ITEM_INDENT: &str = r#"<w:ind w:left="720" w:hanging="360"/>"#;

fn docx() -> Vec<u8> {
    let w = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
    let footnotes_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:footnotes xmlns:w="{w}"><w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote><w:footnote w:id="1"><w:p><w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:footnoteRef/></w:r><w:r><w:t xml:space="preserve"> Accepted proofs of address:</w:t></w:r></w:p><w:p><w:pPr>{ITEM_INDENT}</w:pPr><w:r><w:t>a utility bill from the last three months;</w:t></w:r></w:p><w:p><w:pPr>{ITEM_INDENT}</w:pPr><w:r><w:t>a bank statement showing the</w:t></w:r></w:p></w:footnote></w:footnotes>"#
    );
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{w}"><w:body><w:p><w:r><w:t>Proof of address is required.</w:t></w:r><w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:footnoteReference w:id="1"/></w:r></w:p><w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/footnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdFn" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes" Target="footnotes.xml"/></Relationships>"#;
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions = zip::write::FileOptions::default();
        for (name, data) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", document_xml.as_str()),
            ("word/footnotes.xml", footnotes_xml.as_str()),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

fn edit_note(doc: &Document, body: &str, mode: &str) -> Result<Document, String> {
    let json = serde_json::json!({"ops":[{"op":"edit_note","note_id":"1","note_kind":"footnote",
        "body":body}],"materialization_mode":mode,"revision":{"author":"Editor"}})
    .to_string();
    let txn = stemma::edit_v4::parse_transaction(&json)
        .map_err(|e| format!("{e:?}"))?
        .into_edit_transaction()
        .map_err(|e| format!("{e:?}"))?;
    doc.apply(&txn).map_err(|e| e.to_string())
}

/// Each paragraph of footnote 1 as (pPr, visible text).
fn note_paragraphs(doc: &Document) -> Vec<(String, String)> {
    let bytes = doc.serialize(&ExportOptions::default()).expect("save");
    let xml = String::from_utf8(
        stemma::docx::DocxArchive::read(&bytes)
            .unwrap()
            .get("word/footnotes.xml")
            .unwrap()
            .to_vec(),
    )
    .unwrap()
    .replace(" />", "/>");
    let start = xml.find(r#"w:id="1""#).expect("footnote 1");
    let note = &xml[start..start + xml[start..].find("</w:footnote>").unwrap()];
    note.split("</w:p>")
        .filter(|p| p.contains("<w:r"))
        .map(|p| {
            let ppr = p
                .find("<w:pPr>")
                .map(|i| p[i..i + p[i..].find("</w:pPr>").unwrap()].to_string())
                .unwrap_or_default();
            let mut text = String::new();
            let mut rest = p;
            while let Some(at) = rest.find("<w:t") {
                rest = &rest[at..];
                let open_end = rest.find('>').unwrap() + 1;
                let close = rest.find("</w:t>").unwrap();
                if !rest[..open_end].ends_with("/>") && rest.starts_with("<w:t") {
                    text.push_str(&rest[open_end..close]);
                }
                rest = &rest[close + "</w:t>".len()..];
            }
            (ppr, text)
        })
        .collect()
}

const FIXED: &str = " Accepted proofs of address:\na utility bill from the last three months;\na bank statement showing the current address and the account holder's full name.";

#[test]
fn each_line_edits_its_paragraph_and_the_list_items_survive() {
    let doc = Document::parse(&docx()).expect("parse");
    for mode in ["tracked_change", "direct"] {
        let edited = edit_note(&doc, FIXED, mode).unwrap_or_else(|e| panic!("{mode}: {e}"));
        let accepted = edited.read_accepted().expect("accept");
        let paragraphs = note_paragraphs(&accepted);
        assert_eq!(
            paragraphs.len(),
            3,
            "{mode}: three paragraphs: {paragraphs:?}"
        );
        assert_eq!(
            paragraphs[2].1,
            "a bank statement showing the current address and the account holder's full name.",
            "{mode}"
        );
        assert_eq!(
            paragraphs[1].1, "a utility bill from the last three months;",
            "{mode}: the first item is unchanged"
        );
        for item in &paragraphs[1..] {
            assert!(
                item.0.contains(ITEM_INDENT),
                "{mode}: the items keep their indent: {item:?}"
            );
        }
        if mode == "tracked_change" {
            let rejected = note_paragraphs(&edited.read_rejected().expect("reject"));
            assert_eq!(
                rejected[2].1, "a bank statement showing the",
                "reject restores the item"
            );
        }
    }
}

#[test]
fn a_body_whose_lines_do_not_match_the_paragraphs_is_refused() {
    let doc = Document::parse(&docx()).expect("parse");
    for mode in ["tracked_change", "direct"] {
        let Err(err) = edit_note(&doc, "one line\nanother line", mode) else {
            panic!("{mode}: two lines for three paragraphs must be refused");
        };
        assert!(
            err.contains("3 paragraphs") && err.contains("2 lines"),
            "{mode}: {err}"
        );
    }
}

#[test]
fn a_one_line_body_cannot_tracked_edit_a_note_of_several_paragraphs() {
    let doc = Document::parse(&docx()).expect("parse");
    let Err(err) = edit_note(&doc, "A single new body.", "tracked_change") else {
        panic!("a one-line tracked edit of a three-paragraph note must be refused");
    };
    assert!(err.contains("one line per paragraph"), "{err}");
}
