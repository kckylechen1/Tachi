#!/usr/bin/env bash
# nextest-known-reds-diff.sh — machine-readable known-red gate (#1278 step ②)
#
# Usage:
#   scripts/nextest-known-reds-diff.sh <junit.xml>
#
# Extracts the failed-test set from a nextest JUnit report and diffs it against
# the `known-deterministic-reds` nextest test-group declared in
# `.config/nextest.toml` (board_first ×2 + dispatch_confirmation).
#
# Exit codes:
#   0 — every failure is in the known-red list (matched set may be empty or
#       a subset; extras are forbidden, missing known-reds are OK).
#   1 — at least one failure is outside the known-red list (outsiders printed).
#   2 — usage error; the JUnit report at <junit.xml> is missing/malformed; or
#       `cargo nextest list` succeeded but group(known-deterministic-reds)
#       resolved to zero tests.
#   3 — `cargo nextest list` itself failed (nonzero exit). cargo's stderr is
#       printed, not swallowed, so the failure has a visible diagnostic.
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
FAILED_FILE="$(mktemp)"
trap 'rm -f "${KNOWN_FILE}" "${FAILED_FILE}"' EXIT

# Resolve the named group from live nextest config (source of truth).
# Format: one fully-qualified test name per line (the `name` attr in JUnit /
# the test path after the binary id).
#
# stderr is NOT swallowed here: a `cargo nextest list` failure (bad
# expression, missing group, build error) must be visible, not silently
# turn into an empty KNOWN_FILE with no diagnostic.
if ! (
  cd "${ROOT}"
  # Default human list lines look like: `tachi-server tests::path::to::test`
  # JUnit <testcase name="..."> carries only the `tests::…` path — strip the
  # binary-id prefix so the sets compare.
  cargo nextest list -p tachi-server \
    -E 'group(known-deterministic-reds)' \
    --target-dir "${CARGO_TARGET_DIR:-/Users/kckylechen/.cache/sigil-shared-target}" \
    | sed -n 's/^[^ ]\{1,\} //p' \
    | sed '/^$/d' \
    | sort -u
) > "${KNOWN_FILE}"; then
  echo "nextest-known-reds-diff: cargo nextest list failed (stderr above) — cannot resolve group(known-deterministic-reds)" >&2
  exit 3
fi

if [[ ! -s "${KNOWN_FILE}" ]]; then
  echo "nextest-known-reds-diff: group(known-deterministic-reds) resolved to zero tests — is .config/nextest.toml present?" >&2
  exit 2
fi

python3 - "${JUNIT}" "${FAILED_FILE}" <<'PY'
import sys
import xml.etree.ElementTree as ET

junit_path, out_path = sys.argv[1], sys.argv[2]
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
for suite in suites:
    for case in suite.findall("testcase"):
        if case.find("failure") is None and case.find("error") is None:
            continue
        # JUnit name attr is the rust test path (matches nextest list output).
        name = case.get("name") or ""
        if name:
            failed.append(name)
with open(out_path, "w", encoding="utf-8") as out:
    for name in sorted(set(failed)):
        out.write(name + "\n")
PY

KNOWN_N=$(wc -l < "${KNOWN_FILE}" | tr -d ' ')
FAIL_N=$(wc -l < "${FAILED_FILE}" | tr -d ' ')
echo "nextest-known-reds-diff: known_group=${KNOWN_N} junit_failures=${FAIL_N}"

OUTSIDERS="$(mktemp)"
trap 'rm -f "${KNOWN_FILE}" "${FAILED_FILE}" "${OUTSIDERS}"' EXIT
# Failures not in the known list.
comm -23 "${FAILED_FILE}" "${KNOWN_FILE}" > "${OUTSIDERS}"

if [[ -s "${OUTSIDERS}" ]]; then
  echo "nextest-known-reds-diff: OUTSIDERS (not in known-deterministic-reds):"
  while IFS= read -r line; do
    echo "  - ${line}"
  done < "${OUTSIDERS}"
  exit 1
fi

echo "nextest-known-reds-diff: OK — every failure is in known-deterministic-reds"
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
