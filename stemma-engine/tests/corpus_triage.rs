//! Corpus triage: ZIP health + rich fingerprinting for every DOCX file
//! in a directory tree.
//!
//! Produces a single JSONL file with one record per file, containing:
//! - ZIP health (opens? entry count?)
//! - 27 feature flags (compatible with fingerprint_corpus)
//! - Full element census (every XML element name with count)
//! - Relationship types and part inventory
//! - Structural metrics (paragraph/table/image/field/tracked-change counts)
//!
//! OOXML validation is intentionally omitted here (too slow for 736K files).
//! Run validation as a second pass on the diversity-selected subset, using
//! either `npx @xarsh/ooxml-validator` or the Word Oracle.
//!
//! Output is append-only and resumable: re-running skips already-processed files.
//!
//! Run:
//!   CORPUS_DIR=/path/to/corpus \
//!   RUST_MIN_STACK=67108864 \
//!   cargo test --release --test corpus_triage -- --ignored --nocapture
//!
//! Output: target/corpus-triage.jsonl

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use quick_xml::Reader;
use quick_xml::events::Event;
use rayon::prelude::*;
use serde::Serialize;

mod common;
use common::build_element_census;

// ── Output schema ────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct TriageResult {
    // Identity
    path: String,
    size_bytes: u64,

    // ZIP health
    zip_ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    zip_error: Option<String>,
    zip_entry_count: usize,

    // Parts present in the ZIP
    parts: Vec<String>,

    // Content types from [Content_Types].xml
    content_types: Vec<String>,

    // Relationship types from word/_rels/document.xml.rels
    relationship_types: Vec<String>,

    // ── Feature flags (27, compatible with fingerprint_corpus) ────────
    // Document parts
    has_comments: bool,
    has_comments_extended: bool,
    has_footnotes: bool,
    has_endnotes: bool,
    has_headers: bool,
    has_footers: bool,
    has_numbering: bool,
    has_styles_with_effects: bool,
    // Elements in document.xml
    has_tables: bool,
    has_nested_tables: bool,
    has_images: bool,
    has_math: bool,
    has_fields: bool,
    has_hyperlinks: bool,
    has_bookmarks: bool,
    has_sdts: bool,
    has_tracked_changes: bool,
    has_move_tracking: bool,
    has_section_breaks: bool,
    has_numbering_refs: bool,
    has_tab_stops: bool,
    has_footnote_refs: bool,
    has_endnote_refs: bool,
    // Formatting
    has_bold: bool,
    has_italic: bool,
    has_underline: bool,
    has_highlight: bool,
    has_color: bool,
    has_font_changes: bool,

    // ── Element census (every element in document.xml with count) ────
    element_census: HashMap<String, usize>,

    // ── Structural metrics ───────────────────────────────────────────
    paragraph_count: usize,
    table_count: usize,
    image_count: usize,
    hyperlink_count: usize,
    field_count: usize,
    tracked_change_count: usize,
    feature_count: usize,
}

// ── Test entry point ─────────────────────────────────────────────────────

