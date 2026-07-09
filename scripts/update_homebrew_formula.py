#!/usr/bin/env python3
"""Update the public Homebrew formula for Tachi.

Public distribution is **binary-first** (#728 / #874):

- Source repo ``kckylechen1/tachi`` is private — ``archive/refs/tags/*.tar.gz``
  is not a valid unauthenticated install URL.
- Public artifacts live on ``kckylechen1/homebrew-tachi`` releases
  (tag ``tachi-{version}``), e.g.
  ``tachi-v1.7.0-aarch64-apple-darwin.tar.gz``.
- The formula downloads that tarball and installs the ``tachi`` binary.
  End users do **not** compile from source and do **not** need Rust.

``--mode source`` remains available for maintainer bottle rebuilds that run
inside authenticated CI with a checked-out tree, but is not the public path.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import re
import sys
import urllib.error
import urllib.request


DEFAULT_SOURCE_REPO = "kckylechen1/tachi"
DEFAULT_ASSET_REPO = "kckylechen1/homebrew-tachi"
DEFAULT_FORMULA_CLASS = "Tachi"

# Public binary triples published alongside a release. Expand when CI ships more.
DEFAULT_BINARY_PLATFORMS: dict[str, str] = {
    # Homebrew CPU selector → release asset triple
    "arm": "aarch64-apple-darwin",
    # "intel": "x86_64-apple-darwin",  # enable when asset is published
}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Update a Homebrew formula for Tachi. Default mode is public binary "
            "artifacts on the homebrew-tachi tap (#728/#874)."
        )
    )
    parser.add_argument("formula", help="Path to the formula file to update")
    parser.add_argument(
        "--version", required=True, help="Version without leading v, e.g. 1.7.0"
    )
    parser.add_argument(
        "--mode",
        choices=("binary", "source"),
        default="binary",
        help="binary (default, public) or source (private-archive / bottle rebuild)",
    )
    parser.add_argument(
        "--repo",
        default=DEFAULT_SOURCE_REPO,
        help=f"Source GitHub repository (default: {DEFAULT_SOURCE_REPO})",
    )
    parser.add_argument(
        "--asset-repo",
        default=DEFAULT_ASSET_REPO,
        help=(
            "Public repo hosting binary release assets "
            f"(default: {DEFAULT_ASSET_REPO})"
        ),
    )
    parser.add_argument(
        "--asset-tag",
        default=None,
        help="Release tag on asset-repo (default: tachi-{version})",
    )
    parser.add_argument(
        "--formula-class",
        default=DEFAULT_FORMULA_CLASS,
        help=f"Formula class name used for diagnostics (default: {DEFAULT_FORMULA_CLASS})",
    )
    parser.add_argument(
        "--bottle-manifest",
        help="Path to merged JSON manifest from brew bottle --json (optional bottle block)",
    )
    parser.add_argument(
        "--bottle-root-url",
        help="Base URL for bottle downloads (e.g. https://github.com/owner/repo/releases/download/tag)",
    )
    parser.add_argument(
        "--sha256",
        action="append",
        default=[],
        metavar="TRIPLE=HEX",
        help=(
            "Skip download and use this sha256 for a binary triple "
            "(repeatable). Example: aarch64-apple-darwin=abc..."
        ),
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Print the updated formula to stdout; do not write the file",
    )
    return parser.parse_args()


def download_sha256(url: str) -> str:
    sha = hashlib.sha256()
    try:
        with urllib.request.urlopen(url) as response:
            while True:
                chunk = response.read(1024 * 1024)
                if not chunk:
                    break
                sha.update(chunk)
    except urllib.error.HTTPError as exc:
        raise RuntimeError(
            f"Failed to download {url}: HTTP {exc.code}. "
            "For public binary mode the asset must already be on the public "
            "tap release (homebrew-tachi), not the private source archive."
        ) from exc
    return sha.hexdigest()


def parse_sha_overrides(entries: list[str]) -> dict[str, str]:
    out: dict[str, str] = {}
    for entry in entries:
        if "=" not in entry:
            raise ValueError(f"--sha256 expects TRIPLE=HEX, got: {entry!r}")
        triple, hex_digest = entry.split("=", 1)
        triple = triple.strip()
        hex_digest = hex_digest.strip().lower()
        if not re.fullmatch(r"[0-9a-f]{64}", hex_digest):
            raise ValueError(f"invalid sha256 for {triple}: {hex_digest!r}")
        out[triple] = hex_digest
    return out


def binary_asset_name(version: str, triple: str) -> str:
    return f"tachi-v{version}-{triple}.tar.gz"


def binary_asset_url(asset_repo: str, asset_tag: str, version: str, triple: str) -> str:
    name = binary_asset_name(version, triple)
    return f"https://github.com/{asset_repo}/releases/download/{asset_tag}/{name}"


def replace_or_fail(pattern: str, replacement: str, text: str, label: str) -> str:
    updated, count = re.subn(pattern, replacement, text, count=1, flags=re.MULTILINE)
    if count != 1:
        raise ValueError(f"Could not update {label} in formula")
    return updated


def build_bottle_block(
    root_url: str, platforms: dict[str, str], cellar: str = ":any_skip_relocation"
) -> str:
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
    existing = re.search(r"  bottle do\n.*?  end\n?", text, re.DOTALL)
    if existing:
        return text[: existing.start()] + bottle_block + "\n" + text[existing.end() :]

    insert_point = re.search(r"  def install\b", text)
    if insert_point:
        return (
            text[: insert_point.start()]
            + bottle_block
            + "\n\n"
            + text[insert_point.start() :]
        )

    raise ValueError("Could not find insertion point for bottle block (no def install found)")


def replace_block(text: str, name: str, replacement: str) -> str:
    pattern = rf"  def {name}\b\n.*?\n  end\n?"
    updated, count = re.subn(pattern, replacement + "\n", text, count=1, flags=re.DOTALL)
    if count != 1:
        raise ValueError(f"Could not update {name} block")
    return updated


def strip_disable_and_head(text: str) -> str:
    text = re.sub(
        r'^\s*disable!.*\n',
        "",
        text,
        flags=re.MULTILINE,
    )
    text = re.sub(
        r'^\s*head ".*".*\n',
        "",
        text,
        flags=re.MULTILINE,
    )
    return text


def strip_rust_build_dep(text: str) -> str:
    text = re.sub(
        r'^\s*depends_on "rust"\s*=>\s*:build\s*\n',
        "",
        text,
        flags=re.MULTILINE,
    )
    return text


def ensure_binary_install(text: str) -> str:
    """Public binary formula: unpack release tarball and install `tachi`."""
    install_block = """  def install
    # Public binary tarball layout:
    #   tachi-vX.Y.Z-<triple>/tachi
    # Homebrew cds into the single top-level directory when present.
    bin.install "tachi"
  end"""
    return replace_block(text, "install", install_block)


def ensure_source_install(text: str) -> str:
    """Legacy/source install for authenticated bottle rebuilds only."""
    install_block = """  def install
    system "cargo", "install", *std_cargo_args(path: "crates/tachi-server"),
           "--bin", "tachi-server"
    mv bin/"tachi-server", bin/"tachi"
  end"""
    text = replace_block(text, "install", install_block)
    if 'depends_on "rust" => :build' not in text:
        # Place after license line when present.
        text = re.sub(
            r'(^\s*license\s+".*"\s*\n)',
            r'\1\n  depends_on "rust" => :build\n',
            text,
            count=1,
            flags=re.MULTILINE,
        )
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
        updated = (
            text[: insert_point.start()]
            + service_block
            + "\n\n"
            + text[insert_point.start() :]
        )
        return re.sub(r"\n{3,}(  test do\b)", r"\n\n\1", updated)

    insert_point = re.search(r"  def caveats\b", text)
    if insert_point:
        return (
            text[: insert_point.start()]
            + service_block
            + "\n\n"
            + text[insert_point.start() :]
        )

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
        r"^\s*#\{opt_bin\}/tachi-hub\s*\n",
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
        replacement = (
            hook
            + "\n"
            + '    assert_match "Hub registry", shell_output("#{bin}/tachi hub --help")'
        )
        text = replace_or_fail(
            r'^\s*assert_match "memory \+ Hub MCP server", shell_output\("#\{bin\}/tachi --help"\)$',
            replacement,
            text,
            "tachi hub formula test",
        )

    if "tachi hub stats" not in text:
        hook = "        tachi --no-project-db stats"
        replacement = hook + "\n        tachi hub stats"
        text = replace_or_fail(
            r"^\s*tachi --no-project-db stats$",
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


def ensure_livecheck_public_assets(text: str, asset_repo: str) -> str:
    """Point livecheck at public tap releases, not the private source repo."""
    # Ruby formula livecheck — backslashes must not pass through re.sub replacement
    # (Python would treat ``\d`` as an invalid group reference).
    block = (
        "  livecheck do\n"
        f'    url "https://github.com/{asset_repo}/releases/latest"\n'
        r'    regex(/tachi[._-]v?(\d+(?:\.\d+)+)/i)' "\n"
        "  end"
    )
    match = re.search(r"  livecheck do\n.*?\n  end\n?", text, flags=re.DOTALL)
    if match:
        return text[: match.start()] + block + "\n" + text[match.end() :]

    license_match = re.search(r'^\s*license\s+".*"\s*\n', text, flags=re.MULTILINE)
    if license_match:
        insert_at = license_match.end()
        return text[:insert_at] + "\n" + block + "\n" + text[insert_at:]
    return text + "\n" + block + "\n"


def apply_binary_urls(
    text: str,
    *,
    version: str,
    asset_repo: str,
    asset_tag: str,
    platform_triples: dict[str, str],
    sha_by_triple: dict[str, str],
) -> str:
    """Rewrite url/sha256 as on_macos/on_arm (and optional on_intel) blocks."""
    if "arm" not in platform_triples:
        raise ValueError("binary mode requires at least the arm (aarch64) platform")

    # Drop any previous url/sha256 lines and platform url blocks we manage.
    text = re.sub(r"^\s*url \".*\"\s*\n", "", text, flags=re.MULTILINE)
    text = re.sub(r"^\s*sha256 \".*\"\s*\n", "", text, flags=re.MULTILINE)
    text = re.sub(
        r"\n  on_macos do\n.*?\n  end\n",
        "\n",
        text,
        count=1,
        flags=re.DOTALL,
    )
    text = re.sub(
        r"^\s*depends_on arch: :arm64\s*\n",
        "",
        text,
        flags=re.MULTILINE,
    )

    arm_triple = platform_triples["arm"]
    arm_url = binary_asset_url(asset_repo, asset_tag, version, arm_triple)
    arm_sha = sha_by_triple[arm_triple]

    macos_lines = [
        "  on_macos do",
        "    on_arm do",
        f'      url "{arm_url}"',
        f'      sha256 "{arm_sha}"',
        "    end",
    ]
    if "intel" in platform_triples:
        intel_triple = platform_triples["intel"]
        intel_url = binary_asset_url(asset_repo, asset_tag, version, intel_triple)
        intel_sha = sha_by_triple[intel_triple]
        macos_lines.extend(
            [
                "    on_intel do",
                f'      url "{intel_url}"',
                f'      sha256 "{intel_sha}"',
                "    end",
            ]
        )
    else:
        # Fail loudly on Intel until a public binary exists.
        macos_lines.extend(
            [
                "    on_intel do",
                '      odie "Tachi public binaries are arm64-only for now; '
                'see https://github.com/kckylechen1/homebrew-tachi"',
                "    end",
            ]
        )
    macos_lines.append("  end")
    macos_block = "\n".join(macos_lines) + "\n"

    # Insert platform urls after version or license.
    if re.search(r'^\s*version "', text, flags=re.MULTILINE):
        text = re.sub(
            r'(^\s*version ".*"\s*\n)',
            r"\1" + macos_block,
            text,
            count=1,
            flags=re.MULTILINE,
        )
    else:
        # Inject explicit version + urls after homepage.
        text = re.sub(
            r'(^\s*homepage ".*"\s*\n)',
            rf'\1  version "{version}"\n' + macos_block,
            text,
            count=1,
            flags=re.MULTILINE,
        )

    # Ensure version field exists and matches.
    if re.search(r'^\s*version "', text, flags=re.MULTILINE):
        text = re.sub(
            r'^\s*version ".*"\s*$',
            f'  version "{version}"',
            text,
            count=1,
            flags=re.MULTILINE,
        )
    else:
        text = re.sub(
            r'(^\s*homepage ".*"\s*\n)',
            rf'\1  version "{version}"\n',
            text,
            count=1,
            flags=re.MULTILINE,
        )

    return text


def apply_source_url(text: str, *, repo: str, version: str, sha256: str) -> str:
    tag = f"v{version}"
    tarball_url = f"https://github.com/{repo}/archive/refs/tags/{tag}.tar.gz"
    # Source mode keeps a single url/sha256 (private; CI-auth only).
    if re.search(r'^\s*url "', text, flags=re.MULTILINE):
        text = replace_or_fail(
            r'^\s*url ".*"$', f'  url "{tarball_url}"', text, "url"
        )
    else:
        text = re.sub(
            r'(^\s*homepage ".*"\s*\n)',
            rf'\1  url "{tarball_url}"\n',
            text,
            count=1,
            flags=re.MULTILINE,
        )
    if re.search(r'^\s*sha256 "', text, flags=re.MULTILINE):
        text = replace_or_fail(
            r'^\s*sha256 ".*"$', f'  sha256 "{sha256}"', text, "sha256"
        )
    else:
        text = re.sub(
            r'(^\s*url ".*"\s*\n)',
            rf'\1  sha256 "{sha256}"\n',
            text,
            count=1,
            flags=re.MULTILINE,
        )
    # Strip binary-only on_macos url blocks if present.
    text = re.sub(
        r"\n  on_macos do\n.*?\n  end\n",
        "\n",
        text,
        count=1,
        flags=re.DOTALL,
    )
    return text


def maybe_apply_bottles(
    text: str, bottle_manifest: str | None, bottle_root_url: str | None
) -> str:
    if not bottle_manifest or not bottle_root_url:
        return text
    with open(bottle_manifest, encoding="utf-8") as fh:
        manifest = json.load(fh)

    platforms: dict[str, str] = {}
    cellar = ":any_skip_relocation"
    for entry in manifest:
        for _formula_name, info in entry.items():
            if isinstance(info, dict) and "tags" in info:
                cellar = info.get("cellar", ":any_skip_relocation")
                for platform_tag, tag_info in info["tags"].items():
                    platforms[platform_tag] = tag_info["sha256"]

    if platforms:
        bottle_block = build_bottle_block(bottle_root_url, platforms, cellar)
        text = update_bottle_block(text, bottle_block)
        print(f"  bottle platforms: {', '.join(sorted(platforms))}")
    return text


def main() -> int:
    args = parse_args()
    formula_path = pathlib.Path(args.formula)
    if not formula_path.exists():
        raise FileNotFoundError(f"Formula not found: {formula_path}")

    version = args.version.removeprefix("v")
    asset_tag = args.asset_tag or f"tachi-{version}"
    sha_overrides = parse_sha_overrides(args.sha256)

    updated = formula_path.read_text(encoding="utf-8")
    updated = strip_disable_and_head(updated)
    # Normalize license spelling.
    updated = updated.replace('  license "AGPL-3.0"', '  license "AGPL-3.0-only"')

    if args.mode == "binary":
        platform_triples = dict(DEFAULT_BINARY_PLATFORMS)
        sha_by_triple: dict[str, str] = {}
        for _cpu, triple in platform_triples.items():
            if triple in sha_overrides:
                sha_by_triple[triple] = sha_overrides[triple]
            else:
                url = binary_asset_url(args.asset_repo, asset_tag, version, triple)
                print(f"  fetching sha256: {url}")
                sha_by_triple[triple] = download_sha256(url)

        updated = apply_binary_urls(
            updated,
            version=version,
            asset_repo=args.asset_repo,
            asset_tag=asset_tag,
            platform_triples=platform_triples,
            sha_by_triple=sha_by_triple,
        )
        updated = strip_rust_build_dep(updated)
        updated = ensure_binary_install(updated)
        updated = ensure_livecheck_public_assets(updated, args.asset_repo)
        primary_url = binary_asset_url(
            args.asset_repo, asset_tag, version, platform_triples["arm"]
        )
        primary_sha = sha_by_triple[platform_triples["arm"]]
    else:
        tag = f"v{version}"
        tarball_url = f"https://github.com/{args.repo}/archive/refs/tags/{tag}.tar.gz"
        if args.repo.replace("/", "-") in sha_overrides:
            # allow --sha256 source=...
            sha256 = sha_overrides.get("source") or next(iter(sha_overrides.values()))
        elif "source" in sha_overrides:
            sha256 = sha_overrides["source"]
        else:
            print(f"  fetching sha256: {tarball_url}")
            sha256 = download_sha256(tarball_url)
        updated = apply_source_url(
            updated, repo=args.repo, version=version, sha256=sha256
        )
        updated = ensure_source_install(updated)
        primary_url = tarball_url
        primary_sha = sha256

    updated = ensure_tachi_hub_install(updated)
    updated = ensure_service_block(updated)
    updated = ensure_current_test_block(updated)
    updated = move_caveats_before_test(updated)
    updated = maybe_apply_bottles(updated, args.bottle_manifest, args.bottle_root_url)
    updated = normalize_blank_lines(updated)

    if args.dry_run:
        sys.stdout.write(updated)
    else:
        formula_path.write_text(updated, encoding="utf-8")

    print(f"Updated {args.formula_class} formula ({args.mode})")
    print(f"  version: {version}")
    print(f"  url: {primary_url}")
    print(f"  sha256: {primary_sha}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:
        print(f"error: {exc}", file=sys.stderr)
        raise SystemExit(1)
