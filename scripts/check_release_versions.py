#!/usr/bin/env python3
"""Check that release-facing Tachi version fields stay in sync."""

from __future__ import annotations

import json
import re
import sys
try:
    import tomllib
except ModuleNotFoundError:
    tomllib = None
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]

RUST_MANIFEST_PATHS = (
    "crates/memcore/Cargo.toml",
    "crates/memory-server-capture-gate/Cargo.toml",
    "crates/memory-server-hub-cli/Cargo.toml",
    "crates/memory-server-manifest-audit/Cargo.toml",
    "crates/tachi-server/Cargo.toml",
    "crates/tachi-params/Cargo.toml",
    "crates/memory-server-prompt-envelope/Cargo.toml",
    "crates/memory-server-rescue/Cargo.toml",
    "crates/memory-server-runtime/Cargo.toml",
    "crates/memory-node/Cargo.toml",
    "crates/tachi-bootstrap/Cargo.toml",
    "crates/tachi-dispatch/Cargo.toml",
    "crates/tachi-foundry/Cargo.toml",
    "crates/tachi-hub/Cargo.toml",
    "crates/tachi-llm/Cargo.toml",
    "crates/tachi-merge-ops/Cargo.toml",
)

JS_PACKAGE_PATHS = (
    "crates/memory-node/package.json",
    "integrations/openclaw/package.json",
    "packages/tachi-cli/package.json",
)


