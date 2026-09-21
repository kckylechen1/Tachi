"""Fail-closed aggregation of this workflow's required job families.

Input is GitHub's same-run `needs` context, not caller-written review prose.
This check proves automated CI only; it does not approve review or deployment.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import re
import sys
from typing import Any


class InvalidEvidence(ValueError):
    """The plan or evidence cannot support an acceptance decision."""


def _unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise InvalidEvidence("duplicate JSON field")
        result[key] = value
    return result


def decode(raw: str) -> Any:
    try:
        return json.loads(raw, object_pairs_hook=_unique_object)
    except (ValueError, TypeError) as error:
        # Never echo arbitrary input/outputs or secrets into Actions logs.
        raise InvalidEvidence("invalid JSON evidence") from error


def required_jobs(plan: Any) -> list[str]:
    if not isinstance(plan, dict) or set(plan) != {
        "schema_version", "scope", "required_jobs"
    }:
        raise InvalidEvidence("invalid acceptance plan shape")
    if type(plan["schema_version"]) is not int or plan["schema_version"] != 1:
        raise InvalidEvidence("unsupported acceptance plan version")
    if plan["scope"] != "automated_ci_only":
        raise InvalidEvidence("unsupported acceptance scope")
    jobs = plan["required_jobs"]
    if not isinstance(jobs, list) or not jobs:
        raise InvalidEvidence("empty or invalid required job set")
    if any(not isinstance(job, str) or not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_-]*", job)
           for job in jobs):
        raise InvalidEvidence("invalid required job identity")
    if len(set(jobs)) != len(jobs) or "acceptance" in jobs:
        raise InvalidEvidence("duplicate or recursive required job identity")
    return jobs


def evaluate(plan: Any, needs: Any) -> tuple[bool, dict[str, str]]:
    jobs = required_jobs(plan)
    if not isinstance(needs, dict) or set(needs) != set(jobs):
        raise InvalidEvidence("required job inventory mismatch")
    verdicts: dict[str, str] = {}
    for job in jobs:
        row = needs[job]
        result = row.get("result") if isinstance(row, dict) else None
        if result == "success":
            verdicts[job] = "passed"
        elif result == "failure":
            verdicts[job] = "failed_cause_unclassified"
        elif result == "cancelled":
            verdicts[job] = "cancelled"
        elif result == "skipped":
            verdicts[job] = "not_run"
        else:
            verdicts[job] = "unknown"
    return all(value == "passed" for value in verdicts.values()), verdicts


def main() -> int:
    try:
        root = Path(__file__).resolve().parents[2]
        plan = decode((root / ".github/acceptance-plan.json").read_text(encoding="utf-8"))
        passed, verdicts = evaluate(plan, decode(os.environ.get("CI_NEEDS_JSON", "")))
    except (InvalidEvidence, OSError):
        print("CI acceptance: invalid or missing plan/evidence", file=sys.stderr)
        return 1
    print(json.dumps({"scope": "automated_ci_only", "jobs": verdicts,
                      "accepted": passed}, sort_keys=True))
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
