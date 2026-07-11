//! Integration tests for native table-of-contents insertion via the v4 wire
//! `insert` op with a `{"type":"toc"}` content block.
//!
//! Domain rule: `resolve_toc_spec` (engine-internal) already builds a
//! synthesized `w:fldSimple` TOC field from a role exemplar; this wires it up
//! to the LLM-facing `insert` op. The contract pinned here:
//!
//! 1. `{"type":"toc"}` (levels omitted) produces the field with Word's own
//!    default range/switches (`TOC \o "1-3" \h \z \u`), anchored to the
//!    document's default body paragraph role — no `role` token required;
//! 2. explicit `levels` flow into the instruction text;
//! 3. inserting a toc sets `w:updateFields` on the package, in the SAME
//!    commit, so Word populates the entries on open;
//! 4. tracked mode: the insert is a normal tracked block insert — reject-all
//!    removes it, accept-all keeps it, both leaving a validator-clean
//!    document;
//! 5. the field survives a save + reopen roundtrip.

use stemma::ExportOptions;
use stemma::Resolution;
use stemma::api::Document;
use stemma::docx::DocxArchive;
use stemma::domain::TocFieldSpec;
use stemma::edit_v4::parse_transaction;

/// A doc with real heading paragraphs (Heading1/Heading2, recognized as Word
/// built-in styles even with no `word/styles.xml` part) plus three plain body
/// paragraphs — enough for `default_body_role_id` to pick the body role over
/// either heading role (3 body paragraphs outnumber 1 Heading1 + 1 Heading2).
fn make_doc_with_headings() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Introduction</w:t></w:r></w:p>
<w:p><w:r><w:t>This document explains the process end to end.</w:t></w:r></w:p>
<w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr><w:r><w:t>Background</w:t></w:r></w:p>
<w:p><w:r><w:t>More detail about the background of the project.</w:t></w:r></w:p>
<w:p><w:r><w:t>Final closing remarks for readers.</w:t></w:r></w:p>
<w:sectPr/></w:body></w:document>"#;

    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;

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

fn parse() -> (Document, Vec<String>) {
    let doc = Document::parse(&make_doc_with_headings()).expect("parse fixture");
    let ids = doc.read().blocks.iter().map(|b| b.id.to_string()).collect();
    (doc, ids)
}

/// Apply a v4 insert of `content_json` (a single `Block::Toc` payload) before
/// `anchor_id`. Returns the edited `Document`.
fn insert_toc(doc: &Document, anchor_id: &str, content_json: &str) -> Result<Document, String> {
    insert_toc_mode(doc, anchor_id, content_json, None)
}

/// Same insert with an explicit v4 `materialization_mode` (e.g. `"direct"`).
fn insert_toc_mode(
    doc: &Document,
    anchor_id: &str,
    content_json: &str,
    mode: Option<&str>,
) -> Result<Document, String> {
    let mode_field = mode
        .map(|m| format!(r#""materialization_mode": "{m}","#))
        .unwrap_or_default();
    let json = format!(
        r#"{{
          {mode_field}
          "ops": [{{
            "op": "insert",
            "target": {{ "anchor": "{anchor_id}", "position": "before" }},
            "content": [{content_json}]
          }}],
          "revision": {{ "author": "Agent" }}
        }}"#
    );
    let txn = parse_transaction(&json)
        .map_err(|e| e.to_string())?
        .into_edit_transaction()
        .map_err(|e| e.to_string())?;
    doc.apply(&txn).map_err(|e| e.to_string())
}

fn document_xml(bytes: &[u8]) -> String {
    let archive = DocxArchive::read(bytes).expect("read docx");
    String::from_utf8(
        archive
            .get("word/document.xml")
            .expect("document.xml")
            .to_vec(),
    )
    .expect("utf8")
}

fn settings_xml(bytes: &[u8]) -> Option<String> {
    let archive = DocxArchive::read(bytes).expect("read docx");
    archive
        .get("word/settings.xml")
        .map(|b| String::from_utf8(b.to_vec()).expect("utf8"))
}

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

/// The exact `w:instr` attribute value serialized XML carries for `text`
/// (`"` escaped to `&quot;`, matching the serializer's attribute encoding).
fn xml_attr_escaped(text: &str) -> String {
    text.replace('"', "&quot;")
}

