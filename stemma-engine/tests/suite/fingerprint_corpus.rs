//! Feature fingerprinter for the stress corpus.
//!
//! Scans each parseable stress fixture's raw XML (no full IR parse) and
//! extracts a feature vector: which DOCX structural elements are present.
//! Outputs JSONL to `target/corpus-fingerprints.jsonl`.
//!
//! This enables stratified sampling: pick documents that exercise different
//! combinations of features (tables + hyperlinks, numbered lists + images, etc.)
//! for targeted mutation testing.
//!
//! Run: cargo test --release --test fingerprint_corpus -- --ignored --nocapture

use std::collections::HashMap;
use std::fs;
use std::io::{Cursor, Read};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::common;

// ── Manifest types (duplicated to avoid coupling to stress_manifest.rs) ───

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ExpectedOutcome {
    PassSupported,
    PassUnclear,
    PassNegative,
    FailRegression,
    FailUnsupported,
}

#[derive(Debug, Deserialize)]
struct FixtureExpectation {
    path: String,
    expected_outcome: ExpectedOutcome,
}

#[derive(Debug, Deserialize)]
struct StressManifest {
    fixtures: Vec<FixtureExpectation>,
}

// ── Feature fingerprint ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
struct DocFingerprint {
    path: String,
    size_bytes: u64,
    // Document parts present
    has_comments: bool,
    has_comments_extended: bool,
    has_footnotes: bool,
    has_endnotes: bool,
    has_headers: bool,
    has_footers: bool,
    has_numbering: bool,
    has_styles_with_effects: bool,
    // Elements in document.xml body
    has_tables: bool,
    has_nested_tables: bool,
    has_images: bool,          // w:drawing or w:pict
    has_math: bool,            // m:oMath or m:oMathPara
    has_fields: bool,          // w:fldChar or w:fldSimple
    has_hyperlinks: bool,      // w:hyperlink
    has_bookmarks: bool,       // w:bookmarkStart
    has_sdts: bool,            // w:sdt (content controls)
    has_tracked_changes: bool, // w:ins, w:del, w:rPrChange, w:pPrChange
    has_move_tracking: bool,   // w:moveFrom, w:moveTo
    has_section_breaks: bool,  // multiple w:sectPr
    has_numbering_refs: bool,  // w:numPr in paragraphs
    has_tab_stops: bool,       // w:tabs
    has_footnote_refs: bool,   // w:footnoteReference
    has_endnote_refs: bool,    // w:endnoteReference
    // Formatting complexity
    has_bold: bool,
    has_italic: bool,
    has_underline: bool,
    has_highlight: bool,
    has_color: bool,
    has_font_changes: bool, // w:rFonts
    // Structural complexity
    paragraph_count: usize,
    table_count: usize,
    image_count: usize,
    hyperlink_count: usize,
    field_count: usize,
    tracked_change_count: usize,
    // Derived: number of distinct features (for sorting by complexity)
    feature_count: usize,
}

// ── Test ──────────────────────────────────────────────────────────────────

