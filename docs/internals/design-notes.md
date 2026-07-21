# Design notes

These decision records explain why shipped designs work as they do. They keep
the reasoning a future contributor needs without preserving draft negotiation
history.

## Audit / session review (RFC 0001, shipped)

The full document: [RFC 0001](../rfcs/0001-audit-and-session-review.md).
The decisions:

- **One core, two surfaces.** `review_session` (a handle's open-time
  baseline vs now) and `audit_docx` (any two files) produce the same report.
  The stateless form exists so stemma can certify edits it didn't make.
- **Receipts-only was rejected** as the verification mechanism: receipts
  record what the engine did, which cannot prove the *absence* of unintended
  change, and cannot bind changes to intent. Receipts remain the per-step
  surface; audit is the end-state surface; they must agree.
- **Byte diff was rejected**: churn-vs-change is the problem the canonical
  IR exists to solve (see the [fidelity contract](../guide/fidelity.md)).
- **A hard commit-gate was deferred**: which audit findings are blocking is
  task-dependent, so review-then-save is doctrine, not mechanism. Revisit
  with usage data.
- **Accept-vs-reject attribution is content-based.** Both resolutions remove
  the marker; only content distinguishes them. Anything that grades or
  reports resolution state must compare text, never marker presence.

### Verified-delivery follow-up

The common MCP save path now hard-gates the objective delivery invariants:
unexplained committed delta, changed prior revisions without successful typed
session resolution evidence, untouched-scope violations, and new validator
issues. This does not claim to infer the caller's whole task intent.

A successful accept/reject command records its exact selected identities plus
an independently audited before/after committed-content transition batch.
Final review replays those batches in order. A direct mutation before, between,
or after them breaks the chain and remains unexplained; a block-id allowlist
was rejected because it would let later mutations inherit an earlier
resolution's exemption. Stateless audit remains conservative because a file
pair contains no session command evidence.

## Policy

New design work lands here as a decision record once shipped.
