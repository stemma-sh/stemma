//! Turn two document versions into a native tracked-change document.
//!
//! Run with:
//! `cargo run -p stemma-diff --example redline_from_two_files -- base.docx target.docx redline.docx`

use std::path::PathBuf;

use stemma::ExportOptions;
use stemma::api::Document;

fn main() {
    let [base_path, target_path, output_path] = parse_paths();

    let base_bytes = std::fs::read(&base_path)
        .unwrap_or_else(|error| panic!("read base document {}: {error}", base_path.display()));
    let target_bytes = std::fs::read(&target_path)
        .unwrap_or_else(|error| panic!("read target document {}: {error}", target_path.display()));

    let base = Document::parse(&base_bytes)
        .unwrap_or_else(|error| panic!("parse base document {}: {error}", base_path.display()));
    let target = Document::parse(&target_bytes)
        .unwrap_or_else(|error| panic!("parse target document {}: {error}", target_path.display()));

    let comparison =
        stemma_diff::diff_detailed(&base, &target).expect("compare base and target documents");
    report_diagnostics("base", &comparison.base_diagnostics);
    report_diagnostics("target", &comparison.target_diagnostics);

    let bytes = comparison
        .document
        .serialize(&ExportOptions::default())
        .expect("serialize tracked-change document");
    std::fs::write(&output_path, bytes).unwrap_or_else(|error| {
        panic!(
            "write tracked-change document {}: {error}",
            output_path.display()
        )
    });

    println!("wrote {}", output_path.display());
}

fn report_diagnostics(label: &str, diagnostics: &[stemma::api::Diagnostic]) {
    for diagnostic in diagnostics {
        match &diagnostic.context {
            Some(context) => eprintln!(
                "{label} import {:?}: {} ({context})",
                diagnostic.level, diagnostic.message
            ),
            None => eprintln!(
                "{label} import {:?}: {}",
                diagnostic.level, diagnostic.message
            ),
        }
    }
}

fn parse_paths() -> [PathBuf; 3] {
    let paths = std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    paths.try_into().unwrap_or_else(|paths: Vec<PathBuf>| {
        panic!(
            "expected exactly three paths: base.docx target.docx redline.docx; received {}",
            paths.len()
        )
    })
}
