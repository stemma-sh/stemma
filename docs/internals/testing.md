# Testing

Three tiers, each catching what the previous can't:

1. **The public merge gate** (`just gate`): ~1,060 spec-conformance tests, the
   per-verb fidelity gate (reversibility, accept==direct, opaques never silently
   destroyed), a validator clean-sweep over every checked-in fixture, packaging
   and protocol checks, the supported-toolchain build, and a sentinel corpus of
   in-memory structural witnesses so no structural class regresses unseen.
   Dependency installation may require network access on a fresh clone.
2. **Corpus sweeps** (code in-repo; bulk fixtures env-var-gated, skip
   gracefully when absent): round-trip and redline fixpoint invariants over
   large real-document sets.
3. **The real-Word oracle** is held out because it drives a real Word instance
   and does not run on a public clone. Microsoft Word validates that
   outputs open clean and that Word's own accept/reject agrees with the
   engine's. Every oracle catch is ratcheted back into tier 1 as a hermetic
   sentinel, so the daily gate converges toward Word without needing Word.

The full map covers the invariants catalog, gate recipes, and environment
variables. It is documented in
[stemma-engine/docs/testing_strategy.md](https://github.com/stemma-sh/stemma/blob/main/stemma-engine/docs/testing_strategy.md).
