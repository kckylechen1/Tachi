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


def replace_block(text: str, name: str, replacement: str) -> str:
    pattern = rf"  def {name}\b\n.*?\n  end\n?"
    updated, count = re.subn(pattern, replacement + "\n", text, count=1, flags=re.DOTALL)
    if count != 1:
        raise ValueError(f"Could not update {name} block")
    return updated


def ensure_modern_formula_style(text: str) -> str:
    text = text.replace('  license "AGPL-3.0"', '  license "AGPL-3.0-only"')
    install_block = """  def install
    system "cargo", "install", *std_cargo_args(path: "crates/tachi-server"),
           "--bin", "tachi-server"
    mv bin/"tachi-server", bin/"tachi"
  end"""
    text = replace_block(text, "install", install_block)
    return text


def ensure_current_test_block(text: str) -> str:
    test_block = """  test do
    assert_match version.to_s, shell_output("#{bin}/tachi --version")
    assert_match "memory + Hub MCP server", shell_output("#{bin}/tachi --help")
    assert_match "Hub registry", shell_output("#{bin}/tachi hub --help")
    db_path = testpath/"tachi-homebrew-test.db"
    text = "Homebrew smoke test memory from formula verification with enough " \\
           "characters to avoid the capture floor warning. It validates that " \\
           "the installed Tachi binary can save to an isolated MEMORY_DB_PATH."

    saved = shell_output("MEMORY_DB_PATH=#{db_path} #{bin}/tachi --no-project-db save " \\
                         "--path /scratch/homebrew '#{text}'")
    assert_match "\\"status\\": \\"saved", saved

    stats = shell_output("MEMORY_DB_PATH=#{db_path} #{bin}/tachi --no-project-db stats")
    assert_match "\\"total\\": 1", stats
    assert_match db_path.to_s, stats
  end"""
    updated, count = re.subn(
        r"  test do\b\n.*?\n  end\n?",
        test_block + "\n",
        text,
        count=1,
        flags=re.DOTALL,
    )
    if count != 1:
        raise ValueError("Could not update test block")
    return updated


def move_caveats_before_test(text: str) -> str:
    caveats = re.search(r"\n  def caveats\b\n.*?\n  end\n?", text, re.DOTALL)
    test = re.search(r"\n  test do\b\n.*?\n  end\n?", text, re.DOTALL)
    if not caveats or not test or caveats.start() < test.start():
        return text
    caveats_block = caveats.group(0).strip("\n")
    without = text[: caveats.start()] + text[caveats.end() :]
    test = re.search(r"\n  test do\b", without)
    if not test:
        return text
    return without[: test.start()] + "\n" + caveats_block + "\n" + without[test.start() :]


def normalize_blank_lines(text: str) -> str:
    return re.sub(r"\n{3,}", "\n\n", text)


def ensure_service_block(text: str) -> str:
    """Formula service runs a global/background daemon, not a cwd-bound project daemon."""
    service_block = """  service do
    run [opt_bin/"tachi", "--daemon", "--port", "6919", "--no-project-db"]
    environment_variables PATH:                           std_service_path_env,
                          TACHI_DAEMON_IDLE_TIMEOUT_SECS: "0",
                          TACHI_PROFILE:                  "standard"
    log_path var/"log/tachi.log"
    error_log_path var/"log/tachi.err.log"
  end"""

    existing = re.search(r"  service do\n.*?  end\n?", text, re.DOTALL)
    if existing:
        updated = text[: existing.start()] + service_block + "\n\n" + text[existing.end() :]
        return re.sub(r"\n{3,}(  test do\b)", r"\n\n\1", updated)

    insert_point = re.search(r"  test do\b", text)
    if insert_point:
        updated = text[: insert_point.start()] + service_block + "\n\n" + text[insert_point.start() :]
        return re.sub(r"\n{3,}(  test do\b)", r"\n\n\1", updated)

    insert_point = re.search(r"  def caveats\b", text)
    if insert_point:
        return text[: insert_point.start()] + service_block + "\n\n" + text[insert_point.start() :]

    raise ValueError("Could not find insertion point for service block")


def ensure_tachi_hub_install(text: str) -> str:
    """Formula ships only `tachi`; Hub inspection is `tachi hub` (no second binary)."""
    legacy_installs = [
        'bin.install buildpath/"target/release/tachi_hub" => "tachi-hub"',
        'bin.install buildpath/"target/release/tachi-hub" => "tachi-hub"',
        'bin.install buildpath/"scripts/tachi-hub-compat.sh", :rename => "tachi-hub"',
    ]
    for line in legacy_installs:
        text = text.replace(f"    {line}\n", "")
        text = text.replace(f"    {line}", "")

    text = re.sub(
        r'^\s*#\{opt_bin\}/tachi-hub\s*\n',
        "",
        text,
        flags=re.M,
    )
    text = re.sub(
        r'^\s*assert_match version\.to_s, shell_output\("#\{bin\}/tachi-hub --version"\)\s*\n',
        "",
        text,
        flags=re.M,
    )
    text = re.sub(
        r'^\s*assert_match "Inspect Tachi Hub registry", shell_output\("#\{bin\}/tachi-hub --help"\)\s*\n',
        "",
        text,
        flags=re.M,
    )
    text = re.sub(
        r"^\s*tachi-hub\s+stats\s*\n",
        "",
        text,
        flags=re.M,
    )

    if 'shell_output("#{bin}/tachi hub --help")' not in text:
        hook = '    assert_match "memory + Hub MCP server", shell_output("#{bin}/tachi --help")'
        replacement = hook + '\n' + '    assert_match "Hub registry", shell_output("#{bin}/tachi hub --help")'
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

    version_line = '    assert_match version.to_s, shell_output("#{bin}/tachi --version")\n'
    if text.count(version_line) > 1:
        first = text.find(version_line)
        prefix = text[: first + len(version_line)]
        suffix = text[first + len(version_line) :].replace(version_line, "")
        text = prefix + suffix

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
    updated = ensure_modern_formula_style(updated)
    updated = ensure_tachi_hub_install(updated)
    updated = ensure_service_block(updated)
    updated = ensure_current_test_block(updated)
    updated = move_caveats_before_test(updated)
    updated = normalize_blank_lines(updated)

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
