#!/usr/bin/env python3
"""Qualify the stemma-mcp safe-artifact boundary over its stdio wire.

This is an exact-binary conformance harness, not an in-process test. It starts
the supplied executable with a synthetic ``STEMMA_MCP_WORKSPACE_ROOT``, copies
the repository's licensed public DOCX sample into that root, and exercises the
read and create-new persistence boundary through MCP.

Both advertised tool surfaces are qualified: a ``core``-profile session covers
open/execute_plan/save (including the comparison producer plan), and an
``advanced``-profile session covers the audit and review render verbs that
exist only there. ``STEMMA_MCP_PROFILE`` is pinned explicitly per session and
every other ambient ``STEMMA_MCP_*`` variable is dropped — inheriting one
would silently change which server configuration this report qualifies.

The JSON summary is written to stdout with the invoked executable's stable
SHA-256, platform, and UTC run bounds. Every case is mandatory: a failed case or
a case blocked by unavailable platform provisioning makes the process exit
nonzero. In particular, inability to create a symlink or hard link is reported
as ``blocked`` rather than silently skipped.

Usage:
    python3 stemma-mcp/safe_artifact_conformance.py target/debug/stemma-mcp
"""

import argparse
import collections
import datetime
import hashlib
import json
import os
import platform
import queue
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import zipfile
from pathlib import Path


SUITE_ID = "stemma.mcp.safe_artifact_conformance/v1"
DEFAULT_FIXTURE = (
    Path(__file__).resolve().parents[1]
    / "stemma-examples"
    / "samples"
    / "safe-agreement.docx"
)
EXPECTED_CASE_COUNT = 21
STDOUT_EOF = object()


class BlockedCase(Exception):
    """A mandatory case could not provision its platform prerequisite."""


class McpClient:
    """Small JSON-RPC stdio client with bounded waits and drained diagnostics."""

    def __init__(self, binary, env, timeout_seconds):
        self.timeout_seconds = timeout_seconds
        self._id = 0
        self._messages = queue.Queue()
        self._stderr = collections.deque(maxlen=100)
        self._non_json_stdout = collections.deque(maxlen=20)
        self.proc = subprocess.Popen(
            [str(binary)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            errors="replace",
            bufsize=1,
            env=env,
        )
        self._stdout_thread = threading.Thread(
            target=self._pump_stdout,
            name="stemma-conformance-stdout",
            daemon=True,
        )
        self._stderr_thread = threading.Thread(
            target=self._pump_stderr,
            name="stemma-conformance-stderr",
            daemon=True,
        )
        self._stdout_thread.start()
        self._stderr_thread.start()

    def _pump_stdout(self):
        try:
            for line in self.proc.stdout:
                try:
                    self._messages.put(json.loads(line))
                except json.JSONDecodeError:
                    self._non_json_stdout.append(line.rstrip())
        finally:
            self._messages.put(STDOUT_EOF)

    def _pump_stderr(self):
        for line in self.proc.stderr:
            self._stderr.append(line.rstrip())

    def _diagnostic_tail(self):
        diagnostics = []
        if self._non_json_stdout:
            diagnostics.append("stdout=" + " | ".join(self._non_json_stdout))
        if self._stderr:
            diagnostics.append("stderr=" + " | ".join(self._stderr))
        if not diagnostics:
            return "no server diagnostics"
        return "; ".join(diagnostics)

    def _send(self, method, params=None, notification=False):
        message = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            message["params"] = params
        if not notification:
            self._id += 1
            message["id"] = self._id
        try:
            self.proc.stdin.write(json.dumps(message, separators=(",", ":")) + "\n")
            self.proc.stdin.flush()
        except (BrokenPipeError, OSError) as error:
            raise RuntimeError(
                "server stdin closed while sending {}: {}; {}".format(
                    method, error, self._diagnostic_tail()
                )
            )
        if notification:
            return None
        return self._read_result(self._id, method)

    def _read_result(self, wanted_id, method):
        deadline = time.monotonic() + self.timeout_seconds
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise RuntimeError(
                    "timed out after {:.1f}s waiting for {}; {}".format(
                        self.timeout_seconds, method, self._diagnostic_tail()
                    )
                )
            try:
                message = self._messages.get(timeout=remaining)
            except queue.Empty:
                continue
            if message is STDOUT_EOF:
                raise RuntimeError(
                    "server closed stdout while waiting for {} (exit {}); {}".format(
                        method, self.proc.poll(), self._diagnostic_tail()
                    )
                )
            if message.get("id") != wanted_id:
                continue
            if "error" in message:
                raise RuntimeError(
                    "JSON-RPC error from {}: {}".format(method, message["error"])
                )
            return message["result"]

    def initialize(self):
        result = self._send(
            "initialize",
            {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "safe-artifact-conformance",
                    "version": "1",
                },
            },
        )
        self._send("notifications/initialized", {}, notification=True)
        return result

    def call(self, name, arguments):
        result = self._send(
            "tools/call", {"name": name, "arguments": arguments}
        )
        payload = result.get("structuredContent")
        if payload is None:
            content = result.get("content") or []
            if not content or "text" not in content[0]:
                raise RuntimeError("{} returned no structured payload".format(name))
            payload = json.loads(content[0]["text"])
        return bool(result.get("isError", False)), payload

    def close(self):
        if self.proc.poll() is None:
            try:
                self.proc.stdin.close()
            except (BrokenPipeError, OSError):
                pass
            try:
                self.proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.proc.terminate()
                try:
                    self.proc.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    self.proc.kill()
                    self.proc.wait(timeout=3)
        self._stdout_thread.join(timeout=1)
        self._stderr_thread.join(timeout=1)


