#!/usr/bin/env python3

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import re
import sys
import urllib.request


DEFAULT_REPO = "kckylechen1/tachi"
DEFAULT_FORMULA_CLASS = "Tachi"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Update a Homebrew formula to the current Tachi release tarball and sha256."
    )
    parser.add_argument("formula", help="Path to the formula file to update")
    parser.add_argument(
        "--version", required=True, help="Version without leading v, e.g. 0.12.2"
    )
    parser.add_argument(
        "--repo",
        default=DEFAULT_REPO,
        help=f"GitHub repository in owner/name form (default: {DEFAULT_REPO})",
    )
    parser.add_argument(
        "--formula-class",
        default=DEFAULT_FORMULA_CLASS,
        help=f"Formula class name used for diagnostics (default: {DEFAULT_FORMULA_CLASS})",
    )
    parser.add_argument(
        "--bottle-manifest",
        help="Path to merged JSON manifest from brew bottle --json (enables bottle block)",
    )
    parser.add_argument(
        "--bottle-root-url",
        help="Base URL for bottle downloads (e.g. https://github.com/owner/repo/releases/download/tag)",
    )
    return parser.parse_args()


def download_sha256(url: str) -> str:
    sha = hashlib.sha256()
    with urllib.request.urlopen(url) as response:
        while True:
            chunk = response.read(1024 * 1024)
            if not chunk:
                break
            sha.update(chunk)
    return sha.hexdigest()


def replace_or_fail(pattern: str, replacement: str, text: str, label: str) -> str:
    updated, count = re.subn(pattern, replacement, text, count=1, flags=re.MULTILINE)
    if count != 1:
        raise ValueError(f"Could not update {label} in formula")
    return updated


def build_bottle_block(root_url: str, platforms: dict[str, str], cellar: str = ":any_skip_relocation") -> str:
    """Generate a bottle do...end block from platform sha256 hashes."""
    lines = ["  bottle do"]
    lines.append(f'    root_url "{root_url}"')
    for platform in sorted(platforms):
        sha = platforms[platform]
        lines.append(f'    sha256 cellar: {cellar}, {platform}: "{sha}"')
    lines.append("  end")
    return "\n".join(lines)


def update_bottle_block(text: str, bottle_block: str) -> str:
    """Insert or replace the bottle do...end block in the formula."""
    # Replace existing bottle block if present
    existing = re.search(r"  bottle do\n.*?  end\n?", text, re.DOTALL)
    if existing:
        return text[: existing.start()] + bottle_block + "\n" + text[existing.end() :]

    # Insert before def install
    insert_point = re.search(r"  def install\b", text)
    if insert_point:
        return text[: insert_point.start()] + bottle_block + "\n\n" + text[insert_point.start() :]

    raise ValueError("Could not find insertion point for bottle block (no def install found)")


def ensure_tachi_hub_install(text: str) -> str:
    """Install a `tachi-hub` shim that forwards to `tachi hub` (no second Rust binary)."""
    shim_src = 'buildpath/"scripts/tachi-hub-compat.sh"'
    shim_install = f'    bin.install {shim_src}, :rename => "tachi-hub"'

    text = text.replace(
        'bin.install buildpath/"target/release/tachi_hub" => "tachi-hub"',
        shim_install,
    )
    text = text.replace(
        'bin.install buildpath/"target/release/tachi-hub" => "tachi-hub"',
        shim_install,
    )

    if shim_install not in text:
        install_line = '    bin.install buildpath/"target/release/memory-server" => "tachi"'
        replacement = install_line + "\n" + shim_install
        text = replace_or_fail(
            r'^\s*bin\.install buildpath/"target/release/memory-server" => "tachi"$',
            replacement,
            text,
            "tachi-hub compat shim install",
        )

    if 'shell_output("#{bin}/tachi hub --help")' not in text:
        hook = '    assert_match "memory + Hub MCP server", shell_output("#{bin}/tachi --help")'
        replacement = (
            hook
            + '\n'
            + '    assert_match version.to_s, shell_output("#{bin}/tachi --version")\n'
            + '    assert_match "Hub registry", shell_output("#{bin}/tachi hub --help")'
        )
        text = replace_or_fail(
            r'^\s*assert_match "memory \+ Hub MCP server", shell_output\("#\{bin\}/tachi --help"\)$',
            replacement,
            text,
            "tachi hub formula test",
        )

    if 'tachi hub stats' not in text:
        hook = '        tachi --no-project-db stats'
        replacement = hook + '\n        tachi hub stats'
        text = replace_or_fail(
            r'^\s*tachi --no-project-db stats$',
            replacement,
            text,
            "tachi hub caveat smoke test",
        )

    return text


def main() -> int:
    args = parse_args()
    formula_path = pathlib.Path(args.formula)
    if not formula_path.exists():
        raise FileNotFoundError(f"Formula not found: {formula_path}")

    version = args.version.removeprefix("v")
    tag = f"v{version}"
    tarball_url = f"https://github.com/{args.repo}/archive/refs/tags/{tag}.tar.gz"
    sha256 = download_sha256(tarball_url)

    updated = formula_path.read_text()
    updated = replace_or_fail(
        r'^\s*url ".*"$', f'  url "{tarball_url}"', updated, "url"
    )
    updated = replace_or_fail(
        r'^\s*sha256 ".*"$', f'  sha256 "{sha256}"', updated, "sha256"
    )
    updated = ensure_tachi_hub_install(updated)

    # Optional: inject bottle block
    if args.bottle_manifest and args.bottle_root_url:
        with open(args.bottle_manifest) as f:
            manifest = json.load(f)

        # Extract per-platform sha256 from merged manifest array
        platforms: dict[str, str] = {}
        cellar = ":any_skip_relocation"
        for entry in manifest:
            for _formula_name, info in entry.items():
                if isinstance(info, dict) and "tags" in info:
                    cellar = info.get("cellar", ":any_skip_relocation")
                    for platform_tag, tag_info in info["tags"].items():
                        platforms[platform_tag] = tag_info["sha256"]

        if platforms:
            bottle_block = build_bottle_block(args.bottle_root_url, platforms, cellar)
            updated = update_bottle_block(updated, bottle_block)
            print(f"  bottle platforms: {', '.join(sorted(platforms))}")

    formula_path.write_text(updated)

    print(f"Updated {args.formula_class} formula")
    print(f"  version: {version}")
    print(f"  url: {tarball_url}")
    print(f"  sha256: {sha256}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:
        print(f"error: {exc}", file=sys.stderr)
        raise SystemExit(1)
