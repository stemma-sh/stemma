# @stemma-sh/mcp

A [Model Context Protocol](https://modelcontextprotocol.io) server for
tracked-changes `.docx` editing, powered by the
[stemma](https://github.com/stemma-sh/stemma) engine. This package ships
prebuilt native binaries (Linux, macOS, Windows) behind a tiny launcher — no
Rust toolchain required.

> **Pre-1.0.** A `0.x` minor release may break API and wire contracts —
> deliberately, with changelog notice. The
> [stability policy](https://github.com/stemma-sh/stemma/blob/main/docs/guide/stability.md)
> states exactly what you can depend on today.

Run it:

```bash
npx -y @stemma-sh/mcp
```

Or wire it into an MCP client (stdio transport):

```json
{
  "mcpServers": {
    "stemma": {
      "command": "npx",
      "args": ["-y", "@stemma-sh/mcp"]
    }
  }
}
```

The server speaks JSON-RPC over stdio and takes no arguments (`--help` /
`--version` aside). Documents are passed as tool arguments
(`open_docx { "path": ... }`); every edit is applied as a proper tracked
change and gated through an OOXML validator before bytes are written.

Full tool reference, recipes, and refusal vocabulary:
[the stemma MCP reference](https://github.com/stemma-sh/stemma/blob/main/docs/reference/mcp.md).

The platform binary is selected via `optionalDependencies`
(`@stemma-sh/mcp-<platform>`); installing with `--omit=optional` will leave
the launcher without a binary, and it will say so rather than guess.

Dual-licensed under MIT or Apache-2.0.