class CaseRunner:
    def __init__(self):
        self.results = []

    def run(self, case_id, operation):
        try:
            evidence = operation()
            result = {
                "id": case_id,
                "mandatory": True,
                "status": "passed",
            }
            if evidence:
                result["evidence"] = evidence
        except BlockedCase as error:
            result = {
                "id": case_id,
                "mandatory": True,
                "status": "blocked",
                "error": str(error),
            }
        except Exception as error:  # Each case must report instead of aborting the suite.
            result = {
                "id": case_id,
                "mandatory": True,
                "status": "failed",
                "error": "{}: {}".format(type(error).__name__, error),
            }
        self.results.append(result)


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def sha256_file(path):
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as handle:
        while True:
            chunk = handle.read(1024 * 1024)
            if not chunk:
                break
            digest.update(chunk)
            size += len(chunk)
    return digest.hexdigest(), size


def exact_file_identity(path):
    digest, size = sha256_file(path)
    return {"bytes": size, "sha256": digest}


def utc_now():
    return (
        datetime.datetime.now(datetime.timezone.utc)
        .isoformat()
        .replace("+00:00", "Z")
    )


def strip_windows_extended_prefix(path):
    """Normalize Rust/Windows ``canonicalize`` prefixes for comparison."""
    if path.startswith("\\\\?\\UNC\\"):
        return "\\\\" + path[8:]
    if path.startswith("\\\\?\\"):
        return path[4:]
    return path


def normalized_real_path(path):
    resolved = os.path.normpath(os.path.realpath(os.fspath(path)))
    if os.name == "nt":
        resolved = strip_windows_extended_prefix(resolved)
    return os.path.normcase(resolved)


def normalized_supplied_path(path):
    return os.path.normcase(os.path.normpath(os.fspath(path)))


