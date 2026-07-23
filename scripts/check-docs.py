#!/usr/bin/env python3
"""Validate the repository's public Markdown documentation."""

from __future__ import annotations

import re
import sys
from pathlib import Path
from urllib.parse import unquote


ROOT = Path(__file__).resolve().parent.parent
DOCS = ROOT / "docs"
DOCS_SITE = "https://stemma.sh/docs"
MARKDOWN_LINK = re.compile(r"!?\[[^\]]*\]\(([^)]+)\)")
HEADING = re.compile(r"^#{1,6}\s+(.+?)\s*$", re.MULTILINE)


def github_slug(value: str) -> str:
    value = re.sub(r"<[^>]+>", "", value)
    value = value.replace("`", "").replace("*", "").replace("_", "")
    value = value.strip().lower()
    value = re.sub(r"[^\w\s-]", "", value, flags=re.UNICODE)
    value = re.sub(r"\s+", "-", value)
    return re.sub(r"-+", "-", value)


def heading_anchors(path: Path) -> set[str]:
    counts: dict[str, int] = {}
    anchors: set[str] = set()
    for heading in HEADING.findall(path.read_text(encoding="utf-8")):
        base = github_slug(heading)
        seen = counts.get(base, 0)
        counts[base] = seen + 1
        anchors.add(base if seen == 0 else f"{base}-{seen}")
    return anchors


def split_target(raw: str) -> tuple[str, str]:
    target = raw.strip()
    if target.startswith("<") and target.endswith(">"):
        target = target[1:-1]
    target = target.split(maxsplit=1)[0]
    path, separator, fragment = target.partition("#")
    return unquote(path), unquote(fragment) if separator else ""


def main() -> int:
    errors: list[str] = []
    markdown = [ROOT / "README.md", *sorted(DOCS.rglob("*.md"))]

    for source in markdown:
        text = source.read_text(encoding="utf-8")
        relative_source = source.relative_to(ROOT)

        for line_number, line in enumerate(text.splitlines(), start=1):
            if "—" in line:
                errors.append(
                    f"{relative_source}:{line_number}: em dash is not allowed"
                )
            if "–" in line:
                errors.append(
                    f"{relative_source}:{line_number}: en dash is not allowed"
                )
            if " - " in line or line.endswith(" -"):
                errors.append(
                    f"{relative_source}:{line_number}: spaced dash punctuation "
                    "is not allowed"
                )

        for raw_target in MARKDOWN_LINK.findall(text):
            path_text, fragment = split_target(raw_target)

            # Links to the published docs site are still links into docs/:
            # map https://stemma.sh/docs[/<path>] back onto the source file so
            # existence and anchors stay validated (the site serves the same
            # tree GitBook publishes from docs/).
            if path_text == DOCS_SITE or path_text.startswith(DOCS_SITE + "/"):
                page = path_text[len(DOCS_SITE) :].strip("/")
                target = DOCS / (f"{page}.md" if page else "README.md")
                if not target.exists():
                    errors.append(
                        f"{relative_source}: docs-site link has no source page: "
                        f"{raw_target}"
                    )
                elif fragment and fragment not in heading_anchors(target):
                    errors.append(
                        f"{relative_source}: missing anchor #{fragment} in "
                        f"{target.relative_to(ROOT)}"
                    )
                continue

            if (
                not path_text
                and not fragment
                or "://" in path_text
                or path_text.startswith("mailto:")
            ):
                continue

            target = source if not path_text else (source.parent / path_text).resolve()
            try:
                target.relative_to(ROOT)
            except ValueError:
                errors.append(
                    f"{relative_source}: link escapes repository: {raw_target}"
                )
                continue

            # docs/ is the GitBook publishing root (.gitbook.yaml `root`).
            # The published site cannot serve files outside it, so a relative
            # link from a docs page must stay inside docs/; anything in the
            # wider repository needs an absolute URL.
            if source.is_relative_to(DOCS) and not target.is_relative_to(DOCS):
                errors.append(
                    f"{relative_source}: link escapes the GitBook root "
                    f"(use an absolute URL): {raw_target}"
                )
                continue

            if not target.exists():
                errors.append(
                    f"{relative_source}: missing link target: {raw_target}"
                )
                continue

            if fragment and target.suffix.lower() == ".md":
                anchors = heading_anchors(target)
                if fragment not in anchors:
                    errors.append(
                        f"{relative_source}: missing anchor #{fragment} in "
                        f"{target.relative_to(ROOT)}"
                    )

    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1

    print(f"docs check passed: {len(markdown)} public Markdown files")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
