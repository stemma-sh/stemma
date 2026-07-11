# Stemma — Summary

A standalone overview of what stemma is, what it does, and why it is shaped the way it is. This is the front door. For depth, read the two companion documents:

- **`domain-model.md`** — the canonical, engineer-facing model: the core object, the invariants, the public surface, the crate split. Authoritative where anything overlaps.
- **`user/guide.md`** — a one-page tour of the `Document` API for someone who just wants to use it.

---

## What stemma is

**Stemma is a headless Rust engine for working with documents that carry attributed change.** It parses a Word document (`.docx`) into a typed in-memory model, lets you discover or author changes, materializes those changes as valid tracked-change OOXML, and proves the result is valid before it leaves the engine.

The name is from textual criticism: a *stemma* is the tree of how a text changed across copies. That is the domain. Stemma is not "a DOCX reader." It is a model of **the structure of change in a document**.

The useful mental model is a **compiler whose source language is a `.docx` and whose target language is also a `.docx`**, with a typed intermediate representation in between. It can read a document, hand the IR to a transformation pass (diff, edit, accept-all, reject-all), and emit a new, valid document from the result.

Stemma is a self-contained library with a small public API. It has no network, no database, no UI, and no opinion about how it is deployed.

---

## Why it exists

A `.docx` is a ZIP of XML. The tooling landscape around it splits into two unsatisfying camps:

- **Treat it as opaque bytes or flattened text.** You lose all structure and cannot write changes back without corrupting the file.
- **Treat it as raw XML to string-edit.** Fragile, and one wrong edit breaks the document.

And almost every existing DOCX library treats **tracked changes** (`w:ins`, `w:del`, `w:moveFrom`/`w:moveTo`, `w:pPrChange`) as either invisible or as raw XML to copy past. That is exactly the hard and valuable part. Tracked changes are how negotiation, review, and authorship-with-attribution actually happen in Word documents, and nothing models them as first-class data.

Stemma is built around that gap. Two properties make it a real engine rather than a glorified XML serializer:

1. **Tracked changes are part of the type system.** They are not a side overlay. The model represents them at three granularities (whole block, inline segment, the paragraph mark itself), preserves authorship and revision metadata, supports moves and tracked formatting changes, and every transformation pass (diff, edit, normalize, serialize, validate) understands them. At edit time you choose whether to emit native tracked revisions or to apply the change directly.
2. **Opaque preservation, with no silent loss.** Anything stemma does not semantically model (equations, drawings, embedded objects, content controls, complex fields, unusual footnotes) round-trips byte-faithfully: the IR carries the raw XML plus a content hash and the serializer re-emits it unchanged. An edit to text in a paragraph next to an equation cannot destroy the equation. An edit that *would* destroy an opaque anchor fails with a named error listing every missing anchor, rather than dropping it.

---

## The core object

A normal document model answers "what does the document say." Stemma answers a harder question: **what does the document say, what did it say before, and who changed it.**

A tracked-changes DOCX already encodes exactly that. Inside one file, two projections are superimposed: **reject-all** (the baseline, before this round of changes) and **accept-all** (the target, if every change is accepted). Between them sit **attributed deltas**: each insertion, deletion, move, and formatting change, tagged with author and date.

That triple, *baseline + target + attributed deltas held in one structure*, is the core object. Internally it is `CanonDoc`: a tree of tracked blocks (paragraphs, tables, opaque blocks), each wrapped in a tracking status, with deltas at block, segment, and mark granularity. A pristine document is just this object with zero deltas. Everything stemma does is a function over this object.

---

## What it can do

Six capabilities, all over the same model:

