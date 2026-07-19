#!/usr/bin/env python3
"""End-to-end smoke test for stemma-mcp over the real MCP stdio protocol.

Spawns the built binary, performs the MCP handshake, then drives a full
edit round-trip: open -> read outline -> apply a tracked-change edit ->
save -> reopen the saved file to confirm the edit is present and the file
is still a valid DOCX the engine can parse.

Usage: python3 smoke_test.py /path/to/stemma-mcp /path/to/input.docx
"""

import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

# The MCP wire and the fixture text are UTF-8; without pinning, Python uses
# the locale codec for pipes (cp1252 on Windows CI), which corrupts both the
# server's receipts and this script's own progress output.
sys.stdout.reconfigure(encoding="utf-8")
sys.stderr.reconfigure(encoding="utf-8")


class McpClient:
    def __init__(self, binary, env=None):
        self.proc = subprocess.Popen(
            [binary],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            encoding="utf-8",
            bufsize=1,
            env=env,
        )
        self._id = 0

    def _send(self, method, params=None, is_notification=False):
        msg = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            msg["params"] = params
        if not is_notification:
            self._id += 1
            msg["id"] = self._id
        self.proc.stdin.write(json.dumps(msg) + "\n")
        self.proc.stdin.flush()
        if is_notification:
            return None
        return self._read_result(self._id)

    def _read_result(self, want_id):
        while True:
            line = self.proc.stdout.readline()
            if not line:
                raise RuntimeError("server closed stdout unexpectedly")
            try:
                msg = json.loads(line)
            except json.JSONDecodeError:
                continue
            if msg.get("id") == want_id:
                if "error" in msg:
                    raise RuntimeError(f"RPC error: {msg['error']}")
                return msg["result"]

    def initialize(self):
        result = self._send(
            "initialize",
            {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "smoke", "version": "0"},
            },
        )
        self._send("notifications/initialized", {}, is_notification=True)
        return result

    def list_tools(self):
        return self._send("tools/list", {})

    def call(self, name, arguments):
        result = self._send("tools/call", {"name": name, "arguments": arguments})
        # structured_content carries our JSON payload; fall back to text content.
        if result.get("structuredContent") is not None:
            payload = result["structuredContent"]
        else:
            payload = json.loads(result["content"][0]["text"])
        return result.get("isError", False), payload

    def close(self):
        self.proc.stdin.close()
        self.proc.terminate()
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()


def fail(msg):
    print(f"FAIL: {msg}")
    sys.exit(1)


def status_name(block_status):
    """The block's tracking status as a bare string.

    An index row's `block_status` is either the string "normal" or an object
    whose `status` field names the tracked state ("inserted", "deleted", ...).
    """
    if isinstance(block_status, str):
        return block_status
    return block_status["status"]


