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
      "args": ["-y", "@stemma-sh/mcp"],
      "env": {
        "STEMMA_MCP_WORKSPACE_ROOT": "/absolute/path/to/documents"
      }
    }
  }
}
```

The server speaks JSON-RPC over stdio and takes no arguments (`--help` /
`--version` aside). Documents are passed as tool arguments
(`open_docx { "path": ... }`). `STEMMA_MCP_WORKSPACE_ROOT` confines every
read and output path; when unset it is the canonical startup current directory.
Relative paths resolve under it and read symlinks may not escape it.

Image files supplied by `path` default to a 20 MiB per-image cap
(`STEMMA_MCP_MAX_IMAGE_BYTES`) and a 50 MiB aggregate cap per transaction
(`STEMMA_MCP_MAX_IMAGE_TOTAL_BYTES`). Both measure bytes before base64 expansion,
return `artifact_source_too_large` when exceeded, and accept `0` to disable the
corresponding limit.

Every output is create-new: an existing destination or input alias is refused,
with no overwrite option. Output is validated, staged in the destination
directory, committed without clobbering, and verified by byte length and SHA-256
before success. This protects ordinary mistakes and failed writes; it is not a
sandbox against a hostile same-user process or a power-loss durability promise.

Full tool reference, recipes, and refusal vocabulary:
[the stemma MCP reference](https://github.com/stemma-sh/stemma/blob/main/docs/reference/mcp.md).

The platform binary is selected via `optionalDependencies`
(`@stemma-sh/mcp-<platform>`); installing with `--omit=optional` will leave
the launcher without a binary, and it will say so rather than guess.

Dual-licensed under MIT or Apache-2.0.
