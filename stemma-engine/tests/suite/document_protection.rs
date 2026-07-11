//! `w:documentProtection` import contract (ISO/IEC 29500-1 §17.15.1.29).
//!
//! A protected document is NOT refused at import. The engine reports the
//! declaration honestly and leaves policy to the host:
//!
//! 1. the declaration is a typed, queryable fact on the opened document
//!    (`Document::document_protection`), with the edit mode, three-state
//!    enforcement, and credential-presence flag;
//! 2. an enforced declaration emits an import diagnostic (through the existing
//!    import-diagnostics channel) saying engine edits do not honor protection;
//! 3. the `w:documentProtection` element round-trips VERBATIM through
//!    `word/settings.xml` — the engine never enforces, rewrites, or drops it;
//! 4. an out-of-enum `w:edit` value fails the import loudly (no silent coercion).

use stemma::ExportOptions;
use stemma::api::Document;
use stemma::{DocProtectEdit, DocxRuntime, SimpleRuntime};

/// Build a DOCX whose `word/settings.xml` is exactly `settings_xml` (or omit the
/// part when `None`). The body is a single trivial paragraph.
fn make_docx(settings_xml: Option<&str>) -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>hello</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;

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

/// A settings.xml carrying exactly the given `documentProtection` element.
fn settings_with(protection_element: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{protection_element}</w:settings>"#
    )
}

fn part_bytes(docx: &[u8], name: &str) -> Option<Vec<u8>> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(docx)).expect("zip");
    let mut file = zip.by_name(name).ok()?;
    use std::io::Read;
    let mut data = Vec::new();
    file.read_to_end(&mut data).expect("read part");
    Some(data)
}