- **Parse.** DOCX bytes to typed IR. Namespace-aware across the Microsoft, DrawingML, VML, and math families; honors Markup Compatibility; pre-resolves the style cascade and synthesizes rendered numbering text. Hard safety gates on ZIP size, decompressed size, and XML depth, with path-traversal and encrypted-package defenses. Fails loudly on unknown structural elements rather than guessing.
- **Diff.** Compare two clean documents into a structured diff: block-level changes plus word-granularity inline changes, with move detection, paragraph split/join detection, and recursive table-cell diffing. A run that merely became bold is one formatting change, not a delete-plus-insert.
- **Extract.** Read the changes out of a single already-redlined document as data (`Normal + Deleted` reproduces the base; `Normal + Inserted` reproduces the target).
- **Edit.** Apply a typed `EditTransaction`: an ordered, atomic list of steps (replace paragraph text, insert/delete/replace/move block ranges, change a paragraph role, replace a hyperlink or table, and more). Every step carries an `expect` precondition, so a stale or mis-targeted edit fails before mutating anything. Either every step applies or none do.
- **Project.** Collapse tracked changes into a clean tree: accept-all, reject-all, or resolve a selected subset.
- **Serialize and validate.** Emit a new DOCX, then re-parse the output bytes and check roughly 20 codified invariants from ECMA-376 / ISO 29500 / MS-OI29500 (package integrity, relationship correctness, ECMA-376 Annex A element ordering, cross-part references, tracked-change well-formedness) before the bytes leave the engine.

The roundtrip guarantee is **structural-canonical equivalence**, not byte-equality: `parse(serialize(parse(A)))` equals `parse(A)` under the canonical comparator, while unmodeled (opaque) content is re-emitted byte-for-byte.

---

## How it is shaped

The persistence contract is the spine. Stemma divides everything into **durable** and **ephemeral**:

| Concept | Stemma type | Durability |
|---|---|---|
| source document | DOCX bytes | **durable**, the only authoritative artifact |
| parsed model (IR) | `CanonDoc` | ephemeral, engine-version-bound |
| compilation unit | `EditSnapshot` (IR + unmodeled OOXML parts) | ephemeral |
| edit spec | `EditTransaction` | **durable**, a small JSON replay log |
| diff / apply output | derived | ephemeral |

Store the **DOCX bytes plus the edit transactions**; everything else is re-derivable on cold start. The IR is never persisted, so storage is never coupled to an engine version.

The public surface is intentionally small: a `Document` handle, a read-only `DocumentView` projection for inspection, the durable value types, and a handful of verbs (`parse`, `read`, `diff`, `apply`, `project`, `serialize`, `check`). The IR itself stays private by design, because exposing it would freeze it. An optional in-memory `SimpleRuntime` provides handle-keyed session management on top, for callers that want it.

The guiding philosophy throughout is **no silent fallbacks**: if input is invalid, an invariant breaks, or an edit cannot be applied safely, stemma returns a clear, typed, actionable error instead of best-effort-ing into an unknown state. Documented limitations live almost entirely on the *input* side (a small set of real-world documents stemma refuses to import); the output side is solid.

---

## Maturity

The maturity story is real and is the point of the engine. Roughly 80,000 lines of Rust. About 1,060 spec-compliance tests run on every change, each tied to a behavioral constraint from ECMA-376 / ISO 29500 / MS-OI29500, with only two intentionally disabled (both for documented, non-gap reasons). Tests run in two tiers: a fast **daily** tier that must always pass, and a **nightly** tier that adds large corpus sweeps, fuzzing, and Docker-based fidelity runs. A post-serialization validator enforces the OOXML invariants at the output boundary. An optional export hook (`ExportValidator`) lets a caller gate every emitted DOCX through their own external Microsoft Word automation check before the bytes are returned, because structurally-correct OOXML can still trip Word's repair dialog, and a redline that does not open clean in Word is a non-negotiable failure.

---

## Using stemma from an agent: stemma-mcp

`stemma-mcp` is a [Model Context Protocol](https://modelcontextprotocol.io) server that exposes the engine to agents over stdio. It is how a coding agent such as Claude Code can edit a real Word document without corrupting it: instead of unzipping and string-editing XML (fragile) or flattening to text (lossy and write-only-once), the agent drives the same typed, fail-loud engine the rest of stemma uses.

It exposes the engine as a handful of tools: open a `.docx` and get a stable, id-bearing outline; read it as honest "extended markdown" (reads like a contract, but every block carries its id and tracked changes show as `<ins>`/`<del>`); inspect a single block's spans; find a phrase; apply a typed edit transaction as atomic tracked changes (with the same `expect` precondition guarding against stale edits); save; and compare two files into a redline. Install it with `claude mcp add stemma -- /path/to/stemma-mcp` and ask the agent to open a document, inspect it, and make changes. See `stemma-mcp/README.md`.

The point of stemma-mcp is that the engine's structure-aware, tracked-change editing is reusable well beyond any one product, including directly by agents.
