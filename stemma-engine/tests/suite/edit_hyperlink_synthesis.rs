//! End-to-end synthesis of NEW inline content through the v4 JSON transaction
//! adapter — the genuine product path where a `replace`/`insert` op's
//! replacement paragraph content carries inline `marks` or a nested
//! `Inline::Hyperlink`.
//!
//! These exercise a previously-dark production path: no existing test sends a
//! paragraph replace whose inline content carries `marks` or a fresh hyperlink.
//! Such content lowers through `v4_inlines_to_paragraph_content`
//! (`ContentFragment::StyledText` / `ContentFragment::NewHyperlink`) and, on
//! apply, through `synthesize_new_hyperlink_inline` + the NewHyperlink
//! inline-diff reconstruction (`src/edit/mod.rs`).
//!
//! Everything is bytes-in: the BASE document is a synthetic `.docx` imported
//! through the public `Document::parse`. Edits are authored as the public v4
//! JSON shape and adapted via `edit_v4::parse_transaction(..).into_edit_transaction()`,
//! then applied via `Document::apply`.
//!
//! DOMAIN RULES pinned here (tracked-change reversibility + opaque preservation):
//!   - inserted marked text survives accept-all and carries its mark;
//!   - a synthesized hyperlink is exactly one inserted opaque of kind Hyperlink
//!     with url==href and display text==the link body;
//!   - the surrounding UNCHANGED text is not re-inserted (minimal tracked change);
//!   - reject-all restores the exact original text and removes the new content;
//!   - the redline DOCX passes the blocking validator (opens clean);
//!   - a pre-existing opaque inline survives the edit (opaque-preservation).
//!
//! Daily, corpus-free.

use std::io::{Cursor, Read, Write};

use stemma::api::Document;
use stemma::edit_v4::parse_transaction;
use stemma::{BlockNode, ExportMode, ExportOptions, InlineNode, OpaqueKind, ValidatorLevel};
use zip::ZipWriter;
use zip::write::FileOptions;

// ─── DOCX fixtures (bytes-in) ────────────────────────────────────────────────

const CONTENT_TYPES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;

const PACKAGE_RELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;

