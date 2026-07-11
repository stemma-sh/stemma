//! Latency bench for the public `Document` facade.
//!
//! Measures the engine's claims ("millisecond startup, sub-millisecond save")
//! against real numbers. Times each phase with `std::time::Instant` over N
//! iterations and reports p50/p95. Run with a RELEASE build:
//!
//!   cargo run -p stemma --release --example bench
//!
//! Phases measured, on a small doc (a few paragraphs) and a large doc:
//!   (1) Document::parse                  — cold open
//!   (2) apply (one tracked ReplaceParagraphText) — single edit
//!   (3) serialize (validator Off vs Blocking)    — the "save"
//!   (4) full open -> one edit -> save round-trip
//!   (5) read projections: to_markdown, outline   — large doc
//!
//! Large doc selection:
//!   - prefer the biggest *.docx under $STEMMA_CORPUS_ROOT/backend/samples
//!   - else fall back to a synthesized ~1000-paragraph DOCX built in-process.
//!
//! This example only READS the engine through `stemma::api` + the v4 wire path
//! (the same surface quickstart.rs uses). It does not touch engine internals.

use std::io::Write as _;
use std::time::{Duration, Instant};

use stemma::api::Document;
use stemma::edit_v4::parse_transaction;
use stemma::{ExportMode, ExportOptions, ValidatorLevel};

/// One timed phase: name + the per-iteration durations.
struct Samples {
    name: String,
    durs: Vec<Duration>,
}

impl Samples {
    fn percentile(&self, p: f64) -> Duration {
        // nearest-rank on a sorted copy.
        let mut sorted = self.durs.clone();
        sorted.sort();
        if sorted.is_empty() {
            return Duration::ZERO;
        }
        let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
        let idx = rank.saturating_sub(1).min(sorted.len() - 1);
        sorted[idx]
    }
    fn min(&self) -> Duration {
        self.durs.iter().copied().min().unwrap_or(Duration::ZERO)
    }
    fn p50(&self) -> Duration {
        self.percentile(50.0)
    }
    fn p95(&self) -> Duration {
        self.percentile(95.0)
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Time `f` `iters` times, returning the per-iteration samples. Runs a few
/// warmup iterations first so we measure warm cache, not first-touch faults.
fn bench<F: FnMut()>(name: &str, iters: usize, warmup: usize, mut f: F) -> Samples {
    for _ in 0..warmup {
        f();
    }
    let mut durs = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t0 = Instant::now();
        f();
        durs.push(t0.elapsed());
    }
    Samples {
        name: name.to_string(),
        durs,
    }
}

/// Build one tracked `replace(paragraph)` transaction targeting `block_id`
/// (pinned with `guard`), parsed+adapted just like quickstart.rs. Built once
/// outside the timed loop so phase (2) measures the engine apply, not JSON
/// parsing.
fn build_edit_txn(block_id: &str, guard: &str) -> stemma::edit::EditTransaction {
    let txn_json = format!(
        r#"{{
            "ops": [
                {{ "op": "replace",
                   "target": "{block_id}",
                   "guard": "{guard}",
                   "content": {{ "type": "paragraph",
                                 "content": [ {{ "type": "text", "text": "Benchmark replacement sentence." }} ] }} }}
            ],
            "revision": {{ "author": "bench" }},
            "summary": "latency bench edit"
        }}"#
    );
    parse_transaction(&txn_json)
        .expect("bench txn JSON is schema-valid")
        .into_edit_transaction()
        .expect("bench v4 txn adapts to an EditTransaction")
}

