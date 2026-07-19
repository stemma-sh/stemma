//! Word-reject structural invariant for `ReplaceSpanText` / `replace`:
//! **the tracked envelope (`w:ins` / `w:del`) wraps ONLY the true delta.**
//!
//! Word's reject-all restores `w:del` text and drops `w:ins` text. So any
//! unchanged run or opaque anchor that leaks INTO a `w:ins`/`w:del` makes Word's
//! reject diverge from the original — even when stemma's own canonical reject
//! (which works on the typed IR, not the markup) still reproduces the baseline.
//! That circularity is exactly what the Word oracle caught for
//! `anchor_after_insert` and `between_anchors_delete`.
//!
//! These daily, Word-free tests assert the structural invariant directly on the
//! serialized `document.xml`: every unchanged inline outside the edited span is
//! emitted outside any tracked envelope. They are the standing guard the gold
//! oracle confirmed.
//!
//! Daily, corpus-free.

use stemma::api::Document;
use stemma::edit::{
    ContentFragment, EditStep, EditTransaction, MaterializationMode, ParagraphContent,
    ResolvedSpanEndpoint, ResolvedSpanSelector,
};
use stemma::{BlockNode, ExportOptions, InlineNode, NodeId, Resolution, RevisionInfo};

// ─── Fixtures ──────────────────────────────────────────────────────────────

fn make_docx_with_body(body_inner: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
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

fn extract_document_xml(docx: &[u8]) -> String {
    use std::io::Read;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(docx)).expect("open zip");
    let mut file = zip
        .by_name("word/document.xml")
        .expect("document.xml present");
    let mut s = String::new();
    file.read_to_string(&mut s).expect("read document.xml");
    s
}

fn block_id_guard_anchors(docx: &[u8]) -> (String, String, Vec<String>) {
    let doc = Document::parse(docx).expect("parse");
    let view = doc.read();
    let id = view.blocks[0].id.to_string();
    let guard = view.blocks[0].guard.clone();
    let mut anchors = Vec::new();
    for tb in &doc.snapshot().canonical.blocks {
        if let BlockNode::Paragraph(p) = &tb.block
            && p.id.to_string() == id
        {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::OpaqueInline(o) = inline {
                        anchors.push(o.id.to_string());
                    }
                }
            }
        }
    }
    (id, guard, anchors)
}