/// Build a `.docx` from a body fragment and an optional document-rels fragment
/// (the inner `<Relationship>` entries; the namespace wrapper is added here).
fn make_docx(body_inner: &str, extra_doc_rels: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
    );
    let doc_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{extra_doc_rels}</Relationships>"#
    );
    let mut buf = Vec::new();
    {
        let mut zip = ZipWriter::new(Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        zip.start_file("[Content_Types].xml", opts).unwrap();
        zip.write_all(CONTENT_TYPES_XML.as_bytes()).unwrap();
        zip.start_file("_rels/.rels", opts).unwrap();
        zip.write_all(PACKAGE_RELS_XML.as_bytes()).unwrap();
        zip.start_file("word/_rels/document.xml.rels", opts)
            .unwrap();
        zip.write_all(doc_rels.as_bytes()).unwrap();
        zip.start_file("word/document.xml", opts).unwrap();
        zip.write_all(document_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

/// A single plain-text paragraph.
fn plain_docx(text: &str) -> Vec<u8> {
    make_docx(
        &format!(r#"<w:p><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p>"#),
        "",
    )
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// The id and guard of the first read-view block — the addressing + staleness
/// pin for an op authored against what we just read.
fn first_id_and_guard(doc: &Document) -> (String, String) {
    let view = doc.read();
    let b = view.blocks.first().expect("at least one block");
    (b.id.to_string(), b.guard.clone())
}

/// Apply a v4 transaction JSON through the real adapter + apply path.
fn apply_v4(doc: &Document, json: &str) -> Document {
    let txn = parse_transaction(json)
        .expect("v4 schema check")
        .into_edit_transaction()
        .expect("v4 -> EditTransaction adapt");
    doc.apply(&txn).expect("apply v4 transaction")
}

/// Serialize a document to redline bytes and return its `word/document.xml`.
fn document_xml_of(doc: &Document) -> String {
    let bytes = doc
        .serialize(&ExportOptions {
            mode: ExportMode::Redline,
            validator_level: ValidatorLevel::Off,
            validator: None,
        })
        .expect("serialize");
    read_document_xml(&bytes)
}

fn read_document_xml(docx_bytes: &[u8]) -> String {
    let mut zip = zip::ZipArchive::new(Cursor::new(docx_bytes)).expect("zip");
    let mut xml = String::new();
    zip.by_name("word/document.xml")
        .expect("document.xml present")
        .read_to_string(&mut xml)
        .expect("read document.xml");
    xml
}

/// Assert the redline export passes the BLOCKING validator (Word would open it
/// clean). `serialize` returns `Err` when a blocking rule fires, so a successful
/// serialize IS the assertion.
fn assert_opens_clean(doc: &Document) {
    doc.serialize(&ExportOptions {
        mode: ExportMode::Redline,
        validator_level: ValidatorLevel::Blocking,
        validator: None,
    })
    .expect("redline export must pass the blocking validator (open-clean)");
}

/// All opaque inlines of the first paragraph in document order, as
/// `(id, kind-clone)` pairs.
fn first_paragraph_opaques(doc: &Document) -> Vec<(String, OpaqueKind)> {
    for tb in &doc.snapshot().canonical.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            let mut out = Vec::new();
            for inline in p.all_inlines_owned() {
                if let InlineNode::OpaqueInline(o) = inline {
                    out.push((o.id.to_string(), o.kind.clone()));
                }
            }
            return out;
        }
    }
    panic!("no paragraph in document");
}

// ─── Test 1: inline marks through a paragraph replace ────────────────────────

#[test]
fn replace_with_marked_text_inserts_a_bold_run_that_survives_accept_and_reverts_on_reject() {
    // Domain rule: a v4 replace whose content is `[{text, marks:[bold]}]` is a
    // tracked change that is reversible BOTH ways — accept-all yields the new
    // text on an inserted run carrying `<w:b/>` (verified by serialize ->
    // re-import), and reject-all restores the exact original runs.
    let doc = Document::parse(&plain_docx("plain words")).expect("parse");
    let (id, guard) = first_id_and_guard(&doc);

    let json = format!(
        r#"{{
          "ops": [{{
            "op": "replace",
            "target": "{id}",
            "guard": "{guard}",
            "content": {{
              "type": "paragraph",
              "content": [{{ "type": "text", "text": "bold words", "marks": [{{ "type": "bold" }}] }}]
            }}
          }}],
          "revision": {{ "author": "synthesis-test" }}
        }}"#
    );
    let edited = apply_v4(&doc, &json);

    // The edit IS validator-clean as a redline.
    assert_opens_clean(&edited);

    // Accept-all: the new text is present, the old gone.
    let accepted = edited.read_accepted().expect("accept-all");
    let acc_text = accepted.to_text();
    assert!(
        acc_text.contains("bold words"),
        "accept-all keeps new text: {acc_text:?}"
    );
    assert!(
        !acc_text.contains("plain"),
        "accept-all drops original text: {acc_text:?}"
    );

    // The accept-all run carries the bold mark. Re-import the accepted bytes and
    // inspect the run formatting on the inserted text (an INDEPENDENT oracle:
    // serializer + re-parser, not the in-memory IR).
    let acc_bytes = accepted
        .serialize(&ExportOptions {
            mode: ExportMode::Redline,
            validator_level: ValidatorLevel::Off,
            validator: None,
        })
        .expect("serialize accepted");
    let reimported = Document::parse(&acc_bytes).expect("re-import accepted");
    let mut found_bold = false;
    for tb in &reimported.snapshot().canonical.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for inline in p.all_inlines_owned() {
                if let InlineNode::Text(t) = inline
                    && t.text.contains("bold")
                {
                    found_bold = t.marks.contains(&stemma::Mark::Bold);
                }
            }
        }
    }
    assert!(
        found_bold,
        "the inserted 'bold words' run must carry the Bold mark after accept + roundtrip"
    );

    // The redline (pre-accept) export carries the inserted text inside <w:ins>
    // and the bold property on that inserted run.
    let redline_xml = document_xml_of(&edited);
    let ins_pos = redline_xml
        .find("<w:ins")
        .expect("redline has a <w:ins> envelope");
    let ins_tail = &redline_xml[ins_pos..];
    assert!(
        ins_tail.contains("bold words"),
        "the new text must be inside <w:ins>: {ins_tail:.400}"
    );

    // Reject-all: the original text is restored exactly, the new text gone.
    let rejected = edited.read_rejected().expect("reject-all");
    let rej_text = rejected.to_text();
    assert!(
        rej_text.contains("plain words"),
        "reject-all restores original: {rej_text:?}"
    );
    assert!(
        !rej_text.contains("bold"),
        "reject-all drops the inserted text: {rej_text:?}"
    );
}

