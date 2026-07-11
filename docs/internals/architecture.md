# Architecture

A codemap for contributors. One pipeline, four crates:

```
DOCX bytes -> import -> CanonDoc -> edit / diff -> apply -> serialize -> DOCX bytes
             (stemma-engine)                                  ^ linter gates output
```

- **stemma-engine** — everything above. `stemma-mcp` (agents, stdio) and
  `stemma-api` (HTTP) are thin transports over the same verbs;
  `stemma-examples` is a browser review editor on the HTTP API.

Inside the engine, the load-bearing ideas:

- **`CanonDoc`** (`domain/`): ordered `TrackedBlock`s (paragraph / table /
  opaque), each block a tree of tracked segments and inline nodes with
  stable ids. Non-body stories (footnotes, headers, comments) are parallel
  collections on the same document.
- **Preserved remainder**: any pPr/rPr/inner-XML child the IR has no typed
  field for is captured verbatim (`PreservedProp`) and re-emitted at its
  schema position — unmodeled ≠ dropped. The typed fields and the preserved
  bag partition each property space; growing the model means moving an
  element from the bag to a field on both the parse and serialize sides.
- **Revision model** (`tracked_model.rs`): every tracked-change kind carries
  what reject must restore (formatting changes snapshot the complete prior
  properties). `enumerate_revisions` is the single source both reads and
  selective resolution address — if it can't see a revision, nothing can
  resolve it.
- **Edit layer** (`edit/`, `edit_v4.rs`): the v4 wire grammar parses at the
  edge into typed `EditStep`s; verbs validate preconditions (expect /
  `semantic_hash` guards), apply in canonical space, and are proven by the
  per-verb fidelity gate (reversibility, accept==direct, opaque inventory
  never shrinks).
- **Serializer + linker** (`serialize/`, `docx_validate*`): rebuilds parts
  from the IR, then a post-serialization OOXML linker (content types,
  ordering, annotation pairing, cross-refs) gates the bytes. Byte identity
  is explicitly not a goal — see the
  [fidelity contract](../guide/fidelity.md).

Deeper dives live next to the code: `stemma-engine/docs/` (invariants
catalog, testing strategy) and per-module `AGENTS.md` files.
