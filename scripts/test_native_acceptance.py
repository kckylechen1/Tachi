"""Acceptance evidence must identify a successful, completed native tool call."""
import json
import os
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import patch

from scripts import native_acceptance as acceptance


def codex_event(item, method="item/completed", direction="agent"):
    return {"direction": direction, "message": {"method": method, "params": {"item": item}}}


def acp_event(update, session="session-1"):
    return {"direction": "agent", "message": {
        "method": "session/update", "params": {"sessionId": session, "update": update},
    }}


class WorkerEvidenceTests(unittest.TestCase):
    def test_codex_requires_completed_success_from_the_injected_tool(self):
        item = {"id": "call-1", "type": "mcpToolCall", "server": "tachi",
                "tool": "tachi_memory", "arguments": {"action": "briefing"},
                "status": "completed", "error": None}
        self.assertTrue(acceptance.worker_used([codex_event(item)], "mcp"))
        rejected = [
            codex_event({**item, "status": "inProgress"}, method="item/started"),
            codex_event(item, method="item/started"),
            codex_event({**item, "status": "failed"}),
            codex_event({**item, "server": "other"}),
            codex_event({**item, "tool": "tachi_staff"}),
            codex_event({**item, "arguments": {"action": "search"}}),
            codex_event({**item, "error": {"message": "MCP request failed"}}),
            codex_event(item, direction="client"),
        ]
        for event in rejected:
            with self.subTest(event=event):
                self.assertFalse(acceptance.worker_used([event], "mcp"))

    def test_codex_shell_requires_successful_terminal_event(self):
        item = {"id": "call-1", "type": "commandExecution", "status": "completed", "exitCode": 0}
        self.assertTrue(acceptance.worker_used([codex_event(item)], "shell"))
        for event in (codex_event(item, method="item/started"),
                      codex_event({**item, "status": "failed"}),
                      codex_event({**item, "exitCode": 1})):
            with self.subTest(event=event):
                self.assertFalse(acceptance.worker_used([event], "shell"))

    def test_acp_completion_inherits_tool_identity_and_input(self):
        start = acp_event({"sessionUpdate": "tool_call", "toolCallId": "memory",
                           "name": "tachi_tachi_memory", "title": "tachi_tachi_memory",
                           "kind": "other", "status": "pending", "rawInput": {"action": "briefing"}})
        completed = acp_event({"sessionUpdate": "tool_call_update", "toolCallId": "memory",
                               "status": "completed", "content": [], "rawOutput": {}})
        self.assertFalse(acceptance.worker_used([start], "mcp"))
        self.assertFalse(acceptance.worker_used([completed], "mcp"))
        self.assertTrue(acceptance.worker_used([start, completed], "mcp"))
        failed = acp_event({"sessionUpdate": "tool_call_update", "toolCallId": "memory",
                            "status": "failed", "title": "tachi_tachi_memory",
                            "rawOutput": {"error": "MCP unavailable"}})
        self.assertFalse(acceptance.worker_used([start, failed], "mcp"))

    def test_legacy_acp_preserves_tool_name_when_completed_title_changes(self):
        events = [
            acp_event({"sessionUpdate": "tool_call", "toolCallId": "memory",
                       "title": "tachi_tachi_memory", "kind": "other", "status": "pending", "rawInput": {}}),
            acp_event({"sessionUpdate": "tool_call_update", "toolCallId": "memory",
                       "title": "tachi_tachi_memory", "status": "in_progress",
                       "rawInput": {"action": "briefing"}}),
            acp_event({"sessionUpdate": "tool_call_update", "toolCallId": "memory",
                       "title": "Workspace briefing", "status": "completed", "kind": "other",
                       "rawInput": {"action": "briefing"}, "rawOutput": {"output": "briefing"}}),
        ]
        self.assertFalse(acceptance.worker_used(events[:2], "mcp"))
        self.assertTrue(acceptance.worker_used(events, "mcp"))

    def test_acp_rejects_mentions_other_tools_and_other_actions(self):
        for name, arguments in (("other_tachi_memory", {"action": "briefing"}),
                                ("tachi_tachi_staff", {"action": "briefing"}),
                                ("tachi_tachi_memory", {"action": "search"}),
                                ("shell", {"command": "echo tachi_tachi_memory"})):
            events = [
                acp_event({"sessionUpdate": "tool_call", "toolCallId": "call",
                           "name": name, "title": name, "status": "pending", "rawInput": arguments}),
                acp_event({"sessionUpdate": "tool_call_update", "toolCallId": "call", "status": "completed",
                           "content": [{"type": "content", "content": {"type": "text", "text": "tachi_memory"}}]}),
            ]
            with self.subTest(name=name, arguments=arguments):
                self.assertFalse(acceptance.worker_used(events, "mcp"))

    def test_acp_does_not_combine_different_calls_or_sessions(self):
        start = acp_event({"sessionUpdate": "tool_call", "toolCallId": "memory",
                           "name": "tachi_tachi_memory", "status": "pending", "rawInput": {"action": "briefing"}})
        for call_id, session in (("other", "session-1"), ("memory", "session-2")):
            completed = acp_event({"sessionUpdate": "tool_call_update", "toolCallId": call_id,
                                   "status": "completed"}, session=session)
            with self.subTest(call_id=call_id, session=session):
                self.assertFalse(acceptance.worker_used([start, completed], "mcp"))

    def test_acp_shell_completion_inherits_execute_kind(self):
        start = acp_event({"sessionUpdate": "tool_call", "toolCallId": "shell",
                           "name": "shell", "kind": "execute", "status": "pending",
                           "rawInput": {"command": "git rev-parse --is-inside-work-tree"}})
        for status in ("completed", "failed"):
            completed = acp_event({"sessionUpdate": "tool_call_update", "toolCallId": "shell",
                                   "status": status, "rawOutput": {}})
            with self.subTest(status=status):
                self.assertEqual(acceptance.worker_used([start, completed], "shell"), status == "completed")


class HostCredentialFingerprintTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.env = patch.dict(os.environ, {"XDG_DATA_HOME": self.directory.name})
        self.env.start()
        self.addCleanup(self.env.stop)
        self.store = Path(self.directory.name) / "opencode"
        self.store.mkdir()
        self.db = self.store / "opencode.db"
        self.auth = self.store / "auth.json"

    def test_legacy_auth_without_db_is_fingerprinted_without_exposing_material(self):
        self.auth.write_text(json.dumps({"openai": {"type": "api", "key": "fixture-secret"}}))
        fingerprint = acceptance.host_credential_fingerprint("opencode")
        self.assertNotEqual(fingerprint, "absent")
        self.assertNotIn("fixture-secret", fingerprint)
        self.assertFalse(self.db.exists())
        self.auth.write_text(json.dumps({"openai": {"type": "api", "key": "changed-fixture-secret"}}))
        self.assertNotEqual(acceptance.host_credential_fingerprint("opencode"), fingerprint)

    def test_pre_v2_session_database_falls_back_to_legacy_auth(self):
        with sqlite3.connect(self.db) as connection:
            connection.execute("CREATE TABLE session (id TEXT)")
        self.auth.write_text('{"openai":{"type":"api","key":"fixture-secret"}}')
        before = (self.db.read_bytes(), self.auth.read_bytes())
        fingerprint = acceptance.host_credential_fingerprint("opencode")
        self.assertNotEqual(fingerprint, "absent")
        self.assertEqual((self.db.read_bytes(), self.auth.read_bytes()), before)
        self.auth.write_text('{"openai":{"type":"api","key":"changed-fixture-secret"}}')
        self.assertNotEqual(acceptance.host_credential_fingerprint("opencode"), fingerprint)

    def test_v2_active_credential_update_changes_fingerprint(self):
        with sqlite3.connect(self.db) as connection:
            connection.execute("CREATE TABLE credential (integration_id TEXT, time_updated INTEGER, active INTEGER)")
            connection.execute("INSERT INTO credential VALUES ('openai', 100, 1)")
        before = self.db.read_bytes()
        fingerprint = acceptance.host_credential_fingerprint("opencode")
        self.assertEqual(self.db.read_bytes(), before)
        with sqlite3.connect(self.db) as connection:
            connection.execute("UPDATE credential SET time_updated = 101")
        self.assertNotEqual(acceptance.host_credential_fingerprint("opencode"), fingerprint)

    def test_unsupported_credential_layout_is_not_silently_accepted_as_legacy(self):
        with sqlite3.connect(self.db) as connection:
            connection.execute("CREATE TABLE credential (unexpected TEXT)")
        with self.assertRaises(sqlite3.OperationalError):
            acceptance.host_credential_fingerprint("opencode")


if __name__ == "__main__":
    unittest.main()