// ─── Test 2: new hyperlink mid-paragraph ─────────────────────────────────────

#[test]
fn replace_with_new_hyperlink_inserts_one_opaque_link_minimally_and_reverts() {
    // Domain rules:
    //   (a) accept-all => exactly one inserted OpaqueInline of kind Hyperlink
    //       with url==href and display text==the link body;
    //   (b) the surrounding UNCHANGED text is NOT re-inserted (minimal tracked
    //       change): the unchanged words are not inside <w:ins>;
    //   (c) reject-all => hyperlink gone, original text intact;
    //   (d) the redline DOCX passes the blocking validator.
    let doc = Document::parse(&plain_docx("see here for more")).expect("parse");
    let (id, guard) = first_id_and_guard(&doc);

    // Replace the paragraph with: "see " + <link>the policy</link> + " for more".
    // Only the middle word "here" is replaced by a link; the surrounding text is
    // unchanged.
    let href = "https://example.com/policy";
    let json = format!(
        r#"{{
          "ops": [{{
            "op": "replace",
            "target": "{id}",
            "guard": "{guard}",
            "content": {{
              "type": "paragraph",
              "content": [
                {{ "type": "text", "text": "see " }},
                {{ "type": "hyperlink", "attrs": {{ "href": "{href}" }},
                   "content": [{{ "type": "text", "text": "the policy" }}] }},
                {{ "type": "text", "text": " for more" }}
              ]
            }}
          }}],
          "revision": {{ "author": "synthesis-test" }}
        }}"#
    );
    let edited = apply_v4(&doc, &json);

    // (d) open-clean.
    assert_opens_clean(&edited);

    // (a) accept-all => exactly one Hyperlink opaque, url==href, text==body.
    let accepted = edited.read_accepted().expect("accept-all");
    let opaques = first_paragraph_opaques(&accepted);
    let links: Vec<_> = opaques
        .iter()
        .filter_map(|(_, k)| match k {
            OpaqueKind::Hyperlink(d) => Some(d),
            _ => None,
        })
        .collect();
    assert_eq!(
        links.len(),
        1,
        "accept-all yields exactly one synthesized hyperlink"
    );
    assert_eq!(
        links[0].url.as_deref(),
        Some(href),
        "the synthesized hyperlink carries the authored href"
    );
    assert_eq!(
        links[0].text, "the policy",
        "the synthesized hyperlink's display text is the link body"
    );

    // accept-all text shows the surrounding words; the hyperlink body itself is
    // an opaque inline, rendered by `to_text()` as the object-replacement char
    // (U+FFFC), not its display text. The link's display text is asserted above
    // via `HyperlinkData.text`. The surrounding text and replaced-word checks
    // are the text-layer post-conditions.
    let acc_text = accepted.to_text();
    assert!(
        acc_text.contains("see"),
        "surrounding text present: {acc_text:?}"
    );
    assert!(
        acc_text.contains("for more"),
        "trailing text present: {acc_text:?}"
    );
    assert!(
        !acc_text.contains("here"),
        "the replaced word is gone: {acc_text:?}"
    );
    // The link body lives in the opaque, not the flat text stream.
    assert!(
        !acc_text.contains("the policy"),
        "hyperlink display text is opaque, not part of the flat text stream: {acc_text:?}"
    );

    // (b) minimal tracked change: the UNCHANGED words "see" / "for more" must not
    // sit inside a <w:ins> envelope in the redline. We check that each unchanged
    // word has at least one occurrence outside any <w:ins>...</w:ins> span.
    let redline_xml = document_xml_of(&edited);
    assert!(
        word_occurs_outside_ins(&redline_xml, "see"),
        "unchanged word 'see' must not be re-inserted (minimal change)"
    );
    assert!(
        word_occurs_outside_ins(&redline_xml, "for more"),
        "unchanged trailing text must not be re-inserted (minimal change)"
    );
    // The new hyperlink itself is an inserted change: a <w:hyperlink> appears.
    assert!(
        redline_xml.contains("<w:hyperlink"),
        "redline emits the new <w:hyperlink>"
    );

    // (c) reject-all => link gone, original text intact.
    let rejected = edited.read_rejected().expect("reject-all");
    let rej_opaques = first_paragraph_opaques(&rejected);
    assert!(
        !rej_opaques
            .iter()
            .any(|(_, k)| matches!(k, OpaqueKind::Hyperlink(_))),
        "reject-all removes the synthesized hyperlink"
    );
    let rej_text = rejected.to_text();
    assert!(
        rej_text.contains("see here for more"),
        "reject-all restores original: {rej_text:?}"
    );
    assert!(
        !rej_text.contains("the policy"),
        "reject-all drops the link body: {rej_text:?}"
    );
}