/// Find a transaction that `doc` can actually apply AND serialize cleanly.
///
/// We probe plain-paragraph blocks (no opaque/decoration segments — those hit
/// the engine's `UnsupportedEdit: "decoration without raw XML cannot be
/// serialized"` materialize path) in document order, building a tracked
/// `ReplaceParagraphText` against each and attempting one apply+serialize. The
/// first that round-trips clean is returned. Returns `Err` with the last
/// engine error if no plain block applies cleanly — the caller then records the
/// edit phases as unavailable rather than fabricating a number.
fn find_applicable_txn(doc: &Document) -> Result<stemma::edit::EditTransaction, String> {
    use stemma::view::{BlockRole, SegmentView};
    let view = doc.read();
    let mut last_err = "no plain-paragraph block found".to_string();
    for b in &view.blocks {
        // Only plain paragraphs with real text and no opaque inline anchors.
        let is_plain_para = matches!(b.role, BlockRole::Paragraph | BlockRole::Heading { .. });
        let has_opaque = b
            .segments
            .iter()
            .any(|s| matches!(s, SegmentView::Opaque { .. }));
        if !is_plain_para || has_opaque || b.text.trim().is_empty() {
            continue;
        }
        let txn = build_edit_txn(&b.id.to_string(), &b.guard);
        match doc.apply(&txn) {
            Ok(edited) => {
                // Must also serialize without the decoration-materialize panic.
                match edited.serialize(&export_opts(ValidatorLevel::Off)) {
                    Ok(_) => return Ok(txn),
                    Err(e) => last_err = format!("{:?}: {}", e.code, e.message),
                }
            }
            Err(e) => last_err = format!("{:?}: {}", e.code, e.message),
        }
    }
    Err(last_err)
}

fn export_opts(level: ValidatorLevel) -> ExportOptions {
    ExportOptions {
        mode: ExportMode::Redline,
        validator_level: level,
        validator: None,
    }
}

/// Synthesize a minimal valid DOCX with `n` body paragraphs, in-process.
/// Used only when the corpus is absent. Parts: [Content_Types].xml, _rels/.rels,
/// word/document.xml, word/_rels/document.xml.rels — the minimum `Document::parse`
/// requires.
fn synth_docx(n: usize) -> Vec<u8> {
    let mut body = String::with_capacity(n * 200);
    for i in 0..n {
        body.push_str(&format!(
            "<w:p><w:r><w:t>Synthetic paragraph number {i}. It carries enough words to resemble a real clause of moderate length for serialization timing.</w:t></w:r></w:p>"
        ));
    }
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:body>{body}<w:sectPr><w:pgSz w:w="12240" w:h="15840"/></w:sectPr></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;
    let root_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"></Relationships>"#;

    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, content) in [
            ("[Content_Types].xml", content_types.to_string()),
            ("_rels/.rels", root_rels.to_string()),
            ("word/document.xml", document),
            ("word/_rels/document.xml.rels", doc_rels.to_string()),
        ] {
            zip.start_file(name, opts).expect("zip start_file");
            zip.write_all(content.as_bytes()).expect("zip write");
        }
        zip.finish().expect("zip finish");
    }
    buf
}

/// Find the largest *.docx under the corpus, if present.
fn largest_corpus_docx() -> Option<(std::path::PathBuf, u64)> {
    let root = std::env::var("STEMMA_CORPUS_ROOT").ok()?;
    let samples = std::path::Path::new(&root).join("backend/samples");
    if !samples.is_dir() {
        return None;
    }
    let mut best: Option<(std::path::PathBuf, u64)> = None;
    let mut stack = vec![samples];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("docx") {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                if best.as_ref().map(|(_, s)| size > *s).unwrap_or(true) {
                    best = Some((path, size));
                }
            }
        }
    }
    best
}

/// Print a table of phases for one doc class.
fn print_table(label: &str, rows: &[Samples]) {
    println!("\n### {label}");
    println!(
        "| {:<58} | {:>10} | {:>10} | {:>10} | {:>5} |",
        "phase", "min (ms)", "p50 (ms)", "p95 (ms)", "n"
    );
    println!(
        "|{:-<60}|{:-<12}|{:-<12}|{:-<12}|{:-<7}|",
        "", "", "", "", ""
    );
    for s in rows {
        if s.durs.is_empty() {
            println!(
                "| {:<58} | {:>10} | {:>10} | {:>10} | {:>5} |",
                s.name, "n/a", "n/a", "n/a", 0
            );
        } else {
            println!(
                "| {:<58} | {:>10.4} | {:>10.4} | {:>10.4} | {:>5} |",
                s.name,
                ms(s.min()),
                ms(s.p50()),
                ms(s.p95()),
                s.durs.len()
            );
        }
    }
}

