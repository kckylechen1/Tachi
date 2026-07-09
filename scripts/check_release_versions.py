#!/usr/bin/env python3
"""Check that release-facing Tachi version fields stay in sync."""

from __future__ import annotations

import json
import re
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def read(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


def cargo_version(path: str) -> str:
    with (ROOT / path).open("rb") as fh:
        data = tomllib.load(fh)
    return str(data["package"]["version"])


def package_json_version(path: str) -> str:
    return str(json.loads(read(path))["version"])


def package_lock_versions(path: str) -> list[str]:
    data = json.loads(read(path))
    versions = [str(data["version"])]
    root_pkg = data.get("packages", {}).get("", {})
    if "version" in root_pkg:
        versions.append(str(root_pkg["version"]))
    return versions


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
    expected = cargo_version("crates/tachi-server/Cargo.toml")
    tag = f"v{expected}"
    errors: list[str] = []

    cargo_files = [
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
    ]
    for path in cargo_files:
        require_match(path, cargo_version(path), expected, errors)

    lock_versions = cargo_lock_versions(
        {
            "memcore",
            "memory-server-capture-gate",
            "memory-server-hub-cli",
            "memory-server-manifest-audit",
            "tachi-server",
            "tachi-params",
            "memory-server-prompt-envelope",
            "memory-server-rescue",
            "memory-server-runtime",
            "memory-node",
            "tachi-bootstrap",
            "tachi-dispatch",
            "tachi-foundry",
            "tachi-hub",
            "tachi-llm",
            "tachi-merge-ops",
        }
    )
    for name in [
        "memcore",
        "memory-server-capture-gate",
        "memory-server-hub-cli",
        "memory-server-manifest-audit",
        "tachi-server",
        "tachi-params",
        "memory-server-prompt-envelope",
        "memory-server-rescue",
        "memory-server-runtime",
        "memory-node",
        "tachi-bootstrap",
        "tachi-dispatch",
        "tachi-foundry",
        "tachi-hub",
        "tachi-llm",
        "tachi-merge-ops",
    ]:
        require_match(f"Cargo.lock {name}", lock_versions.get(name, ""), expected, errors)

    json_files = [
        "crates/memory-node/package.json",
        "integrations/openclaw/package.json",
        "packages/tachi-cli/package.json",
    ]
    for path in json_files:
        require_match(path, package_json_version(path), expected, errors)

    lock_files = [
        "crates/memory-node/package-lock.json",
        "integrations/openclaw/package-lock.json",
        "packages/tachi-cli/package-lock.json",
    ]
    for path in lock_files:
        for index, version in enumerate(package_lock_versions(path)):
            require_match(f"{path} version[{index}]", version, expected, errors)

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