def assert_identity(identity, expected_path, expected_role, expected_supplied):
    require(isinstance(identity, dict), "artifact identity is not an object")
    require(identity.get("role") == expected_role, "unexpected artifact role")
    digest = identity.get("digest") or {}
    require(digest.get("algorithm") == "sha256", "identity is not SHA-256")
    expected_digest, expected_bytes = sha256_file(expected_path)
    require(digest.get("hex") == expected_digest, "receipt digest differs from bytes")
    require(identity.get("bytes") == expected_bytes, "receipt byte count differs from bytes")
    require(
        normalized_real_path(identity.get("resolved_path"))
        == normalized_real_path(expected_path),
        "receipt resolved path differs from committed/read path",
    )
    require(
        normalized_supplied_path(identity.get("supplied_path"))
        == normalized_supplied_path(expected_supplied),
        "receipt supplied path differs from MCP argument",
    )
    return {"bytes": expected_bytes, "sha256": expected_digest}


def assert_output_artifact(artifact, expected_path, expected_role, expected_supplied):
    require(isinstance(artifact, dict), "output artifact is not an object")
    require(
        artifact.get("collision_policy") == "create_new",
        "output did not report create-new collision policy",
    )
    require(
        artifact.get("disposition") == "created",
        "output did not report created disposition",
    )
    return assert_identity(
        artifact.get("identity"), expected_path, expected_role, expected_supplied
    )


def status_name(block_status):
    if isinstance(block_status, str):
        return block_status
    return block_status.get("status")


def stage_snapshot(*roots):
    found = []
    for root in roots:
        for path in root.rglob(".stemma-stage-*"):
            found.append(normalized_real_path(path))
    return sorted(found)


def require_state(state, key, reason):
    if key not in state:
        raise BlockedCase(reason)
    return state[key]


def conformance_transaction(index, summary, apply_op_id):
    """One tracked replace of the fixture's first editable paragraph."""
    target = next(
        (
            block
            for block in index
            if block.get("role") == "paragraph"
            and status_name(block.get("block_status")) == "normal"
            and len((block.get("text_preview") or "").strip()) > 3
        ),
        None,
    )
    require(target is not None, "public fixture has no editable paragraph")
    original_text = target["text_preview"].strip()
    return {
        "ops": [
            {
                "op": "replace",
                "target": target["id"],
                "expect": original_text.split()[0],
                "content": {
                    "type": "paragraph",
                    "content": [
                        {
                            "type": "text",
                            "text": "STEMMA CONFORMANCE: " + original_text,
                        }
                    ],
                },
            }
        ],
        "revision": {
            "author": "stemma-conformance",
            "date": "2026-07-12T00:00:00Z",
            "apply_op_id": apply_op_id,
        },
        "summary": summary,
    }


def assert_server_version(payload, expected, surface):
    require(expected, "initialize omitted the exact server build identity")
    require(
        payload.get("server_version") == expected,
        "{} receipt build identity differs from initialize".format(surface),
    )


