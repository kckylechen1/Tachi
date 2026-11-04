"""Retirement refusal and preserved Hub inventory planning guards."""
import contextlib
import io
import json
import os
import subprocess
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from scripts import register_mcps_to_hub as registrar
from scripts import load_skills_to_hub as loader


class RegistrarTests(unittest.TestCase):
    def test_write_modes_refuse_before_subprocess_or_config_write(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            config = root / "mcp.json"
            before = '{"mcpServers":{"direct":{"command":"keep"}}}'
            config.write_text(before)
            for argv in ([], ["--sync-only"], ["--no-sync-agent-config"],
                         ["--agent-transport", "stdio"], ["--scope", "project"],
                         ["--disable-hidden", "--no-sync-agent-config"]):
                for profile in ("", "standard", "ops", "admin"):
                    with self.subTest(argv=argv, profile=profile), \
                         patch.dict(os.environ, {"HOME": tmp, "TACHI_PROFILE": profile}), \
                         patch("sys.argv", ["registrar", *argv]), \
                         patch.object(registrar.subprocess, "Popen") as spawn, \
                         patch.object(registrar.subprocess, "run") as run, \
                         contextlib.redirect_stderr(io.StringIO()) as err:
                        self.assertFalse(registrar.main())
                        self.assertIn("stdio has been retired", err.getvalue())
                        self.assertIn("does not reproduce every batch scope/policy", err.getvalue())
                        spawn.assert_not_called()
                        run.assert_not_called()
                        self.assertEqual(config.read_text(), before)
                        self.assertEqual(sorted(p.name for p in root.iterdir()), ["mcp.json"])

    def test_dry_run_preserves_third_party_stdio_and_policy_without_starting(self):
        with patch("sys.argv", ["registrar", "--dry-run"]), \
             patch.object(registrar.subprocess, "Popen") as spawn, \
             contextlib.redirect_stdout(io.StringIO()) as out:
            self.assertTrue(registrar.main())
            plans = [json.loads(line) for line in out.getvalue().splitlines()]
        spawn.assert_not_called()
        self.assertEqual([p["id"] for p in plans], [m["id"] for m in registrar.MCP_SERVERS])
        self.assertEqual(plans[0]["definition"]["transport"], "stdio")
        self.assertEqual(plans[0]["definition"]["command"], "npx")
        self.assertEqual(plans[2]["definition"]["policy"]["owner_agent"], "stock")

    def inventory(self):
        return [{"id": m["id"], "cap_type": "mcp", "db": "global", "callable": True,
                 "enabled": True, "review_status": "approved"} for m in registrar.MCP_SERVERS]

    def test_inventory_requires_exact_scoped_approved_enabled_callable_evidence(self):
        registrar.confirm_hub_mcp_servers(self.inventory(), "global")
        for value in (None, {}, [], [None]):
            with self.subTest(value=value), self.assertRaises(RuntimeError):
                registrar.confirm_hub_mcp_servers(value, "global")
        for field, invalid in (("db", "project"), ("cap_type", "skill"),
                               ("callable", 1), ("callable", False),
                               ("enabled", 1), ("enabled", False),
                               ("review_status", "pending")):
            for missing in (False, True):
                items = self.inventory()
                if missing: items[0].pop(field)
                else: items[0][field] = invalid
                with self.subTest(field=field, invalid=invalid, missing=missing), self.assertRaises(RuntimeError):
                    registrar.confirm_hub_mcp_servers(items, "global")
        items = self.inventory()
        for malformed in (items[1:], [*items, items[0]]):
            with self.assertRaises(RuntimeError):
                registrar.confirm_hub_mcp_servers(malformed, "global")

    def test_tool_evidence_rejects_transport_payload_and_shape_errors(self):
        good = {"result": {"content": [{"type": "text", "text": "[]"}]}}
        self.assertEqual(registrar.tool_result(good), [])
        for response in (None, {"error": {}}, {"result": {"isError": True}},
                         {"result": {"content": []}},
                         {"result": {"content": [{"type": "text", "text": "!"}]}},
                         {"result": {"content": [{"type": "text", "text": '{"error":"blocked"}'}]}}):
            with self.subTest(response=response), self.assertRaises(RuntimeError):
                registrar.tool_result(response)

    def test_skill_loader_keeps_planning_and_refuses_all_writes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            skill = root / "example" / "SKILL.md"
            skill.parent.mkdir()
            skill.write_text("---\nname: Example\n---\nSynthetic skill")
            for dry in (False, True):
                with patch("sys.argv", ["loader", "--skills-dir", tmp, *(["--dry-run"] if dry else [])]), \
                     patch.object(loader.subprocess, "Popen") as spawn, \
                     contextlib.redirect_stdout(io.StringIO()) as out, \
                     contextlib.redirect_stderr(io.StringIO()) as err:
                    self.assertEqual(loader.main(), 0 if dry else 1)
                    self.assertIn("PLAN" if dry else "stdio has been retired", out.getvalue() if dry else err.getvalue())
                    spawn.assert_not_called()
                    self.assertEqual(skill.read_text(), "---\nname: Example\n---\nSynthetic skill")


class InstallerRetirementTests(unittest.TestCase):
    def test_legacy_plugin_option_refuses_before_any_install_command(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            commands = root / "bin"
            commands.mkdir()
            marker = root / "command-started"
            for name in ("brew", "curl", "node", "npm", "launchctl", "tachi"):
                command = commands / name
                command.write_text('#!/bin/sh\necho started >> "$INSTALL_TEST_MARKER"\nexit 99\n')
                command.chmod(0o700)
            env = {**os.environ, "HOME": str(root), "PATH": str(commands),
                   "INSTALL_TEST_MARKER": str(marker)}
            result = subprocess.run(
                ["/bin/bash", str(Path(__file__).with_name("install.sh")), "--dir", str(root / "plugin")],
                env=env, capture_output=True, text=True, check=False,
            )
            self.assertEqual(result.returncode, 1)
            self.assertIn("plugin installation is retired", result.stderr)
            self.assertFalse(marker.exists())
            self.assertEqual(sorted(path.name for path in root.iterdir()), ["bin"])


if __name__ == "__main__":
    unittest.main()
