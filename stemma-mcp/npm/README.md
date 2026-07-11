# npm distribution

`npx -y @stemma-sh/mcp` runs the MCP server without a Rust toolchain. The
pattern is the standard one for native tools on npm (esbuild, Biome): a tiny
JS launcher package whose `optionalDependencies` are per-platform packages
containing nothing but the compiled binary — npm downloads only the one whose
`os`/`cpu` match the host.

| Path | What it is |
|---|---|
| `package/` | The `@stemma-sh/mcp` wrapper, checked in: launcher (`bin/stemma-mcp.js`), manifest, npm README. Its `version` and pinned platform versions are placeholders (`0.0.0-dev`) — stamped at assembly time. |
| `build-npm-packages.sh` | Assembles publish-ready package dirs from built binaries. Version comes from `stemma-mcp/Cargo.toml`; strict by default (all five platforms), `--only <triple>` for one. |
| `smoke-launcher.sh` | End-to-end check: assemble for the host, install like a consumer (packed tarballs), run `--version` and the full `smoke_test.py` protocol pass through npm's bin shim. CI runs this on every push (`npm-launcher` job). |

Publishing happens only from the tag-triggered release workflow
(`.github/workflows/release.yml`); see [RELEASING.md](../../RELEASING.md).
The platform map lives in two places that must stay in sync — the launcher's
`PLATFORM_PACKAGES` and the script's `platforms` table; both say so.

Local run:

```bash
cargo build -p stemma-mcp
bash stemma-mcp/npm/smoke-launcher.sh target/debug/stemma-mcp
```