// ─── 1. Default levels + switches; updateFields set in the same commit ───────

#[test]
fn toc_insert_default_instruction_text_and_updatefields() {
    let (doc, ids) = parse();
    let edited = insert_toc(&doc, &ids[0], r#"{"type":"toc"}"#).expect("insert toc");

    // Product default: `w:updateFields` is forced on as part of THIS commit.
    assert_eq!(
        edited.snapshot().update_fields_on_open().unwrap(),
        Some(true),
        "inserting a toc must set updateFields on open in the same commit"
    );

    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let xml = document_xml(&bytes);
    // A freshly tracked-INSERTED field is lowered to the complex form (`begin`
    // + `instrText` + `end`, all inside one `w:ins`) — `w:fldSimple` legally
    // cannot ride inside `w:ins` (see `append_tracked_inserted_complex_field`'s
    // doc comment, verified against the live-Word oracle). The plain
    // `w:fldSimple` form only appears once the insert is ACCEPTED (see
    // `toc_insert_tracked_accept_all_keeps_it` below).
    assert!(
        xml.contains("w:fldChar") && xml.contains("w:instrText"),
        "pending tracked insert must be the lowered complex-field form: {xml}"
    );

    let expected_instr = TocFieldSpec {
        levels: stemma::domain::TocLevelsSpec { from: 1, to: 3 },
        include_hyperlinks: true,
        hide_page_numbers_in_web: true,
        use_outline_levels: true,
    }
    .instruction_text();
    assert_eq!(expected_instr, r#"TOC \o "1-3" \h \z \u"#);
    assert!(
        xml.contains(&expected_instr),
        "expected instr {expected_instr:?} in: {xml}"
    );

    let settings = settings_xml(&bytes).expect("settings.xml present");
    assert!(
        settings.contains("updateFields") && settings.contains(r#"w:val="true""#),
        "settings.xml must carry updateFields=true: {settings}"
    );

    assert_validator_clean(&bytes, "toc insert default levels");
}

// ─── 2. Explicit levels flow into the instruction text ───────────────────────

#[test]
fn toc_insert_explicit_levels_flow_into_instruction_text() {
    let (doc, ids) = parse();
    let edited = insert_toc(
        &doc,
        &ids[0],
        r#"{"type":"toc","levels":{"from":2,"to":4}}"#,
    )
    .expect("insert toc with explicit levels");

    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let xml = document_xml(&bytes);
    // Pending tracked insert: complex-field form, instr is element text (no
    // attribute escaping — see the default-levels test above for why).
    assert!(
        xml.contains(r#"\o "2-4""#),
        "expected levels 2-4 in instr: {xml}"
    );
}

// ─── 3. Invalid levels refuse at the wire edge, naming the constraint ────────

#[test]
fn toc_insert_invalid_levels_refused() {
    let (_doc, ids) = parse();
    let json = format!(
        r#"{{
          "ops": [{{ "op": "insert",
                     "target": {{ "anchor": "{}", "position": "before" }},
                     "content": [{{"type":"toc","levels":{{"from":5,"to":2}}}}] }}],
          "revision": {{ "author": "Agent" }}
        }}"#,
        ids[0]
    );
    let err = parse_transaction(&json).expect_err("inverted levels must be refused");
    assert!(
        err.to_string().contains("1 <= from <= to <= 9"),
        "error must name the constraint, got: {err}"
    );
}

// ─── 4. Tracked mode: reject-all removes it, accept-all keeps it ────────────

#[test]
fn toc_insert_tracked_reject_all_removes_it_cleanly() {
    let (doc, ids) = parse();
    let edited = insert_toc(&doc, &ids[0], r#"{"type":"toc"}"#).expect("insert toc");

    let rejected = edited.project(Resolution::RejectAll).expect("reject-all");
    let bytes = rejected
        .serialize(&ExportOptions::default())
        .expect("serialize rejected");
    let xml = document_xml(&bytes);
    assert!(
        !xml.contains("fldSimple") && !xml.contains("w:instrText") && !xml.contains("TOC"),
        "reject-all must remove the inserted toc field entirely: {xml}"
    );
    assert_validator_clean(&bytes, "toc insert reject-all");
}

#[test]
fn toc_insert_tracked_accept_all_keeps_it() {
    let (doc, ids) = parse();
    let edited = insert_toc(&doc, &ids[0], r#"{"type":"toc"}"#).expect("insert toc");

    let accepted = edited.project(Resolution::AcceptAll).expect("accept-all");
    let bytes = accepted
        .serialize(&ExportOptions::default())
        .expect("serialize accepted");
    let xml = document_xml(&bytes);
    // Accepted (steady-state, no pending revision) content reverts to the
    // plain `w:fldSimple` form (`w:instr` is an XML ATTRIBUTE here, so its
    // quotes ARE escaped — unlike the pending complex-field form's
    // `w:instrText` element text, which carries them literally).
    assert!(
        xml.contains("w:fldSimple"),
        "accept-all must keep the inserted toc field: {xml}"
    );
    let expected_instr_attr = xml_attr_escaped(r#"TOC \o "1-3" \h \z \u"#);
    assert!(
        xml.contains(&expected_instr_attr),
        "expected instr attr {expected_instr_attr:?} in: {xml}"
    );
    // Accepted output carries no leftover w:ins wrapper around the kept block.
    assert!(
        !xml.contains("<w:ins "),
        "accept-all must not leave a w:ins wrapper: {xml}"
    );
    assert_validator_clean(&bytes, "toc insert accept-all");
}

// ─── 4b. DIRECT mode: same field as accept-all(tracked) — the two must agree ─
//
// Domain rule (edit_direct_mode.rs): Direct == the already-accepted state of
// the same TrackedChange edit. Accept-all of a tracked toc insert keeps a
// `w:fldSimple` TOC (test above), so a direct-mode toc insert must produce
// exactly that — NOT an empty paragraph. This guards a past regression where a
// `mode:"direct"` toc insert returned applied:true and serialized an empty
// paragraph with no field machinery — a silent fallback, forbidden by
// CLAUDE.md's prime directive.

#[test]
fn toc_insert_direct_mode_equals_accepted_tracked_insert() {
    let (doc, ids) = parse();
    let edited = insert_toc_mode(&doc, &ids[0], r#"{"type":"toc"}"#, Some("direct"))
        .expect("direct-mode toc insert");

    // The commit must also force updateFields, same as the tracked path.
    assert_eq!(
        edited.snapshot().update_fields_on_open().unwrap(),
        Some(true),
        "direct toc insert must set updateFields on open in the same commit"
    );

    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize direct");
    let xml = document_xml(&bytes);
    assert!(
        xml.contains("w:fldSimple"),
        "direct-mode toc insert must serialize the accepted fldSimple form, \
         not silently drop the field: {xml}"
    );
    let expected_instr_attr = xml_attr_escaped(r#"TOC \o "1-3" \h \z \u"#);
    assert!(
        xml.contains(&expected_instr_attr),
        "expected instr attr {expected_instr_attr:?} in: {xml}"
    );
    assert!(
        !xml.contains("<w:ins "),
        "direct mode must not leave a w:ins wrapper: {xml}"
    );
    assert_validator_clean(&bytes, "toc insert direct mode");
}

// ─── 5. Roundtrip: save, reopen, the field is still there ───────────────────

#[test]
fn toc_insert_survives_save_reopen_roundtrip() {
    let (doc, ids) = parse();
    let edited = insert_toc(&doc, &ids[0], r#"{"type":"toc"}"#).expect("insert toc");
    // Accept first: the roundtrip pins the STEADY-STATE field (the form the
    // document carries once the tracked insert is resolved), not the
    // pending tracked-complex-field lowering (covered above).
    let accepted = edited.project(Resolution::AcceptAll).expect("accept-all");
    let bytes = accepted
        .serialize(&ExportOptions::default())
        .expect("serialize");

    let reopened = Document::parse(&bytes).expect("reopen saved docx");
    assert_eq!(
        reopened.snapshot().update_fields_on_open().unwrap(),
        Some(true),
        "updateFields survives reopen"
    );

    let reserialized = reopened
        .serialize(&ExportOptions::default())
        .expect("reserialize reopened doc");
    let xml = document_xml(&reserialized);
    assert!(
        xml.contains("fldSimple") && xml.contains("TOC"),
        "toc field survives a reopen + reserialize roundtrip: {xml}"
    );
    assert_validator_clean(&reserialized, "toc insert roundtrip");
}
