#!/usr/bin/env python3
"""Local build.sh installer: Cargo-reported artifacts and reversible replacements."""

from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile
import uuid
import unicodedata

ROOT = Path(__file__).resolve().parents[1]
NAMES = ("tachi-server", "tachi-clean")


def destination(raw: str) -> Path:
    path = Path(raw).absolute()
    # Resolve parent aliases, but preserve a final symlink for backup/replacement.
    return path.parent.resolve() / path.name


def preflight(paths: list[Path]) -> None:
    for index, path in enumerate(paths):
        if path.is_dir() or (path.exists() and not (path.is_file() or path.is_symlink())):
            raise RuntimeError(f"unsafe install target: {path}")
        # Refuse portable name ambiguity even on a case-sensitive host. Parent
        # symlinks were resolved by destination(); NFD/casefold also catches
        # absent leaves and parent spellings on case-insensitive macOS volumes.
        parts = tuple(unicodedata.normalize("NFD", part.casefold()) for part in path.parts)
        for other in paths[:index]:
            other_parts = tuple(unicodedata.normalize("NFD", part.casefold()) for part in other.parts)
            shared = min(len(parts), len(other_parts))
            # Compare directory entries, not final symlink referents: a normal
            # existing tachi link legitimately points to INSTALL_BIN.
            entry = path.lstat() if path.exists() or path.is_symlink() else None
            other_entry = other.lstat() if other.exists() or other.is_symlink() else None
            same_entry = (entry is not None and other_entry is not None
                          and (entry.st_dev, entry.st_ino) == (other_entry.st_dev, other_entry.st_ino))
            if parts[:shared] == other_parts[:shared] or same_entry:
                raise RuntimeError(f"overlapping or ambiguous install targets: {path} and {other}")
        parent = path.parent
        while not parent.exists():
            parent = parent.parent
        if not parent.is_dir():
            raise RuntimeError(f"install parent is not a directory: {parent}")


def build() -> dict[str, Path]:
    command = ["cargo", "build", "--release", "-p", "tachi-server", "--bin", "tachi-server",
               "-p", "tachi-clean", "--bin", "tachi-clean", "--locked", "--message-format=json"]
    print(">> " + " ".join(command), flush=True)
    # Cargo itself resolves CARGO_TARGET_DIR, config build.target[-dir], and triples.
    # Never infer an output path from a target directory or fall back to old files.
    artifacts: dict[str, Path] = {}
    finished = False
    with tempfile.TemporaryFile(mode="w+t") as output:
        result = subprocess.run(command, cwd=ROOT, stdout=output, check=False)
        output.seek(0)
        for line in output:
            item = json.loads(line)
            if item.get("reason") == "compiler-message":
                rendered = item.get("message", {}).get("rendered")
                if rendered:
                    print(rendered, end="", file=sys.stderr)
            if item.get("reason") == "build-finished":
                finished = item.get("success") is True
            target = item.get("target", {})
            name = target.get("name")
            if (item.get("reason") == "compiler-artifact" and name in NAMES
                    and "bin" in target.get("kind", []) and item.get("executable")
                    and item.get("profile", {}).get("test") is False):
                path = Path(item["executable"])
                if not path.is_absolute():
                    raise RuntimeError("Cargo executable path must be absolute")
                if name in artifacts and artifacts[name] != path:
                    raise RuntimeError(f"ambiguous Cargo executable for {name}")
                artifacts[name] = path
        if result.returncode:
            raise RuntimeError(f"cargo build failed (exit={result.returncode})")
    if not finished or set(artifacts) != set(NAMES):
        raise RuntimeError("Cargo did not confirm both requested executable artifacts")
    for path in artifacts.values():
        if not path.is_file() or not os.access(path, os.X_OK):
            raise RuntimeError(f"Cargo executable missing or not executable: {path}")
    return artifacts


