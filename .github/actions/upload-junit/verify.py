"""Require a successful report upload, never the continued-error conclusion."""
import json
import os


def confirmed(steps):
    if not isinstance(steps, dict) or steps.get('report', {}).get('outcome') != 'success':
        return False
    for name in ('first', 'second', 'third'):
        row = steps.get(name, {})
        if not isinstance(row, dict):
            return False
        outputs = row.get('outputs', {})
        if (row.get('outcome') == 'success' and isinstance(outputs, dict)
                and str(outputs.get('artifact-id', '')).isdigit()
                and int(outputs['artifact-id']) > 0):
            return True
    return False


if __name__ == '__main__':
    try:
        ok = confirmed(json.loads(os.environ.get('UPLOAD_STEPS', '')))
    except (ValueError, TypeError, AttributeError):
        ok = False
    if not ok:
        raise SystemExit('JUnit upload missing or exhausted; CI remains failed')
    print('JUnit upload confirmed; test outcome remains independent')
