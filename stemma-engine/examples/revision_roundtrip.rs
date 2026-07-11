//! Corpus revision-preservation prep: parse + reserialize every document in a
//! manifest, writing the roundtripped bytes next to a copy of the original so
//! the Word Oracle can compare revision inventories (original vs roundtrip).
//!
//! Usage: cargo run -p stemma --release --example revision_roundtrip -- \
//!            <manifest.json> <outdir>
//!
//! The manifest is a JSON array of {"path": ..., "hash": ...} entries (see
//! /tmp/scan_tracked.py). For each entry the roundtrip output lands in
//! `<outdir>/<hash>.roundtrip.docx` and the original is copied to
//! `<outdir>/<hash>.orig.docx`; parse/serialize failures are recorded in
//! `<outdir>/errors.jsonl` and skipped (parse coverage is a different
//! invariant — this one is about revision fidelity of documents we accept).

use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (manifest_path, outdir) = (&args[1], &args[2]);
    std::fs::create_dir_all(outdir).expect("create outdir");

    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(manifest_path).expect("read manifest"))
            .expect("parse manifest");
    let entries = manifest.as_array().expect("manifest array");

    let mut errors = std::fs::File::create(format!("{outdir}/errors.jsonl")).expect("errors file");
    let (mut ok, mut failed) = (0usize, 0usize);

    for entry in entries {
        let path = entry["path"].as_str().expect("path");
        let hash = entry["hash"].as_str().expect("hash");
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                writeln!(
                    errors,
                    r#"{{"hash":"{hash}","stage":"read","error":{:?}}}"#,
                    e.to_string()
                )
                .unwrap();
                failed += 1;
                continue;
            }
        };
        let doc = match stemma::api::Document::parse(&bytes) {
            Ok(d) => d,
            Err(e) => {
                writeln!(
                    errors,
                    r#"{{"hash":"{hash}","stage":"parse","error":{:?}}}"#,
                    e.to_string()
                )
                .unwrap();
                failed += 1;
                continue;
            }
        };
        let out = match doc.serialize(&stemma::ExportOptions::default()) {
            Ok(b) => b,
            Err(e) => {
                writeln!(
                    errors,
                    r#"{{"hash":"{hash}","stage":"serialize","error":{:?}}}"#,
                    e.to_string()
                )
                .unwrap();
                failed += 1;
                continue;
            }
        };
        std::fs::write(format!("{outdir}/{hash}.orig.docx"), &bytes).expect("write orig");
        std::fs::write(format!("{outdir}/{hash}.roundtrip.docx"), &out).expect("write roundtrip");
        ok += 1;
    }
    println!("roundtrip ok: {ok}, failed: {failed}");
}
