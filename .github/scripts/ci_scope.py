"""Conservative CI scheduling; unproven input always selects full checks."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import subprocess

PROFILES = {'full', 'archive_prose', 'node_presentation', 'post_merge'}
NODE_PRESENTATION = {
    'packages/tachi-cli/src/utils/ui.ts',
    'packages/tachi-cli/src/utils/i18n.ts',
}


def exclusions(plan: dict, profile: str) -> list[str]:
    profiles = plan.get('applicability_profiles')
    if not isinstance(profiles, dict) or set(profiles) != PROFILES:
        raise ValueError('invalid applicability profiles')
    for name, allowed in [('full', set()), ('archive_prose', {'rust', 'node'}),
                          ('node_presentation', {'rust'}), ('post_merge', {'rust', 'node'})]:
        rows = profiles[name]
        if (not isinstance(rows, list) or any(not isinstance(x, str) for x in rows)
                or len(rows) != len(set(rows)) or not set(rows) <= allowed):
            raise ValueError('invalid profile exclusions')
    if profile not in PROFILES:
        raise ValueError('unknown applicability profile')
    return profiles[profile]


def git(repo: Path, *args: str) -> bytes:
    return subprocess.check_output(['git', '-C', str(repo), *args], stderr=subprocess.PIPE)


def classify(repo: Path, event: str, requested: str, base: str, *,
             ref: str = '', before: str = '', protected: bool = False) -> dict:
    result = {'profile': 'full', 'reason': 'unproven_diff', 'base': None,
              'requested_head': None, 'checkout_sha': None, 'checkout_tree': None}
    post_merge = event == 'push' and ref == 'refs/heads/main' and protected
    if event != 'pull_request' and not post_merge:
        return dict(result, reason='non_pr_event')
    try:
        for sha in ((requested,) if post_merge else (requested, base)):
            if not re.fullmatch(r'[0-9a-f]{40}|[0-9a-f]{64}', sha):
                return result
            if git(repo, 'rev-parse', '--verify', sha + '^{commit}').decode().strip() != sha:
                return result
        checkout = git(repo, 'rev-parse', 'HEAD').decode().strip()
        tree = git(repo, 'rev-parse', 'HEAD^{tree}').decode().strip()
        result.update(base=before if post_merge else base, requested_head=requested,
                      checkout_sha=checkout, checkout_tree=tree)
        if git(repo, 'status', '--porcelain', '--untracked-files=normal').strip():
            return dict(result, reason='dirty_source')
        if post_merge:
            # Scheduling policy, not a transfer of historical test PASS. The
            # owner-approved PR-only, strict, dual-check main ruleset must be
            # active before rollout. A non-merge/range push stays full.
            parents = git(repo, 'rev-list', '--parents', '-n', '1', 'HEAD').decode().split()
            if checkout == requested and len(parents) == 3 and before == parents[1]:
                return dict(result, profile='post_merge', reason='protected_single_merge_push')
            return dict(result, reason='unproven_merge_push')
        # PR checkout must contain the candidate and advertised base. Missing
        # shallow ancestry is not permission to narrow checks.
        git(repo, 'merge-base', '--is-ancestor', requested, checkout)
        git(repo, 'merge-base', '--is-ancestor', base, checkout)
        raw = git(repo, 'diff', '--raw', '-z', '--no-renames', '--no-abbrev', base, checkout, '--')
        fields = raw.split(b'\0')
        if fields[-1] != b'' or len(fields) < 3 or (len(fields) - 1) % 2:
            return result
        kinds = set()
        for i in range(0, len(fields) - 1, 2):
            meta = fields[i].decode('ascii').split()
            path = fields[i + 1].decode('utf-8')
            # Existing regular files only: additions, deletes, renames (shown
            # as delete/add), mode/type changes and symlinks retain full checks.
            if len(meta) != 5 or meta[0] != ':100644' or meta[1] != '100644' or meta[4] != 'M':
                return dict(result, reason='structural_change')
            if path.startswith('docs/archive/') and path.endswith('.md'):
                kinds.add('archive_prose')
            elif path in NODE_PRESENTATION:
                kinds.add('node_presentation')
            else:
                return dict(result, reason='unclassified_path')
        if len(kinds) == 1:
            return dict(result, profile=kinds.pop(), reason='approved_existing_file_scope')
        return dict(result, reason='mixed_scope')
    except (OSError, subprocess.CalledProcessError, UnicodeError):
        return result


def current_scope(repo: Path) -> dict:
    return classify(repo, os.environ.get('GITHUB_EVENT_NAME', ''),
                    os.environ.get('PR_HEAD_SHA', ''), os.environ.get('PR_BASE_SHA', ''),
                    ref=os.environ.get('GITHUB_REF', ''),
                    before=os.environ.get('PUSH_BEFORE', ''),
                    protected=os.environ.get('GITHUB_REF_PROTECTED') == 'true')


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
                               'run_node': str('node' not in excluded).lower(),
                               'checkout_tree': scope['checkout_tree'] or ''}.items():
                out.write(f'{key}={value}\n')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
