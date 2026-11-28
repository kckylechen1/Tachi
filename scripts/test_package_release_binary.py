"""Regression tests for scripts/package_release_binary.sh.

The packager runs with --skip-build against a stand-in binary and stand-in
rustc, uname and codesign, so nothing is compiled, signed or executed for real.
"""

from __future__ import annotations

import hashlib
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/package_release_binary.sh"
HOST = "aarch64-apple-darwin"
VERSION = "9.9.9"


@unittest.skipUnless(shutil.which("shasum"), "shasum is required by the packager")
class PackageReleaseBinaryTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tempdir = tempfile.TemporaryDirectory()
        self.root = Path(self.tempdir.name)
        self.out = self.root / "out"
        self.out.mkdir()
        self.target = self.root / "target"
        self.fake_bin = self.root / "fake-bin"
        self.fake_bin.mkdir()
        self.codesign_log = self.root / "codesign"
        self.smoke_log = self.root / "smoke"
        self.write_tool("rustc", f"#!/bin/sh\nprintf 'host: {HOST}\\n'\n")
        self.write_tool("uname", '#!/bin/sh\necho Darwin\n')
        self.write_tool(
            "codesign",
            f"#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"{self.codesign_log}\"\n",
        )

    def tearDown(self) -> None:
        self.tempdir.cleanup()

    def write_tool(self, name: str, body: str) -> None:
        path = self.fake_bin / name
        path.write_text(body, encoding="utf-8")
        path.chmod(0o755)

    def stage_binary(self, relative: str, *, runs: bool = True) -> None:
        path = self.target / relative / "release/tachi-server"
        path.parent.mkdir(parents=True, exist_ok=True)
        verdict = "echo 'tachi 9.9.9-test'" if runs else "exit 126"
        path.write_text(
            f"#!/bin/sh\nprintf 'ran\\n' >> \"{self.smoke_log}\"\n{verdict}\n",
            encoding="utf-8",
        )
        path.chmod(0o755)

    def run_packager(self, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["/bin/bash", str(SCRIPT), VERSION, "--skip-build", *args],
            cwd=ROOT,
            env=os.environ
            | {
                "OUT_DIR": str(self.out),
                "CARGO_TARGET_DIR": str(self.target),
                "PATH": f"{self.fake_bin}:{os.environ['PATH']}",
            },
            capture_output=True,
            text=True,
            check=False,
        )

    def lines(self, path: Path) -> list[str]:
        return path.read_text(encoding="utf-8").splitlines() if path.exists() else []

    def test_a_native_package_is_smoke_tested_signed_and_leaves_only_the_tarball(self) -> None:
        self.stage_binary("")
        result = self.run_packager()
        self.assertEqual(result.returncode, 0, result.stderr)

        stem = f"tachi-v{VERSION}-{HOST}"
        tarball = self.out / f"{stem}.tar.gz"
        self.assertTrue(tarball.is_file())
        self.assertFalse((self.out / stem).exists(), "the staging directory was left behind")
        self.assertEqual(self.lines(self.codesign_log), [f"--force -s - {self.out / stem}/tachi"])
        self.assertEqual(self.lines(self.smoke_log), ["ran"])

        digest = hashlib.sha256(tarball.read_bytes()).hexdigest()
        sums = self.out / f"tachi-v{VERSION}-SHASUMS256.txt"
        self.assertEqual(sums.read_text(encoding="utf-8"), f"{digest}  {stem}.tar.gz\n")
        self.assertEqual(
            sorted(entry.name for entry in self.out.iterdir()),
            sorted([tarball.name, sums.name]),
        )

    def test_a_cross_built_package_is_neither_executed_nor_signed(self) -> None:
        triple = "x86_64-unknown-linux-gnu"
        self.stage_binary(triple, runs=False)
        result = self.run_packager("--triple", triple)
        self.assertEqual(result.returncode, 0, result.stderr)

        self.assertIn(f"smoke test skipped: {triple} does not run on {HOST}", result.stdout)
        self.assertEqual(self.lines(self.codesign_log), [], "a non-Apple triple was signed")
        self.assertEqual(self.lines(self.smoke_log), [], "a foreign binary was executed")
        self.assertTrue((self.out / f"tachi-v{VERSION}-{triple}.tar.gz").is_file())

    def test_shasums_rewrite_preserves_signature_entry_and_skips_arm64_smoke_on_x86_host(self) -> None:
        self.write_tool("rustc", "#!/bin/sh\nprintf 'host: x86_64-apple-darwin\\n'\n")
        triple = "aarch64-apple-darwin"
        self.stage_binary(triple, runs=False)
        sums = self.out / f"tachi-v{VERSION}-SHASUMS256.txt"
        sig_line = f"{'0' * 64}  tachi-v{VERSION}-{triple}.tar.gz.sig\n"
        old_line = f"{'1' * 64}  tachi-v{VERSION}-{triple}.tar.gz\n"
        sums.write_text(sig_line + old_line, encoding="utf-8")

        result = self.run_packager("--triple", triple)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"smoke test skipped: {triple} does not run on x86_64-apple-darwin", result.stdout)
        self.assertEqual(self.lines(self.smoke_log), [], "arm64 binary was executed on x86_64 host")

        digest = hashlib.sha256((self.out / f"tachi-v{VERSION}-{triple}.tar.gz").read_bytes()).hexdigest()
        self.assertEqual(
            sums.read_text(encoding="utf-8"),
            f"{sig_line}{digest}  tachi-v{VERSION}-{triple}.tar.gz\n",
        )

    def test_an_apple_triple_is_not_packaged_unsigned_from_another_host(self) -> None:
        triple = "aarch64-apple-darwin"
        self.write_tool("rustc", "#!/bin/sh\nprintf 'host: aarch64-unknown-linux-gnu\\n'\n")
        self.write_tool("uname", "#!/bin/sh\necho Linux\n")
        self.stage_binary(triple, runs=False)

        result = self.run_packager("--triple", triple)

        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("refusing to package", result.stderr)
        self.assertEqual(self.lines(self.codesign_log), [])
        self.assertEqual(self.lines(self.smoke_log), [])
        self.assertEqual([entry.name for entry in self.out.iterdir()], [], "an unsigned tarball was produced")

    def test_a_failed_manifest_rewrite_leaves_the_existing_sums_untouched(self) -> None:
        triple = "aarch64-apple-darwin"
        self.write_tool("rustc", "#!/bin/sh\nprintf 'host: x86_64-apple-darwin\\n'\n")
        self.stage_binary(triple, runs=False)
        # `awk` reads the manifest with `-v target=`. It dies part-way there and
        # keeps working for the digest line.
        self.write_tool(
            "awk",
            '#!/bin/sh\ncase "$1" in\n  -v) printf \'%s\\n\' "half a line"; exit 1;;\nesac\n'
            'exec /usr/bin/awk "$@"\n',
        )
        sums = self.out / f"tachi-v{VERSION}-SHASUMS256.txt"
        sig_line = f"{'0' * 64}  tachi-v{VERSION}-{triple}.tar.gz.sig\n"
        old_line = f"{'1' * 64}  tachi-v{VERSION}-{triple}.tar.gz\n"
        sums.write_text(sig_line + old_line, encoding="utf-8")

        result = self.run_packager("--triple", triple)

        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("could not rewrite", result.stderr)
        self.assertEqual(
            sums.read_text(encoding="utf-8"),
            sig_line + old_line,
            "the manifest was replaced by an incomplete one",
        )
        leftovers = [
            entry.name
            for entry in self.out.iterdir()
            if entry.name.startswith(".tachi-SHASUMS256")
        ]
        self.assertEqual(leftovers, [], "a temporary manifest was left behind")


if __name__ == "__main__":
    unittest.main()
