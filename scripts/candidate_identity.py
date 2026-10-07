"""Record source identity without turning tree equality into test acceptance."""
from __future__ import annotations

import argparse
import json
import re
import subprocess
from pathlib import Path


def git(repo: Path, *args: str) -> str:
    return subprocess.check_output(
        ['git', '-C', str(repo), *args], text=True, stderr=subprocess.PIPE
    ).strip()


def commit(repo: Path, value: str) -> str:
    if not re.fullmatch(r'[0-9a-f]{40}|[0-9a-f]{64}', value):
        raise ValueError('expected a full commit object ID')
    resolved = git(repo, 'rev-parse', '--verify', value + '^{commit}')
    if resolved != value:
        raise ValueError('object is not a commit')
    return resolved


def record(repo: Path, requested: str, base: str = '') -> dict:
    requested = commit(repo, requested)
    if base:
        base = commit(repo, base)
    if git(repo, 'status', '--porcelain', '--untracked-files=normal'):
        raise ValueError('source checkout is dirty')
    checkout = git(repo, 'rev-parse', 'HEAD')
    requested_tree = git(repo, 'rev-parse', requested + '^{tree}')
    checkout_tree = git(repo, 'rev-parse', 'HEAD^{tree}')
    relation = ('same_commit' if checkout == requested else
                'same_tree' if requested_tree == checkout_tree else 'different_tree')
    return {
        'schema_version': 1,
        'scope': 'source_identity_only',
        'requested_head': requested,
        'requested_tree': requested_tree,
        'base': base or None,
        'checkout_sha': checkout,
        'checkout_tree': checkout_tree,
        'relation': relation,
        # This record is deliberately not a test receipt or an acceptance gate.
        'acceptance_granted': False,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', type=Path, default=Path.cwd())
    parser.add_argument('--requested-head', required=True)
    parser.add_argument('--base', default='')
    args = parser.parse_args()
    try:
        result = record(args.repo, args.requested_head, args.base)
    except (ValueError, OSError, subprocess.CalledProcessError):
        parser.exit(1, 'candidate identity: invalid, unavailable or dirty source\n')
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