#[test]
#[ignore = "corpus triage — run on demand with CORPUS_DIR"]
fn triage_corpus() {
    // On-demand diagnostic: with no corpus to scan there is nothing to triage.
    // Skip gracefully (matching the corpus-dependent ignored tiers) so the
    // `gate-confidence` / `--ignored` run stays green when CORPUS_DIR is unset,
    // per the graceful-skip contract in stemma-engine/docs/testing_strategy.md.
    let Ok(corpus_dir) = std::env::var("CORPUS_DIR") else {
        eprintln!(
            "SKIP triage_corpus: CORPUS_DIR is not set (set it to a corpus root, \
             e.g. /path/to/corpus, to run this diagnostic)"
        );
        return;
    };
    let corpus_path = Path::new(&corpus_dir);
    assert!(
        corpus_path.is_dir(),
        "CORPUS_DIR does not exist: {corpus_dir}"
    );

    let out_path = PathBuf::from("target/corpus-triage.jsonl");

    // ── Discover all .docx files ─────────────────────────────────────
    eprintln!("Scanning {corpus_dir} for .docx files ...");
    let all_files = discover_docx_files(corpus_path);
    eprintln!("Found {} .docx files", all_files.len());

    // ── Load already-processed paths for resumability ────────────────
    let done: HashSet<String> = if out_path.exists() {
        let reader = BufReader::new(fs::File::open(&out_path).unwrap());
        reader
            .lines()
            .filter_map(|line| {
                let line = line.ok()?;
                // Extract "path" field from JSON without full parse
                let start = line.find("\"path\":\"")?;
                let rest = &line[start + 8..];
                let end = rest.find('"')?;
                Some(rest[..end].to_string())
            })
            .collect()
    } else {
        HashSet::new()
    };

    let todo: Vec<&PathBuf> = all_files
        .iter()
        .filter(|f| {
            let rel = f.strip_prefix(corpus_path).unwrap_or(f);
            !done.contains(&rel.to_string_lossy().to_string())
        })
        .collect();

    eprintln!(
        "Total: {} | Already done: {} | Remaining: {}",
        all_files.len(),
        done.len(),
        todo.len()
    );

    if todo.is_empty() {
        eprintln!("All files already processed!");
        print_summary(&out_path, all_files.len());
        return;
    }

    // ── Process in parallel ──────────────────────────────────────────
    let output_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&out_path)
        .expect("open output file");
    let writer = Mutex::new(BufWriter::new(output_file));

    let processed = AtomicUsize::new(0);
    let start = Instant::now();

    todo.par_iter().for_each(|file_path| {
        let bytes = match fs::read(file_path) {
            Ok(b) => b,
            Err(_) => return,
        };

        let rel_path = file_path
            .strip_prefix(corpus_path)
            .unwrap_or(file_path)
            .to_string_lossy()
            .to_string();

        let result = triage_file(&rel_path, &bytes);
        let json = serde_json::to_string(&result).unwrap();

        {
            let mut w = writer.lock().unwrap();
            writeln!(w, "{json}").unwrap();
        }

        let n = processed.fetch_add(1, Ordering::Relaxed) + 1;
        if n.is_multiple_of(5000) {
            let elapsed = start.elapsed().as_secs_f64();
            let rate = n as f64 / elapsed;
            let eta = (todo.len() - n) as f64 / rate;
            eprintln!(
                "  [{n:>7}/{:>7}] {rate:.0} files/s | ETA {:.0}m",
                todo.len(),
                eta / 60.0
            );
        }
    });

    // Flush
    writer.lock().unwrap().flush().unwrap();

    let elapsed = start.elapsed();
    eprintln!(
        "\nDone: {} files in {:.0}s ({:.0} files/s)",
        todo.len(),
        elapsed.as_secs_f64(),
        todo.len() as f64 / elapsed.as_secs_f64()
    );

    print_summary(&out_path, all_files.len());
}

// ── File discovery ───────────────────────────────────────────────────────

fn discover_docx_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_docx_recursive(root, &mut files);
    files.sort();
    files
}

fn collect_docx_recursive(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_docx_recursive(&path, out);
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("docx"))
        {
            out.push(path);
        }
    }
}

// ── Per-file triage ──────────────────────────────────────────────────────

