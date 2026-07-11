//! Sub-stream A contract: the **unified staleness guard**.
//!
//! The block-level staleness guard: the block semantic hash (`guard`) is the
//! SINGLE staleness mechanism — an edit's precondition is a hash of the target
//! block, so a stale edit against changed content is refused. The previous
//! literal-substring `expect` is downgraded to advisory.
//!
//! These tests encode the DESIRED behavior (CLAUDE.md "tests encode desired
//! behavior, not current behavior"):
//!   - a matching guard applies even when `expect` is absent or changed;
//!   - a mismatching guard fails loud (`StaleEdit` / `BlockSemanticHashMismatch`);
//!   - back-compat: an op with no span + no guard behaves exactly like the
//!     pre-Phase-3 expect-gated `ReplaceParagraphText`;
//!   - `guard` and `semantic_hash` are aliases — supplying both with different
//!     values is rejected (`ConflictingGuard`), never silently reconciled.
//!
//! Daily, corpus-free: every fixture is a synthesized in-memory DOCX.

use stemma::api::Document;
use stemma::edit_v4::{SchemaError, parse_transaction};

// ─── Fixtures ──────────────────────────────────────────────────────────────

fn make_test_docx(paragraphs: &[&str]) -> Vec<u8> {
    let mut body = String::new();
    for para in paragraphs {
        body.push_str(&format!(r#"<w:p><w:r><w:t>{para}</w:t></w:r></w:p>"#));
    }
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
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

/// First block's id and live guard hash.
fn first_block_id_and_guard(doc: &Document) -> (String, String) {
    let view = doc.read();
    let b = &view.blocks[0];
    (b.id.to_string(), b.guard.clone())
}

/// Apply a v4 JSON transaction through the full wire path.
fn apply_v4(doc: &Document, json: &str) -> Result<Document, stemma::RuntimeError> {
    let txn = parse_transaction(json)
        .expect("schema valid")
        .into_edit_transaction()
        .expect("translate");
    doc.apply(&txn)
}

/// A `replace(paragraph)` op JSON with the given optional `guard`/`expect`.
fn replace_op_json(
    target: &str,
    new_text: &str,
    guard: Option<&str>,
    expect: Option<&str>,
) -> String {
    let guard_field = guard
        .map(|g| format!(r#", "guard": "{g}""#))
        .unwrap_or_default();
    let expect_field = expect
        .map(|e| format!(r#", "expect": "{e}""#))
        .unwrap_or_default();
    format!(
        r#"{{
          "ops": [
            {{ "op": "replace", "target": "{target}",
               "content": {{ "type": "paragraph", "role": null,
                             "content": [{{ "type": "text", "text": "{new_text}" }}] }}
               {guard_field}{expect_field} }}
          ],
          "revision": {{ "author": "Counsel" }}
        }}"#
    )
}

// ─── Guard is authoritative ──────────────────────────────────────────────────

#[test]
fn matching_guard_applies_even_with_absent_expect() {
    // Domain rule: when the block guard matches, the block is fresh; the op
    // applies. `expect` is advisory — its ABSENCE must not block the edit.
    let doc = Document::parse(&make_test_docx(&["The term is 30 days."])).expect("parse");
    let (id, guard) = first_block_id_and_guard(&doc);

    let json = replace_op_json(&id, "The term is 60 days.", Some(&guard), None);
    let edited = apply_v4(&doc, &json).expect("matching guard, no expect: applies");

    assert!(
        edited.read().blocks[0].text.contains("60 days"),
        "the replacement must have applied under a matching guard with no expect"
    );
}

#[test]
fn matching_guard_applies_even_with_changed_expect() {
    // Domain rule: a matching guard is authoritative; a STALE/wrong `expect`
    // substring is advisory and must not fail the step.
    let doc = Document::parse(&make_test_docx(&["The term is 30 days."])).expect("parse");
    let (id, guard) = first_block_id_and_guard(&doc);

    // `expect` names text that is NOT in the block — under the old dual-gate
    // this would fail; under the unified-guard contract the guard wins.
    let json = replace_op_json(
        &id,
        "The term is 60 days.",
        Some(&guard),
        Some("a phrase that does not appear"),
    );
    let edited = apply_v4(&doc, &json).expect("matching guard wins over a stale expect");

    assert!(edited.read().blocks[0].text.contains("60 days"));
}

#[test]
fn mismatching_guard_fails_loud() {
    // Domain rule: a guard that does not match the current block means the
    // caller addressed a stale snapshot — the op must fail loud, never apply.
    let doc = Document::parse(&make_test_docx(&["The term is 30 days."])).expect("parse");
    let (id, _guard) = first_block_id_and_guard(&doc);

    let json = replace_op_json(
        &id,
        "The term is 60 days.",
        Some("deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"),
        None,
    );
    let err = match apply_v4(&doc, &json) {
        Ok(_) => panic!("a mismatching guard must fail, not apply"),
        Err(e) => e,
    };
    assert_eq!(
        err.code,
        stemma::ErrorCode::StaleEdit,
        "a mismatching guard is a stale edit: {err:?}"
    );
}

// ─── Back-compat: no span + no guard == old expect-gated behavior ────────────

#[test]
fn no_guard_present_expect_is_the_gate_and_matches() {
    // Back-compat: pre-Phase-3 callers send only `expect`. With no guard, the
    // expect substring is the authoritative gate — a present substring applies.
    let doc = Document::parse(&make_test_docx(&["The term is 30 days."])).expect("parse");
    let (id, _guard) = first_block_id_and_guard(&doc);

    let json = replace_op_json(&id, "The term is 60 days.", None, Some("30 days"));
    let edited = apply_v4(&doc, &json).expect("present expect, no guard: applies");
    assert!(edited.read().blocks[0].text.contains("60 days"));
}

#[test]
fn no_guard_present_expect_miss_fails_byte_identically_to_pre_phase3() {
    // Back-compat: with NO guard, an absent `expect` substring fails exactly as
    // the pre-Phase-3 `ReplaceParagraphText` expect gate did (ExpectMismatch ->
    // StaleEdit). This pins that the fallback path is unchanged.
    let doc = Document::parse(&make_test_docx(&["The term is 30 days."])).expect("parse");
    let (id, _guard) = first_block_id_and_guard(&doc);

    let json = replace_op_json(&id, "The term is 60 days.", None, Some("not in the block"));
    let err = match apply_v4(&doc, &json) {
        Ok(_) => panic!("absent expect, no guard: must fail, not apply"),
        Err(e) => e,
    };
    assert_eq!(err.code, stemma::ErrorCode::StaleEdit);
    // The structured detail must be the expect-mismatch variant, not a hash one.
    let stale = err
        .details
        .stale_edit
        .as_deref()
        .expect("stale_edit detail present");
    assert!(
        matches!(stale, stemma::StaleEditDetails::ExpectMismatch { .. }),
        "no-guard miss is an expect mismatch (legacy gate), got {stale:?}"
    );
}

// ─── Conflicting guard / semantic_hash aliases ───────────────────────────────

#[test]
fn conflicting_guard_and_semantic_hash_is_rejected() {
    // Domain rule: `guard` and `semantic_hash` are aliases for ONE token. If a
    // caller supplies both and they disagree, we reject — never silently pick
    // one (CLAUDE.md: no silent fallbacks).
    let doc = Document::parse(&make_test_docx(&["The term is 30 days."])).expect("parse");
    let (id, guard) = first_block_id_and_guard(&doc);

    let json = format!(
        r#"{{
          "ops": [
            {{ "op": "replace", "target": "{id}",
               "content": {{ "type": "paragraph", "role": null,
                             "content": [{{ "type": "text", "text": "x" }}] }},
               "guard": "{guard}",
               "semantic_hash": "0000000000000000000000000000000000000000000000000000000000000000" }}
          ],
          "revision": {{ "author": "Counsel" }}
        }}"#
    );
    let err =
        parse_transaction(&json).expect_err("conflicting guard/semantic_hash must be rejected");
    assert!(
        matches!(err, SchemaError::ConflictingGuard { .. }),
        "expected ConflictingGuard, got {err:?}"
    );
}

#[test]
fn equal_guard_and_semantic_hash_is_accepted() {
    // The pair is allowed when byte-equal: they name the same token. The op
    // applies as if a single guard were supplied.
    let doc = Document::parse(&make_test_docx(&["The term is 30 days."])).expect("parse");
    let (id, guard) = first_block_id_and_guard(&doc);

    let json = format!(
        r#"{{
          "ops": [
            {{ "op": "replace", "target": "{id}",
               "content": {{ "type": "paragraph", "role": null,
                             "content": [{{ "type": "text", "text": "The term is 60 days." }}] }},
               "guard": "{guard}", "semantic_hash": "{guard}" }}
          ],
          "revision": {{ "author": "Counsel" }}
        }}"#
    );
    let edited = apply_v4(&doc, &json).expect("equal guard+semantic_hash applies");
    assert!(edited.read().blocks[0].text.contains("60 days"));
}

// ─── Direct-engine assertion: guarded miss does not surface ExpectMismatch ───

#[test]
fn guarded_apply_never_returns_expect_mismatch() {
    // White-box: at the engine level, a matching guard with a wrong expect must
    // NOT produce EditError::ExpectMismatch (it is advisory). This guards the
    // exact failure mode the contract change removes.
    let doc = Document::parse(&make_test_docx(&["Alpha beta gamma."])).expect("parse");
    let (id, guard) = first_block_id_and_guard(&doc);

    let json = replace_op_json(&id, "Alpha delta gamma.", Some(&guard), Some("zzz"));
    let txn = parse_transaction(&json)
        .expect("schema")
        .into_edit_transaction()
        .expect("translate");
    // `check` runs the same validation as `apply` without mutating; it must NOT
    // report a stale-edit (`ExpectMismatch` maps to `ErrorCode::StaleEdit`).
    match doc.check(&txn) {
        Ok(()) => {}
        Err(err) if err.code == stemma::ErrorCode::StaleEdit => {
            panic!("a matching guard must downgrade expect to advisory, not fail stale: {err:?}")
        }
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}
