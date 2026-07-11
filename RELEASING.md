# Releasing

A release is one tag push; CI does the rest. This file is the maintainer
checklist — contributors never need it.

## One-time setup (before the first release)

1. **npm org.** Claim the `stemma-sh` org on npmjs.com (it must match the
   GitHub org; the packages are `@stemma-sh/mcp` and `@stemma-sh/mcp-<platform>`).
   If a different scope is ever chosen, update it in all three places that
   spell it out: `stemma-mcp/npm/package/bin/stemma-mcp.js` (the platform map),
   `stemma-mcp/npm/build-npm-packages.sh`, and the docs that print install
   commands (`stemma-mcp/npm/package/README.md`, `stemma-mcp/README.md`,
   `README.md`).
2. **npm token (first release only).** Create a granular automation token
   scoped to the org with read/write, short expiry, and store it as the
   `NPM_TOKEN` repository secret. There is no need to bootstrap-publish
   anything first: scoped packages cannot be squatted, and a scope-wide
   granular token can create new packages.
3. **After the first release: switch npm to trusted publishing.** OIDC
   trusted publishers can only be configured on packages that already exist,
   which is why v0.1.0 goes out with the token. Once it has: on npmjs.com,
   for EACH of the six packages (`@stemma-sh/mcp` + the five platform
   packages), Settings → Trusted Publisher → GitHub Actions with org
   `stemma-sh`, repo `stemma`, workflow filename `release.yml` (npm does not
   validate these fields — a typo surfaces only as a 404 at the next
   publish). Then delete the npm token and the `NPM_TOKEN` secret, and drop
   `NODE_AUTH_TOKEN` from the workflow (the `id-token: write` permission it
   needs is already declared; OIDC needs npm >= 11.5.1, so add an
   `npm install -g npm@latest` step at that point). Provenance attestations
   are already generated either way — the token path passes `--provenance`
   explicitly.
4. **crates.io.** All four crate names (`stemma`, `stemma-mcp`, `stemma-cli`,
   `stemma-api`) were free as of 2026-07-11 — but unlike npm scopes,
   crates.io names are unscoped and first-come. Claim `stemma` by publishing
   the real engine crate at (or immediately after) launch, before anyone
   else can. Log in with GitHub, `cargo login` with an API token (scoped to
   publish, short expiry), then see "crates.io" under Per release below.
   The first publish is deliberately manual and local: a long-lived registry
   token must never live in CI secrets (any compromised workflow step can
   read them), and trusted publishing cannot be configured on a crate that
   does not exist yet.
5. **After the first release: switch crates.io to trusted publishing.** Same
   endgame as npm: on crates.io, for each published crate, Settings →
   Trusted Publishing → add the GitHub repo (`stemma-sh/stemma`) and
   workflow (`release.yml`). Then a `crates-publish` job can be added to
   release.yml using OIDC (`id-token: write`, no stored token), and the
   local API token is revoked. Until that job exists, crates publishes stay
   manual per release.
6. **Mailboxes.** `security@stemma.sh` and `conduct@stemma.sh` must be live —
   SECURITY.md and CODE_OF_CONDUCT.md point at them.

## Per release

1. Update versions. `stemma-mcp/Cargo.toml` is the single source of truth for
   the npm packages — the assembly script stamps every package.json from it.
   Keep `stemma-mcp/mcpb/manifest.json` and
   `stemma-mcp/plugin/.claude-plugin/plugin.json` in step.
2. Move the `[Unreleased]` CHANGELOG section under the new version heading.
3. `just gate` — green, no exceptions.
4. Commit, then tag and push:

   ```bash
   git tag v<version>       # must equal the stemma-mcp crate version
   git push origin main v<version>
   ```

5. CI takes over (`.github/workflows/release.yml`):
   - **version-guard** refuses a tag that disagrees with the crate version;
   - **build** produces the five-platform binary matrix
     (linux x64/arm64 glibc, macOS x64/arm64, windows x64);
   - **npm-publish** assembles and publishes the platform packages first, the
     `@stemma-sh/mcp` wrapper last — all five or nothing;
   - **github-release** attaches tarballs/zip + `sha256sums.txt` to the tag's
     release.
6. **crates.io** (manual and deliberate — the workflow does not touch cargo).
   Publishing is per-crate opt-in: flip `publish = false` in the crate's
   Cargo.toml as part of the release commit for the crates being published.
   Intended split: `stemma` (the engine — the library adopters embed) and
   `stemma-cli` (`cargo install stemma-cli`) yes; `stemma-mcp` optional (npm
   is its primary channel); `stemma-api` stays unpublished — it is demo
   infrastructure, not a deployable. Then, with a registry token in the
   environment:

   ```bash
   just publish-crates
   ```

   The recipe enforces the order (engine first — `cargo publish` waits for
   the index — then the CLI, whose `stemma` dependency carries both `path`
   and `version`), dry-runs each crate immediately before its real publish,
   refuses a dirty working tree, and asks for confirmation.
7. Verify from a clean machine: `npx -y @stemma-sh/mcp --version` prints the
   released version, and (if crates were published) `cargo install
   stemma-cli && stemma --version` works.

## What guards this path day to day

The `npm-launcher` job in `ci.yml` assembles the host-platform packages from a
fresh build on every push and drives the full MCP protocol through npm's bin
shim (`stemma-mcp/npm/smoke-launcher.sh` — also runnable locally). The
packaging can't silently drift from the server.
