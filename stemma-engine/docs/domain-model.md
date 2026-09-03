# Stemma — The Domain Model

This is the canonical description of the engine model. Read it before changing
the public API, native tracked-change carriers, package operations, or crate
boundaries. For a task-oriented introduction, see the
[user guide](user/guide.md).

## 1. What the engine is

Stemma is a typed engine for Word documents that carry attributed change. It
parses DOCX into a canonical document and package model, applies explicit edits,
materializes native Word tracked-change carriers, resolves them, and refuses to
emit bytes that violate its validation contract.

Document comparison is a downstream product built on this engine. The engine
does not infer how two independently authored documents correspond.

## 2. The core object

A tracked Word document contains two readings in one physical package:

- **Reject** reconstructs the state before the pending changes.
- **Accept** reconstructs the state after the pending changes.

Between those readings are attributed insertions, deletions, moves, property
changes, and structural changes. Internally, an `EditSnapshot` holds:

```text
canonical tracked document
+ package scaffold and relationships
+ validation and review state
```

`CanonDoc` is the typed working model. `Document` is the supported facade.
DOCX bytes are the durable artifact; snapshots and canonical IDs are
engine-version-bound working state.

## 3. Engine responsibilities

The engine owns behavior that remains useful when a caller explicitly requests
an edit:

- lossless package parsing and accepted-reading normalization;
- the canonical content, story, table, section, and revision models;
- explicit edit transactions and their preconditions;
- native Word carriers for text, properties, moves, tables, sections, fields,
  equations, content controls, comments, and notes;
- relationship-closure import and collision-safe transport remapping;
- content-type and package closure;
- Accept, Reject, and selective resolution;
- audit and untouched-state verification;
- serialization and validation.

The engine does **not** own:

- block alignment between two documents;
- similarity-based pairing;
- split, merge, move, or replacement inference;
- comparison presentation and revision-envelope choices;
- benchmark capability censuses or comparison research plans.

That boundary is compile-time enforced: `stemma-diff` depends on `stemma`; the
engine has no production dependency back to comparison.

## 4. The engine flow

Every engine-authored edit follows one direction:

```text
DOCX bytes
  -> parse and disclose supported normalization
  -> EditSnapshot
  -> typed EditTransaction
  -> native tracked document state
  -> package materialization
  -> serialize
  -> validate and reparse
  -> DOCX bytes
```

Raw XML is parsed at the package edge. Core transformations operate on typed,
validated state. An unsupported or ambiguous carrier is an error, never a cue
to switch silently to a weaker representation.

## 5. Authored and discovered change

There are two producers of a tracked document:

- `Document::apply` authors a change the caller explicitly described.
- `stemma_diff::diff` discovers a relationship between two accepted documents.

They intentionally do not share an input grammar. `EditTransaction` stays a
small, durable authoring vocabulary; a whole-document comparison plan may be
larger and contain correspondence that no explicit edit request needs.

They do share the engine's model and Word laws. Comparison may choose which
safe carrier to use, but it may not redefine what that carrier means on Accept,
Reject, selective resolution, serialization, or package closure.

## 6. Native operations are the correctness seam

The load-bearing invariant is not “all callers use one diff algorithm.” It is:

> Each native operation has one engine-owned semantic law, and every caller is
> verified against the same Accept, Reject, package, and serialization rules.

Examples include:

```text
insert or delete exact content
change run, paragraph, table, or section properties
move a range
switch an explicit reference
import a closed relationship graph
remap a package-local transport identity
retain proved-passive source state
```

The local before/after compiler in `local_change` is an engine utility for a
region the caller already selected. It may tokenize and produce native inline
changes, but it never aligns independent documents or creates semantic lineage.

The comparison crate owns its whole-document change model and orchestration. It
lowers that model into engine state, then uses engine serialization, resolution,
package, and verification behavior. If comparison needs a new Word carrier,
the reusable carrier law belongs in the engine; the rule choosing that carrier
belongs downstream.

## 7. Public surface

The supported engine facade is deliberately small:

| Operation | Meaning |
|---|---|
| `Document::parse` | Parse DOCX bytes into editable state; refuse unsafe input and disclose any supported normalization. |
| `Document::diagnostics` | Read immutable import provenance and normalization warnings. |
| `Document::read` | Inspect a stable read projection. |
| `Document::check` | Validate an edit without mutation. |
| `Document::apply` | Author an explicit edit. |
| `Document::project` | Accept, reject, or selectively resolve revisions. |
| `Document::revisions` | Enumerate every resolvable pending revision. |
| `Document::review` | Audit changes against the retained baseline. |
| `Document::serialize` | Emit validated DOCX bytes. |
| `validate` | Validate bytes without opening a `Document`. |

Comparison is exposed by the separate `stemma-diff` facade:

```rust,ignore
let redline = stemma_diff::diff(&base, &target)?;
let attributed = stemma_diff::diff_as(&base, &target, "Reviewer")?;
```

There is one Stemma comparison and one Stemma presentation. Internal alignment,
proof, package-plan, and capability types are not product vocabulary.

## 8. Crate boundaries

```text
stemma-diff  --->  stemma
stemma       -X->  stemma-diff
```

- **`stemma`** owns the Word-compatible document engine.
- **`stemma-diff`** owns comparison inference and presentation and compiles its
  result through the engine.
- Product transports such as the CLI, HTTP API, and MCP server depend on the
  facade appropriate to the operation they expose.

The typed IR is available as an unstable integration surface for workspace and
downstream compiler code. It is not a durable storage format and must not be
confused with the supported `Document` facade.

## 9. Named invariants

- **No silent fallback.** Unknown or unrepresentable state produces a contextual
  error or typed refusal.
- **Disclosed normalization.** A bounded, Word-compatible import correction is
  observable through `Document::diagnostics`; exact raw-byte validation remains
  a separate question.
- **Accepted-input comparison.** Comparison consumes each input's accepted
  reading; pre-existing pending revisions are flattened and disclosed.
- **No invented lineage.** Similarity may guide presentation only after semantic
  correspondence is established; ambiguity stays explicit.
- **Accept/Reject law.** Every emitted native carrier has defined and tested
  terminal projections.
- **Enumerable revisions.** A serialized revision that the read model cannot
  enumerate is invalid because selective resolution could not address it.
- **Closed package.** Every internal relationship resolves and every part has a
  content type.
- **Complete transport rewrite.** A remapped package identity is valid only when
  every reference is rewritten.
- **Opaque preservation.** An edit that would drop an unmodeled object or anchor
  refuses with context.
- **Directness preservation.** Authored direct properties remain distinct from
  inherited effective properties.
- **Tri-state properties.** Absent, enabled, and explicitly disabled are distinct
  where Word distinguishes them.
- **Native Word semantics.** In-memory projection is not evidence when native
  Word interprets a carrier differently.
- **Validated output.** Bytes do not leave the engine as successful output until
  the configured post-serialization gate passes.

## 10. Tests as postconditions

Engine tests should state Word or model laws: the requested edit is exact,
Reject restores the prior state, Accept keeps the requested state, untouched
objects remain untouched, and the serialized package is valid.

Comparison tests should state inference and presentation laws: correspondence is
honest, unrelated regions are not force-paired, the chosen presentation is
reviewable, and the resulting document still satisfies the engine postconditions.

A benchmark score can reveal a candidate mechanism. It cannot redefine any of
the invariants above.
