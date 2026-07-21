# RFC 0001: `audit`, session review and document-edit certification

- Status: **implemented** (v1 includes sections 1 through 4 and `render`,
  2026-07-04; intent manifests and the disposition accept/reject split are
  v1.1; see Implementation notes)
- Date: 2026-07-04
- Scope: stemma-engine (core), stemma-mcp (surface), SKILL doctrine

## Summary

Promote verification from an internal discipline (the benchmark gates, the
validator, the receipts) to a first-class product capability, in two surfaces
that share one core:

1. **Session review** (the primary workflow): a document handle retains its
   open-time **baseline**; at any point the caller asks for a review of
   *everything this session changed*. The result contains a structured census,
   proof that everything else is untouched, and optionally a redline rendering
   of the change set. Saving is the commit gate: review, then save if it looks
   right.
2. **Stateless audit** (the same core, externalized): `audit(before, after)`
   certifies ANY pair of documents, including edits made by something that
   is not stemma, such as another tool, a human, or a raw-XML agent. This is the
   "trust layer" surface: stemma as the thing that tells you what an edit
   did, regardless of who made it.

Both produce the same `AuditReport`. The session form is sugar over the
stateless form with the baseline supplied by the handle.

## Motivation (all of it measured, none of it hypothetical)

- **The universal failure mode is "declared success without read-back."**
  Per-run forensics across two model generations (sonnet-4-6, sonnet-5,
  haiku-4-5 waves, 2026-07) found the one behavioral defect present in
  *every* wrong-result stemma-arm run: the agent verified against its live
  view and never reconstructed what a reviewer actually receives. Three f1
  runs made a byte-identical over-scoped edit and asserted success. The
  fix-side doctrine ("re-read the accept-all projection and diff against
  intent before saving") currently costs several tool calls and judgment;
  this RFC makes it ONE call with a structured answer.
- **The gates are the prototype.** Every number in `docs/benchmarks.md` is
  produced by hand-rolled Python that drives the engine to answer exactly
  this RFC's question: what changed, was it what was asked, is everything
  else intact, is the package valid. That logic is re-implemented per lane.
  Productizing it deletes the duplication and gives users the same
  instrument the suite trusts.
- **Certification is the durable value.** The 2026-07 model sweeps showed
  editing competence commoditizing from the top while verification did not:
  a model can edit a document; it cannot certify its own edit. A
  90%-correct process with silent failures still requires 100% human review.
  The product is the *deleted review step*, and that requires an auditor
  that is deterministic and model-independent.
- **The domain already has the right UX for a diff.** `compare_docx`
  (`diff_documents`) renders base→target as a tracked-changes document. The
  redline is the native, universally understood diff visualization.
  Session review reuses it unchanged.

## The design question: verb vs. transactional workflow

Considered as rivals; adopted as one core with two doors.

The transactional shape is already the library's shape. `Document::apply`
is persistent and immutable, and it returns a new document:

```text
doc = open(path)                     # baseline snapshot retained here
doc = doc.apply(step1).apply(step2)
read(doc)                            # projections work at any point
doc = doc.apply(step3).apply(step4).apply(step5)
read(doc)
report = doc.review()                # ← the new piece: baseline → now
save(doc, out_path)                  # the commit gate: save iff it looks right
```

What is genuinely new is only: (a) the handle keeps the open-time snapshot
(an `Arc` clone; snapshots are already immutable and shared, so this is
nearly free), and (b) `review()` computes the `AuditReport` between baseline
and current. `commit()` is deliberately NOT a new write primitive:
`save_docx` already is the only way bytes leave the engine, and it already
runs the output gate. "Commit" in the user-visible sense = review + save;
fusing them into one verb is rejected for v1 (see Open questions) so that
review stays pure/free and save stays unchanged.

The stateless verb falls out of the same core because the session form is
just `audit(baseline, current)`. Exposing it separately is what buys the
certification story: the auditor must not require that stemma made the
edits, or it can never certify anyone else's.

## The `AuditReport`

One structure, five sections, every claim engine-derived:

1. **Revision census delta.** These are tracked changes present in `after` but
   not in `before`, attributed by the same watermark discipline the write receipts
   already use (`max_revision_id` at baseline; enumeration via the same walk
   as `list_revisions`). Each row: `{revision_id, author, kind, block_id,
   excerpt}`. Pre-existing revisions are listed separately with their
   disposition (`untouched | accepted | rejected`), computed by presence /
   content comparison against the baseline census. Acceptance and rejection are
   distinguished by CONTENT (committed-text comparison), never by marker
   absence (marker absence alone gives false "reverted" verdicts when a
   marker is merely rewritten).
2. **Untracked (direct) delta.** These are blocks whose committed content differs
   between baseline and after without a covering tracked change, via
   `diff_documents`. In a session opened for tracked work this section being
   non-empty is itself a finding ("something changed with no redline").
3. **Untouched proof.** Every block outside sections 1 and 2 is verified
   semantically identical to baseline (block identity + `semantic_hash`,
   falling back to content comparison for hash-cleared blocks), across ALL
   story parts (body, footnotes, headers/footers, comments). This is the
   suite's input-untouched / untouched-block-fidelity invariant as an API
   guarantee: *"everything you didn't touch is provably untouched."*
4. **Package verdict.** This is the existing validator report on `after`.
5. **Intent check (optional).** A caller-supplied manifest
   (`expected: [{kind, block_hint?, must_contain?, must_not_contain?}]`)
   is diffed against sections 1 and 2. Expectations with no matching change, and
   changes matched by no expectation, are each listed. This is the
   self-verify doctrine made mechanical; v1 keeps the manifest grammar
   minimal (presence/absence needles per change), not a query language.

The report can also include an optional **rendered redline**. The
`render: {path}` option materializes sections 1 and 2 as a tracked-changes docx
via the existing `diff_documents`
path (for tracked-only sessions this is equivalent to the pending document
itself; for direct-mode or mixed sessions it is the only way to SEE the
delta as a redline).

## Wire surface (stemma-mcp)

```jsonc
// Session review. No new state model is needed because the handle exists.
review_session({ "doc_id": "doc_1", "render": null | {"path": "/tmp/review.docx"},
                 "expected": null | [...] })
  -> { "session": {"census": [...], "direct_delta": [...]},
       "preexisting": [{"revision_id": 641, "disposition": "accepted", ...}],
       "untouched": {"verified": 212, "parts": ["document","footnotes"], "violations": []},
       "validator": {"ok": true},
       "intent": {"unmet": [], "unexpected": []} }

// Stateless certification generalizes compare_docx.
audit_docx({ "before_path": "...", "after_path": "...",
             "render": ..., "expected": ... })  -> same shape
```

`compare_docx` remains (it answers "produce a redline"); `audit_docx`
answers "certify what happened" and subsumes it when `render` is set.
Library API: `Document::review(&self) -> AuditReport` (baseline captured by
`Document::parse`), `stemma::audit(before: &[u8], after: &[u8], ...)`.

## Non-goals

- Not a VCS: one baseline per handle, no history walking, no three-way
  merge. Re-opening resets the baseline.
- Not byte-identity: the canonicalization-fidelity doctrine stands
  (guide/fidelity.md); "untouched" means semantically identical under the
  engine's model, with the ratchet suite continuing to police the
  canonicalization itself.
- Not a Word replacement for comparison: `audit` certifies; Word renders.

## Alternatives considered

- **Receipts-only (event log of applies).** Rejected as the sole mechanism:
  receipts record what the engine DID, which cannot prove the absence of
  unintended effects and cannot bind changes to intent. The f1 over-scope
  runs sailed through accurate receipts. Receipts remain the per-step
  surface; audit is the end-state surface. They should agree, and a
  disagreement is a bug in one of them (good invariant, cheap to test).
- **Commit-gate that refuses to save on a dirty audit.** Rejected for v1:
  which findings are blocking is task-dependent (a failing intent check may
  be exactly what the user wants to ship). The gate was doctrine (SKILL:
  review → save), not mechanism. A later verified-delivery follow-up adopted
  a hard gate for objective delivery invariants only; it does not claim to
  infer whole-task intent. See the implementation notes below.
- **Byte diff of the packages.** Rejected: churn-vs-change is precisely the
  problem the canonical IR exists to solve.

## Rollout

1. **v1 (engine + MCP)**: baseline retention; `AuditReport` sections 1 through 4;
   `review_session` / `audit_docx`; `render`. Spec tests per section,
   including: tracked session, direct session, mixed, pre-existing-redline
   resolution attribution, a deliberately-corrupted "untouched" violation,
   receipts↔audit agreement.
2. **v1.1**: intent manifests (section 5); SKILL doctrine update
   ("review before save" as the standing rule, with a disclosed suite version
   bump); the save-time one-liner (`save_docx` returns
   `review_summary: {session_changes, untouched_verified, validator_ok}`).
3. **Dogfood**: migrate the benchmark gates' shared census/committed-text
   helpers to call `audit_docx` where equivalent. The suite becomes the
   product's first consumer, and gate-vs-engine drift becomes impossible
   by construction.
4. **Bindings**: `audit`/`review` are the first verbs exposed if/when
   Python bindings land because they are the library market's front door.

## Open questions

- Naming: `review_session` vs `diff_session` vs `pending`. "Diff" undersells
  sections 3 through 5; "audit" may read heavy for the in-session form.
  *(Resolved in v1: `review_session` for the session door, `audit_docx` for
  the stateless door.)*
- Should `save_docx` grow an optional `require: {validator_ok: true,
  untouched_ok: true}` hard gate? *(Resolved by the verified-delivery
  follow-up: MCP save always gates the objective delivery invariants before
  path creation; task-intent matching remains outside that verdict.)*
- Multi-handle audit (baseline from a DIFFERENT file than the session's
  own open, e.g. audit against the pre-negotiation base): trivially
  supported by `audit_docx`; unclear if the session form needs it.
- Whether `disposition` attribution for pre-existing revisions belongs in
  v1 or v1.1. It is the most subtle part because it contains the accept/reject
  content-distinguisher logic from the resolution gates) and the most
  valuable for the negotiation workflow. *(Resolved: v1 ships
  `untouched | modified | resolved`, judged by record identity and content;
  the accepted-vs-rejected split of `resolved` is v1.1, and the committed
  effect of each resolution is meanwhile annotated on the direct-delta rows
  it coincides with. The effect is attributed and never silently dropped.)*

