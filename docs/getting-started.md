# Create your first redline

Use this guide when you have an original Word document and a revised version.
By the end, you will have one new `.docx` containing native tracked changes
that a reviewer can accept or reject in Microsoft Word.

## What you need

- Rust and Cargo, if the `stemma` CLI is not installed yet.
- The original `.docx`.
- The revised `.docx`.
- A new path for the output. Stemma never overwrites an existing file.

## 1. Install the CLI

```bash
cargo install stemma-cli
```

Confirm the command is available:

```bash
stemma --version
```

## 2. Create the redline

```bash
stemma compare as-sent.docx as-returned.docx \
  -o what-changed.docx \
  --author "Approved Reviewer"
```

Stemma reports the created artifact on stderr:

```text
wrote redline to what-changed.docx (<n> tracked revisions); bytes=<n> sha256=<hex> collision_policy=create_new disposition=created
```

The output contract is:

- rejecting every discovered change reconstructs `as-sent.docx`;
- accepting every discovered change reconstructs `as-returned.docx`;
- neither input is modified;
- an existing output path is refused.

Omit `--author` if the comparison should be anonymous. Passing an empty author
is an error.

## 3. Inspect and validate the result

List the pending revisions:

```bash
stemma extract what-changed.docx --format json
```

Validate the package and print its exact byte identity:

```bash
stemma validate what-changed.docx
```

Success starts with `OK:` and includes the document's byte count and SHA-256.
Open `what-changed.docx` in Word to step through the tracked changes.

## What to do next

- Have a list of exact approved replacements instead of two documents?
  [Apply approved changes](guides/apply-approved-changes.md).
- Need to accept or reject some of the revisions?
  [Review and resolve changes](guides/review-and-resolve.md).
- Want an agent to inspect and edit documents?
  [Use Stemma with an agent](guides/use-with-an-agent.md).
- Hit an error?
  [Troubleshooting](help/troubleshooting.md).

For every flag and output guarantee, see the
[CLI reference](reference/cli.md).
