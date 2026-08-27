#!/usr/bin/env bash
# Operator-side stray-build killer for the trusted acceptance runner (#1865).
#
# Run this ON THE HOST after a forced run or job cancellation, from OUTSIDE
# the runner work root (e.g. from the developer checkout). The GitHub runner
# terminates the step process it spawned, but a cancelled cargo/nextest/rustc
# process tree can outlive it (#1865 acceptance 7: a forced cancellation must
# leave no live Cargo/nextest/rustc descendant).
#
# Safety scoping (review rounds 1-2): the kill set is defined by process
# working directory -- anything whose CWD sits inside the runner's _work root
# belongs to a runner job and is a kill candidate -- minus the script's own
# process and its ancestors, so the operator's control plane is never
# signalled even if invoked from inside _work. An editor, indexer, or shell
# that merely mentions a _work path in its arguments but lives elsewhere is
# reported as foreign and is never signalled. If lsof discovery fails the
# script exits non-zero with UNKNOWN state instead of reporting success.
# Always run `list` first and read the output before `kill`.
#
# Usage:
#   runner_kill_strays.sh list    # show candidates + foreign argv matches
#   runner_kill_strays.sh kill    # SIGTERM, wait, then SIGKILL survivors

set -euo pipefail

mode="${1:-list}"
runner_root="${RUNNER_ROOT:-$HOME/runner-tachi}"
# SCAN_ROOT narrows the domain for job-side use: runner_hygiene.sh preflight
# invokes this killer with SCAN_ROOT="$GITHUB_WORKSPACE" so only processes
# holding that one workspace are terminated.
scan_root="${SCAN_ROOT:-${runner_root}/_work}"

if [ ! -d "${scan_root}" ]; then
  echo "kill-strays: runner work root ${scan_root} does not exist; nothing to scan" >&2
  exit 0
fi
# lsof reports physical (symlink-resolved) cwd paths (e.g. /private/tmp for
# /tmp on macOS); compare against the physical work root or every match is
# silently missed. The argv scan keeps both spellings (ERE-escaped) so
# processes quoting either form are visible in `list`.
logical_root="${scan_root}"
scan_root="$(cd "${scan_root}" && pwd -P)"

ere_escape() {
  printf '%s' "$1" | sed 's#[][\\.*^$(){}?+|/]#\\&#g'
}

argv_pattern="($(ere_escape "${logical_root}")|$(ere_escape "${scan_root}"))"

# Move our own cwd out of the scan domain: this script's transient pipeline
# children (awk/subshells) inherit cwd and would otherwise appear as -- and
# be signalled as -- phantom candidates when the operator invokes it from
# inside _work. Ancestors are excluded separately via protected_pids.
if ! cd "${HOME:-/}" >/dev/null 2>&1; then
  cd / >/dev/null 2>&1 || true
fi

protected_pids() {
  # This script and every ancestor: never signal the control plane.
  local pid="$$"
  while [ "${pid}" -gt 1 ] 2>/dev/null && [ -n "${pid}" ]; do
    echo "${pid}"
    pid="$(ps -o ppid= -p "${pid}" 2>/dev/null | tr -d '[:space:]')"
    [ -n "${pid}" ] || break
  done
  return 0
}

snapshot() {
  # One lsof pass over every process; LSOF_LINES holds "p<pid>/fcwd/n<cwd>"
  # records. A failed pass is an error, not an empty result.
  if ! LSOF_LINES="$(lsof -d cwd -Fn 2>/dev/null)"; then
    return 3
  fi
}

snapshot_cwd_under_root() {
  # "<pid> TAB <cwd>" for every snapshot record whose cwd is under the
  # physical work root, excluding protected pids.
  local prot
  prot="$(protected_pids | tr '\n' '|')"
  prot="${prot%|}"
  printf '%s\n' "${LSOF_LINES:-}" | awk -v root="${scan_root}" -v prot="^(${prot})$" '
    /^p[0-9]+$/ { pid = substr($0, 2) }
    /^n\// {
      cwd = substr($0, 2)
      if ((cwd == root || index(cwd, root "/") == 1) && pid !~ prot) {
        printf "%s\t%s\n", pid, cwd
      }
    }'
}

