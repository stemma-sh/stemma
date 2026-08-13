# Use Stemma with an agent

Use this guide when you want an MCP-capable agent to inspect and edit Word
documents through a path-based local server. Stemma does not upload the
document to a Stemma-operated service. The MCP client or its configured model
provider may receive selected document content through tool calls; consult the
client's data policy.

The default MCP profile exposes five tools:

`open_docx`, `inspect_docx`, `execute_plan`, `verify_docx`, and `save_docx`.

## 1. Register the server

Released npm packages include prebuilt binaries for Linux, macOS, and Windows.
For Claude Code, open the narrowest directory containing the documents and
media the agent needs, then register Stemma privately in that project:

```bash
cd /path/to/documents
claude mcp add --scope local stemma -- npx -y @stemma-sh/mcp
claude mcp list
```

The list should show Stemma as connected. Claude Code supplies its stable
project directory to the local server, and Stemma automatically confines file
access to it. To use a different directory, append
`--workspace-root /absolute/path/to/documents` after `@stemma-sh/mcp`.

For a project-scoped configuration with an explicit document boundary, use:

```json
{
  "mcpServers": {
    "stemma": {
      "type": "stdio",
      "command": "npx",
      "args": [
        "-y",
        "@stemma-sh/mcp",
        "--workspace-root",
        "/absolute/path/to/documents"
      ]
    }
  }
}
```

Tool paths cannot escape the workspace root. This is an application boundary,
not an operating-system sandbox against a hostile process running as the same
user.

## 2. Ask for one bounded task

For example:

> Open `agreement.docx`. Replace exactly one occurrence of “30 days” with
> “45 days” as a tracked change by “Approved Reviewer”. Preview the plan,
> apply it, verify the result, and save it as `agreement-redline.docx`.

The intended tool sequence is:

1. `open_docx` returns a document handle, compact index, and exact input
   identity.
2. `inspect_docx` locates and reads only the relevant block.
3. `execute_plan` previews the explicit edit, then applies the same plan.
4. `verify_docx` checks tracked-only change, untouched content, existing
   revisions, and validation.
5. `save_docx` reruns the session audit, refuses a non-deliverable result
   before creating the output path, then commits a new output path.

The explicit verification step lets you inspect and remediate the evidence
before delivery. Saving remains independently verification-gated.
The source is never overwritten. An existing destination is refused.

## 3. Review what was saved

A successful save returns the output byte count and SHA-256. Open the redline in
Word and confirm that accepting the change produces the intended text and
rejecting it restores the original text.

For exact tool arguments and receipts, see the
[MCP core reference](../reference/mcp.md). The optional 31-tool expert surface
is documented separately in the
[MCP advanced reference](../reference/mcp-advanced.md).

If registration, workspace access, or saving fails, see
[Troubleshooting](../help/troubleshooting.md).

When one instruction must produce several mutually dependent outputs, use a
declared task instead of treating each save as independent success. See
[Verify a multi-document task delivery](verify-task-delivery.md).
