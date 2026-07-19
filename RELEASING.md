# Releasing

A release is one manually started candidate run. CI builds and exposes the
exact artifacts, a protected environment pauses promotion for Word
qualification, and the same run binds the version tag, publishes npm, then
publishes a verified GitHub release only after approval. This file is the
maintainer checklist; contributors never need it.

## One-time setup (before the first release)

1. **npm org.** Claim the `stemma-sh` org on npmjs.com (it must match the
   GitHub org; the packages are `@stemma-sh/mcp` and `@stemma-sh/mcp-<platform>`).
   If a different scope is ever chosen, update it in all three places that
   spell it out: `stemma-mcp/npm/package/bin/stemma-mcp.js` (the platform map),
   `stemma-mcp/npm/build-npm-packages.sh`, and the docs that print install
   commands (`stemma-mcp/npm/package/README.md`, `stemma-mcp/README.md`,
   `README.md`).
2. **npm trusted publishing.** On npmjs.com, for EACH of the six packages
   (`@stemma-sh/mcp` + the five platform
   packages), Settings → Trusted Publisher → GitHub Actions with org
   `stemma-sh`, repo `stemma`, workflow filename `release.yml` (npm does not
   validate these fields — a typo surfaces only as a 404 at the next
   publish). Do not store an npm token in Actions. The publish job has
   `id-token: write`, uses Node 24, enforces the Node 22.14.0/npm 11.5.1
   trusted-publishing floors, and requests provenance attestations.
3. **Protected release environment.** In GitHub repository settings, create an
   environment named exactly `release`. Add required reviewers, prevent
   self-review, and disallow administrator bypass. The workflow's promotion
   job must remain pending there while the exact build artifacts are run
   through Word qualification. An unprotected environment defeats the release
   gate and is a release blocker. Before building, CI queries the environment
   and refuses to continue unless it finds named required reviewers with
   self-review prevention; administrator-bypass policy remains a maintainer
   setup check.
4. **Protected release-tag ruleset.** In repository Settings → Rules →
   Rulesets, create one active tag ruleset named exactly
   `protected-release-tags`. Include exactly `v*` (the API reports
   `refs/tags/v*`), exclude nothing, enable **Restrict updates** and **Restrict
   deletions**, do not restrict creation, and configure no bypass actors. The
   workflow verifies the visible name, target, enforcement, conditions, and
   rule types before building and again immediately before npm publication.
   GitHub redacts bypass actors from tokens without ruleset-write access, so
   the no-bypass setting remains a maintainer setup check. A missing or changed
   ruleset is a release blocker.
5. **Immutable GitHub releases.** In repository Settings → General → Releases,
   enable **Release immutability**. This setting applies only to releases
   published after it is enabled. It is required before running this workflow:
   publication freezes the approved assets and tag, and the final workflow
   assertion fails unless GitHub reports `immutable: true`. Tag rules alone do
   not prevent a contents writer from replacing or deleting release assets.
6. **crates.io.** The workspace has five package names: `stemma` (engine),
   `stemma-artifacts` (host artifact boundary), `stemma-cli`, `stemma-mcp`,
   and `stemma-api`. As of 2026-07-12, the project's `stemma` and
   `stemma-cli` 0.1.0 releases exist in the registry; `stemma-artifacts` has
   not been published yet and must be claimed before publishing a CLI version
   that depends on it. `stemma-mcp` and `stemma-api` are intentionally absent
   from crates.io. Log in with GitHub, `cargo login` with an API token (scoped
   to publish, short expiry), then see "crates.io" under Per release below.
   A first publish stays deliberately manual and local: a long-lived registry
   token must never live in CI secrets (any compromised workflow step can
   read it), and trusted publishing cannot be configured on a crate that does
   not exist yet.
7. **After the first release: switch crates.io to trusted publishing.** Same
   endgame as npm: on crates.io, for each published crate, Settings →
   Trusted Publishing → add the GitHub repo (`stemma-sh/stemma`) and
   workflow (`release.yml`). Then a `crates-publish` job can be added to
   release.yml using OIDC (`id-token: write`, no stored token), and the
   local API token is revoked. Until that job exists, crates publishes stay
   manual per release.
8. **Mailboxes.** `security@stemma.sh` and `conduct@stemma.sh` must be live —
   SECURITY.md and CODE_OF_CONDUCT.md point at them.

## Per release

1. Update versions. Releases use stable `MAJOR.MINOR.PATCH` versions only;
   prerelease and build metadata are refused rather than silently assigned to
   npm's `latest` tag. `stemma-mcp/Cargo.toml` is the single source of truth for
   the npm packages — the assembly script stamps every package.json from it.
   Keep `stemma-mcp/mcpb/manifest.json` and
   `stemma-mcp/plugin/.claude-plugin/plugin.json` in step. For a crates.io
   release, also update `stemma`, `stemma-artifacts`, and `stemma-cli` package
   versions together, plus the `stemma`/`stemma-artifacts` registry version
   requirements in the CLI and MCP manifests; commit the resulting lockfile.
