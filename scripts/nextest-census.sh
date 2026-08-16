#!/usr/bin/env bash
# nextest-census.sh — append one JSONL row per failed test in the covered
# packages (tachi-server + tachi-bootstrap-tests + tachi-contract-tests +
# tachi-credential-profile + tachi-github-runtime + tachi-lesson-forge +
# tachi-build-broker) (#1278 step ①)
#
# Usage:
#   scripts/nextest-census.sh
#
# Behavior:
#   1. Runs `cargo nextest run -p tachi-server -p tachi-bootstrap-tests -p tachi-contract-tests
#      -p tachi-credential-profile -p tachi-github-runtime -p tachi-lesson-forge -p tachi-build-broker --no-fail-fast` once
#      with JUnit output enabled. Set NEXTEST_TEST_THREADS
#      to pass an explicit nextest concurrency setting through to the run. The
#      package set must stay in sync with NEXTEST_PACKAGES in
#      scripts/nextest-known-reds-diff.sh — both cover the same test set, and a
#      package listed in one but not the other makes the two disagree about
#      what a complete run is.
#   2. Parses the JUnit XML into one JSONL line per failed test:
#        {run_id, test_threads, target_dir, target_state_at_invocation,
#         run_runtime_s, test_id, failure_line1_hash, recurrence, ...}
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
# Locking note (repair round 2): the read-classify-append critical section is
# serialized across concurrent invocations by a `mkdir`-based lock directory
# (see `census_lock_acquire` below) that self-heals from a SIGKILL'd holder --
# a dead-holder or aged-with-no-pid-file lock is reclaimed by the next waiter
# instead of wedging every future run behind a 30s timeout forever.
#
# No production code. Re-running accumulates in the JSONL file.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
  TARGET_SOURCE="CARGO_TARGET_DIR"
  TARGET_DIR="${CARGO_TARGET_DIR}"
else
  TARGET_SOURCE="default"
  TARGET_DIR="/Users/kckylechen/.cache/sigil-shared-target"
fi

census_target_state() {
  local target="$1"
  if [[ ! -e "${target}" ]]; then
    echo "absent"
  elif [[ ! -d "${target}" ]]; then
    echo "not_a_directory"
  elif [[ -n "$(find "${target}" -mindepth 1 -maxdepth 1 -print -quit)" ]]; then
    echo "nonempty"
  else
    echo "empty"
  fi
}

TEST_THREADS="${NEXTEST_TEST_THREADS:-default}"
CENSUS_DIR="${NEXTEST_CENSUS_DIR:-${TARGET_DIR}/nextest-census}"
# nextest store dir is workspace-relative (`[store] dir = "target/nextest"`),
# not under --target-dir. The census profile writes junit.xml there.
JUNIT_PATH="${ROOT}/target/nextest/census/junit.xml"
JSONL="${CENSUS_DIR}/census.jsonl"

RUN_ID="$(uuidgen | tr '[:upper:]' '[:lower:]')"
STARTED_AT="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
RUN_STARTED_EPOCH="$(date +%s)"

echo "nextest-census: run_id=${RUN_ID} started_at=${STARTED_AT}"
echo "nextest-census: test_threads=${TEST_THREADS}"
echo "nextest-census: junit=${JUNIT_PATH}"
echo "nextest-census: jsonl=${JSONL}"

# Ensure no stale junit confuses this run.
rm -f "${JUNIT_PATH}"

TARGET_STATE_AT_INVOCATION="$(census_target_state "${TARGET_DIR}")"
case "${TARGET_STATE_AT_INVOCATION}" in
  absent|empty) TARGET_CLEAN_AT_INVOCATION=true ;;
  *) TARGET_CLEAN_AT_INVOCATION=false ;;
esac
echo "nextest-census: target_dir=${TARGET_DIR} source=${TARGET_SOURCE} state_at_invocation=${TARGET_STATE_AT_INVOCATION} clean_at_invocation=${TARGET_CLEAN_AT_INVOCATION}"

set +e
(
  cd "${ROOT}"
  nextest_args=(nextest run -p tachi-server -p tachi-bootstrap-tests -p tachi-contract-tests -p tachi-credential-profile -p tachi-github-runtime -p tachi-lesson-forge -p tachi-build-broker --no-fail-fast --profile census --target-dir "${TARGET_DIR}")
  if [[ "${TEST_THREADS}" != "default" ]]; then
    nextest_args+=(--test-threads "${TEST_THREADS}")
  fi
  cargo "${nextest_args[@]}"
)
NEXTEST_EXIT=$?
set -e
FINISHED_AT="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
RUN_RUNTIME_S=$(( $(date +%s) - RUN_STARTED_EPOCH ))

if [[ ! -f "${JUNIT_PATH}" ]]; then
  echo "nextest-census: STOP — JUnit XML not produced at ${JUNIT_PATH}" >&2
  echo "nextest-census: nextest exit=${NEXTEST_EXIT}; census profile expected to write this path." >&2
  exit 2