/// Emit a JSON record for one phase (for perf.json).
fn json_phase(s: &Samples) -> String {
    format!(
        r#"      {{ "phase": {:?}, "min_ms": {:.6}, "p50_ms": {:.6}, "p95_ms": {:.6}, "n": {} }}"#,
        s.name,
        ms(s.min()),
        ms(s.p50()),
        ms(s.p95()),
        s.durs.len()
    )
}

/// Run the full phase suite over `bytes`, return (table-rows, paragraph count).
fn run_suite(bytes: &[u8], iters: usize, warmup: usize) -> (Vec<Samples>, usize) {
    let doc0 = Document::parse(bytes).expect("parse for suite");
    let block_count = doc0.read().blocks.len();

    let mut rows = Vec::new();

    // (1) cold parse
    rows.push(bench(
        "(1) Document::parse (cold open)",
        iters,
        warmup,
        || {
            let _ = Document::parse(bytes).expect("parse");
        },
    ));

    // (2) single tracked apply. Probe for a block that applies+serializes
    //     cleanly (see find_applicable_txn). If none does, the apply/round-trip
    //     phases are recorded empty (surfaced as N/A) rather than fabricated —
    //     and the engine error is printed so it shows up as a finding.
    let applicable = find_applicable_txn(&doc0);
    match &applicable {
        Ok(txn) => {
            rows.push(bench(
                "(2) apply (1 tracked ReplaceParagraph)",
                iters,
                warmup,
                || {
                    let _ = doc0.apply(txn).expect("apply");
                },
            ));
        }
        Err(e) => {
            eprintln!(
                "FINDING: no plain block applies+serializes cleanly; apply/round-trip N/A. last engine error: {e}"
            );
            rows.push(Samples {
                name: "(2) apply (1 tracked ReplaceParagraph) [N/A: engine error]".to_string(),
                durs: Vec::new(),
            });
        }
    }

    // (3a) serialize, validator Off (the hot/default save path)
    rows.push(bench(
        "(3a) serialize (validator Off)",
        iters,
        warmup,
        || {
            let _ = doc0
                .serialize(&export_opts(ValidatorLevel::Off))
                .expect("serialize off");
        },
    ));

    // (3b) serialize, validator Blocking (the disk/MCP save path)
    rows.push(bench(
        "(3b) serialize (validator Blocking)",
        iters,
        warmup,
        || {
            let _ = doc0
                .serialize(&export_opts(ValidatorLevel::Blocking))
                .expect("serialize blocking");
        },
    ));

    // (4) full open -> one edit -> save round-trip (validator Off, the hot loop).
    //     The txn is pinned to a block of doc0; since we re-parse the SAME bytes
    //     the ids/guards match, so apply succeeds each iteration. This phase =
    //     phase(1) + phase(2) + phase(3a), so it is the slowest; set
    //     BENCH_SKIP_ROUNDTRIP=1 to skip it (its value is recoverable as the sum
    //     of the component phases).
    let skip_rt = std::env::var("BENCH_SKIP_ROUNDTRIP").as_deref() == Ok("1");
    match (&applicable, skip_rt) {
        (Ok(txn), false) => {
            rows.push(bench(
                "(4) open->edit->save (validator Off)",
                iters,
                warmup,
                || {
                    let d = Document::parse(bytes).expect("rt parse");
                    let edited = d.apply(txn).expect("rt apply");
                    let _ = edited
                        .serialize(&export_opts(ValidatorLevel::Off))
                        .expect("rt serialize");
                },
            ));
        }
        (Ok(_), true) => {
            rows.push(Samples {
                name: "(4) open->edit->save (validator Off) [skipped: = phases 1+2+3a]".to_string(),
                durs: Vec::new(),
            });
        }
        (Err(_), _) => {
            rows.push(Samples {
                name: "(4) open->edit->save (validator Off) [N/A: engine error]".to_string(),
                durs: Vec::new(),
            });
        }
    }

    // (5) read projections
    rows.push(bench("(5a) to_markdown", iters, warmup, || {
        let _ = doc0.to_markdown();
    }));
    rows.push(bench("(5b) outline", iters, warmup, || {
        let _ = doc0.outline();
    }));

    (rows, block_count)
}

