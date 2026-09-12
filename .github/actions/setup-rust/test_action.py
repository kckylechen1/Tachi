import pathlib
import json
import os
import shutil
import subprocess
import tempfile
import textwrap
import unittest


ACTION = pathlib.Path(__file__).with_name("action.yml")


class SetupRustActionContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.action = ACTION.read_text(encoding="utf-8")

    def test_pinned_toolchain_controls_current_and_future_command_lookup(self):
        locate = "rustup which --toolchain $toolchain rustc"
        current_path = '$env:PATH = "$toolchainBin$([IO.Path]::PathSeparator)$env:PATH"'
        future_path = "Add-Content -Path $env:GITHUB_PATH -Value $toolchainBin"
        future_toolchain = 'Add-Content -Path $env:GITHUB_ENV -Value "RUSTUP_TOOLCHAIN=$toolchain"'

        self.assertIn(locate, self.action)
        self.assertIn("$toolchainBin = Split-Path -Parent $rustcPath", self.action)
        self.assertIn('$env:RUSTUP_TOOLCHAIN = $toolchain', self.action)
        self.assertIn(current_path, self.action)
        self.assertIn(future_path, self.action)
        self.assertIn(future_toolchain, self.action)
        self.assertLess(self.action.index(locate), self.action.index(current_path))
        self.assertLess(self.action.index(current_path), self.action.index("& rustc --version"))

    def test_plain_commands_must_exactly_match_rustup_selected_versions(self):
        self.assertIn("$actualRustcVersion -ne $expectedRustcVersion", self.action)
        self.assertIn("$actualCargoVersion -ne $expectedCargoVersion", self.action)
        self.assertIn("rustc $([regex]::Escape($toolchain))", self.action)
        self.assertIn("throw \"Effective rustc version", self.action)
        self.assertIn("throw \"Effective cargo version", self.action)