def run_suite(binary, fixture, timeout_seconds):
    binary_identity = exact_file_identity(binary)
    fixture_digest, fixture_bytes = sha256_file(fixture)
    runner = CaseRunner()
    state = {}
    client = None
    advanced = None

    with tempfile.TemporaryDirectory(prefix="stemma-artifact-conformance-") as base_name:
        base = Path(base_name)
        workspace = base / "workspace"
        outside = base / "outside"
        workspace.mkdir()
        outside.mkdir()
        input_path = workspace / "input.docx"
        outside_input = outside / "outside.docx"
        shutil.copyfile(str(fixture), str(input_path))
        shutil.copyfile(str(fixture), str(outside_input))

        # The server's behavior is selected by STEMMA_MCP_* variables, so pin
        # every one this run depends on and drop the rest.
        env = {
            key: value
            for key, value in os.environ.items()
            if not key.startswith("STEMMA_MCP_")
        }
        env["STEMMA_MCP_WORKSPACE_ROOT"] = str(workspace)
        client = McpClient(
            binary, dict(env, STEMMA_MCP_PROFILE="core"), timeout_seconds
        )
        advanced = McpClient(
            binary, dict(env, STEMMA_MCP_PROFILE="advanced"), timeout_seconds
        )
        try:
            initialized = client.initialize()
            server_info = initialized.get("serverInfo") or {}
            state["server_version"] = server_info.get("version")
            advanced_info = advanced.initialize().get("serverInfo") or {}
            require(
                advanced_info.get("version") == state["server_version"],
                "core and advanced sessions report different server builds",
            )

            def open_relative():
                supplied = "input.docx"
                is_error, payload = client.call("open_docx", {"path": supplied})
                require(not is_error, "open_docx refused an in-root relative source: {}".format(payload))
                assert_server_version(payload, state["server_version"], "open_docx")
                evidence = assert_identity(
                    payload.get("input_artifact"), input_path, "input_docx", supplied
                )
                require(payload.get("doc_id"), "open_docx omitted doc_id")
                require(isinstance(payload.get("index"), list), "open_docx omitted index")
                state["doc_id"] = payload["doc_id"]
                state["index"] = payload["index"]
                return evidence

            runner.run("open.relative_in_root", open_relative)

            def open_absolute():
                supplied = str(input_path.resolve())
                is_error, payload = client.call("open_docx", {"path": supplied})
                require(not is_error, "open_docx refused an in-root absolute source: {}".format(payload))
                assert_server_version(payload, state["server_version"], "open_docx")
                return assert_identity(
                    payload.get("input_artifact"), input_path, "input_docx", supplied
                )

            runner.run("open.absolute_in_root", open_absolute)

            def open_parent_escape():
                is_error, payload = client.call(
                    "open_docx", {"path": os.path.join("..", "outside", "outside.docx")}
                )
                require(is_error, "parent-traversal read unexpectedly succeeded")
                require(
                    payload.get("code") == "artifact_outside_workspace",
                    "parent-traversal read returned unexpected refusal: {}".format(payload),
                )
                return {"code": payload["code"]}

            runner.run("open.parent_escape_refused", open_parent_escape)

            def open_absolute_outside():
                is_error, payload = client.call(
                    "open_docx", {"path": str(outside_input.resolve())}
                )
                require(is_error, "absolute outside read unexpectedly succeeded")
                require(
                    payload.get("code") == "artifact_outside_workspace",
                    "absolute outside read returned unexpected refusal: {}".format(payload),
                )
                return {"code": payload["code"]}

            runner.run("open.absolute_outside_refused", open_absolute_outside)

            def open_symlink_escape():
                link_path = workspace / "outside-link.docx"
                try:
                    os.symlink(str(outside_input.resolve()), str(link_path), target_is_directory=False)
                except (NotImplementedError, OSError) as error:
                    raise BlockedCase(
                        "platform could not provision the mandatory symlink escape: {}".format(error)
                    )
                is_error, payload = client.call(
                    "open_docx", {"path": link_path.name}
                )
                require(is_error, "symlink escape read unexpectedly succeeded")
                require(
                    payload.get("code") == "artifact_outside_workspace",
                    "symlink escape returned unexpected refusal: {}".format(payload),
                )
                return {"code": payload["code"]}

            runner.run("open.symlink_escape_refused", open_symlink_escape)

            def save_fresh_identity():
                doc_id = require_state(
                    state, "doc_id", "requires open.relative_in_root"
                )
                index = require_state(
                    state, "index", "requires open.relative_in_root"
                )
                transaction = conformance_transaction(
                    index,
                    "derive the second conformance document",
                    "safe-artifact-conformance-v1",
                )
                is_error, applied = client.call(
                    "execute_plan",
                    {"doc_id": doc_id, "transaction": transaction, "preview": False},
                )
                require(not is_error, "MCP edit for derived document failed: {}".format(applied))
                require(applied.get("applied") is True, "MCP edit did not report applied=true")

                supplied = "derived.docx"
                derived = workspace / supplied
                is_error, payload = client.call(
                    "save_docx", {"doc_id": doc_id, "path": supplied}
                )
                require(not is_error, "fresh save_docx failed: {}".format(payload))
                assert_server_version(payload, state["server_version"], "save_docx")
                require(derived.is_file(), "fresh save returned without a final artifact")
                evidence = assert_output_artifact(
                    payload.get("output_artifact"),
                    derived,
                    "output_docx",
                    supplied,
                )
                require(
                    payload.get("bytes_written") == evidence["bytes"],
                    "save bytes_written differs from committed bytes",
                )
                inputs = payload.get("input_artifacts") or []
                require(len(inputs) >= 1, "save receipt omitted protected input artifacts")
                assert_identity(inputs[0], input_path, "input_docx", "input.docx")
                require(
                    stage_snapshot(workspace, outside) == [],
                    "fresh save left a staging artifact",
                )
                state["derived_path"] = derived
                state["derived_bytes"] = derived.read_bytes()
                return evidence

            runner.run("save.fresh_identity", save_fresh_identity)

            def refused_write(session, tool, arguments, expected_code, absent=None, preserved=None):
                absent = absent or []
                preserved = preserved or {}
                stages_before = stage_snapshot(workspace, outside)
                is_error, payload = session.call(tool, arguments)
                require(is_error, "{} unexpectedly committed a refused output".format(tool))
                require(
                    payload.get("code") == expected_code,
                    "{} returned unexpected refusal: {}".format(tool, payload),
                )
                for path in absent:
                    require(
                        not path.exists() and not path.is_symlink(),
                        "refused call left final artifact at {}".format(path.name),
                    )
                for path, original in preserved.items():
                    require(path.read_bytes() == original, "refused call changed {}".format(path.name))
                require(
                    stage_snapshot(workspace, outside) == stages_before,
                    "refused call left a staging artifact",
                )
                return {"code": payload["code"]}

            def save_existing_collision():
                doc_id = require_state(state, "doc_id", "requires open.relative_in_root")
                derived = require_state(state, "derived_path", "requires save.fresh_identity")
                original = derived.read_bytes()
                return refused_write(
                    client,
                    "save_docx",
                    {"doc_id": doc_id, "path": derived.name},
                    "artifact_output_exists",
                    preserved={derived: original},
                )

            runner.run("save.existing_collision_preserved", save_existing_collision)

            def save_protected_source():
                doc_id = require_state(state, "doc_id", "requires open.relative_in_root")
                original = input_path.read_bytes()
                return refused_write(
                    client,
                    "save_docx",
                    {"doc_id": doc_id, "path": input_path.name},
                    "artifact_protected_source",
                    preserved={input_path: original},
                )

            runner.run("save.protected_source_refused", save_protected_source)

            def save_windows_stream_syntax():
                doc_id = require_state(state, "doc_id", "requires open.relative_in_root")
                destination = workspace / "input.docx:stemma-output"
                original = input_path.read_bytes()
                return refused_write(
                    client,
                    "save_docx",
                    {"doc_id": doc_id, "path": destination.name},
                    "artifact_commit_failed",
                    absent=[destination],
                    preserved={input_path: original},
                )

            runner.run(
                "save.windows_stream_syntax_refused", save_windows_stream_syntax
            )

            def save_hardlink_alias():
                doc_id = require_state(state, "doc_id", "requires open.relative_in_root")
                alias = workspace / "input-hardlink.docx"
                try:
                    os.link(str(input_path), str(alias))
                except (NotImplementedError, OSError) as error:
                    raise BlockedCase(
                        "platform could not provision the mandatory hard-link alias: {}".format(error)
                    )
                original = input_path.read_bytes()
                return refused_write(
                    client,
                    "save_docx",
                    {"doc_id": doc_id, "path": alias.name},
                    "artifact_protected_source",
                    preserved={input_path: original, alias: original},
                )

            runner.run("save.hardlink_alias_refused", save_hardlink_alias)

            def save_outside():
                doc_id = require_state(state, "doc_id", "requires open.relative_in_root")
                destination = outside / "save-outside.docx"
                return refused_write(
                    client,
                    "save_docx",
                    {"doc_id": doc_id, "path": str(destination.resolve())},
                    "artifact_outside_workspace",
                    absent=[destination],
                )

            runner.run("save.outside_refused", save_outside)

            def compare_fresh_identity():
                require_state(state, "derived_path", "requires save.fresh_identity")
                supplied = "compare.docx"
                destination = workspace / supplied
                is_error, payload = client.call(
                    "execute_plan",
                    {
                        "comparison": {
                            "base_path": "input.docx",
                            "target_path": "derived.docx",
                            "out_path": supplied,
                            "author": "stemma-conformance",
                        },
                        "preview": False,
                    },
                )
                require(not is_error, "fresh comparison plan failed: {}".format(payload))
                assert_server_version(
                    payload, state["server_version"], "execute_plan comparison"
                )
                require(destination.is_file(), "compare returned without a final artifact")
                require(payload.get("change_count", 0) > 0, "compare found no derived change")
                evidence = assert_output_artifact(
                    payload.get("output_artifact"),
                    destination,
                    "output_redline",
                    supplied,
                )
                require(
                    payload.get("bytes_written") == evidence["bytes"],
                    "compare bytes_written differs from committed bytes",
                )
                require(
                    stage_snapshot(workspace, outside) == [],
                    "fresh compare left a staging artifact",
                )
                return evidence

            runner.run("compare.fresh_identity", compare_fresh_identity)

            def compare_protected_source():
                require_state(state, "derived_path", "requires save.fresh_identity")
                original = input_path.read_bytes()
                return refused_write(
                    client,
                    "execute_plan",
                    {
                        "comparison": {
                            "base_path": "input.docx",
                            "target_path": "derived.docx",
                            "out_path": "input.docx",
                        },
                        "preview": False,
                    },
                    "artifact_protected_source",
                    preserved={input_path: original},
                )

            runner.run("compare.protected_source_refused", compare_protected_source)

            def compare_existing_collision():
                require_state(state, "derived_path", "requires save.fresh_identity")
                destination = workspace / "compare-existing.docx"
                original = b"do-not-replace-compare"
                destination.write_bytes(original)
                return refused_write(
                    client,
                    "execute_plan",
                    {
                        "comparison": {
                            "base_path": "input.docx",
                            "target_path": "derived.docx",
                            "out_path": destination.name,
                        },
                        "preview": False,
                    },
                    "artifact_output_exists",
                    preserved={destination: original},
                )

            runner.run("compare.existing_collision_preserved", compare_existing_collision)

            def compare_outside():
                require_state(state, "derived_path", "requires save.fresh_identity")
                destination = outside / "compare-outside.docx"
                return refused_write(
                    client,
                    "execute_plan",
                    {
                        "comparison": {
                            "base_path": "input.docx",
                            "target_path": "derived.docx",
                            "out_path": str(destination.resolve()),
                        },
                        "preview": False,
                    },
                    "artifact_outside_workspace",
                    absent=[destination],
                )

            runner.run("compare.outside_refused", compare_outside)

            def audit_fresh_identity():
                require_state(state, "derived_path", "requires save.fresh_identity")
                supplied = "audit-render.docx"
                destination = workspace / supplied
                is_error, payload = advanced.call(
                    "audit_docx",
                    {
                        "before_path": "input.docx",
                        "after_path": "derived.docx",
                        "render": {"path": supplied},
                    },
                )
                require(not is_error, "fresh audit render failed: {}".format(payload))
                assert_server_version(payload, state["server_version"], "audit_docx")
                require(destination.is_file(), "audit returned without a final render")
                render = payload.get("render") or {}
                assert_server_version(
                    render, state["server_version"], "audit_docx render"
                )
                evidence = assert_output_artifact(
                    render.get("output_artifact"),
                    destination,
                    "audit_redline",
                    supplied,
                )
                require(
                    render.get("bytes_written") == evidence["bytes"],
                    "audit render byte count differs from committed bytes",
                )
                require(
                    stage_snapshot(workspace, outside) == [],
                    "fresh audit render left a staging artifact",
                )
                return evidence

            runner.run("audit.render_fresh_identity", audit_fresh_identity)

            def audit_collision():
                require_state(state, "derived_path", "requires save.fresh_identity")
                destination = workspace / "audit-existing.docx"
                original = b"do-not-replace-audit"
                destination.write_bytes(original)
                return refused_write(
                    advanced,
                    "audit_docx",
                    {
                        "before_path": "input.docx",
                        "after_path": "derived.docx",
                        "render": {"path": destination.name},
                    },
                    "artifact_output_exists",
                    preserved={destination: original},
                )

            runner.run("audit.render_collision_preserved", audit_collision)

            def audit_outside():
                require_state(state, "derived_path", "requires save.fresh_identity")
                destination = outside / "audit-outside.docx"
                return refused_write(
                    advanced,
                    "audit_docx",
                    {
                        "before_path": "input.docx",
                        "after_path": "derived.docx",
                        "render": {"path": str(destination.resolve())},
                    },
                    "artifact_outside_workspace",
                    absent=[destination],
                )

            runner.run("audit.render_outside_refused", audit_outside)

            def review_fresh_identity():
                # review_session renders the session delta (baseline -> now),
                # so the advanced session opens the fixture and applies its own
                # tracked edit before rendering.
                is_error, opened = advanced.call("open_docx", {"path": "input.docx"})
                require(not is_error, "advanced open_docx failed: {}".format(opened))
                assert_server_version(
                    opened, state["server_version"], "advanced open_docx"
                )
                require(opened.get("doc_id"), "advanced open_docx omitted doc_id")
                require(
                    isinstance(opened.get("index"), list),
                    "advanced open_docx omitted index",
                )
                doc_id = opened["doc_id"]
                transaction = conformance_transaction(
                    opened["index"],
                    "derive the review-session delta",
                    "safe-artifact-conformance-review-v1",
                )
                is_error, applied = advanced.call(
                    "apply_edit", {"doc_id": doc_id, "transaction": transaction}
                )
                require(
                    not is_error,
                    "advanced edit for review session failed: {}".format(applied),
                )
                require(
                    applied.get("applied") is True,
                    "advanced edit did not report applied=true",
                )
                state["review_doc_id"] = doc_id
                supplied = "review-render.docx"
                destination = workspace / supplied
                is_error, payload = advanced.call(
                    "review_session",
                    {"doc_id": doc_id, "render": {"path": supplied}},
                )
                require(not is_error, "fresh session review render failed: {}".format(payload))
                assert_server_version(payload, state["server_version"], "review_session")
                require(destination.is_file(), "session review returned without a final render")
                render = payload.get("render") or {}
                assert_server_version(
                    render, state["server_version"], "review_session render"
                )
                evidence = assert_output_artifact(
                    render.get("output_artifact"),
                    destination,
                    "session_redline",
                    supplied,
                )
                require(
                    render.get("bytes_written") == evidence["bytes"],
                    "review render byte count differs from committed bytes",
                )
                require(
                    stage_snapshot(workspace, outside) == [],
                    "fresh review render left a staging artifact",
                )
                return evidence

            runner.run("review.render_fresh_identity", review_fresh_identity)

            def review_collision():
                doc_id = require_state(
                    state,
                    "review_doc_id",
                    "requires review.render_fresh_identity session delta",
                )
                destination = workspace / "review-existing.docx"
                original = b"do-not-replace-review"
                destination.write_bytes(original)
                return refused_write(
                    advanced,
                    "review_session",
                    {"doc_id": doc_id, "render": {"path": destination.name}},
                    "artifact_output_exists",
                    preserved={destination: original},
                )

            runner.run("review.render_collision_preserved", review_collision)

            def review_outside():
                doc_id = require_state(
                    state,
                    "review_doc_id",
                    "requires review.render_fresh_identity session delta",
                )
                destination = outside / "review-outside.docx"
                return refused_write(
                    advanced,
                    "review_session",
                    {
                        "doc_id": doc_id,
                        "render": {"path": str(destination.resolve())},
                    },
                    "artifact_outside_workspace",
                    absent=[destination],
                )

            runner.run("review.render_outside_refused", review_outside)

            require(
                len(runner.results) == EXPECTED_CASE_COUNT,
                "harness definition drifted from its declared case count",
            )
            counts = {
                status: sum(1 for result in runner.results if result["status"] == status)
                for status in ("passed", "failed", "blocked")
            }
            ok = counts["failed"] == 0 and counts["blocked"] == 0
            client.close()
            client = None
            advanced.close()
            advanced = None
            require(
                exact_file_identity(binary) == binary_identity,
                "MCP executable changed while the conformance suite was running",
            )
            return {
                "binary": str(binary),
                "binary_identity": binary_identity,
                "case_count": len(runner.results),
                "cases": runner.results,
                "counts": counts,
                "fixture": {
                    "bytes": fixture_bytes,
                    "path": str(fixture),
                    "sha256": fixture_digest,
                },
                "mandatory_case_count": len(runner.results),
                "ok": ok,
                "platform": {
                    "machine": platform.machine(),
                    "python": platform.python_version(),
                    "release": platform.release(),
                    "system": platform.system(),
                },
                "server_version": state.get("server_version"),
                "suite": SUITE_ID,
            }
        finally:
            if client is not None:
                client.close()
            if advanced is not None:
                advanced.close()


