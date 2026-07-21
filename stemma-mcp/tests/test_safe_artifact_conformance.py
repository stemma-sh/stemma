#!/usr/bin/env python3
"""Contract tests for the exact-binary safe-artifact conformance harness."""

import os
import sys
import unittest
from pathlib import Path
from unittest import mock


SCRIPT_DIR = Path(__file__).resolve().parents[1]
sys.dont_write_bytecode = True
sys.path.insert(0, str(SCRIPT_DIR))
import safe_artifact_conformance as conformance  # noqa: E402


class SafeArtifactConformanceTests(unittest.TestCase):
    def test_server_environment_pins_suite_profile_and_workspace(self):
        workspace = Path("/synthetic/conformance-workspace")
        inherited = {
            "STEMMA_MCP_PROFILE": "core",
            "STEMMA_MCP_WORKSPACE_ROOT": "/wrong/workspace",
        }
        with mock.patch.dict(os.environ, inherited, clear=False):
            env = conformance.conformance_server_environment(workspace)
            self.assertEqual(env["STEMMA_MCP_PROFILE"], "advanced")
            self.assertEqual(
                env["STEMMA_MCP_WORKSPACE_ROOT"], str(workspace)
            )
            self.assertEqual(os.environ["STEMMA_MCP_PROFILE"], "core")

    def test_required_tool_contract_is_checked_before_cases(self):
        tools = [
            {"name": name}
            for name in sorted(conformance.REQUIRED_TOOLS)
        ]
        self.assertEqual(
            conformance.require_conformance_tools({"tools": tools}),
            conformance.REQUIRED_TOOLS,
        )

        incomplete = [
            tool for tool in tools if tool["name"] != "audit_docx"
        ]
        with self.assertRaisesRegex(
            AssertionError,
            "advanced profile omitted conformance tools audit_docx",
        ):
            conformance.require_conformance_tools({"tools": incomplete})

    def test_malformed_or_paginated_tool_lists_are_rejected(self):
        malformed_payloads = (
            None,
            {},
            {"tools": [{}]},
            {"tools": [{"name": "open_docx"}, {"name": "open_docx"}]},
            {"tools": [], "nextCursor": "more"},
        )
        for payload in malformed_payloads:
            with self.subTest(payload=payload):
                with self.assertRaises(AssertionError):
                    conformance.require_conformance_tools(payload)


if __name__ == "__main__":
    unittest.main()
