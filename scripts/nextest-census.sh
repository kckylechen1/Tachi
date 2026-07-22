#!/usr/bin/env bash
# nextest-census.sh — append one JSONL row per failed tachi-server test (#1278 step ①)
#
# Usage:
#   scripts/nextest-census.sh
#
# Behavior:
#   1. Runs `cargo nextest run -p tachi-server --no-fail-fast` once with JUnit
#      output enabled.
#   2. Parses the JUnit XML into one JSONL line per failed test:
#        {run_id, started_at, test_id, failure_line1_hash, duration_s}
#      failure_line1_hash = sha256 of the first line of the failure message.
#   3. Appends to $CENSUS_DIR/census.jsonl (default:
#      /Users/kckylechen/.cache/sigil-shared-target/nextest-census/census.jsonl)
#      — machine-local evidence, outside the repo.
#   4. Prints a per-run summary: N failed, M previously-seen signatures, K novel.
#
# JUnit note (observation for #1278): the default nextest profile has no
# `[profile.default.junit]` section. JUnit is already configured on the `ci`
# profile (`.config/nextest.toml` → target/nextest/ci/junit.xml). This script
# therefore uses `--profile ci` to enable JUnit without restructuring nextest
# config (T4 must not edit nextest.toml; that is T5 / step ② territory).
#
# No production code. Re-running accumulates in the JSONL file.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-/Users/kckylechen/.cache/sigil-shared-target}"
CENSUS_DIR="${NEXTEST_CENSUS_DIR:-${TARGET_DIR}/nextest-census}"
# nextest store dir is workspace-relative (`[store] dir = "target/nextest"`),
# not under --target-dir. The ci profile writes junit.xml there.
JUNIT_PATH="${ROOT}/target/nextest/ci/junit.xml"
JSONL="${CENSUS_DIR}/census.jsonl"

mkdir -p "${CENSUS_DIR}"

RUN_ID="$(uuidgen | tr '[:upper:]' '[:lower:]')"
STARTED_AT="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

echo "nextest-census: run_id=${RUN_ID} started_at=${STARTED_AT}"
echo "nextest-census: target_dir=${TARGET_DIR}"
echo "nextest-census: junit=${JUNIT_PATH}"
echo "nextest-census: jsonl=${JSONL}"

# Ensure no stale junit confuses this run.
rm -f "${JUNIT_PATH}"

set +e
(
  cd "${ROOT}"
  cargo nextest run -p tachi-server --no-fail-fast --profile ci \
    --target-dir "${TARGET_DIR}"
)
NEXTEST_EXIT=$?
set -e

if [[ ! -f "${JUNIT_PATH}" ]]; then
  echo "nextest-census: STOP — JUnit XML not produced at ${JUNIT_PATH}" >&2
  echo "nextest-census: nextest exit=${NEXTEST_EXIT}; default profile has no junit; ci profile expected to write this path." >&2
  exit 2
fi

# Collect previously-seen failure_line1_hash values (field 4 in compact JSON).
PREV_HASHES="$(mktemp)"
if [[ -f "${JSONL}" ]]; then
  # shellcheck disable=SC2016
  python3 - "${JSONL}" "${PREV_HASHES}" <<'PY'
import json, sys
src, dst = sys.argv[1], sys.argv[2]
seen = set()
with open(src, encoding="utf-8") as f:
    for line in f:
        line = line.strip()
        if not line:
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        h = obj.get("failure_line1_hash")
        if h:
            seen.add(h)
with open(dst, "w", encoding="utf-8") as out:
    for h in sorted(seen):
        out.write(h + "\n")
PY
fi

NEW_ROWS="$(mktemp)"
python3 - "${JUNIT_PATH}" "${RUN_ID}" "${STARTED_AT}" "${NEW_ROWS}" <<'PY'
import hashlib
import json
import re
import sys
import xml.etree.ElementTree as ET

junit_path, run_id, started_at, out_path = sys.argv[1:5]
tree = ET.parse(junit_path)
root = tree.getroot()

# nextest may emit <testsuites><testsuite>… or a bare <testsuite>.
suites = []
if root.tag.endswith("testsuites"):
    suites = list(root)
elif root.tag.endswith("testsuite"):
    suites = [root]
else:
    suites = root.findall(".//testsuite")

rows = []
for suite in suites:
    for case in suite.findall("testcase"):
        failure = case.find("failure")
        error = case.find("error")
        node = failure if failure is not None else error
        if node is None:
            continue
        classname = case.get("classname") or ""
        name = case.get("name") or ""
        test_id = f"{classname}::{name}" if classname else name
        # Prefer message attr; fall back to element text.
        msg = (node.get("message") or "").strip()
        if not msg:
            msg = (node.text or "").strip()
        first_line = msg.splitlines()[0] if msg else ""
        # Rust panic first lines embed a per-run OS thread id:
        #   thread 'foo' (12345) panicked at …
        # Strip it so failure_line1_hash is a stable signature across runs
        # (otherwise every run looks "novel" and the census cannot accumulate).
        first_line = re.sub(r" \(\d+\) panicked", " panicked", first_line)
        digest = hashlib.sha256(first_line.encode("utf-8")).hexdigest()
        try:
            duration_s = float(case.get("time") or "0")
        except ValueError:
            duration_s = 0.0
        rows.append(
            {
                "run_id": run_id,
                "started_at": started_at,
                "test_id": test_id,
                "failure_line1_hash": digest,
                "duration_s": duration_s,
            }
        )

with open(out_path, "w", encoding="utf-8") as out:
    for row in rows:
        out.write(json.dumps(row, ensure_ascii=False) + "\n")
print(len(rows))
PY

FAIL_N=$(wc -l < "${NEW_ROWS}" | tr -d ' ')
REPEAT_M=0
NOVEL_K=0
if [[ "${FAIL_N}" -gt 0 ]]; then
  while IFS= read -r line; do
    hash="$(python3 -c 'import json,sys; print(json.loads(sys.stdin.read())["failure_line1_hash"])' <<<"${line}")"
    if grep -Fxq "${hash}" "${PREV_HASHES}" 2>/dev/null; then
      REPEAT_M=$((REPEAT_M + 1))
    else
      NOVEL_K=$((NOVEL_K + 1))
    fi
  done < "${NEW_ROWS}"
  cat "${NEW_ROWS}" >> "${JSONL}"
fi

rm -f "${PREV_HASHES}" "${NEW_ROWS}"

echo "nextest-census: summary failed=${FAIL_N} previously_seen=${REPEAT_M} novel=${NOVEL_K}"
echo "nextest-census: jsonl_lines=$(wc -l < "${JSONL}" | tr -d ' ') path=${JSONL}"

# Census always records; surface nextest's exit for callers that care.
exit 0
