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
gate: contamination docs-check lint test package-check conformance npm-smoke demo-check msrv

[doc("Validate public documentation links, anchors, and style rules")]
docs-check:
    python3 scripts/check-docs.py

[doc("Build and doctest each publishable crate from its isolated source archive")]
package-check:
    python3 scripts/check-crate-packages.py

# docs/reference/operations.md is generated from the engine op catalog
# (stemma-engine/src/edit_v4/catalog.rs); the gate fails when it drifts.
[doc("Regenerate docs/reference/operations.md from the engine op catalog")]
regen-operations-reference:
    cargo test -p stemma --test operations_reference regenerate_operations_reference -- --ignored

# docs/reference/read-model.md is generated from live engine view values
# (stemma-diff/tests/read_model_reference.rs); the gate fails when it drifts.
[doc("Regenerate docs/reference/read-model.md from the engine read model")]
regen-read-model-reference:
    cargo test -p stemma-diff --test read_model_reference regenerate_read_model_reference -- --ignored

# docs/examples.md is generated from the stemma-engine/examples/ inventory
# (stemma-engine/tests/examples_reference.rs); the gate fails when it drifts.
[doc("Regenerate docs/examples.md from the example inventory")]
regen-examples-reference:
    cargo test -p stemma --test examples_reference regenerate_examples_reference -- --ignored

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

# Publish the crates.io surface in dependency order: engine, comparison,
# host artifact boundary, then CLI. Release CI runs the same script per crate
# through crates.io trusted publishing (release.yml `crates-publish`); this
# recipe is the manual path, needed only for a crate that does not exist on
# crates.io yet (trusted publishing cannot be configured before a crate's
# first version). It needs a registry token (`cargo login` or
# CARGO_REGISTRY_TOKEN). scripts/publish-crate.sh refuses a dirty tree,
# dry-run-verifies each crate immediately before its real publish, skips a
# version crates.io already lists with the identical .crate checksum, refuses
# one with any other checksum, and waits for index visibility before the
# next crate. stemma-mcp (npm is its channel) and stemma-api (demo
# infrastructure) are deliberately not published.
[doc("Publish stemma -> stemma-diff -> stemma-artifacts -> stemma-cli (manual path; release CI does this; see RELEASING.md)")]
[confirm("Publish stemma, stemma-diff, stemma-artifacts, and stemma-cli to crates.io? Published versions are permanent. (y/N)")]
publish-crates: check-toolchain package-check
    bash scripts/publish-crate.sh stemma
    bash scripts/publish-crate.sh stemma-diff
    bash scripts/publish-crate.sh stemma-artifacts
    bash scripts/publish-crate.sh stemma-cli

test:
    cargo test --workspace

[doc("Regenerate and verify the public synthetic redline demo")]
demo-check:
    cargo build -p stemma-cli
    python3 scripts/check-demo.py

# Not part of `gate`: it downloads maturin and pytest from PyPI, and the gate
# must run without network. The release workflow's wheel jobs pin the same
# maturin version.
[doc("Build the stemma-py wheel and run its Python tests in a fresh venv (needs uv + network)")]
py-test:
    #!/usr/bin/env bash
    set -euo pipefail
    work="$(mktemp -d)"
    trap 'rm -rf "$work"' EXIT
    uvx --from maturin==1.15.0 maturin build --manifest-path stemma-py/Cargo.toml --out "$work/wheels"
    uv venv --quiet "$work/venv"
    VIRTUAL_ENV="$work/venv" uv pip install --quiet "$work"/wheels/*.whl pytest
    "$work/venv/bin/python" -m pytest -p no:cacheprovider stemma-py/tests

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