def read(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


def _parse_toml_package_fields(text: str) -> tuple[bool, dict[str, str]]:
    pos = 0
    length = len(text)
    current_section: str | None = None
    package_fields: dict[str, str] = {}
    found_package_header = False

    while pos < length:
        ch = text[pos]
        if ch in " \t\r\n":
            pos += 1
            continue
        if ch == "#":
            while pos < length and text[pos] != "\n":
                pos += 1
            continue

        if ch == "[":
            is_double = pos + 1 < length and text[pos + 1] == "["
            end_bracket = "]]" if is_double else "]"
            header_end = text.find(end_bracket, pos + (2 if is_double else 1))
            if header_end == -1:
                break
            raw_sec = text[pos + (2 if is_double else 1) : header_end].strip()
            if raw_sec == "package":
                found_package_header = True
            current_section = raw_sec
            pos = header_end + (2 if is_double else 1)
            while pos < length and text[pos] != "\n":
                if text[pos] == "#":
                    while pos < length and text[pos] != "\n":
                        pos += 1
                    break
                pos += 1
            continue

        key_start = pos
        while pos < length and text[pos] not in " \t\r\n=#([":
            pos += 1
        key = text[key_start:pos].strip()

        while pos < length and text[pos] in " \t":
            pos += 1

        if pos < length and text[pos] == "=":
            pos += 1
            while pos < length and text[pos] in " \t":
                pos += 1
            if pos >= length:
                break

            val_ch = text[pos]
            if text[pos : pos + 3] in ('"""', "'''"):
                delimiter = text[pos : pos + 3]
                pos += 3
                val_start = pos
                val_end = text.find(delimiter, pos)
                if val_end == -1:
                    val_str = text[val_start:]
                    pos = length
                else:
                    val_str = text[val_start:val_end]
                    pos = val_end + 3
                if current_section == "package":
                    package_fields[key] = val_str
            elif val_ch in ('"', "'"):
                delimiter = val_ch
                pos += 1
                val_chars: list[str] = []
                while pos < length:
                    c = text[pos]
                    if delimiter == '"' and c == "\\" and pos + 1 < length:
                        pos += 1
                        val_chars.append(text[pos])
                        pos += 1
                    elif c == delimiter:
                        pos += 1
                        break
                    else:
                        val_chars.append(c)
                        pos += 1
                val_str = "".join(val_chars)
                if current_section == "package":
                    package_fields[key] = val_str
            elif val_ch in ("[", "{"):
                open_ch = val_ch
                close_ch = "]" if open_ch == "[" else "}"
                depth = 0
                while pos < length:
                    c = text[pos]
                    if c == open_ch:
                        depth += 1
                    elif c == close_ch:
                        depth -= 1
                        if depth == 0:
                            pos += 1
                            break
                    elif text[pos : pos + 3] in ('"""', "'''"):
                        delim = text[pos : pos + 3]
                        pos = text.find(delim, pos + 3)
                        if pos == -1:
                            pos = length
                            break
                        pos += 3
                        continue
                    elif c in ('"', "'"):
                        delim = c
                        pos += 1
                        while pos < length and text[pos] != delim:
                            if delim == '"' and text[pos] == "\\":
                                pos += 1
                            pos += 1
                        if pos < length:
                            pos += 1
                        continue
                    pos += 1
            else:
                while pos < length and text[pos] not in "\r\n#":
                    pos += 1
        else:
            pos += 1

    return found_package_header, package_fields


def cargo_package(path: str) -> tuple[str, str]:
    if tomllib is not None:
        with (ROOT / path).open("rb") as fh:
            data = tomllib.load(fh)
        package = data.get("package")
        if not isinstance(package, dict):
            raise ValueError(f"{path}: missing [package]")

        name = str(package.get("name", "")).strip()
        version = str(package.get("version", "")).strip()
        if not name:
            raise ValueError(f"{path}: missing package.name")
        if not version:
            raise ValueError(f"{path}: missing package.version")
        return name, version

    found_package, fields = _parse_toml_package_fields(read(path))
    if not found_package:
        raise ValueError(f"{path}: missing [package]")
    name = fields.get("name", "").strip()
    if not name:
        raise ValueError(f"{path}: missing package.name")
    version = fields.get("version", "").strip()
    if not version:
        raise ValueError(f"{path}: missing package.version")
    return name, version


def cargo_version(path: str) -> str:
    return cargo_package(path)[1]


def package_json_version(path: str) -> str:
    return str(json.loads(read(path))["version"])


def package_lock_versions(path: str) -> list[str]:
    data = json.loads(read(path))
    versions = [str(data["version"])]
    root_pkg = data.get("packages", {}).get("", {})
    if "version" in root_pkg:
        versions.append(str(root_pkg["version"]))
    return versions


def package_lock_path(package_path: str) -> str:
    return Path(package_path).with_name("package-lock.json").as_posix()


def cargo_lock_versions(names: set[str]) -> dict[str, str]:
    versions: dict[str, str] = {}
    current_name: str | None = None
    for line in read("Cargo.lock").splitlines():
        if line.startswith("name = "):
            current_name = line.split('"', 2)[1]
        elif current_name in names and line.startswith("version = "):
            versions[current_name] = line.split('"', 2)[1]
            current_name = None
    return versions


def require_match(label: str, got: str, expected: str, errors: list[str]) -> None:
    if got != expected:
        errors.append(f"{label}: expected {expected}, got {got}")


def require_contains(label: str, text: str, needle: str, errors: list[str]) -> None:
    if needle not in text:
        errors.append(f"{label}: missing {needle}")


def main() -> int:
    errors: list[str] = []
    cargo_packages: list[tuple[str, str, str]] = []
    toml_error = (tomllib.TOMLDecodeError,) if tomllib is not None else ()
    for path in RUST_MANIFEST_PATHS:
        try:
            name, version = cargo_package(path)
        except (OSError, ValueError) + toml_error as error:
            errors.append(str(error))
            continue
        cargo_packages.append((path, name, version))

    tachi_server = next(
        (version for path, _, version in cargo_packages if path == "crates/tachi-server/Cargo.toml"),
        "",
    )
    if not tachi_server:
        errors.append("crates/tachi-server/Cargo.toml: missing release version")
    expected = tachi_server
    tag = f"v{expected}"

    for path, _, version in cargo_packages:
        require_match(path, version, expected, errors)

    cargo_names = [name for _, name, _ in cargo_packages]
    duplicate_names = sorted({name for name in cargo_names if cargo_names.count(name) > 1})
    if duplicate_names:
        errors.append(
            "Cargo manifests: duplicate package name(s): " + ", ".join(duplicate_names)
        )

    lock_versions = cargo_lock_versions(set(cargo_names))
    for name in cargo_names:
        require_match(f"Cargo.lock {name}", lock_versions.get(name, ""), expected, errors)

    for path in JS_PACKAGE_PATHS:
        require_match(path, package_json_version(path), expected, errors)
        lock_path = package_lock_path(path)
        for index, version in enumerate(package_lock_versions(lock_path)):
            require_match(f"{lock_path} version[{index}]", version, expected, errors)

    yaml_release = re.search(r"^release:\s*(\S+)", read("docs/current-state.agent.yaml"), re.M)
    require_match(
        "docs/current-state.agent.yaml release",
        yaml_release.group(1) if yaml_release else "",
        tag,
        errors,
    )

    pinned_refs = [
        "README.md",
        "README.zh-CN.md",
        "README.classical.md",
        "scripts/install.sh",
        "docs/INSTALL.md",
        "integrations/openclaw/README.md",
    ]
    for path in pinned_refs:
        require_contains(path, read(path), f"/{tag}/scripts/install", errors)

    if errors:
        print("release version check failed:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1

    print(f"release version check passed: {expected}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
