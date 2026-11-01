"""Local safety checks; synthetic SQLite cases are not the official release gate."""

import io
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import patch

from scripts import official_schema28_fixture as fixture


class OfficialFixtureTests(unittest.TestCase):
    def test_real_official_long_version_is_accepted_and_preserved(self):
        version = (
            "tachi 1.9.2+git-4e0bad8b1628d23616f76e6d5029206c40d8ded3 "
            "(built 2026-08-05T05:45:50Z)"
        )
        self.assertEqual(fixture.verified_cli_version(version + "\n"), version)

    def test_short_version_is_accepted(self):
        self.assertEqual(fixture.verified_cli_version("tachi 1.9.2\n"), "tachi 1.9.2")

    def test_missing_wrong_or_ambiguous_version_is_rejected(self):
        for version in (
            "", "tachi", "1.9.2", "other 1.9.2", "tachi 2.0.0", "tachi 1.9.20",
            "tachi 1.9.2-rc1", "tachi 1.9.2\nwarning: extra output",
        ):
            with self.subTest(version=version), self.assertRaisesRegex(RuntimeError, "unexpected"):
                fixture.verified_cli_version(version)

    def test_download_verifies_before_writing(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "archive"
            payload = b"test archive bytes"
            with patch.object(fixture.urllib.request, "urlopen", return_value=io.BytesIO(payload)):
                with self.assertRaisesRegex(RuntimeError, "checksum/size mismatch"):
                    fixture.verified_download("archive", fixture.sha256(b"different"), output)
            self.assertFalse(output.exists())
            with patch.object(fixture.urllib.request, "urlopen", return_value=io.BytesIO(payload)):
                fixture.verified_download("archive", fixture.sha256(payload), output)
            self.assertEqual(output.read_bytes(), payload)

    def test_oversized_download_is_not_materialized(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "archive"
            payload = b"12345"
            with patch.object(fixture, "MAX_DOWNLOAD_BYTES", 4), patch.object(
                fixture.urllib.request, "urlopen", return_value=io.BytesIO(payload)
            ):
                with self.assertRaisesRegex(RuntimeError, "checksum/size mismatch"):
                    fixture.verified_download("archive", fixture.sha256(payload), output)
            self.assertFalse(output.exists())

    def test_child_environment_drops_credentials_and_preserves_real_home(self):
        with tempfile.TemporaryDirectory() as temporary:
            real_home = Path(temporary)
            live = real_home / "isolated"
            inherited = {
                "PATH": "/usr/bin:/bin", "HOME": str(real_home),
                "GH_TOKEN": "sentinel-gh-token", "GITHUB_TOKEN": "sentinel-github-token",
                "OPENAI_API_KEY": "sentinel-provider-key", "TACHI_HOME": "/unrelated",
                "SIGIL_HOME": "/unrelated-legacy",
            }
            with patch.object(Path, "home", return_value=real_home):
                environment = fixture.isolated_environment(live, inherited)
            self.assertEqual(environment["HOME"], str(real_home))
            self.assertEqual(environment["TACHI_HOME"], str(live))
            self.assertFalse({"GH_TOKEN", "GITHUB_TOKEN", "OPENAI_API_KEY", "SIGIL_HOME"} & environment.keys())
            self.assertNotIn("sentinel-", str(environment))

    def test_existing_home_configuration_refuses_without_reading_or_removing_it(self):
        for relative in (".secrets/master.env", ".sigil/config.env"):
            with self.subTest(relative=relative), tempfile.TemporaryDirectory() as temporary:
                real_home = Path(temporary)
                configuration = real_home / relative
                configuration.parent.mkdir()
                configuration.write_bytes(b"unchanged sentinel configuration")
                with patch.object(Path, "home", return_value=real_home):
                    with self.assertRaisesRegex(RuntimeError, "clean hosted runner"):
                        fixture.isolated_environment(real_home / "live", {})
                self.assertEqual(configuration.read_bytes(), b"unchanged sentinel configuration")

    def test_dangling_home_configuration_symlink_also_refuses(self):
        with tempfile.TemporaryDirectory() as temporary:
            real_home = Path(temporary)
            configuration = real_home / ".sigil/config.env"
            configuration.parent.mkdir()
            configuration.symlink_to(real_home / "missing")
            with patch.object(Path, "home", return_value=real_home):
                with self.assertRaisesRegex(RuntimeError, "clean hosted runner"):
                    fixture.isolated_environment(real_home / "live", {})
            self.assertTrue(configuration.is_symlink())

    def test_pinned_snapshot_capture_preserves_committed_wal_row(self):
        # Generic SQLite data exercises the copy mechanics, not schema-28 provenance.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "live"
            source.mkdir()
            db = source / "memory.db"
            writer = sqlite3.connect(db)
            writer.execute("PRAGMA journal_mode=WAL")
            writer.execute("CREATE TABLE items(value TEXT)")
            writer.commit()
            writer.close()
            pinned = fixture.read_only(db)
            try:
                pinned.execute("BEGIN")
                self.assertEqual(pinned.execute("SELECT count(*) FROM items").fetchone()[0], 0)
                writer = sqlite3.connect(db)
                writer.execute("INSERT INTO items VALUES ('committed in WAL')")
                writer.commit()
                writer.close()
                self.assertEqual(pinned.execute("SELECT count(*) FROM items").fetchone()[0], 0)
                copied = fixture.copy_pinned_directory(source, root / "backup")
                self.assertGreater(copied["memory.db-wal"]["bytes"], 0)
                self.assertEqual(fixture.file_inventory(source), copied)
            finally:
                pinned.close()
            reopened = fixture.read_only(root / "backup/memory.db")
            try:
                self.assertEqual(reopened.execute("SELECT value FROM items").fetchall(), [("committed in WAL",)])
            finally:
                reopened.close()

    def test_missing_wal_refuses_before_creating_backup(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "live"
            source.mkdir()
            (source / "memory.db").write_bytes(b"main file")
            with self.assertRaisesRegex(RuntimeError, "nonempty committed WAL"):
                fixture.copy_pinned_directory(source, root / "backup")
            self.assertFalse((root / "backup").exists())

    def test_unexpected_file_or_symlink_is_not_silently_omitted(self):
        for symlink in (False, True):
            with self.subTest(symlink=symlink), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                source = root / "live"
                source.mkdir()
                (source / "memory.db").write_bytes(b"main file")
                (source / "memory.db-wal").write_bytes(b"wal file")
                if symlink:
                    (source / "memory.db-extra").symlink_to(source / "memory.db")
                else:
                    (source / "unrelated").write_bytes(b"not part of fixture")
                with self.assertRaisesRegex(RuntimeError, "unexpected fixture entry"):
                    fixture.copy_pinned_directory(source, root / "backup")
                self.assertFalse((root / "backup").exists())

    def test_change_during_copy_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "live"
            source.mkdir()
            (source / "memory.db").write_bytes(b"main file")
            (source / "memory.db-wal").write_bytes(b"wal file")
            copyfile = fixture.shutil.copyfile

            def change_after_copy(original, destination):
                copyfile(original, destination)
                original.write_bytes(original.read_bytes() + b"changed")

            with patch.object(fixture.shutil, "copyfile", side_effect=change_after_copy):
                with self.assertRaisesRegex(RuntimeError, "changed during"):
                    fixture.copy_pinned_directory(source, root / "backup")


if __name__ == "__main__":
    unittest.main()
