#!/usr/bin/env bash
# nextest-known-reds-diff.sh — machine-readable known-red gate (#1278 step ②)
#
# Usage:
#   scripts/nextest-known-reds-diff.sh <junit.xml>
#
# Extracts the failed-test set from a nextest JUnit report and diffs it against
# the `known-deterministic-reds` nextest test-group declared in
# `.config/nextest.toml` (currently EMPTY — the last two members, board_first
# x2, were re-verified green and removed 2026-08-05; the third historical
# member's test was deleted on main by 280983bea, #1319 C2). With an empty
# roster the gate reduces to "any failure fails the gate". Keep this header
# in sync with the group.
#
# #1413 concern 5 — two hardening passes:
#   * Exact membership: the group is resolved from live nextest config, and the
#     `test(/regex/)` filter in .config/nextest.toml is anchored to the EXACT
#     full test paths, so a future test cannot be substring-absorbed into the
#     known-reds group.
#   * Completeness gate: before declaring OK, the report is checked against the
#     full `cargo nextest list -p tachi-server` set — a valid-but-incomplete
#     (truncated) JUnit is refused (exit 4), since it could hide a new red that
#     never got to run.
#
# Exit codes:
#   0 — every failure is in the known-red list AND the JUnit covers the full
#       expected test set (matched failures may be empty or a subset; extras
#       and missing expected tests are both forbidden).
#   1 — at least one failure is outside the known-red list (outsiders printed).
#   2 — usage error; the JUnit report at <junit.xml> is missing/malformed; the
#       known group resolved to zero tests; or the expected set resolved empty.
#   3 — `cargo nextest list` itself failed (nonzero exit). cargo's stderr is
#       printed, not swallowed, so the failure has a visible diagnostic.
#   4 — INCOMPLETE_JUNIT: the JUnit parsed cleanly but is missing one or more
#       tests that `cargo nextest list` says belong to the suite (a truncated
#       run). The gate refuses to declare OK over a possibly-hidden red.
#
# Test seams (also useful for pre-resolved/offline CI): set
#   NEXTEST_KNOWN_REDS_KNOWN_LIST=<file>      overrides group(...) resolution
#   NEXTEST_KNOWN_REDS_EXPECTED_LIST=<file>   overrides full-suite resolution
# Each override file is one fully-qualified test name per line. When set, no
# `cargo nextest list` is invoked for that set, so the gate logic is exercisable
# without a toolchain.
#
# No test source edits. No retries.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
JUNIT="${1:-}"

if [[ -z "${JUNIT}" || ! -f "${JUNIT}" ]]; then
  echo "usage: $0 <junit.xml>" >&2
  exit 2
fi

KNOWN_FILE="$(mktemp)"
EXPECTED_FILE="$(mktemp)"
FAILED_FILE="$(mktemp)"
PRESENT_FILE="$(mktemp)"
trap 'rm -f "${KNOWN_FILE}" "${EXPECTED_FILE}" "${FAILED_FILE}" "${PRESENT_FILE}"' EXIT

# Resolve a set of nextest test names into $1 (one fully-qualified name per
# line, binary id stripped, sorted+unique). $2 is an optional nextest -E filter
# expression (e.g. group(known-deterministic-reds)); empty means the full
# package list. stderr is NOT swallowed: a `cargo nextest list` failure must be
# visible, not silently become an empty file.
#
# --color never is load-bearing, not cosmetic: under CLICOLOR/FORCE_COLOR
# environments cargo/nextest emit ANSI escapes in the human list output even
# though stdout isn't a real TTY inside this subshell/pipeline. Those escapes
# land inside the captured test-name field and break every string-equality
# comparison downstream (comm(1) does no ANSI-aware normalization). The `sed`
# is defense in depth against any escape a different invocation path injects.
resolve_nextest_list() {
  local out_file="$1" filter_expr="${2:-}"
  (
    cd "${ROOT}"
    # Default human list lines look like: `tachi-server tests::path::to::test`
    # JUnit <testcase name="..."> carries only the `tests::…` path — strip the
    # binary-id prefix so the sets compare.
    if [[ -n "${filter_expr}" ]]; then
      cargo nextest list -p tachi-server -E "${filter_expr}" --color never \
        --target-dir "${CARGO_TARGET_DIR:-/Users/kckylechen/.cache/sigil-shared-target}"
    else
      cargo nextest list -p tachi-server --color never \
        --target-dir "${CARGO_TARGET_DIR:-/Users/kckylechen/.cache/sigil-shared-target}"
    fi \
      | sed -E 's/\x1b\[[0-9;]*m//g' \
      | sed -n 's/^[^ ]\{1,\} //p' \
      | sed '/^$/d' \
      | sort -u
  ) > "${out_file}"
}

# Resolve the known-red group (source of truth = .config/nextest.toml).
if [[ -n "${NEXTEST_KNOWN_REDS_KNOWN_LIST:-}" && -s "${NEXTEST_KNOWN_REDS_KNOWN_LIST}" ]]; then
  # Pre-resolved/test seam: trust the supplied list verbatim.
  sort -u "${NEXTEST_KNOWN_REDS_KNOWN_LIST}" > "${KNOWN_FILE}"
else
  if ! resolve_nextest_list "${KNOWN_FILE}" 'group(known-deterministic-reds)'; then
    echo "nextest-known-reds-diff: cargo nextest list failed (stderr above) — cannot resolve group(known-deterministic-reds)" >&2
    exit 3
  fi
