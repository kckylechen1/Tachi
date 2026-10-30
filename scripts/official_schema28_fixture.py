#!/usr/bin/env python3
"""Generate a disposable WAL fixture with the checksum-pinned official v1.9.2 CLI."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import sqlite3
import subprocess
import tarfile
import tempfile
import urllib.request


ARCHIVE = "tachi-v1.9.2-aarch64-apple-darwin.tar.gz"
ARCHIVE_SHA256 = "cad6dbceccd87279572ce81915e0d930547697404cceda3ac679a7c1d4f7b8bb"
CHECKSUMS = "tachi-v1.9.2-SHASUMS256.txt"
CHECKSUMS_SHA256 = "083593236be8d8babd63bbc1dcb44768798bf697a654ad99ebb6daf5fc886f1d"
RELEASE_URL = "https://github.com/kckylechen1/Tachi/releases/download/v1.9.2/"
RELEASE_COMMIT = "4e0bad8b1628d23616f76e6d5029206c40d8ded3"
BINARY_MEMBER = "tachi-v1.9.2-aarch64-apple-darwin/tachi"
MEMORY_TEXT = (
    "Official Tachi v1.9.2 schema 28 WAL migration fixture. "
    "This is a disposable validation memory with no production data."
)
MAX_DOWNLOAD_BYTES = 64 * 1024 * 1024


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def verified_cli_version(output: str) -> str:
    version = output.strip()
    fields = version.split()
    if (
        len(version.splitlines()) != 1 or len(fields) < 2 or fields[0] != "tachi"
        or fields[1].split("+", 1)[0] != "1.9.2"
    ):
        raise RuntimeError(f"unexpected official CLI version: {version}")
    # --version includes Git identity and build time; retain all of it.
    return version


def verified_download(name: str, expected: str, destination: Path) -> None:
    with urllib.request.urlopen(RELEASE_URL + name, timeout=60) as response:
        data = response.read(MAX_DOWNLOAD_BYTES + 1)
    if len(data) > MAX_DOWNLOAD_BYTES or sha256(data) != expected:
        raise RuntimeError(f"official release checksum/size mismatch: {name}")
    destination.write_bytes(data)


def isolated_environment(live: Path, inherited: dict[str, str]) -> dict[str, str]:
    # v1.9.2 also probes these two real-home files even with --no-project-db.
    # Refuse existing configuration rather than loading or moving it.
    real_home = Path.home()
    for relative in (".secrets/master.env", ".sigil/config.env"):
        path = real_home / relative
        if path.exists() or path.is_symlink():
            raise RuntimeError(f"fixture requires a clean hosted runner: {path}")
    environment = {
        key: inherited[key]
        for key in ("PATH", "HOME", "TMPDIR", "LANG", "LC_ALL")
        if key in inherited
    }
    environment.update(
        TACHI_HOME=str(live),
        TACHI_EMBEDDED_MCP="1",
        ENABLE_PIPELINE="0",
        TACHI_DISABLE_AUTO_DAEMON="1",
        TACHI_DISABLE_STDIO_PROXY="1",
    )
    return environment


def read_only(path: Path) -> sqlite3.Connection:
    return sqlite3.connect(path.as_uri() + "?mode=ro", uri=True, isolation_level=None)


def view(connection: sqlite3.Connection) -> dict:
    return {
        "schema_version": connection.execute("PRAGMA user_version").fetchone()[0],
        "journal_mode": connection.execute("PRAGMA journal_mode").fetchone()[0],
        "rows": connection.execute(
            "SELECT id, hex(CAST(text AS BLOB)) FROM memories ORDER BY id"
        ).fetchall(),
        "edges": connection.execute("SELECT count(*) FROM memory_edges").fetchone()[0],
    }


def file_inventory(directory: Path) -> dict[str, dict]:
    inventory = {}
    for path in sorted(directory.iterdir()):
        if path.is_symlink() or not path.is_file() or not path.name.startswith("memory.db"):
            raise RuntimeError(f"unexpected fixture entry: {path.name}")
        data = path.read_bytes()
        inventory[path.name] = {"bytes": len(data), "sha256": sha256(data)}
    if "memory.db" not in inventory or inventory.get("memory.db-wal", {}).get("bytes", 0) == 0:
        raise RuntimeError("fixture requires memory.db and nonempty committed WAL")
    return inventory


def copy_pinned_directory(source: Path, destination: Path) -> dict[str, dict]:
    """Copy an isolated, writer-free directory while its old reader pins the WAL."""
    before = file_inventory(source)
    destination.mkdir()
    for name in before:
        shutil.copyfile(source / name, destination / name)
    if file_inventory(source) != before or file_inventory(destination) != before:
        raise RuntimeError("fixture changed during whole-directory capture")
    return before


def generate(output: Path) -> None:
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        raise RuntimeError("official v1.9.2 fixture generation requires Darwin arm64")
    output.mkdir()
    commands = []
    with tempfile.TemporaryDirectory(prefix="schema28-producer-", dir=output.parent) as temporary:
        scratch = Path(temporary)
        live = scratch / "live"
        global_dir = live / "global"
        global_dir.mkdir(parents=True)
        environment = isolated_environment(live, dict(os.environ))
        archive_path = scratch / ARCHIVE
        checksums_path = scratch / CHECKSUMS
        verified_download(ARCHIVE, ARCHIVE_SHA256, archive_path)
        verified_download(CHECKSUMS, CHECKSUMS_SHA256, checksums_path)
        expected_line = f"{ARCHIVE_SHA256}  {ARCHIVE}"
        if checksums_path.read_text(encoding="ascii").strip() != expected_line:
            raise RuntimeError("official checksum manifest does not match the pinned archive")
        with tarfile.open(archive_path, "r:gz") as archive:
            member = archive.getmember(BINARY_MEMBER)
            if not member.isfile() or member.size > MAX_DOWNLOAD_BYTES:
                raise RuntimeError("official CLI archive member is not a bounded regular file")
            extracted = archive.extractfile(member)
            if extracted is None:
                raise RuntimeError("official CLI archive has no binary")
            with extracted:
                binary_bytes = extracted.read()
        binary = scratch / "tachi-v1.9.2"
        binary.write_bytes(binary_bytes)
        binary.chmod(0o700)

        def cli(label: str, arguments: list[str]) -> str:
            command = [str(binary), *arguments]
            result = subprocess.run(
                command, cwd=live, env=environment, capture_output=True, timeout=120,
            )
            (output / f"{label}.stdout").write_bytes(result.stdout)
            (output / f"{label}.stderr").write_bytes(result.stderr)
            commands.append({"argv": command, "exit_code": result.returncode})
            if result.returncode != 0:
                raise RuntimeError(f"official CLI {label} failed; see retained stdout/stderr")
            return result.stdout.decode("utf-8")

        version = verified_cli_version(cli("version", ["--version"]))
        db = global_dir / "memory.db"
        common = ["--global-db", str(db), "--no-project-db"]
        cli("initialize", [*common, "list", "--path-prefix", "/", "--limit", "1"])
        pinned = read_only(db)
        try:
            pinned.execute("BEGIN")
            before = view(pinned)
            if before != {"schema_version": 28, "journal_mode": "wal", "rows": [], "edges": 0}:
                raise RuntimeError(f"official CLI did not create an empty schema-28 store: {before}")
            cli("remember", [
                *common, "remember", "--scope", "general", "--path",
                "/notes/schema28-wal-fixture", "--force", MEMORY_TEXT,
            ])
            if view(pinned) != before:
                raise RuntimeError("the original reader did not retain its empty snapshot")
            committed_connection = read_only(db)
            try:
                committed = view(committed_connection)
            finally:
                committed_connection.close()
            rows = committed["rows"]
            if (
                committed["schema_version"] != 28 or committed["journal_mode"] != "wal"
                or committed["edges"] != 0 or len(rows) != 1
                or rows[0][1] != MEMORY_TEXT.encode("utf-8").hex().upper()
            ):
                raise RuntimeError(f"official CLI did not commit the single expected memory: {committed}")
            # Both CLI writers have exited. Keep only the old read snapshot
            # alive until the entire disconnected backup has been copied.
            files = copy_pinned_directory(global_dir, output / "global")
        finally:
            pinned.close()
        provenance = {
            "format": "official-schema28-wal-v1",
            "release_url": RELEASE_URL,
            "release_commit": RELEASE_COMMIT,
            "archive_sha256": ARCHIVE_SHA256,
            "checksum_manifest_sha256": CHECKSUMS_SHA256,
            "binary_sha256": sha256(binary_bytes),
            "binary_version": version,
            "platform": {"system": platform.system(), "machine": platform.machine()},
            "commands": commands,
            "committed_view": committed,
            "fixture_files": files,
        }
        (output / "provenance.json").write_text(json.dumps(provenance, indent=2) + "\n")
        print(json.dumps(provenance, indent=2))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True, help="new evidence directory")
    arguments = parser.parse_args()
    generate(arguments.output.absolute())


if __name__ == "__main__":
    main()