## Implementation notes (v1, 2026-07-04)

What shipped: `Document::review()` (baseline captured at parse),
`stemma::audit(before, after)`, MCP `review_session` / `audit_docx` with
`render`, plus completion of the revision enumeration as a prerequisite
(headers, footers, and comments were resolvable but unlisted; an audit
census built on the old walk would have silently under-reported). Three
instrument decisions made during implementation, each forced by a test:

- **Census attribution is by record identity, never id ranges.** Export
  renumbers `w:id`, and non-stemma tools are under no obligation to
  allocate above the baseline's max id, so `id > watermark` is a
  session-side property only. Records match on
  `(story, revision_id, kind, author)` with content compared separately.
  This also makes the disposition judgment content-based rather
  than marker-absence-based.
- **The untouched proof compares under the roundtrip comparator's
  fidelity classification**, not the block guard hash (a staleness guard
  that deliberately ignores formatting and revision metadata) and not raw
  structural equality (two independent parses legitimately differ in
  importer-assigned internal node ids). Everything that is document
  content compares; parse-time addressing does not.
- **Unauditable inputs are refused, not audited around.** A quarantined
  block or an unparseable opaque carrying tracked changes holds revisions
  the census cannot see; the audit fails loudly rather than certify a
  document it cannot fully enumerate.

`RevisionKind` also became a first-class sum type (insert, delete, and the
six `*PrChange` carriers), so census rows and `list_revisions` filters name
the exact carrier instead of a collapsed "format".