#[test]
fn absent_protection_reads_none_and_emits_no_diagnostic() {
    // A document with settings.xml but no documentProtection → None, no diag.
    let bytes = make_docx(Some(&settings_with(r#"<w:defaultTabStop w:val="720"/>"#)));
    let doc = Document::parse(&bytes).expect("parse");
    assert!(doc.document_protection().is_none());

    let rt = SimpleRuntime::new();
    let imported = rt.import_docx(&bytes).expect("import");
    assert!(imported.canonical.document_protection.is_none());
    assert!(
        !imported
            .diagnostics
            .iter()
            .any(|d| d.message.contains("protection")),
        "no protection diagnostic for an unprotected document"
    );
}

#[test]
fn no_settings_part_reads_none() {
    let bytes = make_docx(None);
    let doc = Document::parse(&bytes).expect("parse");
    assert!(doc.document_protection().is_none());
}

#[test]
fn each_edit_mode_reads_back() {
    for (raw, expected) in [
        ("none", DocProtectEdit::None),
        ("readOnly", DocProtectEdit::ReadOnly),
        ("comments", DocProtectEdit::Comments),
        ("trackedChanges", DocProtectEdit::TrackedChanges),
        ("forms", DocProtectEdit::Forms),
    ] {
        let bytes = make_docx(Some(&settings_with(&format!(
            r#"<w:documentProtection w:edit="{raw}" w:enforcement="1"/>"#
        ))));
        let doc = Document::parse(&bytes).expect("parse");
        let p = doc.document_protection().expect("protection present");
        assert_eq!(p.edit, Some(expected), "edit={raw}");
        assert_eq!(p.enforcement, Some(true));
        assert!(!p.has_credential);
    }
}

#[test]
fn enforcement_off_is_declared_but_inert_and_emits_no_diagnostic() {
    // enforcement="0" is a declared-but-inert restriction: the flag reads back,
    // but no diagnostic fires (nothing is being enforced).
    let bytes = make_docx(Some(&settings_with(
        r#"<w:documentProtection w:edit="readOnly" w:enforcement="0"/>"#,
    )));
    let doc = Document::parse(&bytes).expect("parse");
    let p = doc.document_protection().expect("present");
    assert_eq!(p.edit, Some(DocProtectEdit::ReadOnly));
    assert_eq!(p.enforcement, Some(false));

    let rt = SimpleRuntime::new();
    let imported = rt.import_docx(&bytes).expect("import");
    assert!(
        !imported
            .diagnostics
            .iter()
            .any(|d| d.message.contains("enforced protection")),
        "enforcement=off must not emit the enforced-protection diagnostic"
    );
}

#[test]
fn enforced_protection_emits_diagnostic_naming_edit_mode() {
    let bytes = make_docx(Some(&settings_with(
        r#"<w:documentProtection w:edit="trackedChanges" w:enforcement="1"/>"#,
    )));
    let rt = SimpleRuntime::new();
    let imported = rt.import_docx(&bytes).expect("import");
    let diag = imported
        .diagnostics
        .iter()
        .find(|d| d.message.contains("enforced protection"))
        .expect("enforced-protection diagnostic must fire");
    assert!(
        diag.message.contains("edit=trackedChanges"),
        "diagnostic must name the edit mode: {}",
        diag.message
    );
    assert!(
        diag.message.contains("do not honor protection"),
        "diagnostic must state edits are not honored: {}",
        diag.message
    );
}

#[test]
fn credential_presence_flag_set_without_storing_material() {
    // A real-world protected doc with legacy hash/salt: presence is reported.
    let bytes = make_docx(Some(&settings_with(
        r#"<w:documentProtection w:edit="forms" w:enforcement="1" w:cryptProviderType="rsaAES" w:cryptAlgorithmClass="hash" w:cryptAlgorithmType="typeAny" w:cryptAlgorithmSid="14" w:cryptSpinCount="100000" w:hash="9oe2mQ+abc/DEF+ghiJKL==" w:salt="mn0PqR+stu/VWX=="/>"#,
    )));
    let doc = Document::parse(&bytes).expect("parse");
    let p = doc.document_protection().expect("present");
    assert!(p.has_credential, "hash/salt presence must be reported");
    assert_eq!(p.edit, Some(DocProtectEdit::Forms));
}

#[test]
fn protection_element_round_trips_verbatim_through_settings() {
    // The engine never enforces or rewrites protection: the element survives
    // serialize unchanged, and the flag reads back identically after reparse.
    let element = r#"<w:documentProtection w:edit="readOnly" w:enforcement="1" w:cryptProviderType="rsaAES" w:cryptAlgorithmClass="hash" w:cryptAlgorithmType="typeAny" w:cryptAlgorithmSid="14" w:cryptSpinCount="100000" w:hash="9oe2mQ+abc/DEF+ghiJKL==" w:salt="mn0PqR+stu/VWX=="/>"#;
    let bytes = make_docx(Some(&settings_with(element)));

    let doc = Document::parse(&bytes).expect("parse");
    let before = doc.document_protection().cloned().expect("present");

    let out = doc.serialize(&ExportOptions::default()).expect("serialize");
    let out_settings =
        String::from_utf8(part_bytes(&out, "word/settings.xml").expect("settings.xml")).unwrap();
    assert!(
        out_settings.contains(element),
        "documentProtection must round-trip verbatim; got settings: {out_settings}"
    );

    let reparsed = Document::parse(&out).expect("reparse");
    assert_eq!(
        reparsed.document_protection().cloned(),
        Some(before),
        "the declared protection fact must be identical after serialize -> reparse"
    );
}

#[test]
fn unknown_edit_value_fails_import_loudly() {
    let bytes = make_docx(Some(&settings_with(
        r#"<w:documentProtection w:edit="lockdown" w:enforcement="1"/>"#,
    )));
    let err = match Document::parse(&bytes) {
        Ok(_) => panic!("unknown w:edit must fail the import"),
        Err(e) => e,
    };
    assert!(
        err.message.contains("lockdown"),
        "error must name the offending value: {}",
        err.message
    );
}
