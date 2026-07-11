//! Evaluation / diagnostic harness for vocabulary extraction.
//! Run with: cargo test --test vocabulary_eval -- --ignored --nocapture

use std::fs;
use std::time::Instant;

use stemma::vocabulary::{self, DocumentVocabulary, RoleFrequency};
use stemma::{CanonDoc, DocxRuntime, SimpleRuntime};

use crate::common;

fn load_docx(path: &str) -> Option<CanonDoc> {
    let bytes = fs::read(path).ok()?;
    let runtime = SimpleRuntime::new();
    let import = runtime.import_docx(&bytes).ok()?;
    let view = runtime.view(&import.doc_handle).ok()?;
    Some(std::sync::Arc::unwrap_or_clone(view.canonical))
}

fn print_vocabulary(name: &str, doc: &CanonDoc, vocab: &DocumentVocabulary, elapsed_us: u128) {
    let all_paras = common::all_paragraphs(doc);
    println!("\n{}", "=".repeat(60));
    println!("  {name}");
    println!(
        "  {} paragraphs, extracted in {elapsed_us}µs",
        all_paras.len()
    );
    println!("{}", "=".repeat(60));

    println!("\n  Paragraph roles ({}):", vocab.paragraph_roles.len());
    for role in &vocab.paragraph_roles {
        let freq = match role.frequency {
            RoleFrequency::Primary => "PRIMARY",
            RoleFrequency::Common => "common ",
            RoleFrequency::Minor => "minor  ",
            RoleFrequency::Rare => "rare   ",
        };
        let num_tag = if role.has_numbering {
            match &role.numbering_source {
                Some(vocabulary::NumberingSource::Auto) => " [auto-num]",
                Some(vocabulary::NumberingSource::LiteralPrefix) => " [literal-num]",
                None => " [num?]",
            }
        } else {
            ""
        };
        let text = if role.exemplar_text.is_empty() {
            "(empty)".to_string()
        } else {
            let s = &role.exemplar_text;
            if s.chars().count() > 60 {
                let truncated: String = s.chars().take(60).collect();
                format!("\"{truncated}…\"")
            } else {
                format!("\"{s}\"")
            }
        };
        println!(
            "    [{freq}] {:30} n={:<4} {}{}",
            role.id, role.count, role.description, num_tag
        );
        println!("             {:30} {text}", "");
    }

    if !vocab.inline_roles.is_empty() {
        println!("\n  Inline roles ({}):", vocab.inline_roles.len());
        for role in &vocab.inline_roles {
            println!("    {:30} {}", role.id, role.description);
        }
    }

    if !vocab.table_roles.is_empty() {
        println!("\n  Table roles ({}):", vocab.table_roles.len());
        for role in &vocab.table_roles {
            println!(
                "    {:30} count={:<4} {}",
                role.id, role.count, role.description
            );
        }
    }
}

#[test]
#[ignore = "diagnostic — run manually with --nocapture"]
fn eval_all_fixtures() {
    let fixtures: Vec<(&str, String)> = vec![
        (
            "safe-valcap-vs-discount",
            "testdata/safe-valcap-vs-discount/before.docx".into(),
        ),
        ("simple-text", "testdata/simple-text/before.docx".into()),
        (
            "twenty-paragraphs",
            "testdata/twenty-paragraphs/before.docx".into(),
        ),
        ("table-changes", "testdata/table-changes/before.docx".into()),
        ("paragraphs", "testdata/paragraphs/before.docx".into()),
        ("long-table", "testdata/long-table/before.docx".into()),
        ("style", "testdata/style/before.docx".into()),
        (
            "safe-us-vs-canada",
            "testdata/safe-us-vs-canada/before.docx".into(),
        ),
        (
            "showcase-landing-page",
            "testdata/showcase-landing-page/before.docx".into(),
        ),
        (
            "edgar-saas",
            common::samples_dir()
                .join("edgar-saas/before.docx")
                .to_string_lossy()
                .to_string(),
        ),
        (
            "humira-epar",
            common::samples_dir()
                .join("humira-epar/before.docx")
                .to_string_lossy()
                .to_string(),
        ),
        (
            "byooviz-epar",
            common::samples_dir()
                .join("byooviz-epar/before.docx")
                .to_string_lossy()
                .to_string(),
        ),
        (
            "academic-regulations",
            common::samples_dir()
                .join("academic-regulations/before.docx")
                .to_string_lossy()
                .to_string(),
        ),
        (
            "eba-annex-ii",
            common::samples_dir()
                .join("eba-annex-ii-track-changes/before.docx")
                .to_string_lossy()
                .to_string(),
        ),
    ];

    let mut total_paras = 0usize;
    let mut total_us = 0u128;
    let mut total_roles = 0usize;

    println!("\n\n========== VOCABULARY EXTRACTION EVALUATION ==========\n");

    for (name, path) in &fixtures {
        let Some(doc) = load_docx(path) else {
            println!("  SKIP {name}: could not load {path}");
            continue;
        };

        let t0 = Instant::now();
        let vocab = vocabulary::extract_vocabulary(&doc);
        let elapsed = t0.elapsed().as_micros();

        let para_count = common::all_paragraphs(&doc).len();
        total_paras += para_count;
        total_us += elapsed;
        total_roles += vocab.paragraph_roles.len();

        print_vocabulary(name, &doc, &vocab, elapsed);
    }

    println!("\n\n========== SUMMARY ==========");
    println!("  Total paragraphs processed: {total_paras}");
    println!(
        "  Total extraction time:      {total_us}µs ({:.1}ms)",
        total_us as f64 / 1000.0
    );
    println!("  Total paragraph roles:      {total_roles}");
    if total_paras > 0 {
        println!(
            "  Compression ratio:          {:.1}x ({total_paras} paragraphs → {total_roles} roles)",
            total_paras as f64 / total_roles as f64
        );
    }
    println!(
        "  Avg time per fixture:       {:.0}µs",
        total_us as f64 / fixtures.len() as f64
    );
    println!();
}

#[test]
#[ignore = "benchmark — run manually with --nocapture"]
fn benchmark_extraction_throughput() {
    let samples = common::samples_dir();
    let large_fixtures: Vec<(&str, String)> = vec![
        (
            "humira-epar",
            samples
                .join("humira-epar/before.docx")
                .to_string_lossy()
                .to_string(),
        ),
        (
            "byooviz-epar",
            samples
                .join("byooviz-epar/before.docx")
                .to_string_lossy()
                .to_string(),
        ),
        (
            "edgar-saas",
            samples
                .join("edgar-saas/before.docx")
                .to_string_lossy()
                .to_string(),
        ),
    ];

    println!("\n\n========== BENCHMARK (10 iterations each) ==========\n");

    for (name, path) in &large_fixtures {
        let Some(doc) = load_docx(path) else {
            println!("  SKIP {name}");
            continue;
        };

        let para_count = common::all_paragraphs(&doc).len();

        // Warmup.
        let _ = vocabulary::extract_vocabulary(&doc);

        let iterations = 10;
        let t0 = Instant::now();
        for _ in 0..iterations {
            let _ = vocabulary::extract_vocabulary(&doc);
        }
        let total = t0.elapsed();
        let avg_us = total.as_micros() / iterations;

        println!(
            "  {name:25} {para_count:>5} paras  avg={avg_us:>6}µs  ({:.1}ms)  {:.0} paras/ms",
            avg_us as f64 / 1000.0,
            para_count as f64 / (avg_us as f64 / 1000.0)
        );
    }
    println!();
}