snapshot_cwd_of() {
  # cwd of one pid from the snapshot, or "-" when unknown.
  printf '%s\n' "${LSOF_LINES:-}" | awk -v want="$1" '
    /^p[0-9]+$/ { pid = substr($0, 2); seen = (pid == want) }
    seen && /^n\// { print substr($0, 2); found = 1; exit }
    END { if (!found) print "-" }'
}

victims() { snapshot_cwd_under_root | cut -f1; }

foreign_argv_matches() {
  # Processes whose ARGV references _work but whose cwd is elsewhere:
  # informational only, never signalled.
  { pgrep -f "${argv_pattern}" || true; } | while read -r pid; do
    [ -n "${pid}" ] || continue
    cwd="$(snapshot_cwd_of "${pid}")"
    case "${cwd}" in
      "${scan_root}"|"${scan_root}"/*) ;; # already a cwd victim
      *) printf '  pid=%s cwd=%s cmd=%s\n' "${pid}" "${cwd}" "$(cmd_of "${pid}")" ;;
    esac
  done
}

cmd_of() { ps -o command= -p "$1" 2>/dev/null || echo '<gone>'; }

case "${mode}" in
  list)
    snapshot || { echo "::error::kill-strays: lsof discovery failed; live-process state UNKNOWN" >&2; exit 5; }
    v_out="$(snapshot_cwd_under_root)"
    if [ -z "${v_out}" ] && ! pgrep -f "${argv_pattern}" >/dev/null 2>&1; then
      echo "kill-strays: no live processes are rooted in ${scan_root}"
      exit 0
    fi
    if [ -n "${v_out}" ]; then
      echo "kill-strays: KILL CANDIDATES (cwd under ${scan_root}; self/ancestors excluded):"
      printf '%s\n' "${v_out}" | while IFS="$(printf '\t')" read -r pid cwd; do
        printf '  pid=%s cwd=%s cmd=%s\n' "${pid}" "${cwd}" "$(cmd_of "${pid}")"
      done
    else
      echo "kill-strays: no kill candidates with cwd under ${scan_root}"
    fi
    f_out="$(foreign_argv_matches)"
    if [ -n "${f_out}" ]; then
      echo "kill-strays: FOREIGN argv-only matches (NOT signalled; listed for the operator):"
      printf '%s\n' "${f_out}"
    fi
    # End with an explicit success: a trailing `[ ... ] && { ... }` would
    # leave the arm's status at 1 when there is nothing foreign to print
    # (review R5).
    exit 0
    ;;
  kill)
    snapshot || { echo "::error::kill-strays: lsof discovery failed; live-process state UNKNOWN, nothing signalled" >&2; exit 5; }
    pids="$(victims | tr '\n' ' ')"
    if [ -z "${pids// /}" ]; then
      echo "kill-strays: no kill candidates with cwd under ${scan_root} (run 'list' to inspect argv-only matches)"
      exit 0
    fi
    echo "kill-strays: terminating (cwd under ${scan_root}): ${pids}"
    # shellcheck disable=SC2086
    kill -TERM ${pids} 2>/dev/null || true
    sleep 3
    snapshot || { echo "::error::kill-strays: post-TERM lsof discovery failed; survivor state UNKNOWN" >&2; exit 5; }
    pids="$(victims | tr '\n' ' ')"
    if [ -n "${pids// /}" ]; then
      echo "kill-strays: SIGTERM survivors, escalating to SIGKILL: ${pids}" >&2
      # shellcheck disable=SC2086
      kill -9 ${pids} 2>/dev/null || true
      sleep 1
    fi
    snapshot || { echo "::error::kill-strays: post-KILL lsof discovery failed; survivor state UNKNOWN" >&2; exit 5; }
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
