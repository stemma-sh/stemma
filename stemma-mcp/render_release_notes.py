#!/usr/bin/env python3
"""Render a complete GitHub release body from one changelog section."""

import argparse
import re
import sys
from pathlib import Path


VERSION_RE = re.compile(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")


def changelog_section(changelog: str, version: str) -> str:
    if not VERSION_RE.fullmatch(version):
        raise ValueError(f"version must be stable MAJOR.MINOR.PATCH, got {version!r}")

    heading = re.compile(rf"^## \[{re.escape(version)}\](?:\s+—\s+.*)?$", re.MULTILINE)
    match = heading.search(changelog)
    if match is None:
        raise ValueError(f"CHANGELOG has no section for {version}")

    following = changelog[match.end() :]
    next_heading = re.search(r"^## \[", following, re.MULTILINE)
    section = following[: next_heading.start() if next_heading else None].strip()
    if not section:
        raise ValueError(f"CHANGELOG section for {version} is empty")

    highlights = re.findall(r"^- ", section, re.MULTILINE)
    if len(highlights) < 3:
        raise ValueError(
            f"CHANGELOG section for {version} needs at least three release highlights; "
            f"found {len(highlights)}"
        )
    return section


def render_release_notes(changelog: str, version: str) -> str:
    section = changelog_section(changelog, version)
    tag = f"v{version}"
    major = int(version.partition(".")[0])
    release_status = (
        "Stemma is pre-1.0: experimental API and wire contracts may change\n"
        "between `0.x` minor releases with changelog notice."
        if major == 0
        else "See the changelog for compatibility details."
    )
    return f"""# Stemma {tag}

{section}

## Install the MCP server

```bash
npx -y @stemma-sh/mcp@{version}
```

Prebuilt MCP binaries are included for Linux x64/arm64, macOS x64/arm64, and
Windows x64. {release_status} The current focus is editing existing Word
documents, not replacing Word or authoring documents from scratch.

[Quick start](https://github.com/stemma-sh/stemma/blob/{tag}/README.md#quick-start-with-an-ai-assistant) ·
[Synthetic demo](https://github.com/stemma-sh/stemma/tree/{tag}/demo) ·
[Documentation](https://github.com/stemma-sh/stemma/blob/{tag}/docs/README.md) ·
[Live docs](https://stemma.sh/docs) ·
[Full changelog](https://github.com/stemma-sh/stemma/blob/{tag}/CHANGELOG.md)
"""


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("version")
    parser.add_argument(
        "--changelog",
        type=Path,
        default=Path(__file__).resolve().parents[1] / "CHANGELOG.md",
    )
    args = parser.parse_args()
    sys.stdout.write(
        render_release_notes(args.changelog.read_text(encoding="utf-8"), args.version)
    )


if __name__ == "__main__":
    main()