def parse_args(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", help="path to the exact stemma-mcp executable")
    parser.add_argument(
        "--fixture",
        default=str(DEFAULT_FIXTURE),
        help="licensed public DOCX fixture (default: repository safe-agreement sample)",
    )
    parser.add_argument(
        "--timeout-seconds",
        type=float,
        default=60.0,
        help="per-RPC timeout (default: 60)",
    )
    return parser.parse_args(argv)


def main(argv=None):
    started_at = utc_now()
    args = parse_args(argv)
    binary = Path(args.binary).expanduser().resolve()
    fixture = Path(args.fixture).expanduser().resolve()
    try:
        require(binary.is_file(), "MCP binary does not exist: {}".format(binary))
        require(fixture.is_file(), "DOCX fixture does not exist: {}".format(fixture))
        require(zipfile.is_zipfile(str(fixture)), "fixture is not a DOCX/ZIP package")
        require(args.timeout_seconds > 0, "timeout must be positive")
        summary = run_suite(binary, fixture, args.timeout_seconds)
        exit_code = 0 if summary["ok"] else 1
    except Exception as error:
        try:
            binary_identity = exact_file_identity(binary) if binary.is_file() else None
        except OSError:
            binary_identity = None
        summary = {
            "binary": str(binary),
            "binary_identity": binary_identity,
            "case_count": 0,
            "cases": [],
            "counts": {"passed": 0, "failed": 0, "blocked": 0},
            "fixture": {"path": str(fixture)},
            "mandatory_case_count": EXPECTED_CASE_COUNT,
            "ok": False,
            "suite": SUITE_ID,
            "suite_error": "{}: {}".format(type(error).__name__, error),
        }
        exit_code = 2
    summary["finished_at"] = utc_now()
    summary["started_at"] = started_at
    print(json.dumps(summary, indent=2, sort_keys=True))
    return exit_code


if __name__ == "__main__":
    sys.exit(main())
