# stemma-artifacts

`stemma-artifacts` is Stemma's shared host-filesystem boundary. The DOCX engine
accepts and returns bytes; this crate gives CLI and agent transports one policy
for path authority, exact SHA-256 identities, protected inputs, size-bounded
reads, and staged create-new output commits.

This is lockstep Stemma infrastructure, not an independently evolving product
or storage SDK. Keep only cross-transport host-boundary semantics here; DOCX
parsing, transformation, policy, and transport behavior belong elsewhere. If
the boundary stops being shared by multiple shipping transports, fold it back
rather than preserving a crate boundary for its own sake.

Use `PathAuthority::rooted` for agent-controlled paths and
`PathAuthority::explicit` for paths explicitly supplied to a human-invoked
CLI. Successful output includes the exact committed byte length and digest plus
`collision_policy=create_new` and `disposition=created`.

The boundary prevents ordinary path escapes, input aliases, output collisions,
and partial-success reporting. It is not an operating-system sandbox against a
hostile same-user process, a storage-integrity guarantee, or a power-loss
durability promise. Supplied and resolved identity paths must be valid UTF-8 so
receipts can represent them exactly rather than lossily. Non-regular sources
are rejected before open and checked again after open. Windows alternate data
stream syntax is outside the portable contract on every platform and is
refused before read or staging.

The public filesystem contract is documented in the
[MCP reference](https://github.com/stemma-sh/stemma/blob/main/docs/reference/mcp.md#filesystem-and-artifact-boundary).