fn txn(steps: Vec<EditStep>) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("Conformance".to_string()),
            date: Some("2026-06-01T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

fn norm_text(bytes: &[u8]) -> String {
    let doc = Document::parse(bytes).expect("parse for text");
    doc.read()
        .blocks
        .iter()
        .map(|b| b.text.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// A minimal event walk that returns, for each `<w:fldSimple>` and each `<w:t>`
/// text payload, whether it appears INSIDE a `<w:ins>` or `<w:del>` envelope.
/// We track depth of open `w:ins` / `w:del` elements while streaming events.
fn collect_envelope_membership(xml: &str) -> EnvelopeReport {
    use quick_xml::Reader;
    use quick_xml::events::Event;
    let mut reader = Reader::from_str(xml);
    let mut envelope_depth: i32 = 0;
    let mut in_t = false;
    let mut t_in_envelope = false;
    let mut report = EnvelopeReport::default();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = e.name();
                let local = local_name(name.as_ref());
                match local {
                    b"ins" | b"del" => envelope_depth += 1,
                    b"fldSimple" => {
                        report.fields.push(envelope_depth > 0);
                    }
                    b"t" | b"delText" => {
                        in_t = true;
                        t_in_envelope = envelope_depth > 0;
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(e)) => {
                let name = e.name();
                if local_name(name.as_ref()) == b"fldSimple" {
                    report.fields.push(envelope_depth > 0);
                }
            }
            Ok(Event::Text(t)) => {
                if in_t {
                    let txt = t.unescape().unwrap_or_default().into_owned();
                    report.texts.push((txt, t_in_envelope));
                }
            }
            Ok(Event::End(e)) => {
                let name = e.name();
                match local_name(name.as_ref()) {
                    b"ins" | b"del" => envelope_depth -= 1,
                    b"t" | b"delText" => in_t = false,
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => panic!("xml parse error: {e}"),
            _ => {}
        }
    }
    report
}

fn local_name(qname: &[u8]) -> &[u8] {
    match qname.iter().position(|&b| b == b':') {
        Some(i) => &qname[i + 1..],
        None => qname,
    }
}

#[derive(Default, Debug)]
struct EnvelopeReport {
    /// One entry per `<w:fldSimple>`: true if it is inside an ins/del envelope.
    fields: Vec<bool>,
    /// One entry per `<w:t>`/`<w:delText>` payload: (text, inside_envelope).
    texts: Vec<(String, bool)>,
}

impl EnvelopeReport {
    /// Assert a literal text fragment is present and emitted OUTSIDE any
    /// tracked envelope (plain `<w:t>`, so Word leaves it untouched on reject).
    fn assert_unchanged_outside_envelope(&self, fragment: &str) {
        let found: Vec<&(String, bool)> = self
            .texts
            .iter()
            .filter(|(t, _)| t.contains(fragment))
            .collect();
        assert!(
            !found.is_empty(),
            "expected unchanged fragment {fragment:?} in serialized text; texts={:?}",
            self.texts
        );
        for (t, in_env) in found {
            assert!(
                !in_env,
                "unchanged fragment {fragment:?} (run {t:?}) leaked INTO a w:ins/w:del envelope; \
                 Word reject would not restore it verbatim"
            );
        }
    }
}

// ─── BUG 1: anchor-after insert — pure insertion, nothing wrapped but the delta ──

#[test]
fn anchor_after_insert_wraps_only_the_inserted_text() {
    // "See [REF] for details." — insert " (as amended)" right after the field.
    // The field and ALL surrounding text are unchanged: the only tracked
    // envelope must be a single w:ins for the new words. No w:del at all (this
    // is a pure insertion), and no unchanged run inside any envelope.
    let base = make_docx_with_body(
        r#"<w:p><w:r><w:t xml:space="preserve">See </w:t></w:r><w:fldSimple w:instr=" REF A \h "><w:r><w:t>Section 2</w:t></w:r></w:fldSimple><w:r><w:t xml:space="preserve"> for details.</w:t></w:r></w:p>"#,
    );
    let (id, guard, anchors) = block_id_guard_anchors(&base);
    let field_id = anchors.first().expect("field anchor").clone();
    let doc = Document::parse(&base).unwrap();
    let edited = doc
        .apply(&txn(vec![EditStep::ReplaceSpanText {
            block_id: NodeId::from(id.as_str()),
            guard,
            expect: None,
            span: ResolvedSpanSelector::AnchorAfter(NodeId::from(field_id)),
            content: ParagraphContent {
                fragments: vec![ContentFragment::Text(" (as amended)".to_string())],
            },
            rationale: None,
        }]))
        .unwrap();
    let redline = edited.serialize(&ExportOptions::default()).unwrap();
    let xml = extract_document_xml(&redline);
    let report = collect_envelope_membership(&xml);

    // Structural invariant (Word reject): the field stays outside any envelope.
    assert_eq!(report.fields.len(), 1, "exactly one field present");
    assert!(
        !report.fields[0],
        "the unchanged field anchor must stay OUTSIDE any w:ins/w:del envelope"
    );
    // The unchanged head and tail text stay outside any envelope.
    report.assert_unchanged_outside_envelope("See ");
    report.assert_unchanged_outside_envelope("for details.");
    // There is NO deletion — a pure insertion deletes nothing.
    assert!(
        !xml.contains("<w:del ") && !xml.contains(":delText"),
        "a pure insert must not emit any w:del; xml={xml}"
    );

    // And canonical reject==base / accept==target still hold.
    let rej = edited
        .project(Resolution::RejectAll)
        .unwrap()
        .serialize(&ExportOptions::default())
        .unwrap();
    let acc = edited
        .project(Resolution::AcceptAll)
        .unwrap()
        .serialize(&ExportOptions::default())
        .unwrap();
    assert_eq!(norm_text(&rej), norm_text(&base), "reject-all == baseline");
    // norm_text reads only paragraph text (the field display text is opaque and
    // not surfaced), so the accepted visible text is the head + insert + tail.
    assert_eq!(
        norm_text(&acc),
        "See (as amended) for details.",
        "accept-all == target"
    );
}

// ─── BUG 1: between-anchors delete — only the middle run is deleted ──────────

#[test]
fn between_anchors_delete_wraps_only_the_deleted_run() {
    // "A [REF A] middle text [REF B] end." — delete the run between the two
    // fields. Both fields and the head/tail text are unchanged: only the
    // " middle text " run may sit inside a w:del.
    let base = make_docx_with_body(
        r#"<w:p><w:r><w:t xml:space="preserve">A </w:t></w:r><w:fldSimple w:instr=" REF A \h "><w:r><w:t>One</w:t></w:r></w:fldSimple><w:r><w:t xml:space="preserve"> middle text </w:t></w:r><w:fldSimple w:instr=" REF B \h "><w:r><w:t>Two</w:t></w:r></w:fldSimple><w:r><w:t xml:space="preserve"> end.</w:t></w:r></w:p>"#,
    );
    let (id, guard, anchors) = block_id_guard_anchors(&base);
    assert!(anchors.len() >= 2, "two field anchors present");
    let doc = Document::parse(&base).unwrap();
    let edited = doc
        .apply(&txn(vec![EditStep::ReplaceSpanText {
            block_id: NodeId::from(id.as_str()),
            guard,
            expect: None,
            span: ResolvedSpanSelector::Between {
                start: ResolvedSpanEndpoint::Anchor(NodeId::from(anchors[0].clone())),
                end: ResolvedSpanEndpoint::Anchor(NodeId::from(anchors[1].clone())),
            },
            content: ParagraphContent { fragments: vec![] },
            rationale: None,
        }]))
        .unwrap();
    let redline = edited.serialize(&ExportOptions::default()).unwrap();
    let xml = extract_document_xml(&redline);
    let report = collect_envelope_membership(&xml);

    // Both fields stay outside any envelope (Word leaves them on reject).
    assert_eq!(report.fields.len(), 2, "exactly two fields present");
    assert!(
        report.fields.iter().all(|in_env| !*in_env),
        "both unchanged field anchors must stay OUTSIDE any w:ins/w:del envelope"
    );
    // Head and tail text outside any envelope.
    report.assert_unchanged_outside_envelope("A ");
    report.assert_unchanged_outside_envelope(" end.");
    // There is NO insertion — a pure deletion inserts nothing.
    assert!(
        !xml.contains("<w:ins "),
        "a pure delete must not emit any w:ins; xml={xml}"
    );
    // The deleted run is the middle text, and it IS inside a w:del.
    let mid_in_env = report
        .texts
        .iter()
        .find(|(t, _)| t.contains("middle text"))
        .expect("middle text present in redline");
    assert!(
        mid_in_env.1,
        "the deleted middle run must sit inside a w:del envelope"
    );

    let rej = edited
        .project(Resolution::RejectAll)
        .unwrap()
        .serialize(&ExportOptions::default())
        .unwrap();
    let acc = edited
        .project(Resolution::AcceptAll)
        .unwrap()
        .serialize(&ExportOptions::default())
        .unwrap();
    assert_eq!(norm_text(&rej), norm_text(&base), "reject-all == baseline");
    // norm_text surfaces only paragraph text (field display text is opaque), so
    // the accepted visible text is the head + tail with the middle run removed.
    assert_eq!(norm_text(&acc), "A end.", "accept-all == target");
}

// ─── BUG 2: typographic glyph preserved verbatim in the inserted run ─────────

#[test]
fn replace_before_curly_apostrophe_keeps_u2019_in_both_envelopes() {
    // "the Investor’s duty" — replace "Investor" with "Purchaser". The LLM emits
    // the replacement with the SAME curly apostrophe the doc uses. The diff runs
    // over an ASCII-folded copy (so curly-vs-ASCII variants match), but the
    // inserted run must carry the LITERAL U+2019 forward, not the ASCII fold.
    // The deleted (original) run carries U+2019 because kept/deleted chars are
    // pulled from the original TextNode.
    let body = "the Investor\u{2019}s duty";
    let base = make_docx_with_body(&format!(
        r#"<w:p><w:r><w:t xml:space="preserve">{body}</w:t></w:r></w:p>"#
    ));
    let (id, _guard, _) = block_id_guard_anchors(&base);
    let doc = Document::parse(&base).unwrap();
    let edited = doc
        .apply(&txn(vec![EditStep::ReplaceParagraphText {
            block_id: NodeId::from(id.as_str()),
            rationale: None,
            replacement_role: None,
            expect: "Investor".to_string(),
            semantic_hash: None,
            content: ParagraphContent {
                fragments: vec![ContentFragment::Text(
                    "the Purchaser\u{2019}s duty".to_string(),
                )],
            },
        }]))
        .unwrap();
    let redline = edited.serialize(&ExportOptions::default()).unwrap();
    let xml = extract_document_xml(&redline);

    // Pull the deleted and inserted run text out of the envelopes. The shared
    // "'s" / "duty" tail factors out as Equal, so the word-level delta is the
    // whole word "Investor's" -> "Purchaser's", each carrying the apostrophe.
    let report = collect_envelope_membership(&xml);
    let del_run = report
        .texts
        .iter()
        .find(|(t, in_env)| *in_env && t.contains("Investor"))
        .expect("deleted Investor run present");
    let ins_run = report
        .texts
        .iter()
        .find(|(t, in_env)| *in_env && t.contains("Purchaser"))
        .expect("inserted Purchaser run present");

    assert!(
        del_run.0.contains('\u{2019}'),
        "deleted run must keep the literal U+2019: {:?}",
        del_run.0
    );
    assert!(
        ins_run.0.contains('\u{2019}'),
        "inserted run must keep the literal U+2019 (not downgrade to ASCII U+0027): {:?}",
        ins_run.0
    );
    assert!(
        !ins_run.0.contains('\''),
        "inserted run must not contain an ASCII apostrophe U+0027: {:?}",
        ins_run.0
    );
    // Whole-document check: no ASCII apostrophe leaked anywhere.
    assert!(
        !xml.contains('\''),
        "no ASCII apostrophe U+0027 must appear anywhere in the redline; xml={xml}"
    );
}