def probe(path: Path, argument: str) -> None:
    result = subprocess.run([str(path), argument], capture_output=True, text=True, timeout=30)
    if result.returncode != 0:
        raise RuntimeError(f"binary check failed: {path} {argument} (exit={result.returncode})")
    print(result.stdout.splitlines()[0] if result.stdout.strip() else f"checked {path}")


def install(artifacts: dict[str, Path], binaries: list[Path], links: list[Path]) -> None:
    stages: list[tuple[Path, Path]] = []
    replaced: list[tuple[Path, Path | None]] = []
    try:
        for name, target in zip(NAMES, binaries):
            target.parent.mkdir(parents=True, exist_ok=True)
            fd, raw = tempfile.mkstemp(prefix=f".{target.name}.new.", dir=target.parent)
            os.close(fd)
            stage = Path(raw)
            stages.append((stage, target))
            shutil.copyfile(artifacts[name], stage)
            stage.chmod(stat.S_IRUSR | stat.S_IWUSR | stat.S_IXUSR
                        | stat.S_IRGRP | stat.S_IXGRP | stat.S_IROTH | stat.S_IXOTH)
            if sys.platform == "darwin":
                # Preserve the legacy best-effort quarantine/signing behavior.
                for command in (["xattr", "-c", str(stage)], ["codesign", "-s", "-", "-f", str(stage)]):
                    try:
                        subprocess.run(command, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
                    except FileNotFoundError:
                        pass
            probe(stage, "--version" if name == "tachi-server" else "--help")
        for target, binary in zip(links, binaries):
            target.parent.mkdir(parents=True, exist_ok=True)
            fd, raw = tempfile.mkstemp(prefix=f".{target.name}.new.", dir=target.parent)
            os.close(fd)
            stage = Path(raw)
            stages.append((stage, target))
            stage.unlink()
            stage.symlink_to(binary)

        # Nothing live is replaced until every stage and binary check succeeded.
        preflight(binaries + links)
        for stage, target in stages:
            backup = None
            if target.exists() or target.is_symlink():
                backup = target.with_name(target.name + ".backup." + uuid.uuid4().hex)
                os.replace(target, backup)  # Move the symlink itself, never its referent.
            replaced.append((target, backup))
            os.replace(stage, target)
        for name, target in zip(NAMES, binaries):
            probe(target, "--version" if name == "tachi-server" else "--help")
        for target, backup in replaced:
            print(f">> installed {target}")
            if backup:
                print(f">> previous entry retained at {backup}")
    except BaseException:
        failures = []
        for target, backup in reversed(replaced):
            try:
                if backup:
                    os.replace(backup, target)
                else:
                    target.unlink(missing_ok=True)
            except OSError as exc:
                failures.append(f"{target}: {exc}; backup retained at {backup}")
        if failures:
            raise RuntimeError("Rollback incomplete: " + "; ".join(failures))
        raise
    finally:
        for stage, _ in stages:
            stage.unlink(missing_ok=True)


def main() -> int:
    home = Path.home()
    binaries = [destination(os.environ.get("INSTALL_BIN") or str(home / ".cargo/bin/tachi-server")),
                destination(os.environ.get("INSTALL_CLEAN_BIN") or str(home / ".cargo/bin/tachi-clean"))]
    links = [] if os.environ.get("SKIP_LINK", "0") == "1" else [
        destination(os.environ.get("TACHI_LINK") or str(home / "bin/tachi")),
        destination(os.environ.get("CLEAN_LINK") or str(home / "bin/tachi-clean")),
    ]
    try:
        preflight(binaries + links)
        artifacts = build()
        # Refuse output/installation aliases before staging can alter the build.
        resolved_artifacts = {path.resolve() for path in artifacts.values()}
        if any(path in resolved_artifacts or path.resolve() in resolved_artifacts for path in binaries + links):
            raise RuntimeError("install target overlaps a Cargo executable")
        install(artifacts, binaries, links)
        return 0
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as exc:
        print(f"build.sh: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
