#!/usr/bin/env bash
# Operator-side stray-build killer for the trusted acceptance runner (#1865).
#
# Run this ON THE HOST after a forced run or job cancellation. The GitHub
# runner terminates the step process it spawned, but a cancelled
# cargo/nextest/rustc process tree can outlive it (#1865 acceptance 7: a
# forced cancellation must leave no live Cargo/nextest/rustc descendant).
#
# Safety scoping: the pattern matches ONLY processes whose command line
# references the runner's _work root, so concurrent developer builds in other
# checkouts on the same machine are never touched. Verify with a plain
# `pgrep -fl <pattern>` (listing only) before letting this kill anything.
#
# Usage:
#   runner_kill_strays.sh list    # show matches, kill nothing
#   runner_kill_strays.sh kill    # SIGTERM, wait, then SIGTERM->SIG9 survivors

set -euo pipefail

mode="${1:-list}"
runner_root="${RUNNER_ROOT:-$HOME/runner-tachi}"
work_root="${runner_root}/_work"

if [ ! -d "${work_root}" ]; then
  echo "kill-strays: runner work root ${work_root} does not exist; nothing to scan" >&2
  exit 0
fi

matches="$(pgrep -f "${work_root}" | while read -r pid; do ps -o pid=,command= -p "${pid}" 2>/dev/null; done || true)"

case "${mode}" in
  list)
    if [ -z "${matches}" ]; then
      echo "kill-strays: no live processes reference ${work_root}"
    else
      echo "kill-strays: live processes referencing ${work_root}:"
      echo "${matches}"
    fi
    ;;
  kill)
    if [ -z "${matches}" ]; then
      echo "kill-strays: no live processes reference ${work_root}"
      exit 0
    fi
    echo "kill-strays: terminating:"
    echo "${matches}"
    pkill -f "${work_root}" || true
    sleep 3
    remaining="$(pgrep -f "${work_root}" | while read -r pid; do ps -o pid=,command= -p "${pid}" 2>/dev/null; done || true)"
    if [ -n "${remaining}" ]; then
      echo "kill-strays: SIGTERM survivors, escalating to SIGKILL:" >&2
      echo "${remaining}" >&2
      pkill -9 -f "${work_root}" || true
      sleep 1
    fi
    final="$(pgrep -f "${work_root}" | while read -r pid; do ps -o pid=,command= -p "${pid}" 2>/dev/null; done || true)"
    if [ -n "${final}" ]; then
      echo "::error::kill-strays: processes survived SIGKILL; resolve manually:" >&2
      echo "${final}" >&2
      exit 4
    fi
    echo "kill-strays: clean"
    ;;
  *)
    echo "usage: runner_kill_strays.sh list|kill" >&2
    exit 2
    ;;
esac
