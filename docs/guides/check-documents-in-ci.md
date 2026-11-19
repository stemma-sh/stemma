# Check Word documents in CI

Two commands answer two questions about a `.docx` without opening Word:

- `stemma validate <file>`: will Word open this file without repairing it?
- `stemma verify <before> <after>`: is everything that changed between
  these two files a tracked change, with the rest left as it was?

Both print a JSON result and use the same exit codes, so a pipeline can
branch on the code and keep the JSON as the record.

| Exit | `validate` | `verify` |
| --- | --- | --- |
| `0` | `sound` | `pass` |
| `3` | `corrupt` | `fail` |
| `4` | `cannot_assess` | `cannot_assess` |
| `1` | operational error (for example, the file is missing) | the same |

`cannot_assess` is not a pass. It means Stemma found no fault but met
something it does not read, so it gives no verdict. Decide in your pipeline
whether that blocks; the recipes below block on it.

## Install

```sh
cargo install stemma-cli
```

The binary is named `stemma`.

## Gate every document in a repository

Fail the job when any tracked `.docx` is corrupt or cannot be assessed, and
keep each result as an artifact.

```sh
#!/usr/bin/env bash
set -u
mkdir -p stemma-results
failed=0
while IFS= read -r -d '' file; do
  result="stemma-results/$(printf '%s' "$file" | tr '/ ' '__').json"
  stemma validate "$file" --format json > "$result"
  code=$?
  case "$code" in
    0) ;;
    3) echo "corrupt: $file"; failed=1 ;;
    4) echo "cannot assess: $file"; failed=1 ;;
    *) echo "error ($code): $file"; failed=1 ;;
  esac
done < <(git ls-files -z '*.docx')
exit "$failed"
```

The result names the file by its SHA-256 and says what is wrong. For a file
that is not a ZIP archive at all:

```json
{
  "schema": "stemma.validate.v1",
  "verdict": "corrupt",
  "input": {
    "role": "input_docx",
    "supplied_path": "contract.docx",
    "resolved_path": "/work/contract.docx",
    "digest": { "algorithm": "sha256", "hex": "a39891…" },
    "bytes": 9
  },
  "block_count": null,
  "pending_revision_count": null,
  "issues": [
    {
      "code": "PackageInvariant",
      "message": "docx read failed: invalid Zip archive: Invalid zip header",
      "context": null
    }
  ],
  "cannot_assess": null
}
```

## Check that an edit is a clean redline

When a tool or an agent edits a document, compare its output with the
input. `verify` passes only when the output is valid, every change in it is
a tracked change, everything outside the changes reads and looks as it did,
and the revisions that were already pending are still pending.

```sh
stemma verify contract.docx contract.redline.docx > verify.json
case $? in
  0) echo "clean redline" ;;
  3) echo "not a clean redline:"; jq -r '.reasons[]' verify.json; exit 1 ;;
  4) echo "could not compare:"; jq -r '.cannot_assess' verify.json; exit 1 ;;
  *) echo "verify did not run"; exit 1 ;;
esac
```

`reasons` lists every way the result failed:

| Reason | Meaning |
| --- | --- |
| `invalid_result` | The output is not a sound document. |
| `untracked_change` | Text or formatting changed without a tracked change. |
| `untouched_content_changed` | A part of the document no change covers is different. |
| `preexisting_revision_changed` | A revision that was pending before was resolved or altered. |

The default policy compares what the document says and how it looks, so an
output saved by another program passes when that program only rewrote
bookkeeping (save ids, run boundaries, attribute order). To require the
same markup, add `--policy tracked-delivery-v0`.

## In a hosted pipeline

Save the script above as `scripts/check-docx.sh`. Any runner with a Rust
toolchain can then gate a pull request in two commands, and the job fails
when the script exits non-zero:

```sh
cargo install stemma-cli --locked
bash scripts/check-docx.sh
```

Upload the `stemma-results` directory as a build artifact to keep the
verdict for every file next to the commit it was made on.

## What the verdicts are measured against

The reference is Microsoft Word: a document is sound when Word opens it
without offering to repair it. The rules behind the verdict come from
Word's observed behaviour on real documents, on other tools' output and on
deliberately damaged files, and each has a test in this repository.
