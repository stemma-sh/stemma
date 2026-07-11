# Testing

Three tiers, each catching what the previous can't:

1. **The hermetic daily gate** (`just gate` — runs on any clone, no network):
   ~1,060 spec-conformance tests, the per-verb fidelity gate (reversibility,
   accept==direct, opaques never silently destroyed), a validator clean-sweep
   over every checked-in fixture, and a sentinel corpus of in-memory
   structural witnesses so no structural class regresses unseen.
2. **Corpus sweeps** (code in-repo; bulk fixtures env-var-gated, skip
   gracefully when absent): round-trip and redline fixpoint invariants over
   large real-document sets.
3. **The real-Word oracle** (held out — it drives a real Word instance and
   does not run on a public clone): real Microsoft Word validates that
   outputs open clean and that Word's own accept/reject agrees with the
   engine's. Every oracle catch is ratcheted back into tier 1 as a hermetic
   sentinel, so the daily gate converges toward Word without needing Word.

The full map — invariants catalog, gate recipes, environment variables —
is [stemma-engine/docs/testing_strategy.md](../../stemma-engine/docs/testing_strategy.md).
