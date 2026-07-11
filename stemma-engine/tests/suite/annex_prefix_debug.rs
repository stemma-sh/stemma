use std::fs;

use crate::common::samples_dir;
use stemma::docx::DocxArchive;
use stemma::redline_extract::extract_redline;
use stemma::{
    BlockNode, CanonDoc, DocxRuntime, ExportMode, InlineNode, MarkValue, ParagraphNode,
    SimpleRuntime, TrackedBlock, TrackingStatus, TransactionMeta,
};

fn pair_meta() -> TransactionMeta {
    TransactionMeta {
        author: "annex_prefix_debug".to_string(),
        reason: Some("annex prefix debug".to_string()),
        timestamp_utc: Some("2026-04-10T00:00:00Z".to_string()),
    }
}

fn extract_inline_text(inlines: &[InlineNode]) -> String {
    let mut out = String::new();
    for inline in inlines {
        match inline {
            InlineNode::Text(t) => {
                if t.style_props.caps == MarkValue::On {
                    out.push_str(&t.text.to_uppercase());
                } else {
                    out.push_str(&t.text);
                }
            }
            InlineNode::HardBreak(_) => out.push('\n'),
            InlineNode::OpaqueInline(_) => out.push('\u{FFFC}'),
            InlineNode::Decoration(_)
            | InlineNode::CommentRangeStart { .. }
            | InlineNode::CommentRangeEnd { .. }
            | InlineNode::CommentReference { .. } => {}
        }
    }
    out
}

fn paragraph_visible_text(p: &ParagraphNode) -> String {
    if let Some(text) = &p.rendered_text {
        return text.clone();
    }
    let mut out = String::new();
    if let Some(prefix) = &p.literal_prefix {
        out.push_str(prefix);
        out.push('\t');
    }
    out.push_str(&extract_inline_text(&p.all_inlines_owned()));
    out
}

#[derive(Debug)]
struct ParagraphRecord {
    line_index: usize,
    path: String,
    id: String,
    style_id: Option<String>,
    numbering: Option<(u32, u32, String)>,
    materialized_numbering: Option<(u32, u32, String)>,
    literal_prefix: Option<String>,
    rendered_text: Option<String>,
    inline_text: String,
    visible_text: String,
    segment_statuses: Vec<String>,
}

fn collect_paragraph_records(doc: &CanonDoc) -> Vec<ParagraphRecord> {
    let mut records = Vec::new();
    collect_tracked_blocks(&doc.blocks, "body".to_string(), &mut records);
    records
}