fi

# Defer evidence creation until cargo has observed the target. When CENSUS_DIR
# uses its default under TARGET_DIR, creating it earlier would make a fresh
# target nonempty and falsify the invocation-time cleanliness provenance.
mkdir -p "${CENSUS_DIR}"
# Guarantee JSONL exists before the summary reads it, including a first run
# where no tests fail.
touch "${JSONL}"

# Portable file lock (no `flock(1)` dependency -- this script's default
# paths are macOS dev-machine paths, and macOS ships no `flock` binary by
# default). `mkdir` is atomic on any POSIX filesystem, so a lock *directory*
# under CENSUS_DIR serializes the read-classify-append critical section
# below across concurrent invocations of this script without a third-party
# tool.
#
# Stale-lock recovery: a SIGKILL'd holder (or `kill -9` from an impatient
# operator) skips the EXIT trap below, so the lock directory can outlive its
# process. The holder writes its PID into "${LOCK_DIR}/pid" immediately
# after winning the mkdir race. Every waiter that fails to acquire checks
# that PID file each cycle and reclaims (rm -rf + retry) when:
#   - pid file exists but `kill -0 $pid` fails (holder is dead), or
#   - pid file is missing and the lock dir itself is >60s old (killed in
#     the narrow window between mkdir and the pid-file write), or
#   - the lock dir's mtime is >30min old regardless of pid liveness (guards
#     against PID reuse making a genuinely stale lock look "alive").
# A reclaim is logged to stderr and does not consume the 30s acquire budget
# below -- it retries mkdir immediately in the same loop iteration.
LOCK_DIR="${CENSUS_DIR}/census.lock.d"
LOCK_PID_FILE="${LOCK_DIR}/pid"
STALE_AGE_HARD_S=1800  # 30 minutes: reclaim regardless of pid liveness.
STALE_AGE_NOPID_S=60   # pid file missing: give the holder 60s to write it.

# Portable "seconds since mtime" -- try GNU stat, then BSD/macOS stat.
census_lock_age_s() {
  local dir="$1" mtime now
  mtime="$(stat -c %Y "${dir}" 2>/dev/null || stat -f %m "${dir}" 2>/dev/null || echo "")"
  if [[ -z "${mtime}" ]]; then
    echo 0
    return
  fi
  now="$(date +%s)"
  echo $((now - mtime))
}

census_lock_reclaim_if_stale() {
  [[ -d "${LOCK_DIR}" ]] || return 1
  local age
  age="$(census_lock_age_s "${LOCK_DIR}")"
  if [[ -f "${LOCK_PID_FILE}" ]]; then
    local holder_pid
    holder_pid="$(cat "${LOCK_PID_FILE}" 2>/dev/null || echo "")"
    if [[ -n "${holder_pid}" ]] && kill -0 "${holder_pid}" 2>/dev/null; then
      # Holder process is alive. Only override on the pid-reuse guard.
      if [[ "${age}" -ge "${STALE_AGE_HARD_S}" ]]; then
        echo "nextest-census: stale lock from pid ${holder_pid}, reclaiming (age ${age}s >= ${STALE_AGE_HARD_S}s, pid-reuse guard)" >&2
        rm -rf "${LOCK_DIR}"
        return 0
      fi
      return 1
    fi
    echo "nextest-census: stale lock from pid ${holder_pid}, reclaiming (holder not running)" >&2
    rm -rf "${LOCK_DIR}"
    return 0
  fi
  # No pid file yet: either a holder mid-way through acquire (young lock,
  # leave it alone) or a holder killed before it could write the pid file.
  if [[ "${age}" -ge "${STALE_AGE_NOPID_S}" ]]; then
    echo "nextest-census: stale lock (no pid file, age ${age}s >= ${STALE_AGE_NOPID_S}s), reclaiming" >&2
    rm -rf "${LOCK_DIR}"
    return 0
  fi
  return 1
}

census_lock_acquire() {
  local waited=0
  while ! mkdir "${LOCK_DIR}" 2>/dev/null; do
    if census_lock_reclaim_if_stale; then
      continue
    fi
    waited=$((waited + 1))
    if [[ "${waited}" -ge 150 ]]; then
      echo "nextest-census: STOP — could not acquire ${LOCK_DIR} after 30s (stale lock from a crashed run?)" >&2
      exit 3
    fi
    sleep 0.2
  done
  echo "$$" > "${LOCK_PID_FILE}"
}
census_lock_release() {
  rm -f "${LOCK_PID_FILE}"
  rmdir "${LOCK_DIR}" 2>/dev/null || rm -rf "${LOCK_DIR}"
}
census_lock_acquire
trap census_lock_release EXIT

