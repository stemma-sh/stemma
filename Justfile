# Stemma workspace task runner.
#
#   just            — list recipes
#   just gate       — the merge gate: lint + all daily tests (corpus-free, no real-Word oracle)
#   just test       — all daily tests across the workspace
#   just lint       — rustfmt check + clippy across the workspace, warnings denied
#
# The engine crate has a richer, engine-scoped gate with optional host-only
# tiers — see `just -f stemma-engine/Justfile --list`.

default:
    @just --list

# Merge gate: static checks + all daily tests. Must be green with no env set.
gate: lint test

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

# Publish the library surface to crates.io: the engine, then the CLI (in
# that order — the CLI's dependency resolves from the registry). Manual and
# deliberate; see RELEASING.md for where this sits in a release. Fails loud
# unless: the crates' `publish` flags were flipped in the release commit, a
# registry token is present (`cargo login` or CARGO_REGISTRY_TOKEN), and the
# working tree is clean. Each crate is dry-run-verified immediately before
# its real publish. stemma-mcp (npm is its channel) and stemma-api (demo
# infrastructure) are deliberately not published.
[doc("Publish stemma + stemma-cli to crates.io (guarded; see RELEASING.md)")]
[confirm("Publish stemma and stemma-cli to crates.io? Published versions are permanent. (y/N)")]
publish-crates: check-toolchain
    @test -z "$(git status --porcelain)" || { echo "error: working tree not clean — publish only from the release commit"; exit 1; }
    cargo publish -p stemma --dry-run
    cargo publish -p stemma
    cargo publish -p stemma-cli --dry-run
    cargo publish -p stemma-cli

test:
    cargo test --workspace
