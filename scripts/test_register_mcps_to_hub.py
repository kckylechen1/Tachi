"""Retirement refusal and preserved Hub planning guards."""
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
                         patch.object(subprocess, "Popen") as spawn, \
                         patch.object(subprocess, "run") as run, \
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
             patch.object(subprocess, "Popen") as spawn, \
             contextlib.redirect_stdout(io.StringIO()) as out:
            self.assertTrue(registrar.main())
            plans = [json.loads(line) for line in out.getvalue().splitlines()]
        spawn.assert_not_called()
        self.assertEqual([p["id"] for p in plans], [m["id"] for m in registrar.MCP_SERVERS])
        self.assertEqual(plans[0]["definition"]["transport"], "stdio")
        self.assertEqual(plans[0]["definition"]["command"], "npx")
        self.assertEqual(plans[2]["definition"]["policy"]["owner_agent"], "stock")

    def test_skill_loader_keeps_planning_and_refuses_all_writes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            skill = root / "example" / "SKILL.md"
            skill.parent.mkdir()
            skill.write_text("---\nname: Example\n---\nSynthetic skill")
            for dry in (False, True):
                with patch("sys.argv", ["loader", "--skills-dir", tmp, *(["--dry-run"] if dry else [])]), \
                     patch.object(subprocess, "Popen") as spawn, \
                     contextlib.redirect_stdout(io.StringIO()) as out, \
                     contextlib.redirect_stderr(io.StringIO()) as err:
                    self.assertEqual(loader.main(), 0 if dry else 1)
                    self.assertIn("PLAN" if dry else "stdio has been retired", out.getvalue() if dry else err.getvalue())
                    spawn.assert_not_called()
                    self.assertEqual(skill.read_text(), "---\nname: Example\n---\nSynthetic skill")


class InstallerRetirementTests(unittest.TestCase):
    def test_legacy_plugin_installer_refuses_without_forwarding(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            wrapper = root / "install_openclaw_ext.sh"
            wrapper.write_text(Path(__file__).with_name(wrapper.name).read_text())
            marker = root / "install-started"
            installer = root / "install.sh"
            installer.write_text('#!/bin/sh\necho started > "$INSTALL_TEST_MARKER"\n')
            installer.chmod(0o700)
            env = {**os.environ, "INSTALL_TEST_MARKER": str(marker)}
            for args in ([], ["--help"], ["--dir", str(root / "plugin")]):
                with self.subTest(args=args):
                    result = subprocess.run(
                        ["/bin/bash", str(wrapper), *args], env=env,
                        capture_output=True, text=True, check=False,
                    )
                    self.assertEqual(result.returncode, 1)
                    self.assertIn("plugin is retired", result.stderr)
                    self.assertFalse(marker.exists())
                    self.assertEqual(sorted(path.name for path in root.iterdir()),
                                     ["install.sh", "install_openclaw_ext.sh"])

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


class HomebrewInstallerTests(unittest.TestCase):
    def run_installer(self, args, settings=None):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            marker = root / "commands"
            for name, body in {
                "brew": 'printf "brew %s\\n" "$*" >> "$INSTALL_TEST_MARKER"\n'
                        '[ "$1" != list ]\n',
                "tachi": 'printf "tachi 2.0.0-fixture\\n"\n',
                "curl": 'echo curl >> "$INSTALL_TEST_MARKER"; exit 99\n',
                "node": 'echo node >> "$INSTALL_TEST_MARKER"; exit 99\n',
            }.items():
                command = root / name
                command.write_text("#!/bin/sh\n" + body)
                command.chmod(0o700)
            env = {**os.environ, "PATH": str(root) + os.pathsep + os.defpath,
                   "TACHI_VERSION": "latest", "TACHI_DAEMON_PORT": "6919",
                   "INSTALL_TEST_MARKER": str(marker), **(settings or {})}
            result = subprocess.run(
                ["/bin/bash", str(Path(__file__).with_name("install.sh")),
                 "--skip-daemon-service", *args],
                env=env, capture_output=True, text=True, check=False,
            )
            return result, marker.read_text().splitlines() if marker.exists() else []

    def test_release_pins_refuse_before_install_side_effects(self):
        for args, settings in ((["--version", "1.2.0"], {}),
                               ([], {"TACHI_VERSION": "2.0.0"}),
                               (["--version"], {})):
            with self.subTest(args=args, settings=settings):
                result, calls = self.run_installer(args, settings)
                self.assertEqual(result.returncode, 1)
                self.assertIn("supports only", result.stderr)
                self.assertEqual(calls, [])

    def test_ephemeral_and_invalid_ports_refuse_before_install_side_effects(self):
        for port in ("0", "65536", "6919/mcp", "9" * 30):
            with self.subTest(port=port):
                result, calls = self.run_installer([], {"TACHI_DAEMON_PORT": port})
                self.assertEqual(result.returncode, 1)
                self.assertIn("fixed port from 1 to 65535", result.stderr)
                self.assertEqual(calls, [])

    def test_latest_installs_formula_and_reports_binary_without_release_lookup(self):
        result, calls = self.run_installer(
            ["--version", "latest"], {"TACHI_DAEMON_PORT": "06920"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls, ["brew tap kckylechen1/tachi", "brew list tachi",
                                 "brew install tachi"])
        self.assertIn("Tachi version: tachi 2.0.0-fixture", result.stdout)
        self.assertIn("http://127.0.0.1:6920/mcp", result.stdout)


if __name__ == "__main__":
    unittest.main()