NEW_ROWS="$(mktemp)"
python3 - "${JUNIT_PATH}" "${JSONL}" "${RUN_ID}" "${STARTED_AT}" "${FINISHED_AT}" "${RUN_RUNTIME_S}" "${TARGET_DIR}" "${TARGET_SOURCE}" "${TARGET_STATE_AT_INVOCATION}" "${TARGET_CLEAN_AT_INVOCATION}" "${TEST_THREADS}" "${NEXTEST_EXIT}" "${NEW_ROWS}" <<'PY'
from collections import Counter
import hashlib
import json
import re
import sys
import xml.etree.ElementTree as ET

(
    junit_path,
    jsonl_path,
    run_id,
    started_at,
    finished_at,
    run_runtime_s,
    target_dir,
    target_source,
    target_state_at_invocation,
    target_clean_at_invocation,
    test_threads,
    nextest_exit,
    out_path,
) = sys.argv[1:14]
previous = Counter()
with open(jsonl_path, encoding="utf-8") as prior_rows:
    for line in prior_rows:
        try:
            prior = json.loads(line)
        except json.JSONDecodeError:
            continue
        key = (prior.get("test_id"), prior.get("failure_line1_hash"))
        if all(key):
            previous[key] += 1
try:
    tree = ET.parse(junit_path)
except ET.ParseError as err:
    print(
        "nextest-census: STOP — malformed JUnit at "
        f"{junit_path}; nextest_exit={nextest_exit}; refusing capture: {err}",
        file=sys.stderr,
    )
    sys.exit(2)
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
        # test_id KEEPS the binary id (classname). Deliberate, and the reason
        # this script needed no dedupe guard when it was widened to a second
        # package (#1610 Track T): the accumulated signature key is
        # (test_id, failure_line1_hash), so two same-named tests in different
        # binaries stay distinct rows. scripts/nextest-known-reds-diff.sh does
        # the opposite — it STRIPS the binary id to match JUnit names against
        # `cargo nextest list` output — which is exactly why that script carries
        # an AMBIGUOUS_TEST_NAME guard and this one does not. Do not "harmonize"
        # the two.
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
        prior_matching_failures = previous[(test_id, digest)]
        try:
            duration_s = float(case.get("time") or "0")
        except ValueError:
            duration_s = 0.0
        rows.append(
            {
                "run_id": run_id,
                "started_at": started_at,
                "finished_at": finished_at,
                "run_runtime_s": int(run_runtime_s),
                "test_threads": test_threads,
                "target_dir": target_dir,
                "target_source": target_source,
                "target_state_at_invocation": target_state_at_invocation,
                "target_clean_at_invocation": target_clean_at_invocation == "true",
                "nextest_exit": int(nextest_exit),
                "test_id": test_id,
                "failure_line1_hash": digest,
                "duration_s": duration_s,
                "prior_matching_failures": prior_matching_failures,
                "recurrence": "recurrent" if prior_matching_failures else "novel",
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
EMPTY_FAILURE_SET_WITH_NONZERO_EXIT=false
if [[ "${NEXTEST_EXIT}" -ne 0 && "${FAIL_N}" -eq 0 ]]; then
  # Invariant: a nonzero nextest result cannot be represented as a successful
  # zero-failure census. It may be an infrastructure or capture failure that
  # JUnit cannot describe, so preserve the underlying exit after cleanup.
  EMPTY_FAILURE_SET_WITH_NONZERO_EXIT=true
fi
if [[ "${FAIL_N}" -gt 0 ]]; then
  REPEAT_M="$(python3 - "${NEW_ROWS}" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as rows:
    print(sum(json.loads(line).get("recurrence") == "recurrent" for line in rows if line.strip()))
PY
)"
  NOVEL_K=$((FAIL_N - REPEAT_M))
  cat "${NEW_ROWS}" >> "${JSONL}"
fi

rm -f "${NEW_ROWS}"
census_lock_release
trap - EXIT

echo "nextest-census: summary nextest_exit=${NEXTEST_EXIT} runtime_s=${RUN_RUNTIME_S} failed=${FAIL_N} previously_seen=${REPEAT_M} novel=${NOVEL_K}"
echo "nextest-census: jsonl_lines=$(wc -l < "${JSONL}" | tr -d ' ') path=${JSONL}"

if [[ "${EMPTY_FAILURE_SET_WITH_NONZERO_EXIT}" == true ]]; then
  echo "nextest-census: STOP — nextest exited ${NEXTEST_EXIT} but valid JUnit contained zero failure/error nodes; refusing a hollow-green capture." >&2
  exit "${NEXTEST_EXIT}"
fi

# This is a census recorder, not a pass/fail gate: successful capture exits 0
# even when tests fail. nextest_exit remains loud in every failure row and the
# summary above; missing/malformed JUnit, a nonzero exit with no JUnit failure
# rows, or evidence-lock failures still exit nonzero.
exit 0