fn triage_file(path: &str, bytes: &[u8]) -> TriageResult {
    let size_bytes = bytes.len() as u64;

    // 1. ZIP check
    let cursor = Cursor::new(bytes);
    let zip = match zip::ZipArchive::new(cursor) {
        Ok(z) => z,
        Err(e) => {
            return TriageResult {
                path: path.to_string(),
                size_bytes,
                zip_ok: false,
                zip_error: Some(e.to_string()),
                zip_entry_count: 0,
                parts: vec![],
                content_types: vec![],
                relationship_types: vec![],
                has_comments: false,
                has_comments_extended: false,
                has_footnotes: false,
                has_endnotes: false,
                has_headers: false,
                has_footers: false,
                has_numbering: false,
                has_styles_with_effects: false,
                has_tables: false,
                has_nested_tables: false,
                has_images: false,
                has_math: false,
                has_fields: false,
                has_hyperlinks: false,
                has_bookmarks: false,
                has_sdts: false,
                has_tracked_changes: false,
                has_move_tracking: false,
                has_section_breaks: false,
                has_numbering_refs: false,
                has_tab_stops: false,
                has_footnote_refs: false,
                has_endnote_refs: false,
                has_bold: false,
                has_italic: false,
                has_underline: false,
                has_highlight: false,
                has_color: false,
                has_font_changes: false,
                element_census: HashMap::new(),
                paragraph_count: 0,
                table_count: 0,
                image_count: 0,
                hyperlink_count: 0,
                field_count: 0,
                tracked_change_count: 0,
                feature_count: 0,
            };
        }
    };

    let mut zip = zip;
    let zip_entry_count = zip.len();

    // 2. Part inventory
    let parts: Vec<String> = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_string()))
        .collect();

    let has_comments = parts.iter().any(|n| n == "word/comments.xml");
    let has_comments_extended = parts.iter().any(|n| n == "word/commentsExtended.xml");
    let has_footnotes = parts.iter().any(|n| n == "word/footnotes.xml");
    let has_endnotes = parts.iter().any(|n| n == "word/endnotes.xml");
    let has_headers = parts.iter().any(|n| n.starts_with("word/header"));
    let has_footers = parts.iter().any(|n| n.starts_with("word/footer"));
    let has_numbering = parts.iter().any(|n| n == "word/numbering.xml");
    let has_styles_with_effects = parts.iter().any(|n| n == "word/stylesWithEffects.xml");

    // 3. Content types from [Content_Types].xml
    let content_types = read_zip_string(&mut zip, "[Content_Types].xml")
        .map(|xml| extract_content_types(&xml))
        .unwrap_or_default();

    // 4. Relationship types from word/_rels/document.xml.rels
    let relationship_types = read_zip_string(&mut zip, "word/_rels/document.xml.rels")
        .map(|xml| extract_relationship_types(&xml))
        .unwrap_or_default();

    // 5. Read document.xml for fingerprinting + element census
    let doc_xml = read_zip_string(&mut zip, "word/document.xml").unwrap_or_default();

    // Element census via quick-xml
    let element_census = build_element_census(&doc_xml);

    // Feature flags (string scanning, compatible with fingerprint_corpus)
    let count = |pattern: &str| -> usize { doc_xml.matches(pattern).count() };

    let table_count = count("<w:tbl>");
    let has_tables = table_count > 0;
    let has_nested_tables = doc_xml.contains("<w:tc>") && table_count > 1;

    let image_count = count("<w:drawing>") + count("<w:pict>");
    let has_images = image_count > 0;

    let has_math = doc_xml.contains("<m:oMath") || doc_xml.contains("<m:oMathPara");

    let field_count = count("<w:fldChar ") + count("<w:fldSimple ");
    let has_fields = field_count > 0;

    let hyperlink_count = count("<w:hyperlink ");
    let has_hyperlinks = hyperlink_count > 0;

    let has_bookmarks = doc_xml.contains("<w:bookmarkStart ");
    let has_sdts = doc_xml.contains("<w:sdt>");

    let tracked_ins = count("<w:ins ");
    let tracked_del = count("<w:del ");
    let tracked_rpr = count("<w:rPrChange");
    let tracked_ppr = count("<w:pPrChange");
    let tracked_change_count = tracked_ins + tracked_del + tracked_rpr + tracked_ppr;
    let has_tracked_changes = tracked_change_count > 0;

    let has_move_tracking = doc_xml.contains("<w:moveFrom") || doc_xml.contains("<w:moveTo");

    let sect_count = count("<w:sectPr");
    let has_section_breaks = sect_count > 1;

    let has_numbering_refs = doc_xml.contains("<w:numPr>");
    let has_tab_stops = doc_xml.contains("<w:tabs>");
    let has_footnote_refs = doc_xml.contains("<w:footnoteReference ");
    let has_endnote_refs = doc_xml.contains("<w:endnoteReference ");

    let paragraph_count = count("<w:p ") + count("<w:p>");

    let has_bold = doc_xml.contains("<w:b/>") || doc_xml.contains("<w:b ");
    let has_italic = doc_xml.contains("<w:i/>") || doc_xml.contains("<w:i ");
    let has_underline = doc_xml.contains("<w:u ");
    let has_highlight = doc_xml.contains("<w:highlight ");
    let has_color = doc_xml.contains("<w:color ");
    let has_font_changes = doc_xml.contains("<w:rFonts ");

    let feature_count = [
        has_tables,
        has_nested_tables,
        has_images,
        has_math,
        has_fields,
        has_hyperlinks,
        has_bookmarks,
        has_sdts,
        has_tracked_changes,
        has_move_tracking,
        has_section_breaks,
        has_numbering_refs,
        has_comments,
        has_comments_extended,
        has_footnotes,
        has_endnotes,
        has_headers,
        has_footers,
        has_tab_stops,
        has_footnote_refs,
        has_endnote_refs,
        has_bold,
        has_italic,
        has_underline,
        has_highlight,
        has_color,
        has_font_changes,
    ]
    .iter()
    .filter(|&&v| v)
    .count();

    TriageResult {
        path: path.to_string(),
        size_bytes,
        zip_ok: true,
        zip_error: None,
        zip_entry_count,
        parts,
        content_types,
        relationship_types,
        has_comments,
        has_comments_extended,
        has_footnotes,
        has_endnotes,
        has_headers,
        has_footers,
        has_numbering,
        has_styles_with_effects,
        has_tables,
        has_nested_tables,
        has_images,
        has_math,
        has_fields,
        has_hyperlinks,
        has_bookmarks,
        has_sdts,
        has_tracked_changes,
        has_move_tracking,
        has_section_breaks,
        has_numbering_refs,
        has_tab_stops,
        has_footnote_refs,
        has_endnote_refs,
        has_bold,
        has_italic,
        has_underline,
        has_highlight,
        has_color,
        has_font_changes,
        element_census,
        paragraph_count,
        table_count,
        image_count,
        hyperlink_count,
        field_count,
        tracked_change_count,
        feature_count,
    }
}

