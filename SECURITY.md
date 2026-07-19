# Security Policy

## Supported versions

Stemma is pre-1.0. Security fixes land on the latest `main` only; there are no
backported release branches yet. If you are running an older commit, update to
current `main` before reporting.

## Reporting a vulnerability

Please report privately — do **not** open a public issue for a
security-relevant bug.

- Preferred: open a [GitHub private vulnerability advisory](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability)
  on this repository ("Security" tab → "Report a vulnerability").
- Or email: **security@stemma.sh** (subject line starting with `[stemma security]`).

Include the affected commit, a minimal reproducing `.docx` or transaction if you
have one, and what you observed. We'll acknowledge and work a fix on `main`.

## Scope

Stemma's threat model centers on the engine parsing **untrusted `.docx`
input**. What matters for this project:

- **A parsing panic on malformed input is a security-relevant bug**, not just a
  crash. The import path is expected to reject bad input with a typed error, not
  to panic, hang, or exhaust memory. A crafted document that panics the parser
  is in scope.
- **Resource-exhaustion guards exist and should hold.** The importer rejects
  zip bombs (`DocxError::ZipBomb`, `stemma-engine/src/docx.rs`) and refuses
  encrypted packages (`ensure_docx_not_encrypted`,
  `stemma-engine/src/import.rs`) rather than attempting to process them. A
  document that defeats these guards — unbounded memory or CPU from a bounded
  input — is in scope.
- **`stemma-api` is demo infrastructure, not a hardened service.** It is a thin
  HTTP adapter for the browser editor. It binds loopback (`127.0.0.1`) by
  default and has **no authentication, authorization, or rate limiting**. Do
  **not** expose it to an untrusted network; `--host` (bind `0.0.0.0`) is for
  local development only. Deploying `stemma-api` as a public service is a
  misconfiguration, not a vulnerability in stemma.
- **The MCP server has a bounded application-level workspace, not an OS
  sandbox.** `STEMMA_MCP_WORKSPACE_ROOT` confines normal MCP reads and writes;
  it defaults to the canonical startup current directory. Relative paths resolve
  under it, absolute paths must remain inside it, and read symlinks that escape
  it are refused. Set an explicit, narrow root for long-lived or user-scoped MCP
  registrations.
- **MCP image-path reads are bounded before base64 expansion.**
  `STEMMA_MCP_MAX_IMAGE_BYTES` defaults to 20 MiB per image path and
  `STEMMA_MCP_MAX_IMAGE_TOTAL_BYTES` defaults to 50 MiB of image-path input per
  transaction. Either cap returns `artifact_source_too_large` when exceeded;
  `0` disables that guard, which is not recommended for untrusted workloads.
- **Transport output is no-clobber and create-new.** MCP and CLI writes refuse
  every existing destination and input alias, stage in the destination
  directory, commit without replacing an existing path, and verify exact bytes
  and SHA-256 before reporting success. There is no overwrite override in this
  release. For portable behavior, paths containing Windows
  alternate-data-stream syntax in a normal path component are outside this
  contract on every platform and are refused before read or staging.
- **Filesystem sources must be regular files.** The shared edge rejects an
  obvious directory, FIFO, device, or other non-regular source before opening
  it, then checks the opened handle again. The second check is required because
  a same-user process can still race pathname metadata as described below.
- **These boundaries protect ordinary caller mistakes and failure paths.** The
  server still runs with the filesystem privileges of its host process. A
  hostile local process running as the same user can race or mutate filesystem
  state; the boundary also does not guarantee underlying storage integrity or
  power-loss durability. Use OS accounts, containers, or another real sandbox
  when the agent or neighboring processes are adversarial.
- **Receipts are sensitive metadata.** Artifact identities include supplied
  and resolved paths, exact byte lengths, and hashes. They do not embed DOCX
  content, but receipts and logs still require an explicit retention and
  redaction decision before they are shared.
- **Receipt paths are UTF-8 or refused.** A supplied or canonical filesystem
  path that cannot be represented exactly as a JSON string is rejected before
  source bytes are read or output staging begins; paths are never serialized
  lossily.

Findings in the engine's parsing, validation, or serialization of untrusted
documents are high-value reports. Workspace escape, protected-source or
existing-output replacement, partial output reported as success, and
artifact-identity mismatch are also in scope.
