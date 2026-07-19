# Historical Candidate A evaluation record

> Historical status: Candidate A was an unpublished v0.1.0 pre-release build.
> It is not the v0.2.0 release candidate and is not a reachable download. Its
> identities and walkthrough remain here as a record of the earlier evaluation
> preparation.

Stemma's current experimental workflow takes one exact DOCX and a short,
approved old-to-new worklist. It either creates a new Word-native redline plus
an authoritative receipt or explicitly refuses items without guessing.

The boundary is deliberately narrow: exact matching in top-level body
paragraphs, one expected match per item, and new output paths. Top-level table
matches are detected and refused. Nested tables, headers, footers, notes,
comments, and textboxes are unsearched. This is not a general Word-automation or
supported-install claim.

## Candidate A identity

| Field | Value |
|---|---|
| Source | `58a73e7e4576ac41ca906e946343c1212cc6fd52` |
| Target | Linux x86_64 GNU / `x86_64-unknown-linux-gnu` |
| Build stamp | `0.1.0+g58a73e7.candidate-a` |
| Executable bytes | `9,317,160` |
| Executable SHA-256 | `4ea2484f1aa5d24f8c4461fd62ecdc3f3aefdde61d70e03f596f88b6a233dd7e` |
| Distribution manifest SHA-256 | `1f3f652ee1e9836f22da4f729f817980e6d5be9ff9fab0176ba842f17db2ae56` |
| Distribution lock SHA-256 | `b44641abc27ad9b6ea08e37218dc52931bef38a04455cea1871cc8006b4572ed` |
| Prepared archive bytes | `3,568,883` |
| Prepared archive SHA-256 | `5e7e5dfd083b3fdb573ed885d0fb1c9b4411dc9d1a83c1e8df0884cda908ceae` |

The planned evaluation asset name was
`stemma-candidate-a-linux-x86_64-gnu.tar.gz`. Its archive identity will be
preserved above, but the asset was not published. Do not use this historical
identity to qualify a later release.

## Verify and smoke

The planned verification sequence for that unpublished candidate was:

```bash
sha256sum distribution-lock.json distribution-manifest.json stemma
./stemma --version
./scripts/demo-approved-worklist.sh
```

The first two hashes must match the table above, and `stemma` must be exactly
9,317,160 bytes with the listed executable hash. The smoke shows the synthetic
input and worklist, produces and validates a complete redline, summarizes its
receipt, then demonstrates a safe refusal whose receipt exists while its DOCX
does not. The same walkthrough is documented in the
[60-second demo](demo-approved-worklist.md).

For a real evaluation, create a 3-10 item worklist bound to the exact input
identity printed by `stemma validate INPUT.docx`, then use a fresh output path:

```bash
./stemma apply INPUT.docx --worklist worklist.json \
  -o redline.docx --receipt redline.receipt.json
./stemma validate redline.docx
```

Exit `0` is eligible for delivery only when the redline and receipt exist and
their size/hash tuple agrees. Exit `3` is a partial safe refusal, not a
completion; by default it creates no DOCX. Receipts contain paths, hashes,
excerpts, and diagnoses, so handle them as document-sensitive metadata.

## Report an evaluation problem

Use the evaluation-report issue template once repository issue creation is
enabled. Include the source SHA, target, build stamp, executable size/hash,
process exit, output/receipt presence, and the content-safe receipt fields the
template requests. Do not attach a real agreement, worklist, receipt, path,
clause excerpt, or document hash. If a minimized synthetic DOCX is necessary,
confirm it contains no confidential material before attaching it.

Security issues belong in the repository's [security process](../SECURITY.md),
not a public evaluation issue.
