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

## Learn the loop

In reading order: each teaches one idea from the guide chapters, end to end, through the public facade.

### `quickstart`

The full durable loop in one file: parse DOCX bytes, read the projection, author one tracked edit as a typed transaction, serialize, re-parse, and assert the edit landed.

```bash
cargo run -p stemma --example quickstart
```

### `walk_the_document`

Walk the blocks and see that one file is three documents: the redline, the accept-all reading, and the reject-all reading, each projected without mutating the stored document.

```bash
cargo run -p stemma --example walk_the_document
```

### `my_first_edit`

One tracked replacement, end to end: apply through the v4 wire path, read the receipt (which block changed, which revision id was created), and prove the output is validator-clean.

```bash
cargo run -p stemma --example my_first_edit
```

### `redline_from_two_files`

Diff a base and a target into one reviewable redline whose accept-all reading IS the target and whose reject-all reading IS the base.

```bash
cargo run -p stemma --example redline_from_two_files
```

### `resolve_a_redline`

Resolve a two-author redline selectively, then verify by CONTENT: accept and reject both remove the marker, so only the resulting text proves which happened.

```bash
cargo run -p stemma --example resolve_a_redline
```

### `review_before_save`

The review-before-save discipline: one `review()` call reports the tracked census, any untracked delta, an untouched-scope proof, and the validator verdict on the would-be bytes.

```bash
cargo run -p stemma --example review_before_save
```

## Measure and prepare

Operational tools that happen to live in the same directory; not part of the learning path.

### `bench`

Latency benchmark for the facade: p50/p95 for cold parse, one tracked apply, and serialize at each validator level. Needs a release build.

```bash
cargo run -p stemma --release --example bench
```

### `revision_roundtrip`

Corpus preparation: parse and reserialize every document in a manifest so revision inventories can be compared externally. Takes arguments; see the file header.

```bash
cargo run -p stemma --release --example revision_roundtrip
```

## Related

* [Create your first redline](getting-started.md): the CLI-first walkthrough.
* [Embed the engine](reference/embedding.md): the facade these examples exercise.
* [Concepts](guide/concepts.md), [Revisions](guide/revisions.md), [Editing](guide/editing.md): the chapters the learning examples teach.
