"""Fail-closed reverse-dependency crate selection for PR Rust checks."""
from __future__ import annotations

import argparse
from collections import deque
import json
import os
from pathlib import Path
import subprocess
import sys
from typing import Any

from ci_scope import (
    VALID_PROSE_RAW_MODES,
    is_prose_doc_path,
    verified_pr_diff_entries,
)

# Cross-crate compile-time/test source reads (`include_str!`, `include_bytes!`,
# `CARGO_MANIFEST_DIR`) that are not captured by Cargo dependency edges.
EXTRA_REVERSE_DEPS: dict[str, frozenset[str]] = {
    'memcore': frozenset({'tachi-server'}),
    'memory-server-runtime': frozenset({'tachi-server'}),
    'portable-server': frozenset({'tachi-server'}),
}

# `tachi-contract-tests` reads manifests, docs, schemas, and source files across
# workspace crates and completes in ~16s; run it whenever any crate set runs.
ALWAYS_INCLUDED_CRATES: tuple[str, ...] = ('tachi-contract-tests',)

VALID_MEMBER_RAW_MODES = frozenset({
    (':000000', '100644', 'A'),
    (':000000', '100755', 'A'),
    (':100644', '100644', 'M'),
    (':100644', '100755', 'M'),
    (':100755', '100644', 'M'),
    (':100755', '100755', 'M'),
    (':100644', '000000', 'D'),
    (':100755', '000000', 'D'),
})


def _workspace_fallback(reason: str, meta: dict[str, Any] | None = None) -> dict[str, Any]:
    result = {
        'mode': 'workspace',
        'reason': reason,
        'direct_crates': [],
        'crates': [],
        'nextest_args': ['--workspace'],
        'prose_ignored': 0,
    }
    if meta:
        result.update(meta)
    return result


def load_workspace_metadata(repo: Path) -> dict[str, Any]:
    """Load Cargo workspace package metadata without resolving external registry deps."""
    raw = subprocess.check_output(
        [
            'cargo',
            'metadata',
            '--format-version',
            '1',
            '--no-deps',
            '--locked',
            '--offline',
            '--manifest-path',
            str(repo / 'Cargo.toml'),
        ],
        cwd=str(repo),
        stderr=subprocess.PIPE,
    )
    return json.loads(raw.decode('utf-8'))


def build_workspace_graph(
    repo: Path, metadata: dict[str, Any]
) -> tuple[dict[str, str], dict[str, set[str]]]:
    """Return `(member_dirs, reverse_deps)` from `cargo metadata`."""
    packages = metadata.get('packages')
    workspace_ids = metadata.get('workspace_members')
    if not isinstance(packages, list) or not isinstance(workspace_ids, list):
        raise ValueError('invalid cargo metadata shape')
    member_id_set = set(workspace_ids)
    repo_resolved = repo.resolve()
    member_dirs: dict[str, str] = {}
    reverse_deps: dict[str, set[str]] = {}

    member_packages = [
        pkg for pkg in packages
        if isinstance(pkg, dict) and pkg.get('id') in member_id_set
    ]
    if not member_packages:
        raise ValueError('no workspace members in cargo metadata')

    for pkg in member_packages:
        name = pkg.get('name')
        manifest_path = pkg.get('manifest_path')
        if not isinstance(name, str) or not name or not isinstance(manifest_path, str):
            raise ValueError('invalid package metadata')
        manifest_dir = Path(manifest_path).resolve().parent
        rel_dir = manifest_dir.relative_to(repo_resolved).as_posix()
        if rel_dir in ('', '.'):
            raise ValueError('workspace member cannot sit at repository root')
        member_dirs[name] = rel_dir
        reverse_deps.setdefault(name, set())

    for pkg in member_packages:
        name = pkg['name']
        deps = pkg.get('dependencies', ())
        if not isinstance(deps, list):
            raise ValueError('invalid package dependencies')
        for dep in deps:
            if not isinstance(dep, dict):
                continue
            dep_name = dep.get('name')
            # Include all dependency kinds: normal (None), dev, and build.
            if isinstance(dep_name, str) and dep_name in member_dirs and dep_name != name:
                reverse_deps[dep_name].add(name)

    for src_pkg, dependents in EXTRA_REVERSE_DEPS.items():
        if src_pkg in member_dirs:
            for dep_pkg in dependents:
                if dep_pkg in member_dirs and dep_pkg != src_pkg:
                    reverse_deps[src_pkg].add(dep_pkg)

    return member_dirs, reverse_deps


