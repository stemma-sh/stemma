# CLAUDE.md

Working conventions for this repository — for humans and coding agents alike.
Source comments throughout the codebase cite these rules by name; this file is
what they refer to. [CONTRIBUTING.md](CONTRIBUTING.md) has the full setup and
PR expectations; [docs/internals/architecture.md](docs/internals/architecture.md)
is the codemap.

## Build & test

```bash
mise install     # rust (>= 1.91), just, python 3.11 — or install them yourself
just gate        # clippy (-D warnings) + full test suite; the merge gate (~8 min)
```

## Prime directive: no silent fallbacks

If input is missing or invalid, decoding fails, or an invariant breaks: return
a clear, contextual error (or fail loud at the right place) and stop. Never
"best-effort" through an unknown state — continuing in one *is* the bug.

Ruled out: decode-failure → empty default; unknown enum values mapped to a
catch-all; `catch { return ok }`; log-and-continue where continuing isn't
provably safe. A default that is part of a contract must be documented,
visibly deliberate, and test-covered. A *named* isolation boundary that
contains a failure and reports it honestly is fine; absorbing a failure into
apparent success is not.

## Model-first

Start from the data: what are the shapes, the allowed states, the allowed
transitions? Parse and validate at the edges (wire, file, CLI); the core
operates on already-validated domain types. Make invalid states hard to
represent. When something breaks, fix where the data first went wrong — not
where the failure became visible — and never add downstream tolerance for bad
input our own pipeline produced.

## Tests encode the domain, not the implementation

A test's expected value must be justifiable from the domain rule (for this
repo, that is usually ECMA-376/ISO-29500 semantics or observed real-Word
behavior) — never from what the code currently returns. When fixing a bug:
correct the test first, then the code. Characterization tests are allowed but
must be labeled as such.

## Simplicity

No speculative abstraction, no pattern-by-default, generics only with at least
two real call sites today. Prefer duplication over premature abstraction.
Errors are actionable: what failed, why, and the identifiers needed to debug.
