//! Package-level update-fields-on-open invariants (daily tier).
//!
//! `set_update_fields_on_open` authors the `w:updateFields` toggle in
//! `word/settings.xml` (ISO 29500-1 §17.15.1.81). It is an **untracked,
//! package-level** operation — NOT an edit transaction. The contract pinned
//! here:
//!
//! 1. setting on, reserializing, and reparsing surfaces the value;
//! 2. the body (`word/document.xml`) is byte-unchanged and carries NO
//!    `w:ins`/`w:del`/`pPrChange` — the setting is not a tracked change;
//! 3. the serialized package is post-validator-clean;
//! 4. on a document with no settings part, the part is synthesized (and
//!    content-type override + relationship registered) so Word recognizes it;
//! 5. cached field *results* are NOT fabricated — we only set the flag (the
//!    field run text in the body is preserved verbatim);
//! 6. `Some(false)` (explicit off) is distinct from `None` (absent).

use stemma::ExportOptions;
use stemma::api::Document;

/// A DOCX with a body that contains a complex field (a REF whose cached result
/// is "stale-result") and NO `word/settings.xml`. This lets us assert both the
/// part-synthesis path and that the cached result text is preserved verbatim.
fn make_docx_with_field(settings_xml: Option<&str>) -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> REF _Ref1 \h </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>stale-result</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p><w:sectPr/></w:body></w:document>"#;

    let mut overrides = String::new();
    let mut rels_extra = String::new();
    if settings_xml.is_some() {
        overrides.push_str(r#"<Override PartName="/word/settings.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml"/>"#);
        rels_extra.push_str(r#"<Relationship Id="rId10" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings" Target="settings.xml"/>"#);
    }
    let content_types = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>{overrides}</Types>"#
    );
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{rels_extra}</Relationships>"#
    );

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
        if let Some(s) = settings_xml {
            zip.start_file("word/settings.xml", opts).unwrap();
            zip.write_all(s.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

/// Read one part's bytes from a serialized DOCX.
fn part_bytes(docx: &[u8], name: &str) -> Option<Vec<u8>> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(docx)).expect("zip");
    let mut file = zip.by_name(name).ok()?;
    use std::io::Read;
    let mut data = Vec::new();
    file.read_to_end(&mut data).expect("read part");
    Some(data)
}

/// Assert no error-severity findings from the post-serialization validator.
fn assert_validator_clean(bytes: &[u8], label: &str) {
    let validation = stemma::docx_validate::validate_docx(bytes);
    let errors: Vec<String> = validation
        .findings
        .iter()
        .filter(|f| matches!(f.severity, stemma::docx_validate::ValidationSeverity::Error))
        .map(|f| f.to_string())
        .collect();
    assert!(
        errors.is_empty(),
        "[{label}] must be validator-clean, got: {errors:#?}"
    );
}

#[test]
fn set_on_visible_after_round_trip_and_synthesizes_part() {
    let base = Document::parse(&make_docx_with_field(None)).expect("parse");
    assert_eq!(
        base.update_fields_on_open().unwrap(),
        None,
        "fixture must start with no updateFields assertion"
    );

    let edited = base
        .set_update_fields_on_open(Some(true))
        .expect("set updateFields on");
    assert_eq!(edited.update_fields_on_open().unwrap(), Some(true));

    // Survives serialize -> reparse.
    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let reparsed = Document::parse(&bytes).expect("reparse");
    assert_eq!(reparsed.update_fields_on_open().unwrap(), Some(true));

    // The settings part was synthesized and carries the explicit flag.
    let settings = part_bytes(&bytes, "word/settings.xml").expect("settings.xml synthesized");
    let settings_str = String::from_utf8(settings).expect("utf8");
    assert!(
        settings_str.contains("updateFields") && settings_str.contains(r#"w:val="true""#),
        "synthesized settings must carry updateFields w:val=true: {settings_str}"
    );
}

#[test]
fn set_on_is_validator_clean() {
    let base = Document::parse(&make_docx_with_field(None)).expect("parse");
    let edited = base
        .set_update_fields_on_open(Some(true))
        .expect("set updateFields on");
    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    assert_validator_clean(&bytes, "update_fields_on");
}

#[test]
fn setting_is_untracked_and_preserves_body_field_result() {
    let base = Document::parse(&make_docx_with_field(None)).expect("parse");
    let base_bytes = base
        .serialize(&ExportOptions::default())
        .expect("serialize base");
    let base_doc_xml = part_bytes(&base_bytes, "word/document.xml").expect("base document.xml");

    let edited = base
        .set_update_fields_on_open(Some(true))
        .expect("set updateFields on");
    let edited_bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize edited");
    let edited_doc_xml =
        part_bytes(&edited_bytes, "word/document.xml").expect("edited document.xml");

    // The body part is byte-for-byte identical: the setting lives in settings.xml.
    assert_eq!(
        base_doc_xml, edited_doc_xml,
        "update-fields setting must leave word/document.xml byte-unchanged"
    );

    // No tracked-change markup is introduced anywhere in the body. Match the
    // delimited element tag (`<w:ins>` / `<w:ins ` / `<w:del>` / `<w:del `), NOT
    // the bare substring — a field run's `<w:instrText>` contains "<w:ins".
    let body = String::from_utf8(edited_doc_xml).expect("utf8");
    assert!(
        !body.contains("<w:ins ") && !body.contains("<w:ins>"),
        "the setting is not a tracked insert"
    );
    assert!(
        !body.contains("<w:del ") && !body.contains("<w:del>"),
        "the setting is not a tracked delete"
    );
    assert!(
        !body.contains("pPrChange"),
        "the setting is not a tracked format change"
    );

    // The cached field RESULT text is preserved verbatim — we do NOT fabricate a
    // recomputed value; we only ask Word to refresh on open.
    assert!(
        body.contains("stale-result"),
        "cached field result must be preserved verbatim (no fabricated recompute): {body}"
    );
}

#[test]
fn preserves_existing_settings() {
    let existing = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:defaultTabStop w:val="708"/></w:settings>"#;
    let base = Document::parse(&make_docx_with_field(Some(existing))).expect("parse");
    let edited = base
        .set_update_fields_on_open(Some(true))
        .expect("set updateFields on");
    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let settings_str = String::from_utf8(part_bytes(&bytes, "word/settings.xml").unwrap()).unwrap();
    assert!(
        settings_str.contains("defaultTabStop"),
        "existing settings must survive the updateFields write: {settings_str}"
    );
    assert_eq!(
        Document::parse(&bytes)
            .unwrap()
            .update_fields_on_open()
            .unwrap(),
        Some(true)
    );
    assert_validator_clean(&bytes, "update_fields_preserve_existing");
}

#[test]
fn explicit_off_distinct_from_absent() {
    let base = Document::parse(&make_docx_with_field(None)).expect("parse");

    let off = base
        .set_update_fields_on_open(Some(false))
        .expect("set off");
    assert_eq!(off.update_fields_on_open().unwrap(), Some(false));

    let cleared = off.set_update_fields_on_open(None).expect("clear");
    assert_eq!(
        cleared.update_fields_on_open().unwrap(),
        None,
        "None removes the element entirely — distinct from explicit off"
    );
}
