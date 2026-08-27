#!/usr/bin/env bash
# Operator-side stray-build killer for the trusted acceptance runner (#1865).
#
# Run this ON THE HOST after a forced run or job cancellation. The GitHub
# runner terminates the step process it spawned, but a cancelled
# cargo/nextest/rustc process tree can outlive it (#1865 acceptance 7: a
# forced cancellation must leave no live Cargo/nextest/rustc descendant).
#
# Safety scoping (review round 1: argv substring matching alone is too
# broad, and alone it is also too narrow -- a bare `cargo` invoked with the
# workspace as cwd carries no path in argv). The kill set is defined by
# process working directory: anything whose CWD sits inside the runner's
# _work root belongs to a runner job and is a kill candidate; an editor,
# indexer, or shell that merely mentions a _work path in its arguments but
# lives elsewhere is reported as foreign and is never signalled. If lsof
# cannot resolve a cwd the process is treated as foreign, so the failure
# mode is under-killing, never over-killing. Always run `list` first and
# read the output before `kill`.
#
# Usage:
#   runner_kill_strays.sh list    # show candidates + foreign argv matches
#   runner_kill_strays.sh kill    # SIGTERM, wait, then SIGKILL survivors

set -euo pipefail

mode="${1:-list}"
runner_root="${RUNNER_ROOT:-$HOME/runner-tachi}"
work_root="${runner_root}/_work"

if [ ! -d "${work_root}" ]; then
  echo "kill-strays: runner work root ${work_root} does not exist; nothing to scan" >&2
  exit 0
fi
# lsof reports physical (symlink-resolved) cwd paths (e.g. /private/tmp for
# /tmp on macOS); compare against the physical work root or every match is
# silently missed. The argv scan keeps both spellings so processes quoting
# either form are visible in `list`.
logical_root="${work_root}"
work_root="$(cd "${work_root}" && pwd -P)"
argv_pattern="(${logical_root}|${work_root})"

cmd_of() { ps -o command= -p "$1" 2>/dev/null || echo '<gone>'; }

cwd_pids() {
  # One lsof pass over every process; emit "<pid> TAB <cwd>" for cwds under
  # the runner work root. lsof failures degrade to an empty list.
  lsof -d cwd -Fn 2>/dev/null | awk -v root="${work_root}" '
    /^p[0-9]+$/ { pid = substr($0, 2) }
    /^n\// {
      cwd = substr($0, 2)
      if (cwd == root || index(cwd, root "/") == 1) {
        printf "%s\t%s\n", pid, cwd
      }
    }' || true
}

victims() { cwd_pids | cut -f1; }

foreign_argv_matches() {
  # Processes whose ARGV references _work but whose cwd is elsewhere:
  # informational only, never signalled.
  { pgrep -f "${argv_pattern}" || true; } | while read -r pid; do
    [ -n "${pid}" ] || continue
    cwd="$(lsof -a -p "${pid}" -d cwd -Fn 2>/dev/null | awk -F'n' '/^n/{print $2; exit}')"
    case "${cwd:-}" in
      "${work_root}"|"${work_root}"/*) ;; # already a cwd victim
      *) printf '%s\t%s\t%s\n' "${pid}" "${cwd:-unknown-cwd}" "$(cmd_of "${pid}")" ;;
    esac
  done
}

case "${mode}" in
  list)
    v_out="$(cwd_pids)"
    f_out="$(foreign_argv_matches)"
    if [ -z "${v_out}" ] && [ -z "${f_out}" ]; then
      echo "kill-strays: no live processes are rooted in ${work_root}"
      exit 0
    fi
    if [ -n "${v_out}" ]; then
      echo "kill-strays: KILL CANDIDATES (cwd under ${work_root}):"
      printf '%s\n' "${v_out}" | while IFS="$(printf '\t')" read -r pid cwd; do
        printf '  pid=%s cwd=%s cmd=%s\n' "${pid}" "${cwd}" "$(cmd_of "${pid}")"
      done
    else
      echo "kill-strays: no kill candidates with cwd under ${work_root}"
    fi
    if [ -n "${f_out}" ]; then
      echo "kill-strays: FOREIGN argv-only matches (NOT signalled; listed for the operator):"
      printf '%s\n' "${f_out}" | while IFS="$(printf '\t')" read -r pid cwd cmd; do
        printf '  pid=%s cwd=%s cmd=%s\n' "${pid}" "${cwd}" "${cmd}"
      done
    fi
    ;;
  kill)
    pids="$(victims | tr '\n' ' ')"
    if [ -z "${pids// /}" ]; then
      echo "kill-strays: no kill candidates with cwd under ${work_root} (run 'list' to inspect argv-only matches)"
      exit 0
    fi
    echo "kill-strays: terminating (cwd under ${work_root}): ${pids}"
    # shellcheck disable=SC2086
    kill -TERM ${pids} 2>/dev/null || true
    sleep 3
    pids="$(victims | tr '\n' ' ')"
    if [ -n "${pids// /}" ]; then
      echo "kill-strays: SIGTERM survivors, escalating to SIGKILL: ${pids}" >&2
      # shellcheck disable=SC2086
      kill -9 ${pids} 2>/dev/null || true
      sleep 1
    fi
    pids="$(victims | tr '\n' ' ')"
    if [ -n "${pids// /}" ]; then
      echo "::error::kill-strays: processes survived SIGKILL; resolve manually: ${pids}" >&2
      exit 4
    fi
    echo "kill-strays: clean"
    ;;
  *)
    echo "usage: runner_kill_strays.sh list|kill" >&2
    exit 2
    ;;
esac
