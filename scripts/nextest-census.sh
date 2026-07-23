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
# JUnit note (#1278 step ②): a dedicated `[profile.census]` in
# `.config/nextest.toml` enables JUnit for this script. It deliberately does
# NOT set `retries` (unlike the `ci` profile's retries = 2) -- a retried-away
# failure never reaches the JUnit report, so retrying would make the census
# undercount exactly the flaky failures it exists to observe.
#
# No production code. Re-running accumulates in the JSONL file.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-/Users/kckylechen/.cache/sigil-shared-target}"
CENSUS_DIR="${NEXTEST_CENSUS_DIR:-${TARGET_DIR}/nextest-census}"
# nextest store dir is workspace-relative (`[store] dir = "target/nextest"`),
# not under --target-dir. The census profile writes junit.xml there.
JUNIT_PATH="${ROOT}/target/nextest/census/junit.xml"
JSONL="${CENSUS_DIR}/census.jsonl"

mkdir -p "${CENSUS_DIR}"
# Guarantee JSONL exists before the summary line reads it below, even on a
# fresh machine / first-ever run where nothing has failed yet: an empty file
# reads as zero lines rather than tripping `wc -l < missing-file` under `set
# -e`.
touch "${JSONL}"

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
  cargo nextest run -p tachi-server --no-fail-fast --profile census \
    --target-dir "${TARGET_DIR}"
)
NEXTEST_EXIT=$?
set -e

if [[ ! -f "${JUNIT_PATH}" ]]; then
  echo "nextest-census: STOP — JUnit XML not produced at ${JUNIT_PATH}" >&2
  echo "nextest-census: nextest exit=${NEXTEST_EXIT}; census profile expected to write this path." >&2
  exit 2
fi

# Portable file lock (no `flock(1)` dependency -- this script's default
# paths are macOS dev-machine paths, and macOS ships no `flock` binary by
# default). `mkdir` is atomic on any POSIX filesystem, so a lock *directory*
# under CENSUS_DIR serializes the read-classify-append critical section
# below across concurrent invocations of this script without a third-party
# tool.
LOCK_DIR="${CENSUS_DIR}/census.lock.d"
census_lock_acquire() {
  local waited=0
  while ! mkdir "${LOCK_DIR}" 2>/dev/null; do
    waited=$((waited + 1))
    if [[ "${waited}" -ge 150 ]]; then
      echo "nextest-census: STOP — could not acquire ${LOCK_DIR} after 30s (stale lock from a crashed run?)" >&2
      exit 3
    fi
    sleep 0.2
  done
}
census_lock_release() {
  rmdir "${LOCK_DIR}" 2>/dev/null || true
}
census_lock_acquire
trap census_lock_release EXIT

# Collect previously-seen failure_line1_hash values (field 4 in compact JSON).
PREV_HASHES="$(mktemp)"
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
census_lock_release
trap - EXIT

echo "nextest-census: summary failed=${FAIL_N} previously_seen=${REPEAT_M} novel=${NOVEL_K}"
echo "nextest-census: jsonl_lines=$(wc -l < "${JSONL}" | tr -d ' ') path=${JSONL}"

# Census always records; surface nextest's exit for callers that care.
exit 0
