"""Fail-closed aggregation of this workflow's job families.

Input is GitHub's same-run `needs` context, not caller-written review prose.
This check proves automated CI only; it does not approve review or deployment.

Platform applicability -- owner adjudication 2026-09-23 (current delivery
only, after confirming there is no current Windows usage):
`physical-db-identity-windows` is classified OBSERVATIONAL in
`.github/acceptance-plan.json`. Its compatibility stays visible -- all three
crate legs keep executing on windows-latest, raw status is collected and
reported, never rewritten or faked -- but a non-success result does not block
this delivery's CI acceptance. The exclusion is a named per-job plan entry,
not a per-PR flag or general waiver: every other job stays required and fails
closed on failure, cancellation, skip, or unknown evidence, and the needs
inventory stays closed, so an unknown, extra, or dropped job (including the
observational one) is rejected outright. Not retroactive: earlier runs keep
their original verdicts. Windows releases stay prohibited until #1963 is
fixed, and the trusted acceptance runner is macOS-arm64, so this changes no
Linux coverage claim.
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


OBSERVATIONAL_POLICY = (
    "observational exclusions adjudicated 2026-09-23 (owner): "
    "current-delivery acceptance only; raw Windows status retained and "
    "reported, never faked; windows release prohibition until #1963 fixed"
)


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


def classify_jobs(plan: Any) -> tuple[list[str], list[str]]:
    """Split the closed plan shape into required and observational jobs."""
    if not isinstance(plan, dict) or set(plan) != {
        "schema_version", "scope", "required_jobs", "observational_jobs"
    }:
        raise InvalidEvidence("invalid acceptance plan shape")
    if type(plan["schema_version"]) is not int or plan["schema_version"] != 2:
        raise InvalidEvidence("unsupported acceptance plan version")
    if plan["scope"] != "automated_ci_only":
        raise InvalidEvidence("unsupported acceptance scope")
    required = plan["required_jobs"]
    observational = plan["observational_jobs"]
    if not isinstance(required, list) or not required:
        raise InvalidEvidence("empty or invalid required job set")
    if not isinstance(observational, list):
        raise InvalidEvidence("invalid observational job set")
    for field, jobs in (("required_jobs", required),
                        ("observational_jobs", observational)):
        if any(not isinstance(job, str) or not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_-]*", job)
               for job in jobs):
            raise InvalidEvidence(f"invalid {field} identity")
        if len(set(jobs)) != len(jobs) or "acceptance" in jobs:
            raise InvalidEvidence(f"duplicate or recursive {field} identity")
    if set(required) & set(observational):
        raise InvalidEvidence("job classified as both required and observational")
    return required, observational


def _verdict(row: Any, required: bool) -> str:
    result = row.get("result") if isinstance(row, dict) else None
    if required:
        if result == "success":
            return "passed"
        if result == "failure":
            return "failed_cause_unclassified"
        if result == "cancelled":
            return "cancelled"
        if result == "skipped":
            return "not_run"
        return "unknown"
    # Observational (owner adjudication 2026-09-23): never blocking, but the
    # raw outcome stays in the verdict -- visibly excluded, never greened.
    if result == "success":
        return "observational_passed"
    if result == "failure":
        return "observational_failed_excluded"
    if result == "cancelled":
        return "observational_cancelled_excluded"
    if result == "skipped":
        return "observational_not_run_excluded"
    return "observational_unknown_excluded"


def evaluate(plan: Any, needs: Any) -> tuple[bool, dict[str, str]]:
    required, observational = classify_jobs(plan)
    if not isinstance(needs, dict) or set(needs) != set(required) | set(observational):
        # Closed inventory: dropping the observational job (hiding its raw
        # status) or adding an unclassified job both refuse.
        raise InvalidEvidence("required job inventory mismatch")
    verdicts: dict[str, str] = {}
    for job in required:
        verdicts[job] = _verdict(needs[job], required=True)
    for job in observational:
        verdicts[job] = _verdict(needs[job], required=False)
    return all(verdicts[job] == "passed" for job in required), verdicts


def main() -> int:
    try:
        root = Path(__file__).resolve().parents[2]
        plan = decode((root / ".github/acceptance-plan.json").read_text(encoding="utf-8"))
        required, observational = classify_jobs(plan)
        passed, verdicts = evaluate(plan, decode(os.environ.get("CI_NEEDS_JSON", "")))
    except (InvalidEvidence, OSError):
        print("CI acceptance: invalid or missing plan/evidence", file=sys.stderr)
        return 1
    print(json.dumps({
        "scope": "automated_ci_only",
        "policy": OBSERVATIONAL_POLICY,
        "required_jobs": {job: verdicts[job] for job in required},
        "observational_jobs": {job: verdicts[job] for job in observational},
        "observational_exclusions": {
            job: verdicts[job] for job in observational
            if verdicts[job] != "observational_passed"
        },
        "accepted": passed,
    }, sort_keys=True))
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
