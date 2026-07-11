# Design notes

Decision records for shipped designs — the "why it is this way" a future
contributor needs, without the draft-negotiation history.

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

## Policy

New design work lands here as a decision record once shipped.