#[test]
#[ignore = "corpus fingerprinting — run on demand"]
fn fingerprint_stress_corpus() {
    let manifest = load_manifest();
    let parseable: Vec<&FixtureExpectation> = manifest
        .fixtures
        .iter()
        .filter(|f| {
            matches!(
                f.expected_outcome,
                ExpectedOutcome::PassSupported
                    | ExpectedOutcome::PassUnclear
                    | ExpectedOutcome::FailRegression
            )
        })
        .collect();

    eprintln!(
        "Fingerprinting {} parseable stress fixtures...",
        parseable.len()
    );

    let stress_dir = common::stress_dir();
    let fingerprints: Vec<Option<DocFingerprint>> = parseable
        .par_iter()
        .map(|fixture| {
            let resolved = fixture
                .path
                .strip_prefix("stress/")
                .map(|rel| stress_dir.join(rel))
                .unwrap_or_else(|| std::path::PathBuf::from(&fixture.path));
            let bytes = fs::read(&resolved).ok()?;
            fingerprint_docx(&fixture.path, &bytes)
        })
        .collect();

    let mut results: Vec<DocFingerprint> = fingerprints.into_iter().flatten().collect();
    results.sort_by_key(|b| std::cmp::Reverse(b.feature_count));

    // Write JSONL. cargo test runs with the package root as cwd, where no
    // target/ exists (the workspace target lives at the repo root) — create it.
    let out_path = "target/corpus-fingerprints.jsonl";
    fs::create_dir_all("target").expect("create target dir for fingerprint artifact");
    let jsonl: String = results
        .iter()
        .map(|fp| serde_json::to_string(fp).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(out_path, &jsonl).expect("write fingerprints");

    // Summary stats
    let total = results.len();
    let mut feature_counts: HashMap<&str, usize> = HashMap::new();
    for fp in &results {
        if fp.has_tables {
            *feature_counts.entry("tables").or_default() += 1;
        }
        if fp.has_nested_tables {
            *feature_counts.entry("nested_tables").or_default() += 1;
        }
        if fp.has_images {
            *feature_counts.entry("images").or_default() += 1;
        }
        if fp.has_math {
            *feature_counts.entry("math").or_default() += 1;
        }
        if fp.has_fields {
            *feature_counts.entry("fields").or_default() += 1;
        }
        if fp.has_hyperlinks {
            *feature_counts.entry("hyperlinks").or_default() += 1;
        }
        if fp.has_bookmarks {
            *feature_counts.entry("bookmarks").or_default() += 1;
        }
        if fp.has_sdts {
            *feature_counts.entry("sdts").or_default() += 1;
        }
        if fp.has_tracked_changes {
            *feature_counts.entry("tracked_changes").or_default() += 1;
        }
        if fp.has_move_tracking {
            *feature_counts.entry("move_tracking").or_default() += 1;
        }
        if fp.has_comments {
            *feature_counts.entry("comments").or_default() += 1;
        }
        if fp.has_footnotes {
            *feature_counts.entry("footnotes").or_default() += 1;
        }
        if fp.has_endnotes {
            *feature_counts.entry("endnotes").or_default() += 1;
        }
        if fp.has_numbering_refs {
            *feature_counts.entry("numbering").or_default() += 1;
        }
        if fp.has_section_breaks {
            *feature_counts.entry("section_breaks").or_default() += 1;
        }
        if fp.has_headers {
            *feature_counts.entry("headers").or_default() += 1;
        }
        if fp.has_footers {
            *feature_counts.entry("footers").or_default() += 1;
        }
    }

    eprintln!();
    eprintln!("=== Corpus Feature Distribution ({total} docs) ===");
    let mut sorted_features: Vec<_> = feature_counts.iter().collect();
    sorted_features.sort_by(|a, b| b.1.cmp(a.1));
    for (feature, count) in &sorted_features {
        eprintln!(
            "  {feature:20} {count:5} ({:.0}%)",
            **count as f64 / total as f64 * 100.0
        );
    }

    // Top 10 most complex docs
    eprintln!();
    eprintln!("=== Top 20 Most Complex Docs ===");
    for fp in results.iter().take(20) {
        let features: Vec<&str> = [
            (fp.has_tables, "tbl"),
            (fp.has_hyperlinks, "link"),
            (fp.has_fields, "fld"),
            (fp.has_images, "img"),
            (fp.has_tracked_changes, "tc"),
            (fp.has_comments, "cmt"),
            (fp.has_numbering_refs, "num"),
            (fp.has_math, "math"),
            (fp.has_sdts, "sdt"),
            (fp.has_bookmarks, "bkm"),
            (fp.has_footnotes, "fn"),
            (fp.has_section_breaks, "sec"),
        ]
        .iter()
        .filter(|(has, _)| *has)
        .map(|(_, name)| *name)
        .collect();
        eprintln!(
            "  [{:2} features] {} ({}p, {})",
            fp.feature_count,
            fp.path,
            fp.paragraph_count,
            features.join("+"),
        );
    }

    eprintln!();
    eprintln!("Wrote {total} fingerprints to {out_path}");
}

// ── Fingerprinting logic ─────────────────────────────────────────────────

fn fingerprint_docx(path: &str, bytes: &[u8]) -> Option<DocFingerprint> {
    let cursor = Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(cursor).ok()?;

    // Check which parts exist
    let part_names: Vec<String> = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_string()))
        .collect();

    let has_comments = part_names.iter().any(|n| n == "word/comments.xml");
    let has_comments_extended = part_names.iter().any(|n| n == "word/commentsExtended.xml");
    let has_footnotes = part_names.iter().any(|n| n == "word/footnotes.xml");
    let has_endnotes = part_names.iter().any(|n| n == "word/endnotes.xml");
    let has_headers = part_names.iter().any(|n| n.starts_with("word/header"));
    let has_footers = part_names.iter().any(|n| n.starts_with("word/footer"));
    let has_numbering = part_names.iter().any(|n| n == "word/numbering.xml");
    let has_styles_with_effects = part_names.iter().any(|n| n == "word/stylesWithEffects.xml");

    // Read document.xml and scan for elements
    let doc_xml = {
        let mut file = zip.by_name("word/document.xml").ok()?;
        let mut buf = String::new();
        file.read_to_string(&mut buf).ok()?;
        buf
    };

    let count_occurrences = |pattern: &str| -> usize { doc_xml.matches(pattern).count() };

    let table_count = count_occurrences("<w:tbl>");
    let has_tables = table_count > 0;
    // Nested tables: look for tbl inside tc
    let has_nested_tables = doc_xml.contains("<w:tc>") && table_count > 1;

    let image_count = count_occurrences("<w:drawing>") + count_occurrences("<w:pict>");
    let has_images = image_count > 0;

    let has_math = doc_xml.contains("<m:oMath") || doc_xml.contains("<m:oMathPara");

    let field_count = count_occurrences("<w:fldChar ") + count_occurrences("<w:fldSimple ");
    let has_fields = field_count > 0;

    let hyperlink_count = count_occurrences("<w:hyperlink ");
    let has_hyperlinks = hyperlink_count > 0;

    let has_bookmarks = doc_xml.contains("<w:bookmarkStart ");
    let has_sdts = doc_xml.contains("<w:sdt>");

    let tracked_ins = count_occurrences("<w:ins ");
    let tracked_del = count_occurrences("<w:del ");
    let tracked_rpr = count_occurrences("<w:rPrChange");
    let tracked_ppr = count_occurrences("<w:pPrChange");
    let tracked_change_count = tracked_ins + tracked_del + tracked_rpr + tracked_ppr;
    let has_tracked_changes = tracked_change_count > 0;

    let has_move_tracking = doc_xml.contains("<w:moveFrom") || doc_xml.contains("<w:moveTo");

    // Count sectPr occurrences (multiple = section breaks)
    let sect_count = count_occurrences("<w:sectPr");
    let has_section_breaks = sect_count > 1;

    let has_numbering_refs = doc_xml.contains("<w:numPr>");
    let has_tab_stops = doc_xml.contains("<w:tabs>");
    let has_footnote_refs = doc_xml.contains("<w:footnoteReference ");
    let has_endnote_refs = doc_xml.contains("<w:endnoteReference ");

    let paragraph_count = count_occurrences("<w:p ") + count_occurrences("<w:p>");

    // Formatting
    let has_bold = doc_xml.contains("<w:b/>") || doc_xml.contains("<w:b ");
    let has_italic = doc_xml.contains("<w:i/>") || doc_xml.contains("<w:i ");
    let has_underline = doc_xml.contains("<w:u ");
    let has_highlight = doc_xml.contains("<w:highlight ");
    let has_color = doc_xml.contains("<w:color ");
    let has_font_changes = doc_xml.contains("<w:rFonts ");

    // Compute feature count
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

    Some(DocFingerprint {
        path: path.to_string(),
        size_bytes: bytes.len() as u64,
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
        paragraph_count,
        table_count,
        image_count,
        hyperlink_count,
        field_count,
        tracked_change_count,
        feature_count,
    })
}

fn load_manifest() -> StressManifest {
    let path = common::stress_dir().join("manifest.json");
    let raw = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}