@unittest.skipUnless(shutil.which("pwsh"), "PowerShell is required for action execution fixtures")
class SetupRustActionExecutionTests(unittest.TestCase):
    # Execute the actual composite action body, with every tool command replaced
    # in this child PowerShell process. Never resolve the machine's Rust tools.
    def run_action(self, scenario="ready", targets="", auto_install=None):
        action = ACTION.read_text(encoding="utf-8")
        marker = "      run: |\n"
        self.assertEqual(action.count(marker), 1)
        body = textwrap.dedent(action.split(marker, 1)[1])
        with tempfile.TemporaryDirectory(prefix="setup-rust-action-") as temporary:
            root = pathlib.Path(temporary)
            binary_dir = root / "toolchain bin"
            binary_dir.mkdir()
            for binary in ("rustc", "cargo", "rustdoc", "cargo-clippy", "clippy-driver", "rustfmt"):
                (binary_dir / binary).touch()
            (root / "rust-toolchain.toml").write_text('[toolchain]\nchannel = "1.97.0"\n')
            environment = os.environ.copy()
            # Private settings and artifacts; command shims cannot fall through.
            for key in ("RUSTUP_TOOLCHAIN", "RUSTUP_AUTO_INSTALL"):
                environment.pop(key, None)
            environment.update(
                GITHUB_WORKSPACE=str(root), GITHUB_PATH=str(root / "github-path"),
                GITHUB_ENV=str(root / "github-env"), INPUT_TARGETS=targets,
                FIXTURE_SCENARIO=scenario, FIXTURE_BIN=str(binary_dir),
                FIXTURE_LOG=str(root / "commands.jsonl"), FIXTURE_STATE=str(root / "state.json"),
                FIXTURE_PWSH=shutil.which("pwsh"),
                RUSTUP_HOME=str(root / "rustup"), CARGO_HOME=str(root / "cargo"),
            )
            if auto_install is not None:
                environment["RUSTUP_AUTO_INSTALL"] = auto_install
            script = root / "fixture.ps1"
            script.write_text(self.SHIMS + "\ntry {\n" + body + r'''
} catch {
  [Console]::Error.WriteLine($_.Exception.Message)
  exit 1
} finally {
  @{ present = (Test-Path Env:RUSTUP_AUTO_INSTALL); value = $env:RUSTUP_AUTO_INSTALL } |
    ConvertTo-Json -Compress | Set-Content -LiteralPath $env:FIXTURE_STATE
}
''', encoding="utf-8")
            result = subprocess.run(
                [shutil.which("pwsh"), "-NoLogo", "-NoProfile", "-NonInteractive", "-File", str(script)],
                cwd=root, env=environment, capture_output=True, text=True, timeout=30,
            )
            commands = [json.loads(line) for line in (root / "commands.jsonl").read_text().splitlines()]
            state = json.loads((root / "state.json").read_text())
            paths = (root / "github-path").read_text() if (root / "github-path").exists() else ""
            future = (root / "github-env").read_text() if (root / "github-env").exists() else ""
            return result, commands, state, paths, future

    SHIMS = r'''
$global:fixtureInstalled = $false
function Record-Command([string]$Command, [object[]]$Arguments) {
  @{ command = $Command; arguments = @($Arguments); auto = $env:RUSTUP_AUTO_INSTALL } |
    ConvertTo-Json -Compress | Add-Content -LiteralPath $env:FIXTURE_LOG
}
function rustup {
  Record-Command "rustup" $args
  $global:LASTEXITCODE = 0
  $call = $args -join " "
  $scenario = $env:FIXTURE_SCENARIO
  if ($call -eq "toolchain install 1.97.0 --profile minimal --component clippy --component rustfmt --no-self-update") {
    if ($scenario -eq "install-failure") {
      & $env:FIXTURE_PWSH -NoProfile -NonInteractive -Command "exit 23"
      return
    }
    $global:fixtureInstalled = $true
    return
  }
  if ($call.StartsWith("target add --toolchain 1.97.0 ")) {
    if ($scenario -eq "target-failure") {
      & $env:FIXTURE_PWSH -NoProfile -NonInteractive -Command "exit 24"
    }
    return
  }
  if ($call -eq "run 1.97.0 rustc -vV") {
    if ($scenario -in @("absent", "install-failure", "native-failure")) {
      & $env:FIXTURE_PWSH -NoProfile -NonInteractive -Command "exit 17"
      return
    }
    if ($scenario -eq "malformed") { "release: 1.97.0"; return }
    if ($scenario -eq "duplicate-host") { "release: 1.97.0"; "host: fixture-host"; "host: another-host"; return }
    if ($scenario -eq "wrong-release") { "release: 1.96.0"; "host: fixture-host"; return }
    "rustc 1.97.0 (fixture)"; "release: 1.97.0"; "host: fixture-host"; return
  }
  if ($call -eq "component list --toolchain 1.97.0 --installed --quiet") {
    if ($scenario -eq "component-failure") {
      & $env:FIXTURE_PWSH -NoProfile -NonInteractive -Command "exit 18"
      return
    }
    if ($scenario -eq "malformed-components") { "rustc-other-host"; return }
    foreach ($component in @("rustc", "cargo", "rust-std", "clippy", "rustfmt")) {
      if ($scenario -ne "missing-$component") { "$component-fixture-host" }
    }
    return
  }
  if ($call.StartsWith("which --toolchain 1.97.0 ")) {
    $binary = $args[-1]
    if (-not $global:fixtureInstalled -and $scenario -eq "missing-binary-$binary") {
      Join-Path $env:FIXTURE_BIN "absent-$binary"; return
    }
    if (-not $global:fixtureInstalled -and $scenario -eq "which-failure") {
      & $env:FIXTURE_PWSH -NoProfile -NonInteractive -Command "exit 19"
      return
    }
    Join-Path $env:FIXTURE_BIN $binary; return
  }
  if ($call -eq "run 1.97.0 rustc --version") { "rustc 1.97.0 (fixture)"; return }
  if ($call -eq "run 1.97.0 cargo --version") { "cargo 1.97.0 (fixture)"; return }
  throw "Unexpected rustup command: $call"
}
function rustc {
  Record-Command "rustc" $args
  $global:LASTEXITCODE = 0
  if ($env:FIXTURE_SCENARIO -eq "wrong-rustc") { "rustc 1.96.0 (fixture)" }
  else { "rustc 1.97.0 (fixture)" }
}
function cargo {
  Record-Command "cargo" $args
  $global:LASTEXITCODE = 0
  if ($env:FIXTURE_SCENARIO -eq "wrong-cargo") { "cargo 1.96.0 (fixture)" }
  else { "cargo 1.97.0 (fixture)" }
}
'''

    def installs(self, commands):
        return [c for c in commands if c["arguments"][:2] == ["toolchain", "install"]]

    def test_ready_skips_install_and_preserves_verification_and_environment(self):
        for previous in (None, "0", "1", "sentinel"):
            with self.subTest(previous=previous):
                result, commands, state, paths, future = self.run_action(auto_install=previous)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.installs(commands), [])
                self.assertEqual(state, {"present": previous is not None, "value": previous})
                self.assertIn("toolchain bin", paths)
                self.assertIn("RUSTUP_TOOLCHAIN=1.97.0", future)
                self.assertEqual([c["command"] for c in commands][-2:], ["rustc", "cargo"])
                # The final rustc location lookup is the first post-probe call.
                probe = commands[:8]  # rustc details, component list, six binary paths
                self.assertEqual(len(probe), 8)
                self.assertTrue(probe)
                self.assertTrue(all(c["auto"] == "0" for c in probe))

    def test_missing_partial_malformed_and_native_failure_use_exact_install(self):
        scenarios = ["absent", "malformed", "duplicate-host", "wrong-release", "native-failure", "component-failure", "malformed-components", "which-failure"]
        scenarios += ["missing-binary-" + b for b in ("rustc", "cargo", "rustdoc", "cargo-clippy", "clippy-driver", "rustfmt")]
        scenarios += ["missing-" + c for c in ("rustc", "cargo", "rust-std", "clippy", "rustfmt")]
        for scenario in scenarios:
            with self.subTest(scenario=scenario):
                result, commands, state, _, _ = self.run_action(scenario, auto_install="sentinel")
                self.assertEqual(result.returncode, 0, result.stderr)
                installs = self.installs(commands)
                self.assertEqual(len(installs), 1)
                self.assertEqual(installs[0]["arguments"], ["toolchain", "install", "1.97.0", "--profile", "minimal", "--component", "clippy", "--component", "rustfmt", "--no-self-update"])
                self.assertEqual(installs[0]["auto"], "sentinel")
                self.assertEqual(state, {"present": True, "value": "sentinel"})

    def test_install_failure_is_fatal_and_restores_probe_environment(self):
        result, commands, state, paths, future = self.run_action("install-failure", auto_install="sentinel")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(self.installs(commands)), 1)
        self.assertEqual(state, {"present": True, "value": "sentinel"})
        self.assertEqual(paths, "")
        self.assertEqual(future, "")
        self.assertFalse(any(c["command"] in ("rustc", "cargo") for c in commands))

    def test_targets_are_honored_and_target_failure_remains_fatal(self):
        targets = "aarch64-apple-darwin, x86_64-unknown-linux-gnu\nwasm32-unknown-unknown"
        result, commands, _, _, _ = self.run_action(targets=targets)
        self.assertEqual(result.returncode, 0, result.stderr)
        actual = [c["arguments"] for c in commands if c["arguments"][:2] == ["target", "add"]]
        self.assertEqual(actual, [["target", "add", "--toolchain", "1.97.0", target] for target in targets.replace(",", " ").split()])
        failed, commands, _, _, _ = self.run_action("target-failure", targets="wasm32-unknown-unknown")
        self.assertNotEqual(failed.returncode, 0)
        self.assertFalse(any(c["command"] in ("rustc", "cargo") for c in commands))

    def test_effective_command_mismatch_remains_fatal(self):
        for scenario in ("wrong-rustc", "wrong-cargo"):
            with self.subTest(scenario=scenario):
                result, _, _, _, _ = self.run_action(scenario)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("does not match pinned toolchain", result.stderr)


if __name__ == "__main__":
    unittest.main()
