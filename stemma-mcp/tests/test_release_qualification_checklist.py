#!/usr/bin/env python3
"""The release gate shows the qualification checklist before approval.

A job behind a GitHub environment gate starts only after the deployment is
approved, so a checklist printed only by the gated job would reach the
reviewer too late. These checks pin that a non-gated job prints the
source-controlled checklist and that the gated job depends on it. The
workflow is read as text: PyYAML is deliberately not a dependency here.
"""

import re
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
WORKFLOW = REPO / ".github" / "workflows" / "release.yml"
CHECKLIST = REPO / ".github" / "release-qualification-checklist.md"


def job_block(workflow, name):
    match = re.search(r"^  {}:\n".format(re.escape(name)), workflow, re.MULTILINE)
    if match is None:
        raise AssertionError("release.yml has no job named " + name)
    following = re.search(r"^  [a-z0-9-]+:\n", workflow[match.end():], re.MULTILINE)
    end = match.end() + following.start() if following else len(workflow)
    return workflow[match.start():end]


class ReleaseQualificationChecklist(unittest.TestCase):
    def setUp(self):
        self.workflow = WORKFLOW.read_text(encoding="utf-8")

    def test_checklist_names_the_word_qualification_checks(self):
        checklist = CHECKLIST.read_text(encoding="utf-8")
        for required in (
            "candidate-manifest",
            "demo/README.md",
            "without a repair prompt",
            "Demo Reviewer",
            "demo/accepted.txt",
            "demo/rejected.txt",
            "reject the deployment",
        ):
            self.assertIn(required, checklist)
        self.assertGreaterEqual(checklist.count("- [ ]"), 6)

    def test_checklist_is_printed_before_the_gate(self):
        printer = job_block(self.workflow, "qualification-checklist")
        self.assertNotIn("environment:", printer)
        self.assertIn(".github/release-qualification-checklist.md", printer)
        self.assertIn("GITHUB_STEP_SUMMARY", printer)

    def test_gated_approval_waits_for_the_printed_checklist(self):
        approval = job_block(self.workflow, "release-approval")
        self.assertIn("environment: release", approval)
        needs = re.search(r"^    needs: \[(?P<jobs>[^\]]*)\]$", approval, re.MULTILINE)
        self.assertIsNotNone(needs, approval)
        jobs = {job.strip() for job in needs.group("jobs").split(",")}
        self.assertIn("qualification-checklist", jobs)
        self.assertIn(".github/release-qualification-checklist.md", approval)


if __name__ == "__main__":
    unittest.main()
