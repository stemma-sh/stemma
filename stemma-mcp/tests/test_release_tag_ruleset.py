#!/usr/bin/env python3
"""Hermetic tests for release-tag ruleset API verification."""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPT = REPO_ROOT / "stemma-mcp" / "verify_release_tag_ruleset.py"
RULESET_ID = 4242
RULESET_NAME = "protected-release-tags"


def valid_detail(include_bypass=True):
    detail = {
        "conditions": {
            "ref_name": {
                "exclude": [],
                "include": ["refs/tags/v*"],
            }
        },
        "enforcement": "active",
        "id": RULESET_ID,
        "name": RULESET_NAME,
        "rules": [{"type": "deletion"}, {"type": "update"}],
        "target": "tag",
    }
    if include_bypass:
        detail["bypass_actors"] = []
    return detail


class ReleaseTagRulesetTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(
            prefix="stemma-release-tag-ruleset-test-"
        )
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.list_path = self.root / "rulesets.json"
        self.detail_path = self.root / "ruleset.json"

    @staticmethod
    def write_json(path, value):
        path.write_text(
            json.dumps(value, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )

    def run_script(self, *arguments):
        return subprocess.run(
            [sys.executable, str(SCRIPT), *map(str, arguments)],
            cwd=str(REPO_ROOT),
            text=True,
            capture_output=True,
            check=False,
        )

    def test_select_returns_unique_active_named_ruleset_id_without_mutation(self):
        rulesets = [
            {
                "enforcement": "disabled",
                "id": 11,
                "name": RULESET_NAME,
                "target": "tag",
            },
            {
                "enforcement": "active",
                "id": RULESET_ID,
                "name": RULESET_NAME,
                "target": "tag",
            },
            {
                "enforcement": "active",
                "id": 99,
                "name": "another-ruleset",
                "target": "branch",
            },
        ]
        self.write_json(self.list_path, rulesets)
        before = self.list_path.read_bytes()

        result = self.run_script("select", self.list_path)

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), str(RULESET_ID))
        self.assertEqual(self.list_path.read_bytes(), before)

    def test_select_rejects_missing_or_multiple_active_matches(self):
        for rulesets, expected_count in (
            ([], 0),
            (
                [
                    {
                        "enforcement": "active",
                        "id": RULESET_ID,
                        "name": RULESET_NAME,
                    },
                    {
                        "enforcement": "active",
                        "id": RULESET_ID + 1,
                        "name": RULESET_NAME,
                    },
                ],
                2,
            ),
        ):
            with self.subTest(expected_count=expected_count):
                self.write_json(self.list_path, rulesets)
                result = self.run_script("select", self.list_path)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("found {}".format(expected_count), result.stderr)

    def test_select_requires_array_entries_and_positive_integer_id(self):
        for payload, message in (
            ({"id": RULESET_ID}, "JSON array"),
            (["not-an-object"], "must be an object"),
            (
                [
                    {
                        "enforcement": "active",
                        "id": True,
                        "name": RULESET_NAME,
                    }
                ],
                "positive integer",
            ),
        ):
            with self.subTest(message=message):
                self.write_json(self.list_path, payload)
                result = self.run_script("select", self.list_path)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)

    def test_verify_accepts_exact_detail_and_reports_bypass_visibility(self):
        for visible in (True, False):
            with self.subTest(visible=visible):
                detail = valid_detail(include_bypass=visible)
                self.write_json(self.detail_path, detail)
                before = self.detail_path.read_bytes()

                result = self.run_script(
                    "verify", self.detail_path, str(RULESET_ID)
                )

                self.assertEqual(result.returncode, 0, result.stderr)
                receipt = json.loads(result.stdout)
                self.assertTrue(receipt["verified"])
                self.assertEqual(receipt["id"], RULESET_ID)
                self.assertIs(receipt["bypass_actors_visible"], visible)
                self.assertEqual(self.detail_path.read_bytes(), before)

    def test_verify_binds_id_name_target_and_enforcement(self):
        mutations = (
            ("id", RULESET_ID + 1, "selected id"),
            ("name", "almost-protected-release-tags", "ruleset name"),
            ("target", "branch", "target"),
            ("enforcement", "evaluate", "enforcement"),
        )
        for field, value, message in mutations:
            with self.subTest(field=field):
                detail = valid_detail()
                detail[field] = value
                self.write_json(self.detail_path, detail)
                result = self.run_script(
                    "verify", self.detail_path, str(RULESET_ID)
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)

    def test_verify_requires_exact_ref_scope_and_rule_types(self):
        mutations = (
            (lambda detail: detail["conditions"]["ref_name"].update(include=[]), "ref include"),
            (
                lambda detail: detail["conditions"]["ref_name"].update(
                    include=["refs/tags/v*", "refs/tags/release-*"]
                ),
                "ref include",
            ),
            (
                lambda detail: detail["conditions"]["ref_name"].update(
                    exclude=["refs/tags/v0.*"]
                ),
                "ref exclude",
            ),
            (lambda detail: detail.update(rules=[{"type": "update"}]), "rule types"),
            (
                lambda detail: detail.update(
                    rules=[{"type": []}, {"type": "deletion"}]
                ),
                "rule type must be a string",
            ),
            (
                lambda detail: detail.update(
                    rules=[
                        {"type": "update"},
                        {"type": "deletion"},
                        {"type": "creation"},
                    ]
                ),
                "rule types",
            ),
        )
        for mutate, message in mutations:
            with self.subTest(message=message):
                detail = valid_detail()
                mutate(detail)
                self.write_json(self.detail_path, detail)
                result = self.run_script(
                    "verify", self.detail_path, str(RULESET_ID)
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)

    def test_visible_bypass_actors_must_be_an_empty_array(self):
        for bypass in (None, {}, [{"actor_id": 1, "actor_type": "Team"}]):
            with self.subTest(bypass=bypass):
                detail = valid_detail()
                detail["bypass_actors"] = bypass
                self.write_json(self.detail_path, detail)
                result = self.run_script(
                    "verify", self.detail_path, str(RULESET_ID)
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("bypass_actors", result.stderr)

    def test_strict_json_and_id_argument_are_enforced(self):
        self.detail_path.write_text(
            '{"id":4242,"id":4242}', encoding="utf-8"
        )
        duplicate = self.run_script(
            "verify", self.detail_path, str(RULESET_ID)
        )
        self.assertNotEqual(duplicate.returncode, 0)
        self.assertIn("duplicate JSON key", duplicate.stderr)

        self.detail_path.write_text("[NaN]", encoding="utf-8")
        nonfinite = self.run_script(
            "verify", self.detail_path, str(RULESET_ID)
        )
        self.assertNotEqual(nonfinite.returncode, 0)
        self.assertIn("non-finite", nonfinite.stderr)

        self.write_json(self.detail_path, valid_detail())
        bad_id = self.run_script("verify", self.detail_path, "004242")
        self.assertNotEqual(bad_id.returncode, 0)
        self.assertIn("ID argument", bad_id.stderr)


if __name__ == "__main__":
    unittest.main()
