//! `docs/examples.md` is GENERATED from the example inventory in
//! `stemma-engine/examples/`. This test is both the generator and the drift
//! guard:
//!
//! - `examples_reference_is_current` (runs in the gate) renders the page and
//!   fails if the checked-in file differs, naming the fix. It also fails when
//!   an example file exists that the page does not list, or the page lists
//!   one that no longer exists, so the index cannot go stale in either
//!   direction.
//! - `regenerate_examples_reference` (ignored) writes the rendered page; run
//!   it via `just regen-examples-reference`.
//!
//! The blurbs here are curated decoration (like the op-catalog cues); the
//! EXISTENCE and COVERAGE of every example is machine-checked against the
//! directory. The examples themselves are compile-gated by
//! `cargo clippy --all-targets` in the gate.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::PathBuf;

fn doc_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../docs/examples.md")
}

fn examples_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples")
}

struct ExampleEntry {
    name: &'static str,
    blurb: &'static str,
    run: &'static str,
}

struct ExampleGroup {
    title: &'static str,
    intro: &'static str,
    entries: &'static [ExampleEntry],
}

const GROUPS: &[ExampleGroup] = &[
    ExampleGroup {
        title: "Learn the loop",
        intro: "In reading order: each teaches one idea from the guide chapters, \
                end to end, through the public facade.",
        entries: &[
            ExampleEntry {
                name: "quickstart",
                blurb: "The full durable loop in one file: parse DOCX bytes, read the \
                        projection, author one tracked edit as a typed transaction, \
                        serialize, re-parse, and assert the edit landed.",
                run: "cargo run -p stemma --example quickstart",
            },
            ExampleEntry {
                name: "walk_the_document",
                blurb: "Walk the blocks and see that one file is three documents: the \
                        redline, the accept-all reading, and the reject-all reading, \
                        each projected without mutating the stored document.",
                run: "cargo run -p stemma --example walk_the_document",
            },
            ExampleEntry {
                name: "my_first_edit",
                blurb: "One tracked replacement, end to end: apply through the v4 wire \
                        path, read the receipt (which block changed, which revision id \
                        was created), and prove the output is validator-clean.",
                run: "cargo run -p stemma --example my_first_edit",
            },
            ExampleEntry {
                name: "redline_from_two_files",
                blurb: "Diff a base and a target into one reviewable redline whose \
                        accept-all reading IS the target and whose reject-all reading \
                        IS the base.",
                run: "cargo run -p stemma --example redline_from_two_files",
            },
            ExampleEntry {
                name: "resolve_a_redline",
                blurb: "Resolve a two-author redline selectively, then verify by \
                        CONTENT: accept and reject both remove the marker, so only the \
                        resulting text proves which happened.",
                run: "cargo run -p stemma --example resolve_a_redline",
            },
            ExampleEntry {
                name: "review_before_save",
                blurb: "The review-before-save discipline: one `review()` call reports \
                        the tracked census, any untracked delta, an untouched-scope \
                        proof, and the validator verdict on the would-be bytes.",
                run: "cargo run -p stemma --example review_before_save",
            },
        ],
    },
    ExampleGroup {
        title: "Measure and prepare",
        intro: "Operational tools that happen to live in the same directory; not part \
                of the learning path.",
        entries: &[
            ExampleEntry {
                name: "bench",
                blurb: "Latency benchmark for the facade: p50/p95 for cold parse, one \
                        tracked apply, and serialize at each validator level. Needs a \
                        release build.",
                run: "cargo run -p stemma --release --example bench",
            },
            ExampleEntry {
                name: "revision_roundtrip",
                blurb: "Corpus preparation: parse and reserialize every document in a \
                        manifest so revision inventories can be compared externally. \
                        Takes arguments; see the file header.",
                run: "cargo run -p stemma --release --example revision_roundtrip",
            },
        ],
    },
];

fn render() -> String {
    // Coverage drift guard: the curated groups must name exactly the example
    // files that exist, no more and no less.
    let on_disk: BTreeSet<String> = std::fs::read_dir(examples_dir())
        .expect("read stemma-engine/examples/")
        .filter_map(|e| {
            let path = e.expect("dir entry").path();
            (path.extension().and_then(|x| x.to_str()) == Some("rs")).then(|| {
                path.file_stem()
                    .expect("stem")
                    .to_string_lossy()
                    .into_owned()
            })
        })
        .collect();
    let listed: BTreeSet<String> = GROUPS
        .iter()
        .flat_map(|g| g.entries.iter().map(|e| e.name.to_string()))
        .collect();
    assert_eq!(
        on_disk, listed,
        "docs/examples.md must list exactly the files in stemma-engine/examples/; \
         update EXAMPLES in this test and run `just regen-examples-reference`"
    );

    let mut page = String::from(
        "\
# Examples

<!-- GENERATED FILE. Do not edit by hand: this page is rendered from the
     example inventory by stemma-engine/tests/examples_reference.rs, and that
     test fails the gate when the page drifts or an example is added or
     removed without updating it. Regenerate with:

         just regen-examples-reference
-->

Runnable, compile-gated code for the common flows. Every example is a single
file in `stemma-engine/examples/`, uses only the public facade and the v4
wire path (the same path every transport drives), and is compiled with
warnings denied as part of the merge gate, so none of them can silently rot.
Each file's header comment explains, step by step, what it demonstrates.
",
    );

    for group in GROUPS {
        let _ = write!(page, "\n## {}\n\n{}\n", group.title, group.intro);
        for entry in group.entries {
            let _ = write!(
                page,
                "\n### `{name}`\n\n{blurb}\n\n```bash\n{run}\n```\n",
                name = entry.name,
                blurb = entry.blurb,
                run = entry.run,
            );
        }
    }

    let _ = write!(
        page,
        "\n## Related\n\n\
* [Create your first redline](getting-started.md): the CLI-first walkthrough.\n\
* [Embed the engine](reference/embedding.md): the facade these examples exercise.\n\
* [Concepts](guide/concepts.md), [Revisions](guide/revisions.md), [Editing](guide/editing.md): the chapters the learning examples teach.\n"
    );

    for (line_number, line) in page.lines().enumerate() {
        for banned in ["\u{2014}", "\u{2013}", " - "] {
            assert!(
                !line.contains(banned),
                "rendered page line {} contains sequence {banned:?} banned by scripts/check-docs.py: {line}",
                line_number + 1
            );
        }
        assert!(
            !line.ends_with(" -"),
            "rendered page line {} ends with a dash, banned by scripts/check-docs.py: {line}",
            line_number + 1
        );
    }
    page
}

#[test]
fn examples_reference_is_current() {
    let want = render();
    let have = std::fs::read_to_string(doc_path()).unwrap_or_else(|e| {
        panic!("docs/examples.md is missing ({e}); run `just regen-examples-reference`")
    });
    assert!(
        have == want,
        "docs/examples.md is stale relative to stemma-engine/examples/; \
         run `just regen-examples-reference` and commit the result"
    );
}

#[test]
#[ignore = "writes docs/examples.md; run via `just regen-examples-reference`"]
fn regenerate_examples_reference() {
    let path = doc_path();
    std::fs::write(&path, render()).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}