// ── XML helpers ──────────────────────────────────────────────────────────

fn read_zip_string(zip: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str) -> Option<String> {
    let mut file = zip.by_name(name).ok()?;
    let mut buf = String::new();
    file.read_to_string(&mut buf).ok()?;
    Some(buf)
}

// build_element_census now lives in tests/common/mod.rs (shared with the
// element-fidelity gate). Imported above.

fn extract_content_types(xml: &str) -> Vec<String> {
    let mut types = Vec::new();
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                for attr in e.attributes().flatten() {
                    if attr.key.as_ref() == b"ContentType" {
                        let val = String::from_utf8_lossy(&attr.value).to_string();
                        if !types.contains(&val) {
                            types.push(val);
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    types.sort();
    types
}

fn extract_relationship_types(xml: &str) -> Vec<String> {
    let mut types = Vec::new();
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                for attr in e.attributes().flatten() {
                    if attr.key.as_ref() == b"Type" {
                        let val = String::from_utf8_lossy(&attr.value).to_string();
                        if !types.contains(&val) {
                            types.push(val);
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    types.sort();
    types
}

// ── Summary stats ────────────────────────────────────────────────────────

fn print_summary(out_path: &Path, _total_files: usize) {
    let reader = match fs::File::open(out_path) {
        Ok(f) => BufReader::new(f),
        Err(_) => return,
    };

    let mut total = 0usize;
    let mut zip_failures = 0usize;
    let mut feature_counts: HashMap<&'static str, usize> = HashMap::new();
    let mut element_variety: Vec<usize> = Vec::new(); // distinct elements per doc

    for line in reader.lines().map_while(Result::ok) {
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        total += 1;

        if v["zip_ok"] == false {
            zip_failures += 1;
            continue;
        }

        // Count features
        let features = [
            ("tables", "has_tables"),
            ("nested_tables", "has_nested_tables"),
            ("images", "has_images"),
            ("math", "has_math"),
            ("fields", "has_fields"),
            ("hyperlinks", "has_hyperlinks"),
            ("bookmarks", "has_bookmarks"),
            ("sdts", "has_sdts"),
            ("tracked_changes", "has_tracked_changes"),
            ("move_tracking", "has_move_tracking"),
            ("section_breaks", "has_section_breaks"),
            ("comments", "has_comments"),
            ("footnotes", "has_footnotes"),
            ("endnotes", "has_endnotes"),
            ("headers", "has_headers"),
            ("footers", "has_footers"),
            ("numbering", "has_numbering_refs"),
        ];
        for (label, key) in features {
            if v[key] == true {
                *feature_counts.entry(label).or_default() += 1;
            }
        }

        if let Some(census) = v["element_census"].as_object() {
            element_variety.push(census.len());
        }
    }

    let parseable = total - zip_failures;

    eprintln!();
    eprintln!("=== Corpus Triage Summary ===");
    eprintln!("  Total files:       {total:>8}");
    eprintln!("  ZIP failures:      {zip_failures:>8}");
    eprintln!("  Parseable:         {parseable:>8}");
    eprintln!();

    eprintln!("  Feature distribution:");
    let mut sorted: Vec<_> = feature_counts.iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(a.1));
    for (feat, count) in &sorted {
        eprintln!(
            "    {feat:20} {count:>7} ({:.1}%)",
            **count as f64 / parseable.max(1) as f64 * 100.0
        );
    }

    if !element_variety.is_empty() {
        element_variety.sort();
        let median = element_variety[element_variety.len() / 2];
        let max = element_variety.last().unwrap();
        eprintln!();
        eprintln!("  Element variety: median {median} distinct elements/doc, max {max}");
    }

    eprintln!();
    eprintln!("  Output: {}", out_path.display());
}
