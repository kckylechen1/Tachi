#!/usr/bin/env bash
# nextest-slow-diff.sh — operator/local gate for new >20s tests against checked-in roster
# (parallel to known-reds #1278; like the local gitleaks hook). NOT invoked by CI
# (Actions disabled repo-wide per #1562). Run manually by operators / local tooling.
#
# Invocation (exact command + expected input via --final-status-level slow):
#   cargo nextest run -p tachi-server -p memory-server-runtime --locked \
#     --final-status-level slow > /tmp/slow.txt 2>&1
#   scripts/nextest-slow-diff.sh /tmp/slow.txt
#   # (or with --profile ci)
#
# Usage:
#   scripts/nextest-slow-diff.sh <nextest-slow-output.txt>
#
# Exact input format for nextest slow listing (the primary measurement path):
#   Output captured from the --final-status-level slow run above.
#   Contains lines of form (indented):
#     SLOW [  35.999s] ( 590/3647) memory-server-runtime tests::issue_1588_...
#   The script extracts the text after the final ") " as the full test path.
#   A terminal successful Summary is required. Failed, interrupted, or
#   truncated captures are refused rather than treated as zero-slow runs.
#
# Diffs the slow set against scripts/nextest-slow-roster.txt (exact match).
# Exits nonzero on any slow test NOT in the roster (new creep).
#
# Mirrors nextest-known-reds-diff.sh conventions:
#   - exact full test paths (no substring)
#   - LC_ALL=C for comm
#   - mktemp + trap
#   - env override NEXTEST_SLOW_ROSTER=<file>
#   - prints outsiders on failure
#   - no retries, no weakening
#
# The roster lists currently-allowed >20s tests (by design, e.g. intentional 35s
# serialization hold in #1588 test). New tests must not creep over 20s without
# updating roster + justification.
#
# Exit codes:
#   0 — all slow tests are in the roster (or no slows)
#   1 — at least one slow outsider (printed)
#   2 — usage / parse error
#
# No test source edits.

set -euo pipefail

export LC_ALL=C

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
INPUT="${1:-}"

if [[ -z "${INPUT}" || ! -f "${INPUT}" ]]; then
  echo "usage: $0 <nextest-slow-output.txt>" >&2
  exit 2
fi

ROSTER_FILE="${NEXTEST_SLOW_ROSTER:-${ROOT}/scripts/nextest-slow-roster.txt}"
if [[ ! -f "${ROSTER_FILE}" ]]; then
  echo "nextest-slow-diff: roster file not found: ${ROSTER_FILE}" >&2
  exit 2
fi

SLOWS_FILE="$(mktemp)"
ROSTER_SORTED="$(mktemp)"
OUTSIDERS="$(mktemp)"
trap 'rm -f "${SLOWS_FILE}" "${ROSTER_SORTED}" "${OUTSIDERS}"' EXIT

# Normalize roster
sort -u "${ROSTER_FILE}" > "${ROSTER_SORTED}"

# A slow roster is meaningful only for a completed successful measurement.
# A FAIL line proves failure. Missing the terminal Summary proves truncation,
# interruption, or the wrong input format. Both cases must fail closed before
# we inspect SLOW lines, including when a partial capture already contains one.
if grep -q '^ *FAIL \[' "${INPUT}"; then
  echo "nextest-slow-diff: INCOMPLETE_RUN — nextest reported a failed test" >&2
  exit 2
fi
if ! grep -q '^Summary \[' "${INPUT}"; then
  echo "nextest-slow-diff: INCOMPLETE_RUN — terminal nextest Summary missing" >&2
  exit 2
fi
if grep '^Summary \[' "${INPUT}" | grep -qiE '([0-9]+ failed|[0-9]+ cancelled|[0-9]+ canceled|timed out)'; then
  echo "nextest-slow-diff: INCOMPLETE_RUN — terminal nextest Summary is not green" >&2
  exit 2
fi

# Extract test path: for "SLOW [  35.999s] ( 590/3647) <name>" take after ") ".
if grep -q '^ *SLOW \[' "${INPUT}"; then
  grep '^ *SLOW \[' "${INPUT}" | sed 's/.*) //' | sort -u > "${SLOWS_FILE}"
else
  : > "${SLOWS_FILE}"
fi

# C4: strip \r (CRLF) on captured lines before comparison. Cross-platform
# capture transport must not produce false outsiders from trailing \r.
tr -d '\r' < "${SLOWS_FILE}" | sort -u > "${SLOWS_FILE}.tmp" && mv "${SLOWS_FILE}.tmp" "${SLOWS_FILE}"

if [[ ! -s "${SLOWS_FILE}" ]]; then
  echo "nextest-slow-diff: OK — no slow tests in input"
  exit 0
fi


# Outsiders: slows not in roster
comm -23 "${SLOWS_FILE}" "${ROSTER_SORTED}" > "${OUTSIDERS}"

if [[ -s "${OUTSIDERS}" ]]; then
  echo "nextest-slow-diff: NEW SLOW TESTS (not in roster):"
  while IFS= read -r line; do
    echo "  - ${line}"
  done < "${OUTSIDERS}"
  exit 1
fi

echo "nextest-slow-diff: OK — all slow tests are in the roster"
exit 0