2. Move the `[Unreleased]` CHANGELOG section under the new version heading.
3. `just gate` — green, no exceptions.
4. Commit and push the release commit. Do not create the tag yet. Start the
   manual workflow from that exact ref:

   ```bash
   git push origin main
   sha="$(git rev-parse HEAD)"
   gh workflow run release.yml --ref main \
     -f version=<version> -f commit_sha="$sha"
   ```

   The workflow refuses if the selected ref no longer resolves to the supplied
   full SHA. Record the workflow run id and its `GITHUB_SHA`.
5. CI builds the candidate (`.github/workflows/release.yml`):
   - **version-guard** treats manual inputs only as environment data, validates
     stable `MAJOR.MINOR.PATCH` and the exact SHA shape, and refuses a mismatch,
     a commit not reachable from `main`, a tag bound to another SHA, or a
     missing/unprotected `release` environment. A same-SHA tag is accepted only
     as a retry of an already approved source. The guard also requires the
     exact active update/deletion ruleset described above;
   - **build** produces the five-platform binary matrix
     (linux x64/arm64 glibc, macOS x64/arm64, windows x64), stamps every
     binary with `version+g<commit>`, and runs both the edit/reopen smoke and
     mandatory safe-artifact wire harness before upload; each uploaded target
     includes the JSON conformance report and exact binary SHA-256;
   - **candidate-manifest** requires exactly those five native artifacts,
     independently re-hashes every binary/report, verifies native executable
     architecture, build stamp, platform, timestamps, and the stable 21-case
     result set, then uploads one content-minimized create-new manifest;
   - **release-approval** waits at the protected `release` environment. While
     it is pending, download the candidate manifest and exact artifacts, run
     the required Word/client qualification, and record the report. Reject the
     deployment on any open gate; no tag or package has been published yet;
   - approving **release-approval** is the explicit promotion decision. No tag
     exists yet for a first attempt;
   - **release-claim** re-verifies tag protection and creates or confirms the
     lightweight `v<version>` tag at the approved SHA before any irreversible
     package publication. A racing different-SHA tag fails closed;
   - **npm-publish** assembles and publishes the platform packages first, the
     `@stemma-sh/mcp` wrapper last. If a platform publish fails, the wrapper is
     not published. Before assembly it re-verifies every downloaded
     binary/report against the approved manifest. A retry skips an immutable
     version only when its registry `dist.integrity` exactly matches the local
     prepacked tarball. Lifecycle scripts are refused and disabled; the exact
     independently hashed tarball is published, then registry
     visibility/integrity is verified before continuing. The active
     update/deletion ruleset is checked immediately before this irreversible
     step; if a retry tag already exists, its SHA must also match. The qualified
     native bytes embed the source SHA, so another source cannot reuse an
     already-published platform package at the same version and integrity;
   - **github-release** runs only after npm completes, re-verifies the candidate
     again, and attaches deterministic tarballs/zip containing each binary and
     conformance JSON, the aggregate manifest, and `sha256sums.txt`. It stages
     the release as a draft, refuses a differently named or byte-different
     existing uploaded asset, deletes only GitHub's explicitly incomplete
     `starter` assets, completes missing identical-run assets, and publishes
     only after the uploaded asset set is exact and the tag still names the
     approved SHA. The final response must report a published, non-prerelease,
     immutable release;

   Retry a failed publish job from the same workflow run whenever possible so
   it consumes the same frozen manifest and qualification reports. A fresh run
   still fails closed if any already-published npm package or GitHub asset
   differs from its newly approved bytes. A fresh run from another SHA cannot
   reuse the SHA-stamped platform tarballs.
6. **crates.io** (manual and deliberate — the workflow does not touch cargo).
   Publishing is per-crate opt-in through each `Cargo.toml`'s `publish` field.
   Intended split: `stemma` (the engine library), `stemma-artifacts` (the
   shared host-side artifact boundary required by transports), and
   `stemma-cli` (`cargo install stemma-cli`) publish; `stemma-mcp` stays off
   crates.io because npm/prebuilt binaries are its distribution channel;
   `stemma-api` stays unpublished because it is demo infrastructure, not a
   deployable. Then, with a registry token in the environment:

   ```bash
   just publish-crates
   ```

   The recipe enforces `stemma` → `stemma-artifacts` → `stemma-cli`. Each real
   `cargo publish` waits for registry visibility before the next package; the
   CLI's two internal dependencies carry both `path` and `version`. The recipe
   uses the committed lockfile, dry-runs each crate immediately before its
   real publish, refuses a dirty working tree, and asks for confirmation.
7. Verify exact packages from a clean machine:
   `npx -y @stemma-sh/mcp@<version> --version` must print
   `<version>+g<first-12-characters-of-tag-commit>`, and (if
   crates were published) `cargo install stemma-cli --version <version>
   --locked && stemma --version` must install and print `stemma <version>`.
   Record the registry checksum for each `.crate`; crates.io consumers compile
   from that package and therefore do not inherit the MCP binary's git build
   stamp.

## What guards this path day to day

The `npm-launcher` job in `ci.yml` assembles the host-platform packages from a
fresh build on every push and drives the full MCP protocol through npm's bin
shim (`stemma-mcp/npm/smoke-launcher.sh` — also runnable locally). The
packaging can't silently drift from the server.