/// True if `word` appears in `xml` at a position that is NOT inside any
/// `<w:ins ...>...</w:ins>` span. Used to prove unchanged text was not
/// re-inserted by a minimal tracked change.
fn word_occurs_outside_ins(xml: &str, word: &str) -> bool {
    let mut search_from = 0;
    while let Some(rel) = xml[search_from..].find(word) {
        let pos = search_from + rel;
        if !position_inside_ins(xml, pos) {
            return true;
        }
        search_from = pos + word.len();
    }
    false
}

/// Whether byte offset `pos` falls within an open `<w:ins ...> ... </w:ins>`.
fn position_inside_ins(xml: &str, pos: usize) -> bool {
    let before = &xml[..pos];
    // Last <w:ins (an opening or self-closing tag start) vs last </w:ins> before pos.
    let last_open = before.rfind("<w:ins");
    let last_close = before.rfind("</w:ins>");
    match (last_open, last_close) {
        (None, _) => false,
        (Some(o), None) => {
            // An open <w:ins exists with no close before pos. Make sure it isn't a
            // self-closing <w:ins .../> (rare for insertion envelopes, but guard).
            !tag_is_self_closing(&xml[o..])
        }
        (Some(o), Some(c)) => o > c && !tag_is_self_closing(&xml[o..]),
    }
}

/// Whether the tag beginning at `s` (which starts with `<w:ins`) self-closes
/// before its first child.
fn tag_is_self_closing(s: &str) -> bool {
    if let Some(gt) = s.find('>') {
        s.as_bytes()[gt - 1] == b'/'
    } else {
        false
    }
}

// ─── Test 3: new hyperlink next to an existing opaque ────────────────────────

