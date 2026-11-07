import contextlib
import io
import json
import os
import subprocess
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from scripts import setup_agent_mcp as setup


URL = "http://127.0.0.1:6919/mcp"


class SetupAgentMcpTests(unittest.TestCase):
    def run_setup(self, *args):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            result = setup.main(list(args))
        return result, output.getvalue()

    def test_default_is_project_scoped_native_http(self):
        args = setup.parse_args([])
        self.assertEqual(args.transport, "http")
        self.assertEqual(args.config_scope, "project")
        self.assertIsNone(args.url)

    def test_each_supported_host_uses_its_native_http_schema(self):
        for agent in setup.HTTP_AGENTS:
            with self.subTest(agent=agent):
                entry = setup.build_entry(agent, url=URL, workspace=Path("/work/project"))
                self.assertEqual(entry["headers"], {
                    "x-tachi-client": agent,
                    "x-tachi-workspace-root": "/work/project",
                })
                self.assertNotIn("command", entry)
                self.assertNotIn("env", entry)
                self.assertNotIn("args", entry)
                if agent == "antigravity":
                    self.assertEqual(entry["serverUrl"], URL)
                    self.assertNotIn("url", entry)
                    self.assertNotIn("httpUrl", entry)
                elif agent == "gemini-cli":
                    self.assertEqual(entry["httpUrl"], URL)
                    self.assertNotIn("url", entry)  # Gemini's url selects SSE.
                else:
                    self.assertEqual(entry["url"], URL)
                    self.assertNotIn("httpUrl", entry)
                if agent == "claude-code":
                    self.assertEqual(entry["type"], "http")

    def test_persisted_http_endpoint_must_be_explicit_and_fixed(self):
        for url in (None, "http://127.0.0.1:0/mcp", "http://127.0.0.1/mcp",
                    "http://example.com:6919/mcp", "http://token@localhost:6919/mcp",
                    "http://localhost:6919/mcp?token=private", "http://localhost:6919/mcp#x",
                    "http://localhost:6919/sse", "http://localhost:99999/mcp",
                    "http://local\nhost:6919/mcp"):
            with self.subTest(url=url), self.assertRaises(ValueError):
                setup.validate_http_url(url)
        self.assertEqual(setup.validate_http_url(URL), URL)

    def test_http_migration_removes_process_and_stale_identity_fields(self):
        existing = {
            "command": "old-tachi", "args": ["serve"], "cwd": "/wrong/workspace",
            "env": {"MEMORY_DB_PATH": "/wrong/db", "PRIVATE_TOKEN": "private"},
            "headers": {"x-tachi-project": "old", "x-tachi-profile": "admin",
                        "x-tachi-internal-proxy-token": "private"},
            "headersHelper": "old helper", "type": "stdio", "httpUrl": "http://old/mcp",
            "serverUrl": "http://stale/mcp",
            "timeout": 7000, "disabled": False,
        }
        original = json.loads(json.dumps(existing))
        desired = setup.build_entry("claude-code", url=URL, workspace=Path("/work/current"))
        migrated = setup.merge_entry(existing, desired)
        self.assertEqual(existing, original)
        self.assertEqual(migrated, {**desired, "timeout": 7000, "disabled": False})
        self.assertNotIn("private", json.dumps(migrated))
        self.assertEqual(setup.merge_entry(migrated, desired), migrated)

    def test_stdio_refuses_before_creating_or_changing_config(self):
        with self.assertRaisesRegex(ValueError, "stdio has been retired"):
            setup.build_entry("antigravity", transport="stdio")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            config = root / ".mcp.json"
            before = '{"mcpServers":{"other":{"command":"keep"}}}'
            for exists in (False, True):
                if exists:
                    config.write_text(before)
                code, output = self.run_setup("--agent", "claude-code", "--workspace-root", str(root), "--transport", "stdio", "--apply")
                self.assertEqual(code, 1)
                self.assertIn("stdio has been retired", output)
                self.assertEqual(config.read_text() if exists else config.exists(), before if exists else False)
                self.assertEqual(list(root.glob("*.bak.*")), [])

    def test_project_apply_preserves_other_servers_and_is_idempotent(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            config = root / ".mcp.json"
            before = {"setting": "keep", "mcpServers": {
                "sigil-memory": {"command": "tachi", "env": {"SECRET": "private"}},
                "other": {"command": "other-server"},
            }}
            config.write_text(json.dumps(before))
            argv = ("--agent", "claude-code", "--workspace-root", str(root), "--url", URL)
            code, _ = self.run_setup(*argv)
            self.assertEqual(code, 0)
            self.assertEqual(json.loads(config.read_text()), before)
            self.assertEqual(list(root.glob("*.bak.*")), [])
            code, _ = self.run_setup(*argv, "--apply")
            self.assertEqual(code, 0)
            after = json.loads(config.read_text())
            self.assertEqual(after["setting"], "keep")
            self.assertEqual(after["mcpServers"]["other"], before["mcpServers"]["other"])
            self.assertNotIn("sigil-memory", after["mcpServers"])
            self.assertEqual(after["mcpServers"]["tachi"]["headers"]["x-tachi-workspace-root"], str(root.resolve()))
            backups = list(root.glob(".mcp.json.bak.*"))
            self.assertEqual(len(backups), 1)
            self.assertEqual(json.loads(backups[0].read_text()), before)
            self.assertEqual(self.run_setup(*argv, "--apply")[0], 0)
            self.assertEqual(list(root.glob(".mcp.json.bak.*")), backups)

    def test_new_project_config_is_created_only_on_apply(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            argv = ("--agent", "gemini-cli", "--workspace-root", str(root), "--url", URL)
            self.assertEqual(self.run_setup(*argv)[0], 0)
            self.assertFalse((root / ".gemini").exists())
            self.assertEqual(self.run_setup(*argv, "--apply")[0], 0)
            config = json.loads((root / ".gemini/settings.json").read_text())
            self.assertEqual(config["mcpServers"]["tachi"]["httpUrl"], URL)

    def test_user_config_never_infers_a_workspace_from_installer_cwd(self):
        with tempfile.TemporaryDirectory() as tmp:
            config = Path(tmp) / "user.json"
            config.write_text("{}")
            target = {"path": config, "server_key": "tachi"}
            with patch.dict(setup.AGENT_TARGETS, {"claude-code": target}):
                code, _ = self.run_setup("--agent", "claude-code", "--config-scope", "user",
                                         "--url", URL, "--apply")
            self.assertEqual(code, 0)
            headers = json.loads(config.read_text())["mcpServers"]["tachi"]["headers"]
            self.assertNotIn("x-tachi-workspace-root", headers)
            self.assertNotIn("x-tachi-project", headers)

    def test_two_workspaces_get_distinct_bindings_without_global_changes(self):
        with tempfile.TemporaryDirectory() as tmp:
            roots = [Path(tmp) / name for name in ("one", "two")]
            for root in roots:
                root.mkdir()
                with patch.object(setup.Path, "cwd", return_value=root):
                    self.assertEqual(self.run_setup("--agent", "cursor", "--url", URL, "--apply")[0], 0)
            values = [json.loads((root / ".cursor/mcp.json").read_text()) for root in roots]
            bindings = [value["mcpServers"]["tachi"]["headers"]["x-tachi-workspace-root"] for value in values]
            self.assertEqual(bindings, [str(root.resolve()) for root in roots])

    def test_invalid_transport_options_never_write_configuration(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            base = ("--agent", "claude-code", "--workspace-root", str(root), "--apply")
            for extra in (("--url", URL, "--memory-db-path", "/db"),
                          ("--url", URL, "--command", "tachi"),
                          ("--url", URL, "--enable-pipeline", "true"),
                          ("--transport", "stdio", "--url", URL),
                          ("--transport", "stdio", "--project", "named"), ()):
                with self.subTest(extra=extra):
                    self.assertEqual(self.run_setup(*base, *extra)[0], 1)
                    self.assertFalse((root / ".mcp.json").exists())

    def test_malformed_existing_servers_are_not_overwritten(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            config = root / ".mcp.json"
            original = '{"mcpServers": ["invalid"], "keep": true}'
            config.write_text(original)
            code, _ = self.run_setup("--agent", "claude-code", "--workspace-root", str(root),
                                     "--url", URL, "--apply")
            self.assertEqual(code, 1)
            self.assertEqual(config.read_text(), original)
            self.assertEqual(list(root.glob(".mcp.json.bak.*")), [])

    def test_explicit_unsupported_http_host_fails_without_stdio_fallback(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            code, output = self.run_setup("--agent", "trae", "--workspace-root", str(root),
                                          "--url", URL, "--apply")
            self.assertEqual(code, 1)
            self.assertIn("supported native HTTP host", output)
            self.assertEqual(list(root.iterdir()), [])

    def test_antigravity_project_config_uses_current_workspace_and_registered_name(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            with patch.object(setup.Path, "cwd", return_value=root):
                args = ("--agent", "antigravity", "--url", URL)
                self.assertEqual(self.run_setup(*args)[0], 0)
                self.assertFalse((root / ".agents").exists())
                self.assertEqual(self.run_setup(*args, "--apply")[0], 0)
                config = root / ".agents/mcp_config.json"
                entry = json.loads(config.read_text())["mcpServers"]["tachi"]
                self.assertEqual(entry, {"serverUrl": URL, "headers": {
                    "x-tachi-client": "antigravity", "x-tachi-workspace-root": str(root)}})
                self.assertEqual(self.run_setup(*args, "--project", "team-notes", "--apply")[0], 0)
                entry = json.loads(config.read_text())["mcpServers"]["tachi"]
                self.assertEqual(entry["headers"], {
                    "x-tachi-client": "antigravity", "x-tachi-project": "team-notes"})

    def test_installer_from_git_subdirectory_writes_checkout_config(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            nested = root / "src/component"
            nested.mkdir(parents=True)
            with patch.object(setup.Path, "cwd", return_value=nested):
                self.assertEqual(self.run_setup("--agent", "antigravity", "--url", URL, "--apply")[0], 0)
            self.assertFalse((nested / ".agents").exists())
            entry = json.loads((root / ".agents/mcp_config.json").read_text())["mcpServers"]["tachi"]
            self.assertEqual(entry["headers"]["x-tachi-workspace-root"], str(root))

    def test_worktree_config_stays_in_the_calling_checkout(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp).resolve()
            primary, worktree = base / "primary", base / "worktree"
            subprocess.run(["git", "init", "-q", str(primary)], check=True)
            subprocess.run(["git", "-C", str(primary), "-c", "user.name=Fixture",
                            "-c", "user.email=fixture@example.invalid", "commit", "-q", "--allow-empty", "-m", "fixture"], check=True)
            subprocess.run(["git", "-C", str(primary), "worktree", "add", "--detach", "-q", str(worktree)], check=True)
            nested = worktree / "src"
            nested.mkdir()
            with patch.object(setup.Path, "cwd", return_value=nested):
                self.assertEqual(self.run_setup("--agent", "antigravity", "--url", URL, "--apply")[0], 0)
            self.assertTrue((worktree / ".agents/mcp_config.json").is_file())
            self.assertFalse((primary / ".agents").exists())
            entry = json.loads((worktree / ".agents/mcp_config.json").read_text())["mcpServers"]["tachi"]
            self.assertEqual(entry["headers"]["x-tachi-workspace-root"], str(worktree))

    def test_non_git_subdirectory_reuses_existing_workspace_database(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            (root / ".tachi").mkdir()
            (root / ".tachi/tachi-memory.db").touch()
            nested = root / "notes/topic"
            nested.mkdir(parents=True)
            with patch.object(setup.Path, "cwd", return_value=nested):
                self.assertEqual(self.run_setup("--agent", "antigravity", "--url", URL, "--apply")[0], 0)
            self.assertTrue((root / ".agents/mcp_config.json").is_file())
            self.assertFalse((nested / ".agents").exists())

    def test_global_tachi_home_is_not_a_project_ancestor(self):
        for explicit in (False, True):
            for filename in ("memory.db", "tachi-memory.db"):
                with self.subTest(explicit=explicit, filename=filename), tempfile.TemporaryDirectory() as tmp:
                    base = Path(tmp).resolve()
                    home = base / "home"
                    ancestor = base / "configured" if explicit else home
                    tachi_home = ancestor / ".tachi"
                    tachi_home.mkdir(parents=True)
                    (tachi_home / filename).touch()
                    workspace = ancestor / "notes/new"
                    workspace.mkdir(parents=True)
                    environment = {"TACHI_HOME": str(tachi_home) if explicit else "",
                                   "SIGIL_HOME": "", "TACHI_APP_HOME": ""}
                    with patch.dict(os.environ, environment), \
                         patch.object(setup.Path, "home", return_value=home), \
                         patch.object(setup.Path, "cwd", return_value=workspace):
                        self.assertEqual(self.run_setup("--agent", "antigravity", "--url", URL, "--apply")[0], 0)
                    self.assertFalse((ancestor / ".agents").exists())
                    config = json.loads((workspace / ".agents/mcp_config.json").read_text())
                    self.assertEqual(config["mcpServers"]["tachi"]["headers"]["x-tachi-workspace-root"], str(workspace))

    def test_antigravity_global_config_does_not_capture_installer_workspace(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            config = root / "mcp_config.json"
            config.write_text('{"mcpServers":{"other":{"command":"keep"}}}')
            with patch.dict(setup.AGENT_TARGETS, {"antigravity": {"path": config, "server_key": "tachi"}}), \
                 patch.object(setup.Path, "cwd", return_value=root):
                self.assertEqual(self.run_setup("--agent", "antigravity", "--config-scope", "user", "--url", URL, "--apply")[0], 0)
            servers = json.loads(config.read_text())["mcpServers"]
            self.assertEqual(servers["tachi"], {"serverUrl": URL, "headers": {"x-tachi-client": "antigravity"}})
            self.assertEqual(servers["other"], {"command": "keep"})


if __name__ == "__main__":
    unittest.main()
