"""Conservative CI scheduling; unproven input always selects full checks."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import subprocess

PROFILES = {'full', 'archive_prose', 'node_cache', 'post_merge'}


def exclusions(plan: dict, profile: str) -> list[str]:
    profiles = plan.get('applicability_profiles')
    if not isinstance(profiles, dict) or set(profiles) != PROFILES:
        raise ValueError('invalid applicability profiles')
    for name, allowed in [('full', set()), ('archive_prose', {'rust', 'node'}),
                          ('node_cache', {'rust'}), ('post_merge', {'rust', 'node'})]:
        rows = profiles[name]
        if (not isinstance(rows, list) or any(not isinstance(x, str) for x in rows)
                or len(rows) != len(set(rows)) or not set(rows) <= allowed):
            raise ValueError('invalid profile exclusions')
    if profile not in PROFILES:
        raise ValueError('unknown applicability profile')
    return profiles[profile]


def git(repo: Path, *args: str) -> bytes:
    return subprocess.check_output(['git', '-C', str(repo), *args], stderr=subprocess.PIPE)


# Deliberately a byte-level allowlist, not a general YAML/shell parser. Both
# endpoints must contain one known Node cache stanza; everything else must be
# identical. New syntax/pins/versions, commands, guards and mixed edits stay full.
NODE_SETUP = """      - uses: actions/setup-node@a0853c24544627f65ddf259abe73b1d18a591444 # v5.0.0
        with:
          node-version: 20
"""
REMOTE_NPM_CACHE = NODE_SETUP + """          cache: npm
          cache-dependency-path: ${{ matrix.package }}/package-lock.json
"""
LOCAL_NPM_CACHE = """      - name: Select Tachi local npm cache
        # Resolve runner paths inside a step; job-level env cannot use runner context.
        shell: bash
        run: printf 'NPM_CONFIG_CACHE=%s/tachi/npm\\n' "${RUNNER_TOOL_CACHE:?}" >> "$GITHUB_ENV"

""" + NODE_SETUP + """          package-manager-cache: false
"""


def normalize_node_cache(source: str) -> str | None:
    # Require a unique job boundary as well as a unique complete stanza. Only
    # the literal cache leaf under runner-tool-cache/tachi may vary; no shell
    # expressions, traversal, environment variables or YAML aliases are admitted.
    if source.count('\n  node:\n') != 1 or source.count('\n  gitleaks:\n') != 1:
        return None
    before, node = source.split('\n  node:\n')
    if '\n  gitleaks:\n' not in node:
        return None
    node, after = node.split('\n  gitleaks:\n')
    pattern = re.escape(LOCAL_NPM_CACHE).replace('/tachi/npm', '/tachi/[a-z][a-z0-9_-]{0,63}')
    pattern = '(?:' + re.escape(REMOTE_NPM_CACHE) + '|' + pattern + ')'
    matches = list(re.finditer(pattern, node))
    if len(matches) != 1:
        return None
    match = matches[0]
    return before + '\n  node:\n' + node[:match.start()] + '<npm-cache>\n' + node[match.end():] + '\n  gitleaks:\n' + after


def node_cache_only(repo: Path, base: str, checkout: str, path: str) -> bool:
    old = normalize_node_cache(git(repo, 'show', f'{base}:{path}').decode('utf-8'))
    new = normalize_node_cache(git(repo, 'show', f'{checkout}:{path}').decode('utf-8'))
    return old is not None and old == new


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
            # owner-approved PR-only, strict main ruleset requiring
            # `acceptance` must be active before rollout. A non-merge/range push stays full.
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
            elif path == '.github/workflows/ci.yml' and node_cache_only(repo, base, checkout, path):
                kinds.add('node_cache')
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
