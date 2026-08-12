# Synthetic tracked-change demo

This public demo contains no real agreement, party, customer, or matter data.
It is a product walkthrough, not a benchmark lane or evidence for the benchmark
results.

Ask an MCP-capable assistant:

> Open `before.docx`. Replace exactly one occurrence of “24 months” with “36
> months” as a tracked change by “Demo Reviewer”. Preview the plan, apply it,
> verify the result, and save it as `agreement-redline.docx`.

The expected result is [`expected-redline.docx`](expected-redline.docx). Open
it in Microsoft Word to see the native deletion and insertion. Accepting all
changes produces [`accepted.txt`](accepted.txt); rejecting all changes produces
[`rejected.txt`](rejected.txt). The source remains unchanged.

For a deterministic, agent-free check of the same approved replacement, run:

```bash
python3 scripts/check-demo.py
```

That check regenerates the redline from [`worklist.json`](worklist.json),
compares its document projection and revision inventory with the checked-in
expected redline, and verifies both accept-all and reject-all results. Release
qualification separately runs the natural-language workflow and opens the
result in desktop Microsoft Word.

`source.html` is retained only to make the synthetic source text and styling
reviewable. Stemma edits existing DOCX files; it does not turn that HTML file
into a new document.
