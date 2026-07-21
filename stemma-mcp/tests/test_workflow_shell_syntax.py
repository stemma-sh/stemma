"""Every workflow `run:` block must be syntactically valid bash.

A shell syntax error inside a workflow step is invisible to every local check
and to CI itself until the step actually executes. Release-only steps
therefore fail for the first time on release day, after earlier jobs have
already published irreversibly. The 0.2.0 release lost its GitHub release
assets exactly this way: an inline heredoc whose `PY` terminator was indented
to match the surrounding `case` branch, which `<<'PY'` never matches, so bash
consumed the rest of the script and died at end-of-file.

`bash -n` parses without executing, so this is a pure static check. GitHub
expressions (`${{ ... }}`) parse as ordinary bash words and need no
substitution.

The block extractor is deliberately dependency-free: PyYAML is not a declared
dependency of this repo and must not become one for a syntax guard.
"""

import re
import subprocess
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
WORKFLOWS = sorted((REPO / ".github" / "workflows").glob("*.yml"))
SHELL_SCRIPTS = sorted(
    path
    for directory in ("stemma-mcp", "scripts")
    for path in (REPO / directory).rglob("*.sh")
)
BLOCK_RE = re.compile(r"^(?P<indent> *)run: *\|-? *$")


def run_blocks(path):
    """Yield (line_number, script) for each `run: |` block scalar in one file.

    Only literal block scalars are extracted. Folded (`>-`) and single-line
    `run:` values are not shell-syntax hazards in this repo: they carry one
    command with no heredocs or quoting structure.
    """
    lines = path.read_text(encoding="utf-8").splitlines()
    for index, line in enumerate(lines):
        match = BLOCK_RE.match(line)
        if not match:
            continue
        key_indent = len(match.group("indent"))
        body = []
        for candidate in lines[index + 1:]:
            if not candidate.strip():
                body.append("")
                continue
            if len(candidate) - len(candidate.lstrip(" ")) <= key_indent:
                break
            body.append(candidate)
        content = [entry for entry in body if entry]
        if not content:
            continue
        margin = min(len(entry) - len(entry.lstrip(" ")) for entry in content)
        yield index + 1, "\n".join(entry[margin:] if entry else "" for entry in body)


class WorkflowShellSyntax(unittest.TestCase):
    def test_workflows_exist(self):
        self.assertTrue(WORKFLOWS, "no workflow files found to check")

    def test_every_run_block_parses_as_bash(self):
        checked = 0
        for path in WORKFLOWS:
            for line_number, script in run_blocks(path):
                checked += 1
                result = subprocess.run(
                    ["bash", "-n"],
                    input=script,
                    text=True,
                    capture_output=True,
                )
                self.assertEqual(
                    result.returncode,
                    0,
                    "{}:{} run block is not valid bash:\n{}".format(
                        path.name, line_number, result.stderr.strip()
                    ),
                )
        self.assertGreater(checked, 0, "extractor found no run blocks to check")

    def test_every_shell_script_parses_as_bash(self):
        self.assertTrue(SHELL_SCRIPTS, "no shell scripts found to check")
        for path in SHELL_SCRIPTS:
            result = subprocess.run(
                ["bash", "-n", str(path)],
                text=True,
                capture_output=True,
            )
            self.assertEqual(
                result.returncode,
                0,
                "{} is not valid bash:\n{}".format(
                    path.relative_to(REPO), result.stderr.strip()
                ),
            )


if __name__ == "__main__":
    unittest.main()