fi

if [[ ! -s "${KNOWN_FILE}" ]]; then
  echo "nextest-known-reds-diff: group(known-deterministic-reds) resolved to zero tests — is .config/nextest.toml present?" >&2
  exit 2
fi

# Resolve the FULL expected test set for the completeness gate (#1413 concern 5).
if [[ -n "${NEXTEST_KNOWN_REDS_EXPECTED_LIST:-}" && -s "${NEXTEST_KNOWN_REDS_EXPECTED_LIST}" ]]; then
  sort -u "${NEXTEST_KNOWN_REDS_EXPECTED_LIST}" > "${EXPECTED_FILE}"
else
  if ! resolve_nextest_list "${EXPECTED_FILE}" ''; then
    echo "nextest-known-reds-diff: cargo nextest list failed (stderr above) — cannot resolve the expected test set" >&2
    exit 3
  fi
fi

if [[ ! -s "${EXPECTED_FILE}" ]]; then
  echo "nextest-known-reds-diff: expected test set resolved empty — cannot verify completeness" >&2
  exit 2
fi

python3 - "${JUNIT}" "${FAILED_FILE}" "${PRESENT_FILE}" <<'PY'
import sys
import xml.etree.ElementTree as ET

junit_path, failed_path, present_path = sys.argv[1], sys.argv[2], sys.argv[3]
try:
    root = ET.parse(junit_path).getroot()
except FileNotFoundError as err:
    print(f"nextest-known-reds-diff: JUnit file not found: {junit_path} ({err})", file=sys.stderr)
    sys.exit(2)
except ET.ParseError as err:
    print(
        f"nextest-known-reds-diff: JUnit XML at {junit_path} is malformed, cannot parse: {err}",
        file=sys.stderr,
    )
    sys.exit(2)
suites = list(root) if root.tag.endswith("testsuites") else (
    [root] if root.tag.endswith("testsuite") else root.findall(".//testsuite")
)
failed = []
present = []
for suite in suites:
    for case in suite.findall("testcase"):
        # JUnit name attr is the rust test path (matches nextest list output).
        name = case.get("name") or ""
        if name:
            present.append(name)
            if case.find("failure") is not None or case.find("error") is not None:
                failed.append(name)
with open(failed_path, "w", encoding="utf-8") as out:
    for name in sorted(set(failed)):
        out.write(name + "\n")
with open(present_path, "w", encoding="utf-8") as out:
    for name in sorted(set(present)):
        out.write(name + "\n")
PY

KNOWN_N=$(wc -l < "${KNOWN_FILE}" | tr -d ' ')
FAIL_N=$(wc -l < "${FAILED_FILE}" | tr -d ' ')
EXPECTED_N=$(wc -l < "${EXPECTED_FILE}" | tr -d ' ')
PRESENT_N=$(wc -l < "${PRESENT_FILE}" | tr -d ' ')
echo "nextest-known-reds-diff: known_group=${KNOWN_N} junit_failures=${FAIL_N} expected=${EXPECTED_N} junit_present=${PRESENT_N}"

OUTSIDERS="$(mktemp)"
trap 'rm -f "${KNOWN_FILE}" "${EXPECTED_FILE}" "${FAILED_FILE}" "${PRESENT_FILE}" "${OUTSIDERS}"' EXIT
# Failures not in the known list.
comm -23 "${FAILED_FILE}" "${KNOWN_FILE}" > "${OUTSIDERS}"

if [[ -s "${OUTSIDERS}" ]]; then
  echo "nextest-known-reds-diff: OUTSIDERS (not in known-deterministic-reds):"
  while IFS= read -r line; do
    echo "  - ${line}"
  done < "${OUTSIDERS}"
  exit 1
fi

# #1413 concern 5 — completeness gate: refuse a valid-but-incomplete JUnit
# before declaring OK. Missing = expected tests with no testcase in the report
# (a truncated run could hide a new red that never got to run). Supersets (a
# workspace-wide JUnit) are fine — only a missing expected test is a refusal.
MISSING="$(mktemp)"
trap 'rm -f "${KNOWN_FILE}" "${EXPECTED_FILE}" "${FAILED_FILE}" "${PRESENT_FILE}" "${OUTSIDERS}" "${MISSING}"' EXIT
comm -23 "${EXPECTED_FILE}" "${PRESENT_FILE}" > "${MISSING}"
if [[ -s "${MISSING}" ]]; then
  MISSING_N=$(wc -l < "${MISSING}" | tr -d ' ')
  echo "nextest-known-reds-diff: INCOMPLETE_JUNIT — ${MISSING_N} expected test(s) absent from the report (truncated run?); refusing to declare OK" >&2
  echo "nextest-known-reds-diff: first missing (of ${MISSING_N}):" >&2
  head -n 5 "${MISSING}" | while IFS= read -r line; do
    echo "  - ${line}" >&2
  done
  exit 4
fi

echo "nextest-known-reds-diff: OK — every failure is in known-deterministic-reds and the JUnit covers the full expected set"
# Show which known reds matched.
MATCHED="$(mktemp)"
comm -12 "${FAILED_FILE}" "${KNOWN_FILE}" > "${MATCHED}"
if [[ -s "${MATCHED}" ]]; then
  echo "nextest-known-reds-diff: matched:"
  while IFS= read -r line; do
    echo "  - ${line}"
  done < "${MATCHED}"
fi
rm -f "${MATCHED}"
exit 0