#[test]
fn new_hyperlink_lands_next_to_a_preexisting_opaque_which_survives() {
    // The base paragraph already contains an opaque inline (an existing
    // <w:hyperlink>), so the replacement spans >=2 sections and exercises the
    // NewHyperlink inline-diff reconstruction. Domain rules:
    //   - the pre-existing opaque SURVIVES (opaque-preservation): its id is still
    //     present after the edit;
    //   - the new link lands in correct reading order;
    //   - accept/reject identity holds (open-clean + reversible).
    let body = r#"<w:p><w:r><w:t xml:space="preserve">See </w:t></w:r><w:hyperlink r:id="rId100"><w:r><w:t xml:space="preserve">the policy</w:t></w:r></w:hyperlink><w:r><w:t xml:space="preserve"> now</w:t></w:r></w:p>"#;
    let existing_rel = r#"<Relationship Id="rId100" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.com/existing" TargetMode="External"/>"#;
    let doc = Document::parse(&make_docx(body, existing_rel)).expect("parse");
    let (id, guard) = first_id_and_guard(&doc);

    // The pre-existing hyperlink's opaque id and its OpaqueRef wire id (used to
    // reference it from the replacement payload — opaque set-equality invariant).
    let before_opaques = first_paragraph_opaques(&doc);
    let existing_link = before_opaques
        .iter()
        .find(|(_, k)| matches!(k, OpaqueKind::Hyperlink(d) if d.url.as_deref() == Some("https://example.com/existing")))
        .expect("base has the existing hyperlink");
    let existing_id = existing_link.0.clone();

    // Replace the paragraph: keep the existing link (by opaque_ref), then add a
    // brand-new link after it. Opaque set-equality requires the existing opaque
    // to be referenced in the replacement, so we re-include it via opaque_ref.
    let new_href = "https://example.com/new";
    let json = format!(
        r#"{{
          "ops": [{{
            "op": "replace",
            "target": "{id}",
            "guard": "{guard}",
            "content": {{
              "type": "paragraph",
              "content": [
                {{ "type": "text", "text": "See " }},
                {{ "type": "opaque_ref", "attrs": {{ "id": "{existing_id}" }} }},
                {{ "type": "text", "text": " and also " }},
                {{ "type": "hyperlink", "attrs": {{ "href": "{new_href}" }},
                   "content": [{{ "type": "text", "text": "the addendum" }}] }},
                {{ "type": "text", "text": " now" }}
              ]
            }}
          }}],
          "revision": {{ "author": "synthesis-test" }}
        }}"#
    );
    let edited = apply_v4(&doc, &json);

    // open-clean.
    assert_opens_clean(&edited);

    // Opaque-preservation: the pre-existing opaque id is still present after the
    // edit (in the live IR — the edit must not destroy it).
    let after_ids: Vec<String> = first_paragraph_opaques(&edited)
        .into_iter()
        .map(|(idx, _)| idx)
        .collect();
    assert!(
        after_ids.contains(&existing_id),
        "the pre-existing opaque (id {existing_id}) must survive the edit; got {after_ids:?}"
    );

    // Accept-all: both links present, the new one with the authored href, and the
    // existing one's url unchanged. Reading order: existing link before new link.
    let accepted = edited.read_accepted().expect("accept-all");
    let acc_opaques = first_paragraph_opaques(&accepted);
    let link_urls: Vec<Option<String>> = acc_opaques
        .iter()
        .filter_map(|(_, k)| match k {
            OpaqueKind::Hyperlink(d) => Some(d.url.clone()),
            _ => None,
        })
        .collect();
    assert!(
        link_urls.contains(&Some("https://example.com/existing".to_string())),
        "existing link url preserved through accept: {link_urls:?}"
    );
    assert!(
        link_urls.contains(&Some(new_href.to_string())),
        "new link url present through accept: {link_urls:?}"
    );
    // Reading order: existing url appears before the new url in document order.
    let pos_existing = link_urls
        .iter()
        .position(|u| u.as_deref() == Some("https://example.com/existing"))
        .expect("existing in order");
    let pos_new = link_urls
        .iter()
        .position(|u| u.as_deref() == Some(new_href))
        .expect("new in order");
    assert!(
        pos_existing < pos_new,
        "the new link must land AFTER the existing one in reading order: {link_urls:?}"
    );

    // Link bodies are opaque inlines (U+FFFC in `to_text()`); their display text
    // is asserted structurally below via `HyperlinkData.text`. Here we pin that
    // BOTH links' display text is carried by the opaques in reading order.
    let acc_link_texts: Vec<String> = acc_opaques
        .iter()
        .filter_map(|(_, k)| match k {
            OpaqueKind::Hyperlink(d) => Some(d.text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        acc_link_texts,
        vec!["the policy".to_string(), "the addendum".to_string()],
        "both link bodies present in reading order: {acc_link_texts:?}"
    );
    let acc_text = accepted.to_text();
    assert!(acc_text.contains("See"), "lead text present: {acc_text:?}");
    assert!(
        acc_text.contains("and also"),
        "interstitial text present: {acc_text:?}"
    );
    assert!(
        acc_text.contains("now"),
        "trailing text present: {acc_text:?}"
    );

    // Reject-all: original paragraph restored — exactly one hyperlink (the
    // pre-existing), the new one gone, original text intact.
    let rejected = edited.read_rejected().expect("reject-all");
    let rej_links: Vec<Option<String>> = first_paragraph_opaques(&rejected)
        .iter()
        .filter_map(|(_, k)| match k {
            OpaqueKind::Hyperlink(d) => Some(d.url.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        rej_links,
        vec![Some("https://example.com/existing".to_string())],
        "reject-all leaves only the pre-existing hyperlink"
    );
    // The surviving link's display text is the pre-existing body (opaque-carried).
    let rej_link_texts: Vec<String> = first_paragraph_opaques(&rejected)
        .iter()
        .filter_map(|(_, k)| match k {
            OpaqueKind::Hyperlink(d) => Some(d.text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        rej_link_texts,
        vec!["the policy".to_string()],
        "reject-all keeps only the pre-existing link body: {rej_link_texts:?}"
    );
    let rej_text = rejected.to_text();
    assert!(
        rej_text.contains("See"),
        "reject-all restores lead text: {rej_text:?}"
    );
    assert!(
        rej_text.contains("now"),
        "reject-all restores trailing text: {rej_text:?}"
    );
    assert!(
        !rej_text.contains("and also"),
        "reject-all drops the inserted interstitial text: {rej_text:?}"
    );
}

// ─── Bug pin: a synthesized hyperlink must serialize as a tracked insertion ───

// DOMAIN RULE (tracked-change reversibility at the serialized-bytes layer): a
// segment-Inserted hyperlink must serialize as
// `<w:hyperlink><w:ins><w:r>..</w:r></w:ins></w:hyperlink>` (ECMA-376 §17.13.5
// permits w:ins around the hyperlink's inner runs) so Word treats the new link
// as a tracked insertion and reject-all reverts to baseline. Fixed in
// serialize/mod.rs (emit_tracked_chunks DirectInline branch →
// append_tracked_hyperlink_paragraph_opaque).
#[test]
fn synthesized_hyperlink_serializes_with_insertion_tracking() {
    // DOMAIN RULE (tracked-change reversibility at the SERIALIZED-bytes layer):
    // the gold consumption oracle is Word, which reads the bytes — not stemma's
    // in-memory projection. A brand-new hyperlink produced by an LLM `replace`
    // must be a TRACKED insertion in the redline so that "reject all" in Word
    // removes it and the document reverts to baseline.
    //
    // In OOXML a `<w:hyperlink>` is paragraph-level (EG_PContent) and cannot be a
    // child of `<w:ins>` (whose content is EG_RunLevelElts). The spec-legal
    // encoding wraps the hyperlink's INNER run(s) in `<w:ins>`:
    //     <w:hyperlink ...><w:ins ...><w:r><w:t>the policy</w:t></w:r></w:ins></w:hyperlink>
    // which is exactly the shape `word_ir`'s importer already round-trips
    // (see the "runs inside <w:ins>/<w:del> within a hyperlink" test in word_ir).
    //
    // Today the serializer emits the hyperlink with no insertion tracking at all,
    // so this assertion fails — a real serialization bug, not a test bug.
    let doc = Document::parse(&plain_docx("see here for more")).expect("parse");
    let (id, guard) = first_id_and_guard(&doc);
    let json = format!(
        r#"{{
          "ops": [{{
            "op": "replace",
            "target": "{id}",
            "guard": "{guard}",
            "content": {{
              "type": "paragraph",
              "content": [
                {{ "type": "text", "text": "see " }},
                {{ "type": "hyperlink", "attrs": {{ "href": "https://example.com/policy" }},
                   "content": [{{ "type": "text", "text": "the policy" }}] }},
                {{ "type": "text", "text": " for more" }}
              ]
            }}
          }}],
          "revision": {{ "author": "synthesis-test" }}
        }}"#
    );
    let edited = apply_v4(&doc, &json);

    // Precondition (proves the IR side is correct, so the gap is in serialization):
    // the hyperlink opaque sits in a segment whose status is Inserted(_).
    let mut seg_status_is_inserted = false;
    for tb in &edited.snapshot().canonical.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for seg in &p.segments {
                if seg
                    .inlines
                    .iter()
                    .any(|i| matches!(i, InlineNode::OpaqueInline(o) if matches!(o.kind, OpaqueKind::Hyperlink(_))))
                {
                    seg_status_is_inserted = matches!(
                        seg.status,
                        stemma::TrackingStatus::Inserted(_)
                    );
                }
            }
        }
    }
    assert!(
        seg_status_is_inserted,
        "IR precondition: the synthesized hyperlink's segment status must be Inserted"
    );

    // The serialized redline must carry insertion tracking on the new link.
    let redline_xml = document_xml_of(&edited);
    let h_start = redline_xml
        .find("<w:hyperlink")
        .expect("redline emits the new <w:hyperlink>");
    let h_end = redline_xml[h_start..]
        .find("</w:hyperlink>")
        .map(|e| h_start + e)
        .expect("hyperlink element closes");
    let hyperlink_inner = &redline_xml[h_start..h_end];
    assert!(
        hyperlink_inner.contains("<w:ins"),
        "a synthesized (segment-Inserted) hyperlink must carry an in-hyperlink \
         <w:ins> envelope so Word treats the link as a tracked insertion and \
         reject-all reverts to baseline; got:\n{hyperlink_inner}"
    );
}

// ─── Regression: a synthesized link's URL must survive a byte round-trip ──────

#[test]
fn synthesized_hyperlink_url_survives_serialize_and_reimport() {
    // DOMAIN RULE: a new hyperlink is inserted with r_id: None and its external
    // relationship is allocated at export. The redline must therefore serialize
    // BOTH the <w:hyperlink r:id=..> element AND a backing relationship in
    // word/_rels/document.xml.rels — otherwise the link is dangling and its URL
    // is lost on the next save/reopen (a dead link). The in-memory accept view
    // can't catch this (the URL is already on the IR); only a round-trip through
    // bytes exercises the rels part.
    let doc = Document::parse(&plain_docx("see here for more")).expect("parse");
    let (id, guard) = first_id_and_guard(&doc);
    let json = format!(
        r#"{{ "ops": [{{ "op": "replace", "target": "{id}", "guard": "{guard}",
             "content": {{ "type": "paragraph", "content": [
               {{ "type": "text", "text": "see " }},
               {{ "type": "hyperlink", "attrs": {{ "href": "https://example.com/policy" }},
                  "content": [{{ "type": "text", "text": "the policy" }}] }},
               {{ "type": "text", "text": " for more" }} ] }} }}],
             "revision": {{ "author": "rels-test" }} }}"#
    );
    let edited = apply_v4(&doc, &json);
    let redline = edited
        .serialize(&ExportOptions {
            mode: ExportMode::Redline,
            validator_level: ValidatorLevel::Off,
            validator: None,
        })
        .expect("serialize redline");

    let reparsed = Document::parse(&redline).expect("reparse redline bytes");
    let accepted = reparsed.read_accepted().expect("accept");
    let urls: Vec<String> = first_paragraph_opaques(&accepted)
        .into_iter()
        .filter_map(|(_, k)| match k {
            OpaqueKind::Hyperlink(d) => d.url,
            _ => None,
        })
        .collect();
    assert!(
        urls.iter().any(|u| u == "https://example.com/policy"),
        "the inserted hyperlink URL must survive serialize -> reimport; got {urls:?}"
    );
}
