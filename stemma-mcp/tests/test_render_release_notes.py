#!/usr/bin/env python3
"""Hermetic tests for GitHub release-note rendering."""

import sys
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
sys.dont_write_bytecode = True
sys.path.insert(0, str(REPO_ROOT / "stemma-mcp"))
from render_release_notes import changelog_section, render_release_notes  # noqa: E402


CHANGELOG = """# Changelog

## [Unreleased]

## [1.2.3] — 2026-08-12

### Changed

- First concrete change.
- Second concrete change.
- Third concrete change.

## [1.2.2] — 2026-08-01

- Older change.

## [0.5.1] — 2026-08-13

- First pre-1.0 change.
- Second pre-1.0 change.
- Third pre-1.0 change.
"""


class ReleaseNotesTests(unittest.TestCase):
    def test_renders_changelog_install_scope_and_links(self):
        rendered = render_release_notes(CHANGELOG, "1.2.3")

        self.assertIn("# Stemma v1.2.3", rendered)
        self.assertIn("- Third concrete change.", rendered)
        self.assertNotIn("Older change", rendered)
        self.assertIn("npx -y @stemma-sh/mcp@1.2.3", rendered)
        self.assertIn("Linux x64/arm64", rendered)
        self.assertNotIn("pre-1.0", rendered)
        self.assertIn("See the changelog for compatibility details.", rendered)
        self.assertIn("blob/v1.2.3/docs/README.md", rendered)
        self.assertIn("[Live docs](https://stemma.sh/docs)", rendered)
        self.assertIn("blob/v1.2.3/CHANGELOG.md", rendered)

    def test_pre_one_release_includes_pre_one_compatibility_status(self):
        rendered = render_release_notes(CHANGELOG, "0.5.1")

        self.assertIn("Stemma is pre-1.0", rendered)
        self.assertIn("between `0.x` minor releases", rendered)

    def test_refuses_missing_or_non_stable_version(self):
        with self.assertRaisesRegex(ValueError, "no section"):
            changelog_section(CHANGELOG, "1.2.4")
        with self.assertRaisesRegex(ValueError, "stable MAJOR.MINOR.PATCH"):
            changelog_section(CHANGELOG, "1.2.3-rc.1")

    def test_refuses_fewer_than_three_highlights(self):
        sparse = """# Changelog

## [1.2.3] — 2026-08-12

- One.
- Two.
"""
        with self.assertRaisesRegex(ValueError, "at least three"):
            changelog_section(sparse, "1.2.3")


if __name__ == "__main__":
    unittest.main()
