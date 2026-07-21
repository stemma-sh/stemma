# Verify a multi-document task delivery

Use a task manifest when one instruction produces several Word files and the
delivery is correct only if every declared replacement appears in its declared
target. The MCP server fixes the complete declaration before the first task
mutation, binds every input by SHA-256, and writes one create-once manifest when
the task terminates complete or partial. The CLI can verify that manifest later
from the files alone.

This is evidence-carrying delivery, not a signature or a proof of human intent.
The verifier checks that the manifest agrees with the named bytes and revision
identities. It cannot authenticate the producer, prove when a declaration was
made, or detect an effect the caller omitted from the declaration.

## 1. Inspect before declaring

Ordinary, non-task `open_docx` and `inspect_docx` calls are permitted before a
declaration. Use them to discover exact wording, block IDs, and values that must
be carried from a read-only source into a target. Do not mutate a target during
this preflight.

## 2. Declare the whole task on the first task open

The first task-bearing `open_docx` includes every read-only input, every target,
and every effect. Schema v1 accepts exact-count tracked replacements only.

```json
{
  "path": "agreement.docx",
  "task": {
    "task_id": "delivery-7",
    "manifest_path": "delivery/task.json",
    "inputs": [
      {"path": "intake.docx"}
    ],
    "targets": [
      {
        "path": "agreement.docx",
        "effects": [
          {
            "effect_id": "payment-term",
            "op": "replace_text",
            "find": "Payment is due within 30 days.",
            "replace": "Payment is due within 45 days.",
            "match_mode": "exact",
            "scope": {},
            "expected_matches": 1,
            "on_barrier_match": "skip"
          }
        ]
      },
      {
        "path": "schedule.docx",
        "effects": [
          {
            "effect_id": "schedule-term",
            "op": "replace_text",
            "find": "Net 30",
            "replace": "Net 45",
            "match_mode": "exact",
            "scope": {},
            "expected_matches": 1,
            "on_barrier_match": "skip"
          }
        ]
      }
    ]
  }
}
```

Stemma hashes every declared path synchronously before returning. A later target
is opened with its fixed task ID:

```json
{"path":"schedule.docx","task_id":"delivery-7"}
```

That call refuses if the file no longer matches its declaration-time identity.
The task cannot be extended or restated.

## 3. Execute declaration-matched effects

Task-bound mutation goes through `execute_plan` with a
`replacement_worklist`. Every item must name its one declared `effect_id`, and
all replacement fields must match the declaration exactly.

```json
{
  "doc_id": "<doc_id>",
  "replacement_worklist": {
    "author": "Approved Reviewer",
    "replacements": [
      {
        "effect_id": "payment-term",
        "old": "Payment is due within 30 days.",
        "new": "Payment is due within 45 days.",
        "match_mode": "exact",
        "expected_matches": 1,
        "on_barrier_match": "skip"
      }
    ]
  },
  "preview": true,
  "allow_existing_author": false
}
```

Preview is non-mutating and satisfies nothing. Apply the same plan with
`preview:false`. A mismatched, unknown, repeated, or already-applied effect ID
is refused before mutation. Transactions, direct edits, revision resolutions,
and comparisons are not identity-decidable per effect in schema v1, so task
sessions refuse those shapes explicitly; they remain available outside a task.

## 4. Save every target

Save each target to a distinct new path. An earlier target save returns task
status `executing` and `deliverable:false`; it is a committed document, not a
complete task delivery. The save that commits the last target performs the
join and writes the manifest:

- every target committed and every effect identity present: task `complete`,
  successful final save;
- any effect missing or unverifiable: task `partial`, manifest written, final
  save returns a `task_partial` error naming every unsatisfied effect;
- a later output write fails after an earlier output is visible: task
  `partial`, manifest records only committed outputs and claims no identity for
  the failed target;
- the task is abandoned: no manifest, therefore no delivery.

The manifest itself is create-once. Choose a path that does not exist and do
not edit it after creation.

## 5. Verify later from files alone

Keep the manifest, all target inputs and outputs, and every read-only input it
names. Paths in the manifest are relative to its directory:

```bash
stemma verify-task delivery/task.json
```

If the files were relocated separately from the manifest, supply their common
directory:

```bash
stemma verify-task copied/task.json --root copied/artifacts
```

Exit codes are deliberately distinct:

| Exit | Meaning |
|---:|---|
| `0` | Verified complete. |
| `1` | Verified partial: the manifest is consistent, but the task is incomplete. |
| `2` | Verification mismatch: a file hash, audit claim, or revision identity disagrees. |
| `3` | Usage, I/O, malformed JSON, or unknown manifest schema. |

The verifier never treats an unknown schema as a best-effort older version.
