#!/usr/bin/env python3
"""Check that release-facing Tachi version fields stay in sync."""

from __future__ import annotations

import json
import re
import sys
import tomllib
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


def cargo_package(path: str) -> tuple[str, str]:
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
    return str(Path(package_path).with_name("package-lock.json"))


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
    for path in RUST_MANIFEST_PATHS:
        try:
            name, version = cargo_package(path)
        except (OSError, ValueError, tomllib.TOMLDecodeError) as error:
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
