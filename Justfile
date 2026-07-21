# Stemma workspace task runner.
#
#   just            — list recipes
#   just gate       — the merge gate: everything CI runs on push (corpus-free, no real-Word oracle)
#   just test       — all daily tests across the workspace
#   just lint       — rustfmt check + clippy across the workspace, warnings denied
#
# The engine crate has a richer, engine-scoped gate with optional host-only
# tiers — see `just -f stemma-engine/Justfile --list`.

default:
    @just --list

# Merge gate: mirrors ci.yml job for job so a green gate means a green push.
# Must be green with no env set. The only CI coverage this cannot replicate
# locally is the Windows/macOS leg of the test matrix.
gate: contamination docs-check lint test conformance npm-smoke msrv

[doc("Validate public documentation links, anchors, and style rules")]
docs-check:
    python3 scripts/check-docs.py

# Lint results are only meaningful on the toolchain pinned in .mise.toml (new
# stables ship new clippy lints), so refuse to lint on anything else rather
# than report a green that CI will contradict.
[doc("rustfmt check + clippy across the workspace, warnings denied (pinned toolchain)")]
lint: check-toolchain
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings

[doc("Refuse to run on a rustc other than the .mise.toml pin")]
check-toolchain:
    @want="$(sed -n 's/^rust = "\([^"]*\)".*/\1/p' .mise.toml)"; have="$(rustc --version | cut -d' ' -f2)"; if [ "$want" != "$have" ]; then echo "error: toolchain drift — rustc $have active but .mise.toml pins $want. Run: mise install (or rustup default $want)"; exit 1; fi

# Publish the crates.io surface in dependency order: engine, host artifact
# boundary, then CLI. The CLI's two workspace dependencies must already be
# visible in the registry before its package can resolve there. Manual and
# deliberate; see RELEASING.md for where this sits in a release. Fails loud
# unless a registry token is present (`cargo login` or
# CARGO_REGISTRY_TOKEN), every selected crate permits publishing, the lockfile
# is current, and the working tree is clean. Each crate is dry-run-verified
# immediately before its real publish. stemma-mcp (npm is its channel) and
# stemma-api (demo infrastructure) are deliberately not published.
[doc("Publish stemma -> stemma-artifacts -> stemma-cli (guarded; see RELEASING.md)")]
[confirm("Publish stemma, stemma-artifacts, and stemma-cli to crates.io? Published versions are permanent. (y/N)")]
publish-crates: check-toolchain
    @test -z "$(git status --porcelain)" || { echo "error: working tree not clean — publish only from the release commit"; exit 1; }
    cargo publish --locked -p stemma --dry-run
    cargo publish --locked -p stemma
    just _wait-crate-visible stemma stemma-engine/Cargo.toml
    cargo publish --locked -p stemma-artifacts --dry-run
    cargo publish --locked -p stemma-artifacts
    just _wait-crate-visible stemma-artifacts stemma-artifacts/Cargo.toml
    cargo publish --locked -p stemma-cli --dry-run
    cargo publish --locked -p stemma-cli

# crates.io accepts a publish before every index/API mirror can resolve it.
# Bound the wait so a partial release stops explicitly instead of letting the
# dependent package fail with a misleading missing-dependency error.
_wait-crate-visible crate manifest:
    @version="$(sed -n 's/^version = "\([^"]*\)".*/\1/p' {{manifest}} | head -1)"; \
      for attempt in $(seq 1 30); do \
        if cargo info "{{crate}}@$version" --registry crates-io >/dev/null 2>&1; then \
          echo "visible on crates.io: {{crate}}@$version"; \
          exit 0; \
        fi; \
        echo "waiting for crates.io: {{crate}}@$version ($attempt/30)"; \
        sleep 10; \
      done; \
      echo "error: {{crate}}@$version was not visible on crates.io after 5 minutes"; \
      exit 1

test:
    cargo test --workspace

_build-mcp:
    cargo build -p stemma-mcp

# The same packaging unittests and exact-binary wire qualification that CI's
# npm-launcher job runs. The conformance harness pins its own STEMMA_MCP_*
# environment, so this is as hermetic locally as it is on a runner.
[doc("Packaging unittests + safe-artifact conformance over the exact debug binary")]
conformance: _build-mcp
    python3 -m unittest discover -s stemma-mcp/tests -p 'test_*.py'
    python3 stemma-mcp/safe_artifact_conformance.py target/debug/stemma-mcp

[doc("Validate the MCPB manifest and drive the full MCP protocol through npm's bin shim")]
npm-smoke: _build-mcp
    npx --yes @anthropic-ai/mcpb@2.1.2 validate stemma-mcp/mcpb/manifest.json
    bash stemma-mcp/npm/smoke-launcher.sh target/debug/stemma-mcp

# Build on the advertised toolchain floor (crate rust-version fields), like
# CI's msrv job. Separate target dir so the floor build never invalidates the
# daily 1.97 cache.
[doc("Build the workspace on the MSRV floor (rust 1.91)")]
msrv:
    mise install rust@1.91.0
    CARGO_TARGET_DIR=target/msrv mise x rust@1.91.0 -- cargo build --workspace

# Same pattern and char-class self-reference trick as ci.yml's contamination
# job: nothing tracked here may name the maintainer-only repository, a
# contributor's home path, or held-out oracle-infrastructure paths.
[doc("Refuse references to held-out/maintainer-only material (same grep as CI)")]
contamination:
    #!/usr/bin/env bash
    set -euo pipefail
    pattern='stemma[-]private|/home/[a]ndreas|word-oracle[/]'
    if git grep -n -i -I -E "$pattern" -- .; then
        echo "error: forbidden reference to held-out/maintainer-only material in the tracked tree" >&2
        exit 1
    fi
    echo "contamination check passed: no forbidden references in tracked tree"
