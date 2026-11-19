"""Conservative CI scheduling; unproven input always selects full checks."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import subprocess

PROFILES = {'full', 'archive_prose', 'operational_prose', 'docs_prose'}

PROSE_ROOT_PREFIXES = ('docs/', 'wiki/', 'cards/')

# Markdown files read by Rust compile-time/runtime tests or governing CI/delivery
# policy. Changes to any of these always retain full checks (never skipped as prose).
RUST_OR_GOVERNANCE_LOCKED_DOCS = frozenset({
    'AGENTS.md',
    'CLAUDE.md',
    'README.md',
    'README.zh-CN.md',
    'README.classical.md',
    'prompts/tachi_addendum.md',
    'docs/INSTALL.md',
    'docs/engineering/architecture/dispatch-lifecycle.md',
    'docs/engineering/architecture/downstream-sync-surface.md',
    'docs/engineering/architecture/facade-granularity-and-profile-alignment.md',
    'docs/engineering/architecture/host-adapter-lifecycle-v1.md',
    'docs/engineering/architecture/issue-portfolio-governance.md',
    'docs/engineering/architecture/library-identity-runtime.md',
    'docs/engineering/architecture/pattern-timeline-bonding-memory.md',
    'docs/engineering/architecture/portable-kernel-split.md',
    'docs/engineering/architecture/tachi-continuity-memory-architecture.md',
    'docs/engineering/operations/ci-scheduling.md',
})

VALID_PROSE_RAW_MODES = frozenset({
    (':000000', '100644', 'A'),
    (':100644', '100644', 'M'),
    (':100644', '000000', 'D'),
})


def is_prose_doc_path(path: str) -> bool:
    """Return True iff `path` is a non-locked Markdown path under docs/, wiki/, or cards/."""
    if not path or path.startswith('/') or '..' in path.split('/'):
        return False
    if not path.endswith('.md'):
        return False
    if not any(path.startswith(prefix) for prefix in PROSE_ROOT_PREFIXES):
        return False
    return path not in RUST_OR_GOVERNANCE_LOCKED_DOCS


def exclusions(plan: dict, profile: str) -> list[str]:
    profiles = plan.get('applicability_profiles')
    if not isinstance(profiles, dict) or set(profiles) != PROFILES:
        raise ValueError('invalid applicability profiles')
    for name, allowed in [('full', set()), ('archive_prose', {'rust'}),
                          ('operational_prose', {'rust'}), ('docs_prose', {'rust'})]:
        rows = profiles[name]
        if (not isinstance(rows, list) or any(not isinstance(x, str) for x in rows)
                or len(rows) != len(set(rows)) or not set(rows) <= allowed):
            raise ValueError('invalid profile exclusions')
    if profile not in PROFILES:
        raise ValueError('unknown applicability profile')
    return profiles[profile]


def git(repo: Path, *args: str) -> bytes:
    return subprocess.check_output(['git', '-C', str(repo), *args], stderr=subprocess.PIPE)


def verified_pr_diff_entries(
    repo: Path, event: str, requested: str, base: str
) -> tuple[str, dict, list[tuple[str, str, str, str]] | None]:
    """Verify PR git provenance/cleanliness and parse `git diff --raw -z` entries."""
    result = {'base': None, 'requested_head': None,
              'checkout_sha': None, 'checkout_tree': None}
    # Pushes to main (each merge) and manual runs always take full checks: PRs
    # are not required to be up to date with main, so main is where merged
    # results are integration-tested.
    if event != 'pull_request':
        return 'non_pr_event', result, None
    try:
        for sha in (requested, base):
            if not re.fullmatch(r'[0-9a-f]{40}|[0-9a-f]{64}', sha):
                return 'unproven_diff', result, None
            if git(repo, 'rev-parse', '--verify', sha + '^{commit}').decode().strip() != sha:
                return 'unproven_diff', result, None
        checkout = git(repo, 'rev-parse', 'HEAD').decode().strip()
        tree = git(repo, 'rev-parse', 'HEAD^{tree}').decode().strip()
        result.update(base=base, requested_head=requested,
                      checkout_sha=checkout, checkout_tree=tree)
        if git(repo, 'status', '--porcelain', '--untracked-files=normal').strip():
            return 'dirty_source', result, None
        # PR checkout must contain the candidate and advertised base. Missing
        # shallow ancestry is not permission to narrow checks.
        git(repo, 'merge-base', '--is-ancestor', requested, checkout)
        git(repo, 'merge-base', '--is-ancestor', base, checkout)
        raw = git(repo, 'diff', '--raw', '-z', '--no-renames', '--no-abbrev', base, checkout, '--')
        fields = raw.split(b'\0')
        if fields[-1] != b'' or len(fields) < 3 or (len(fields) - 1) % 2:
            return 'unproven_diff', result, None
        entries: list[tuple[str, str, str, str]] = []
        for i in range(0, len(fields) - 1, 2):
            meta = fields[i].decode('ascii').split()
            path = fields[i + 1].decode('utf-8')
            if len(meta) != 5:
                return 'unproven_diff', result, None
            entries.append((meta[0], meta[1], meta[4], path))
        return 'ok', result, entries
    except (OSError, subprocess.CalledProcessError, UnicodeError):
        return 'unproven_diff', result, None


def classify(repo: Path, event: str, requested: str, base: str) -> dict:
    status, meta, entries = verified_pr_diff_entries(repo, event, requested, base)
    result = {'profile': 'full', 'reason': status, **meta}
    if entries is None:
        return result
    for old_mode, new_mode, diff_status, path in entries:
        # Regular non-executable files only (A/M/D, with renames represented as
        # D+A via --no-renames): executable modes, symlinks, and type changes retain full checks.
        if (old_mode, new_mode, diff_status) not in VALID_PROSE_RAW_MODES:
            return dict(result, reason='structural_change')
        if not is_prose_doc_path(path):
            return dict(result, reason='unclassified_path')
    return dict(result, profile='docs_prose', reason='approved_prose_scope')


def current_scope(repo: Path) -> dict:
    return classify(repo, os.environ.get('GITHUB_EVENT_NAME', ''),
                    os.environ.get('PR_HEAD_SHA', ''), os.environ.get('PR_BASE_SHA', ''))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--github-output', action='store_true')
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    scope = current_scope(root)
    plan = json.loads((root / '.github/acceptance-plan.json').read_text())
    excluded = exclusions(plan, scope['profile'])
    print(json.dumps(scope, sort_keys=True))
    if args.github_output:
        with open(os.environ['GITHUB_OUTPUT'], 'a', encoding='utf-8') as out:
            # Fixed fields and closed values only; never emit filenames or
            # arbitrary source text into the Actions output protocol.
            for key, value in {'profile': scope['profile'],
                               'run_rust': str('rust' not in excluded).lower(),
                               'checkout_tree': scope['checkout_tree'] or ''}.items():
                out.write(f'{key}={value}\n')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