def match_member(path: str, member_dirs: dict[str, str]) -> str | None:
    """Match a repo-relative POSIX path to its owning workspace member, longest prefix first."""
    if not path or path.startswith('/') or '..' in path.split('/'):
        return None
    for name, mdir in sorted(member_dirs.items(), key=lambda item: len(item[1]), reverse=True):
        if path == mdir or path.startswith(mdir + '/'):
            return name
    return None


def select_crates(
    repo: Path,
    event: str,
    requested: str,
    base: str,
    *,
    metadata: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Compute fail-closed nextest crate scope for a verified PR diff."""
    status, meta, entries = verified_pr_diff_entries(repo, event, requested, base)
    if entries is None:
        return _workspace_fallback(status, meta)

    try:
        ws_meta = load_workspace_metadata(repo) if metadata is None else metadata
        member_dirs, reverse_deps = build_workspace_graph(repo, ws_meta)
    except (OSError, subprocess.CalledProcessError, ValueError, KeyError, TypeError):
        return _workspace_fallback('metadata_error', meta)

    direct_crates: set[str] = set()
    prose_ignored = 0
    for old_mode, new_mode, diff_status, path in entries:
        member = match_member(path, member_dirs)
        if member is not None:
            if (old_mode, new_mode, diff_status) not in VALID_MEMBER_RAW_MODES:
                return _workspace_fallback('structural_change', meta)
            direct_crates.add(member)
        elif (old_mode, new_mode, diff_status) in VALID_PROSE_RAW_MODES and is_prose_doc_path(path):
            prose_ignored += 1
        else:
            return _workspace_fallback('non_member_or_locked_path', meta)

    if not direct_crates:
        return _workspace_fallback('no_direct_crates', meta)

    affected: set[str] = set(direct_crates)
    queue: deque[str] = deque(direct_crates)
    while queue:
        current = queue.popleft()
        for dependent in reverse_deps.get(current, ()):
            if dependent not in affected:
                affected.add(dependent)
                queue.append(dependent)

    for always_pkg in ALWAYS_INCLUDED_CRATES:
        if always_pkg in member_dirs:
            affected.add(always_pkg)

    selected = sorted(affected)
    nextest_args = [arg for pkg in selected for arg in ('-p', pkg)]
    return {
        'mode': 'scoped',
        'reason': 'reverse_dependency_closure',
        'direct_crates': sorted(direct_crates),
        'crates': selected,
        'nextest_args': nextest_args,
        'prose_ignored': prose_ignored,
        **meta,
    }


def current_selection() -> dict[str, Any]:
    root = Path(__file__).resolve().parents[2]
    return select_crates(
        root,
        os.environ.get('GITHUB_EVENT_NAME', ''),
        os.environ.get('PR_HEAD_SHA', ''),
        os.environ.get('PR_BASE_SHA', ''),
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group()
    group.add_argument(
        '--nextest-args',
        action='store_true',
        help='Emit nextest target flags (one token per line) to stdout and diagnostics to stderr.',
    )
    group.add_argument(
        '--includes',
        metavar='CRATE',
        help='Exit 0 if CRATE is covered (or workspace fallback applies), 1 if excluded.',
    )
    args = parser.parse_args()

    try:
        selection = current_selection()
    except Exception:  # noqa: BLE001 — fail closed to full workspace coverage
        selection = _workspace_fallback('internal_error')

    diagnostic = json.dumps(
        {
            'mode': selection['mode'],
            'reason': selection['reason'],
            'direct_crates': selection['direct_crates'],
            'crates': selection['crates'],
            'prose_ignored': selection['prose_ignored'],
        },
        sort_keys=True,
    )

    if args.nextest_args:
        print(f'ci_affected_crates: {diagnostic}', file=sys.stderr)
        for token in selection['nextest_args']:
            print(token)
        return 0

    if args.includes:
        print(f'ci_affected_crates: {diagnostic}', file=sys.stderr)
        covered = (
            selection['mode'] == 'workspace'
            or args.includes in selection['crates']
        )
        return 0 if covered else 1

    print(json.dumps(selection, sort_keys=True))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