fn collect_tracked_blocks(blocks: &[TrackedBlock], path: String, out: &mut Vec<ParagraphRecord>) {
    for (idx, tracked) in blocks.iter().enumerate() {
        if matches!(tracked.status, TrackingStatus::Deleted(_)) {
            continue;
        }
        let block_path = format!("{path}[{idx}]");
        match &tracked.block {
            BlockNode::Paragraph(p) => push_paragraph_record(p, block_path, out),
            BlockNode::Table(t) => {
                for (row_idx, row) in t.rows.iter().enumerate() {
                    if matches!(row.tracking_status, Some(TrackingStatus::Deleted(_))) {
                        continue;
                    }
                    for (cell_idx, cell) in row.cells.iter().enumerate() {
                        collect_bare_blocks(
                            &cell.blocks,
                            format!("{block_path}/tbl({})/r{row_idx}c{cell_idx}", t.id.0),
                            out,
                        );
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }
}

fn collect_bare_blocks(blocks: &[BlockNode], path: String, out: &mut Vec<ParagraphRecord>) {
    for (idx, block) in blocks.iter().enumerate() {
        let block_path = format!("{path}[{idx}]");
        match block {
            BlockNode::Paragraph(p) => push_paragraph_record(p, block_path, out),
            BlockNode::Table(t) => {
                for (row_idx, row) in t.rows.iter().enumerate() {
                    for (cell_idx, cell) in row.cells.iter().enumerate() {
                        collect_bare_blocks(
                            &cell.blocks,
                            format!("{block_path}/tbl({})/r{row_idx}c{cell_idx}", t.id.0),
                            out,
                        );
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }
}

fn push_paragraph_record(p: &ParagraphNode, path: String, out: &mut Vec<ParagraphRecord>) {
    let visible_text = paragraph_visible_text(p);
    let normalized = normalize_line(&visible_text);
    if normalized.is_empty() {
        return;
    }
    out.push(ParagraphRecord {
        line_index: out.len(),
        path,
        id: p.id.0.to_string(),
        style_id: p.style_id.as_ref().map(ToString::to_string),
        numbering: p
            .numbering
            .as_ref()
            .map(|n| (n.num_id, n.ilvl, n.synthesized_text.clone())),
        materialized_numbering: p
            .materialized_numbering
            .as_ref()
            .map(|n| (n.num_id, n.ilvl, n.synthesized_text.clone())),
        literal_prefix: p.literal_prefix.clone(),
        rendered_text: p.rendered_text.clone(),
        inline_text: extract_inline_text(&p.all_inlines_owned()),
        visible_text,
        segment_statuses: p
            .segments
            .iter()
            .map(|segment| format!("{:?}", segment.status))
            .collect(),
    });
}

fn normalize_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_doc_text(lines: &[String]) -> String {
    lines.join("\n")
}

fn panic_with_mismatch_context(reject_lines: &[String], before_lines: &[String]) -> ! {
    let first_diff = reject_lines
        .iter()
        .zip(before_lines.iter())
        .enumerate()
        .find(|(_, (reject, base))| reject != base)
        .map(|(idx, _)| idx)
        .unwrap_or_else(|| reject_lines.len().min(before_lines.len()));

    let start = first_diff.saturating_sub(2);
    let end = (first_diff + 3).min(reject_lines.len().max(before_lines.len()));

    println!(
        "reject line count={} base line count={} first_diff={}",
        reject_lines.len(),
        before_lines.len(),
        first_diff
    );
    for idx in start..end {
        let reject = reject_lines.get(idx);
        let base = before_lines.get(idx);
        println!("line {idx}: reject={reject:?} base={base:?}");
    }

    panic!("reject-all text does not match base document");
}

fn print_records(label: &str, records: &[ParagraphRecord], needle: &str) {
    println!("=== {label} records matching {needle:?} ===");
    for record in records
        .iter()
        .filter(|record| normalize_line(&record.visible_text).contains(needle))
    {
        println!(
            "[line {}] path={} id={} style={:?} numbering={:?} materialized={:?} literal_prefix={:?}",
            record.line_index,
            record.path,
            record.id,
            record.style_id,
            record.numbering,
            record.materialized_numbering,
            record.literal_prefix,
        );
        println!("  rendered_text={:?}", record.rendered_text);
        println!("  inline_text={:?}", record.inline_text);
        println!("  visible_text={:?}", record.visible_text);
        println!("  segment_statuses={:?}", record.segment_statuses);
    }
}

fn print_xml_snippets(label: &str, bytes: &[u8], needle: &str) {
    let archive = DocxArchive::read(bytes).expect("archive");
    let document_xml = archive.get("word/document.xml").expect("document.xml");
    let xml = String::from_utf8_lossy(document_xml);
    println!("=== {label} XML snippets for {needle:?} ===");
    for (count, offset) in xml.match_indices(needle).enumerate() {
        if count >= 5 {
            break;
        }
        let start = offset.0.saturating_sub(250);
        let end = (offset.0 + needle.len() + 250).min(xml.len());
        println!("{}", &xml[start..end]);
        println!("---");
    }
}

#[test]
#[ignore = "large fixture regression harness for annex prefix removal"]
fn debug_annex_it_solutions_reject_preserves_prefix() {
    let sample_dir = samples_dir().join("annex-it-solutions");
    let before_bytes = fs::read(sample_dir.join("before.docx")).expect("read before");
    let after_bytes = fs::read(sample_dir.join("after.docx")).expect("read after");

    let runtime = SimpleRuntime::new();
    let before = runtime.import_docx(&before_bytes).expect("import before");
    let after = runtime.import_docx(&after_bytes).expect("import after");
    let before_records = collect_paragraph_records(&before.canonical);
    let after_records = collect_paragraph_records(&after.canonical);

    print_records(
        "before",
        &before_records,
        "(3) Instructions concerning specific positions",
    );
    print_records(
        "after",
        &after_records,
        "Instructions concerning specific positions",
    );

    runtime
        .diff_and_redline(&before.doc_handle, &after.doc_handle, pair_meta())
        .expect("diff_and_redline");

    let tracked = runtime
        .tracked_view(&before.doc_handle)
        .expect("tracked_view after diff");
    let tracked_records = collect_paragraph_records(&tracked.canonical);
    print_records(
        "tracked",
        &tracked_records,
        "Instructions concerning specific positions",
    );

    let redline = runtime
        .export_docx(&before.doc_handle, ExportMode::Redline)
        .expect("export redline");
    print_xml_snippets(
        "redline",
        &redline,
        "Instructions concerning specific positions",
    );
    print_xml_snippets("redline", &redline, "(3)");

    let extract = extract_redline(&redline).expect("extract redline");
    let reject_lines: Vec<String> = extract
        .body
        .iter()
        .map(|paragraph| normalize_line(&paragraph.reject_text()))
        .filter(|line| !line.is_empty())
        .collect();
    let before_lines: Vec<String> = before_records
        .iter()
        .map(|record| normalize_line(&record.visible_text))
        .filter(|line| !line.is_empty())
        .collect();

    let mismatch = reject_lines.len() != before_lines.len()
        || normalize_doc_text(&reject_lines) != normalize_doc_text(&before_lines);

    if mismatch {
        panic_with_mismatch_context(&reject_lines, &before_lines);
    }
}