fn main() {
    // Small doc: a real DOCX from stemma-engine/testdata (the SAFE before-template).
    let small_bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/safe-us-vs-canada/before.docx"
    ))
    .expect("read small testdata docx");

    // Large doc: biggest corpus docx, else a synthesized ~1000-paragraph doc.
    let (large_bytes, large_source) = match largest_corpus_docx() {
        Some((path, size)) => {
            let bytes = std::fs::read(&path).expect("read large corpus docx");
            (
                bytes,
                format!(
                    "corpus: {} ({:.1} MiB on disk)",
                    path.display(),
                    size as f64 / 1.048576e6
                ),
            )
        }
        None => {
            let bytes = synth_docx(1000);
            (
                bytes,
                "synthesized: 1000 paragraphs (corpus absent)".to_string(),
            )
        }
    };

    // Iteration counts: many on the small doc (cheap), few on the large doc
    // (each Blocking-serialize iter is ~tens of seconds, so a handful of samples
    // pins p50/p95 — the per-iter variance is tiny next to the magnitude).
    // Overridable via env for a deeper sweep.
    let env_usize = |k: &str, d: usize| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    let small_iters = env_usize("BENCH_SMALL_ITERS", 200);
    let small_warmup = env_usize("BENCH_SMALL_WARMUP", 20);
    let large_iters = env_usize("BENCH_LARGE_ITERS", 7);
    let large_warmup = env_usize("BENCH_LARGE_WARMUP", 1);

    println!("# stemma latency bench");
    println!(
        "build: release (run via `cargo run -p stemma --release --example bench`)\nsmall doc: stemma-engine/testdata/safe-us-vs-canada/before.docx\nlarge doc: {large_source}"
    );

    let (small_rows, small_blocks) = run_suite(&small_bytes, small_iters, small_warmup);
    print_table(
        &format!(
            "SMALL doc ({} blocks, {} bytes, n={small_iters})",
            small_blocks,
            small_bytes.len()
        ),
        &small_rows,
    );

    let (large_rows, large_blocks) = run_suite(&large_bytes, large_iters, large_warmup);
    print_table(
        &format!(
            "LARGE doc ({} blocks, {} bytes, n={large_iters})",
            large_blocks,
            large_bytes.len()
        ),
        &large_rows,
    );

    // Emit perf.json next to the doc tree so the results are committable.
    let json = format!(
        r#"{{
  "bench": "stemma Document facade latency",
  "build": "release",
  "small": {{
    "source": "stemma-engine/testdata/safe-us-vs-canada/before.docx",
    "blocks": {small_blocks},
    "bytes": {small_bytes_len},
    "iters": {small_iters},
    "phases": [
{small_phases}
    ]
  }},
  "large": {{
    "source": {large_source:?},
    "blocks": {large_blocks},
    "bytes": {large_bytes_len},
    "iters": {large_iters},
    "phases": [
{large_phases}
    ]
  }}
}}
"#,
        small_bytes_len = small_bytes.len(),
        large_bytes_len = large_bytes.len(),
        small_phases = small_rows
            .iter()
            .map(json_phase)
            .collect::<Vec<_>>()
            .join(",\n"),
        large_phases = large_rows
            .iter()
            .map(json_phase)
            .collect::<Vec<_>>()
            .join(",\n"),
    );
    let out_path = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/perf.json");
    std::fs::write(out_path, &json).expect("write perf.json");
    println!("\nwrote {out_path}");
}
