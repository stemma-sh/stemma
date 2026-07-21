# Apply approved changes

Use this guide when a human or upstream system has approved exact old-to-new
replacements for one specific Word document. Stemma binds the worklist to the
input bytes, applies each item explicitly, and writes a new native redline plus
a durable receipt.

The focused worklist currently targets top-level body paragraphs. Unsupported,
missing, or ambiguous changes are refused instead of guessed.

## 1. Identify the exact input

```bash
stemma validate agreement.docx
```

A successful result starts with `OK:` and includes the values needed by the
worklist. Copy the reported byte count and SHA-256. If the document changes
later, the worklist will no longer match and Stemma will refuse it.

## 2. Write the worklist

Save this as `changes.json`, replacing the sample identity with the values from
your document:

```json
{
  "schema": "stemma.worklist.v0",
  "input": {
    "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    "bytes": 48271
  },
  "author": "Approved Reviewer",
  "changes": [
    {
      "id": "payment-term",
      "old": "Payment is due within 30 days.",
      "new": "Payment is due within 45 days.",
      "expected_matches": 1
    }
  ]
}
```

Each item names the intended old text, its replacement, and the number of
matches that must exist. Item ids must be unique.

## 3. Apply it

```bash
stemma apply agreement.docx \
  --worklist changes.json \
  -o agreement-redline.docx
```

On complete success, Stemma:

- exits `0`;
- creates `agreement-redline.docx`;
- creates `agreement-redline.docx.receipt.json`;
- returns the same JSON receipt on stdout;
- leaves `agreement.docx` unchanged.

The decisive receipt fields look like this:

```json
{
  "status": "complete",
  "deliverable": true,
  "summary": {
    "total": 1,
    "applied": 1,
    "refused": 0
  }
}
```

The complete receipt also records exact artifact hashes, every item outcome,
and diagnoses for refused changes. Treat it as document-sensitive metadata.

## If an item is refused

If any item refuses, the receipt status is `partial`, `deliverable` is `false`,
and the process exits `3`. By default no DOCX is created. Read the item outcome,
correct the worklist or input, and run again with a new output path.

Do not treat exit `3`, a partial receipt, or `--emit-partial` output as a
completed deliverable.

See [Troubleshooting](../help/troubleshooting.md) for common refusal paths and
the [CLI reference](../reference/cli.md#apply) for the complete worklist,
receipt, and exit-code contract.
