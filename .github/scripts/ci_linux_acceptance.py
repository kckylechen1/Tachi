"""Closed Linux aggregation using the same scheduling authority as Full CI."""
import os
from pathlib import Path
import sys
import json

from ci_acceptance import InvalidEvidence, classify_jobs, decode, _verdict
from ci_scope import current_scope, exclusions


JOBS = {'change-scope', 'rust-gate', 'linux-platform'}


def evaluate(plan, needs, scope):
    classify_jobs(plan)
    if not isinstance(needs, dict) or set(needs) != JOBS:
        raise InvalidEvidence('Linux job inventory mismatch')
    producer = needs['change-scope']
    if not isinstance(producer, dict) or producer.get('result') != 'success':
        raise InvalidEvidence('Linux scope producer failed or missing')
    outputs = producer.get('outputs', {})
    if (not isinstance(outputs, dict) or outputs.get('profile') != scope['profile']
            or outputs.get('checkout_tree') != (scope['checkout_tree'] or '')):
        raise InvalidEvidence('Linux scope producer mismatch')
    omit_rust = 'rust' in exclusions(plan, scope['profile'])
    verdicts = {'change-scope': 'passed'}
    for job in ('rust-gate', 'linux-platform'):
        row = needs[job]
        if omit_rust and isinstance(row, dict) and row.get('result') == 'skipped':
            verdicts[job] = 'not_applicable'
        else:
            verdicts[job] = _verdict(row, required=True)
    return all(value in {'passed', 'not_applicable'} for value in verdicts.values()), verdicts


def main():
    root = Path(__file__).resolve().parents[2]
    try:
        plan = decode((root / '.github/acceptance-plan.json').read_text())
        scope = current_scope(root)
        passed, verdicts = evaluate(plan, decode(os.environ.get('CI_NEEDS_JSON', '')), scope)
    except (InvalidEvidence, OSError, ValueError):
        print('Linux acceptance: invalid or missing evidence', file=sys.stderr)
        return 1
    print(json.dumps({'scope': 'automated_ci_only', 'applicability': scope, 'jobs': verdicts}))
    return 0 if passed else 1


if __name__ == '__main__':
    raise SystemExit(main())
