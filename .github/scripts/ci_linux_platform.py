"""Prove Linux platform coverage from the successful workspace run, without rerunning it."""
import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import sys
import xml.etree.ElementTree as ET

from ci_acceptance import decode
from ci_scope import git

ROOT = Path(__file__).resolve().parents[2]
GROUPS = {'seccomp', 'no_clobber', 'anchored', 'receipt', 'exchange'}


def inventory(raw):
    groups = decode(raw)
    if not isinstance(groups, dict) or set(groups) != GROUPS:
        raise ValueError('platform inventory groups missing')
    for label, row in groups.items():
        if (not isinstance(row, dict) or set(row) != {'suite', 'selector', 'tests'}
                or row['suite'] not in {'memcore', 'tachi-server'}
                or not isinstance(row['selector'], str) or not row['selector']
                or not isinstance(row['tests'], list) or not row['tests']
                or any(not isinstance(t, str) or not t for t in row['tests'])
                or len(set(row['tests'])) != len(row['tests'])
                or any(not matches(label, row, row['suite'], t) for t in row['tests'])):
            raise ValueError('invalid platform inventory')
    return groups


def matches(label, row, suite, name):
    return suite == row['suite'] and (name == row['selector'] if label == 'seccomp'
                                       else row['selector'] in name)


def coverage(xml, groups):
    root = ET.fromstring(xml)
    if root.tag != 'testsuites' or root.get('failures') != '0' or root.get('errors') != '0':
        raise ValueError('unsuccessful workspace report')
    cases = [(suite.get('name', ''), test) for suite in root.findall('testsuite')
             for test in suite.findall('testcase')]
    counts = {}
    for label, row in groups.items():
        selected = [(suite, t) for suite, t in cases
                    if matches(label, row, suite, t.get('name', ''))]
        seen = Counter(t.get('name') for _, t in selected)
        if not selected or any(seen[name] != 1 for name in row['tests']) or any(n != 1 for n in seen.values()):
            raise ValueError(f'{label}: missing or duplicate platform test')
        for suite, test in selected:
            # Default-profile dedicated steps previously had zero retries.
            # Reject skip/failure/error AND nextest flaky/rerun annotations;
            # only output is allowed on a first-attempt successful testcase.
            if test.get('classname') != suite or any(child.tag not in {'system-out', 'system-err'} for child in test):
                raise ValueError(f'{label}: skipped, retried or unsuccessful platform test')
        counts[label] = len(selected)
    return counts


def identity():
    sha = git(ROOT, 'rev-parse', 'HEAD').decode().strip()
    tree = git(ROOT, 'rev-parse', 'HEAD^{tree}').decode().strip()
    run_id, attempt = os.environ.get('GITHUB_RUN_ID', ''), os.environ.get('GITHUB_RUN_ATTEMPT', '')
    if (platform.system() != 'Linux' or os.environ.get('GITHUB_SHA') != sha
            or not re.fullmatch(r'[1-9][0-9]*', run_id)
            or not re.fullmatch(r'[1-9][0-9]*', attempt)):
        raise ValueError('unproven Linux workflow identity')
    return {'sha': sha, 'tree': tree, 'run_id': run_id, 'attempt': int(attempt)}


def verify_receipt(receipt, current, digest, groups, producer):
    if producer != 'success' or not isinstance(receipt, dict) or set(receipt) != {
            'schema', 'identity', 'inventory_sha256', 'report_sha256', 'counts'}:
        raise ValueError('missing successful workspace producer')
    source = receipt['identity']
    if (type(receipt['schema']) is not int or receipt['schema'] != 1
            or not isinstance(source, dict) or set(source) != set(current)
            or any(source[k] != current[k] for k in ('sha', 'tree', 'run_id'))
            or type(source['attempt']) is not int or not 1 <= source['attempt'] <= current['attempt']
            or receipt['inventory_sha256'] != digest
            or not isinstance(receipt['report_sha256'], str)
            or not re.fullmatch(r'[0-9a-f]{64}', receipt['report_sha256'])):
        raise ValueError('workspace receipt identity mismatch')
    counts = receipt['counts']
    if (not isinstance(counts, dict) or set(counts) != GROUPS
            or any(type(counts[k]) is not int or counts[k] < len(groups[k]['tests']) for k in GROUPS)):
        raise ValueError('incomplete platform coverage receipt')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=('produce', 'verify'))
    args = parser.parse_args()
    try:
        raw = (ROOT / '.github/linux-platform-tests.json').read_text()
        groups = inventory(raw)
        digest = hashlib.sha256(raw.encode()).hexdigest()
        current = identity()
        if args.mode == 'produce':
            xml = (ROOT / 'target/nextest/ci/junit.xml').read_bytes()
            receipt = {'schema': 1, 'identity': current, 'inventory_sha256': digest,
                       'report_sha256': hashlib.sha256(xml).hexdigest(), 'counts': coverage(xml, groups)}
            # The caller removes any old report BEFORE nextest and invokes us
            # only on that step's success. Never discover/reuse another report.
            with open(os.environ['GITHUB_OUTPUT'], 'a', encoding='utf-8') as out:
                out.write('receipt=' + json.dumps(receipt, sort_keys=True) + '\n')
        else:
            receipt = decode(os.environ.get('PLATFORM_RECEIPT', ''))
            verify_receipt(receipt, current, digest, groups, os.environ.get('WORKSPACE_RESULT'))
        print(json.dumps(receipt, sort_keys=True))
    except (ValueError, OSError, KeyError, ET.ParseError) as error:
        print(f'Linux platform evidence rejected: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