def main():
    binary, source_docx = sys.argv[1], sys.argv[2]
    workspace = tempfile.TemporaryDirectory(prefix="stemma-mcp-smoke-")
    docx = str(Path(workspace.name) / "input.docx")
    shutil.copy2(source_docx, docx)
    env = os.environ.copy()
    env["STEMMA_MCP_WORKSPACE_ROOT"] = workspace.name
    profile = env.get("STEMMA_MCP_PROFILE", "core")
    expected_version = subprocess.check_output(
        [binary, "--version"], env=env, text=True, encoding="utf-8"
    ).strip()
    client = McpClient(binary, env=env)
    try:
        info = client.initialize()
        if info["serverInfo"]["version"] != expected_version:
            fail(
                "initialize build identity disagrees with --version: "
                f"{info['serverInfo']['version']} != {expected_version}"
            )
        print(f"initialized: server={info['serverInfo']['name']} "
              f"protocol={info['protocolVersion']}")

        tools = client.list_tools()
        names = sorted(t["name"] for t in tools["tools"])
        print(f"tools: {names}")
        # Assert the FULL composed surface is reachable over the wire, not just
        # the base open/read/edit/save router. The agentic + read-projection +
        # read-index routers are merged onto the server's instance router; if
        # `#[tool_handler]` is left pointing at the default `Self::tool_router()`
        # they compile but never register, so list_tools/call_tool only ever see
        # the base tools. This set is the wire-level guard for that regression.
        advanced_expected = {
            # base
            "open_docx", "read_outline", "read_markdown", "read_block", "find",
            "get_section", "apply_edit", "save_docx", "compare_docx", "replace_all",
            # literal find/replace helpers
            "replace_text", "replace_text_batch",
            # read projections
            "read_text", "read_accepted", "read_rejected", "read_redline",
            # read index / windowing / styles / revisions
            "read_index", "read_window", "read_html", "read_styles", "list_revisions",
            # agentic surface
            "accept_changes", "reject_changes", "check_edit", "validate_docx",
            "apply_batch",
            # session review + stateless audit
            "review_session", "audit_docx",
            # compact semantic front end
            "inspect_docx", "execute_plan", "verify_docx",
        }
        core_expected = {
            "open_docx", "inspect_docx", "execute_plan", "verify_docx", "save_docx",
        }
        expected = core_expected if profile == "core" else advanced_expected
        if set(names) != expected:
            fail(f"{profile} tool surface drifted; expected {sorted(expected)}; got {names}")

        # 1. open
        is_err, opened = client.call("open_docx", {"path": docx})
        if is_err:
            fail(f"open_docx error: {opened}")
        if opened["input_artifact"]["digest"]["algorithm"] != "sha256":
            fail(f"open receipt did not report SHA-256 identity: {opened}")
        if opened["server_version"] != expected_version:
            fail(f"open receipt build identity mismatch: {opened}")
        index = opened["index"]
        print(f"opened doc_id={opened['doc_id']} block_count={opened['block_count']}")

        # The compact profile must expose the complete typed transaction
        # vocabulary over the real wire. Exercise both parser-derived fields
        # and the MCP-only image-path alternative (resolved before parsing).
        if profile == "core":
            is_err, catalog = client.call(
                "inspect_docx",
                {"doc_id": opened["doc_id"], "query": "operations"},
            )
            if is_err:
                fail(f"operation catalog error: {catalog}")
            operations = {row["name"]: row for row in catalog["operations"]}
            if catalog["operation_count"] != len(operations):
                fail(f"operation catalog count disagrees with rows: {catalog}")
            required_ops = {
                "replace", "move", "table_op", "edit_note", "comment_create",
                "set_image_attrs", "insert_image", "create_style",
                "set_page_setup", "set_numbering",
            }
            missing = required_ops - operations.keys()
            if missing:
                fail(f"operation catalog omitted required vocabulary: {sorted(missing)}")
            image = operations["insert_image"]
            if "bytes_base64" not in image["parser_fields"]:
                fail(f"image parser vocabulary is incomplete: {image}")
            if image["mcp_edge_fields"] != ["path"] or "path" not in image["accepted_fields"]:
                fail(f"image MCP-edge vocabulary is incomplete: {image}")
            if len(catalog["legacy_surface_routes"]) != 26:
                fail(f"historical surface route map is incomplete: {catalog}")
            print(f"catalogued {len(operations)} transaction operations")

        # pick the first normal paragraph with a usable run of text
        target = next(
            (b for b in index
             if b["role"] == "paragraph" and status_name(b["block_status"]) == "normal"
             and len(b["text_preview"].strip()) > 3),
            None,
        )
        if not target:
            fail("no editable paragraph found in sample")
        expect_word = target["text_preview"].strip().split()[0]
        print(f"target block={target['id']} expect='{expect_word}' "
              f"text={target['text_preview'][:60]!r}")

        new_text = "STEMMA EDIT: " + target["text_preview"]

        # 2. apply a tracked-change replace
        txn = {
            "ops": [{
                "op": "replace",
                "target": target["id"],
                "expect": expect_word,
                "content": {
                    "type": "paragraph",
                    "content": [{"type": "text", "text": new_text}],
                },
            }],
            "revision": {"author": "smoke-test"},
            "summary": "smoke test replace",
        }
        edit_tool = "execute_plan" if profile == "core" else "apply_edit"
        edit_args = {"doc_id": opened["doc_id"], "transaction": txn}
        if profile == "core":
            edit_args["preview"] = False
        is_err, applied = client.call(edit_tool, edit_args)
        if is_err:
            fail(f"{edit_tool} error: {applied}")
        if not applied.get("applied"):
            fail(f"{edit_tool} did not apply: {applied}")
        print(f"applied edit; doc now has {applied['block_count']} blocks")

        # 3. test fail-loud: a stale expect must be rejected
        stale_txn = dict(txn)
        stale_txn["ops"] = [dict(txn["ops"][0], expect="this string is definitely not present zzz")]
        stale_args = {"doc_id": opened["doc_id"], "transaction": stale_txn}
        if profile == "core":
            stale_args["preview"] = False
        is_err, stale = client.call(edit_tool, stale_args)
        if not is_err:
            fail(f"stale edit should have failed but succeeded: {stale}")
        print(f"stale edit correctly rejected: code={stale.get('code')}")

        if profile == "core":
            is_err, verified = client.call("verify_docx", {"doc_id": opened["doc_id"]})
            if is_err or not verified.get("validator", {}).get("ok"):
                fail(f"verify_docx did not certify the open session: {verified}")

        # 4. save
        out = str(Path(workspace.name) / "output.docx")
        is_err, saved = client.call("save_docx", {"doc_id": opened["doc_id"], "path": out})
        if is_err:
            fail(f"save_docx error: {saved}")
        committed = Path(out).read_bytes()
        actual_hash = hashlib.sha256(committed).hexdigest()
        artifact = saved["output_artifact"]
        if artifact["identity"]["digest"]["hex"] != actual_hash:
            fail(f"save receipt hash does not match committed bytes: {saved}")
        if artifact["collision_policy"] != "create_new" or artifact["disposition"] != "created":
            fail(f"save receipt did not report create-new policy: {saved}")
        if saved["server_version"] != expected_version:
            fail(f"save receipt build identity mismatch: {saved}")
        print(f"saved {saved['bytes_written']} bytes to {out}")

        # The same destination must refuse, and the first committed artifact
        # must remain byte-identical after that refusal.
        is_err, collision = client.call(
            "save_docx", {"doc_id": opened["doc_id"], "path": out}
        )
        if not is_err or collision.get("code") != "artifact_output_exists":
            fail(f"second save should refuse an existing destination: {collision}")
        if Path(out).read_bytes() != committed:
            fail("existing destination changed after a refused second save")

        # 5. reopen the saved file -> proves it's a valid DOCX the engine parses,
        #    and that the inserted text is present.
        is_err, reopened = client.call("open_docx", {"path": out})
        if is_err:
            fail(f"reopen error: {reopened}")
        all_text = " ".join(b["text_preview"] for b in reopened["index"])
        if "STEMMA EDIT:" not in all_text:
            fail("inserted text not found in reopened doc")
        inserted_blocks = [
            b for b in reopened["index"]
            if status_name(b["block_status"]) == "inserted"
        ]
        print(f"reopened saved file: {reopened['block_count']} blocks, "
              f"{len(inserted_blocks)} marked inserted, inserted text present ✓")

        print("\nPASS: full open -> edit -> fail-loud -> save -> reopen round-trip works")
    finally:
        client.close()
        workspace.cleanup()


if __name__ == "__main__":
    main()
