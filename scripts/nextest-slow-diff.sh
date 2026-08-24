#!/usr/bin/env bash
# nextest-slow-diff.sh — operator/local gate for new >20s tests against checked-in roster
# (parallel to known-reds #1278; like the local gitleaks hook). NOT invoked by CI
# (Actions disabled repo-wide per #1562). Run manually by operators / local tooling.
#
# Invocation (exact command + expected input via --final-status-level slow):
#   cargo nextest run -p tachi-server -p memory-server-runtime --locked \
#     --final-status-level slow > /tmp/slow.txt 2>&1
#   scripts/nextest-slow-diff.sh /tmp/slow.txt
#   # (or with --profile ci; or a JUnit .xml file; or plain list of full test paths)
#
# Usage:
#   scripts/nextest-slow-diff.sh <nextest-slow-output.txt | slow-list.txt | junit.xml>
#
# Exact input format for nextest slow listing (the primary measurement path):
#   Output captured from the --final-status-level slow run above.
#   Contains lines of form (indented):
#     SLOW [  35.999s] ( 590/3647) memory-server-runtime tests::issue_1588_...
#   The script extracts the text after the final ") " as the full test path.
#   (Also accepts a plain text file with one EXACT full test path per line,
#    or a JUnit XML with time>20s.)
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
  echo "usage: $0 <slow-list.txt | junit.xml>" >&2
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

if [[ "${INPUT}" == *.xml ]]; then
  # Parse JUnit: collect test names where time > 20s
  python3 - "${INPUT}" "${SLOWS_FILE}" <<'PY'
import sys
import xml.etree.ElementTree as ET

junit_path, slows_path = sys.argv[1], sys.argv[2]
try:
    root = ET.parse(junit_path).getroot()
except Exception as err:
    print(f"nextest-slow-diff: JUnit parse error: {err}", file=sys.stderr)
    sys.exit(2)

suites = list(root) if root.tag.endswith("testsuites") else (
    [root] if root.tag.endswith("testsuite") else root.findall(".//testsuite")
)
slows = []
for suite in suites:
    suite_name = suite.get("name") or ""
    for case in suite.findall("testcase"):
        name = case.get("name") or ""
        # nextest splits the binary into classname/testsuite; the roster (and
        # nextest's own SLOW listing) names tests as "<binary> <name>".
        classname = case.get("classname") or ""
        binary = classname or suite_name
        t = case.get("time") or "0"
        try:
            if float(t) > 20.0 and name:
                slows.append(f"{binary} {name}".strip() if binary else name)
        except ValueError:
            pass
with open(slows_path, "w", encoding="utf-8") as out:
    for name in sorted(set(slows)):
        out.write(name + "\n")
PY
else
  # Nextest slow listing output (from --final-status-level slow) or plain list.
  # Extract test path: for "SLOW [  35.999s] ( 590/3647) <name>" take after ") ".
  # Captured nextest output with zero SLOW records (Summary/PASS/FAIL present) is a
  # no-slow run, NOT a plain list. The one-name-per-line fallback applies only when
  # neither SLOW lines nor captured-run markers exist.
  if grep -q '^ *SLOW \[' "${INPUT}"; then
    grep '^ *SLOW \[' "${INPUT}" | sed 's/.*) //' | sort -u > "${SLOWS_FILE}"
  elif grep -qE '^(Summary \[| *FAIL \[| *PASS \[| *SLOW \[| *Starting [0-9]+ test)' "${INPUT}"; then
    # Captured nextest run output with zero SLOW records: this is a run with
    # no slow tests (the documented no-slow case), NOT a plain name list.
    : > "${SLOWS_FILE}"
  else
    sort -u "${INPUT}" > "${SLOWS_FILE}"
  fi
  fi

# C4: strip \r (CRLF) on input lines before comparison. Plain-list inputs
# (e.g. from clipboard, cross-platform, or windows editors) must not
# produce false outsiders due to trailing \r in test names.
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
