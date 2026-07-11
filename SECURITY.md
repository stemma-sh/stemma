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
- The MCP server (`stemma-mcp`) is a local stdio transport. It reads and writes
  files on the host it runs on with the privileges of the process that launched
  it — grant it access only to paths you intend an agent to touch.

Findings in the engine's parsing, validation, or serialization of untrusted
documents are the highest-value reports.
