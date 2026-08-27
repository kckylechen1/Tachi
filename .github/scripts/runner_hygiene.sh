#!/usr/bin/env bash
# Trusted acceptance-runner hygiene gate (#1865).
#
# One tracked script serves both ends of every acceptance-lane job:
#   runner_hygiene.sh preflight <watermark_gib>   -- first step after checkout
#   runner_hygiene.sh cleanup                     -- last step, marked if: always()
#
# Contract (issue #1865, "Runner lifecycle / disk hygiene"):
#   * pre-job: terminate processes a cancelled predecessor left holding the
#     workspace (auto cancel-in-progress can leave escaped cargo/nextest/
#     rustc descendants; review R3), remove file residue, then refuse the
#     job loudly when free disk on the workspace volume is below the
#     watermark. A skip below the watermark is a clear failure with a
#     non-zero exit, never a silent build into disk exhaustion.
#   * post-job: wipe the workspace contents entirely (the next checkout
#     rebuilds it) so nothing squats on the runner between jobs, and report
#     free disk so the run log carries a disk ledger. Caches that live
#     outside the disposable workspace root (~/.rustup toolchains,
#     ~/.cargo registry, the runner's _tool cache, ~/.npm) are bounded by
#     the toolchain/dependency universe, not by job count; they are
#     reported by the operator runbook, not deleted here.
#
# Watermarks (#1865 activation gate): first activation requires >= 80 GiB
# free; heavy Rust work must refuse below 60 GiB free. Light lanes pass a
# smaller sanity watermark at their call site.
#
# Bash-3.2 compatible: the macOS acceptance host resolves /bin/bash.

set -euo pipefail

workspace="${GITHUB_WORKSPACE:-$(pwd)}"

free_kib() {
  # 1-KiB blocks available on the workspace volume, as an integer. The
  # watermark comparison runs on this integer, never on a rounded GiB
  # display (review R1: 59.96 GiB must not round up past a 60 gate).
  df -k "${workspace}" | awk 'NR==2 {print $4}'
}

gib_display() {
  awk -v b="$1" 'BEGIN{printf "%.1f", b / 1024 / 1024}'
}

report_disk() {
  df -h "${workspace}" | tail -1
}

kill_prior_job_strays() {
  # Processes still holding the workspace after a cancelled predecessor
  # would race the successor's build. Delegation target is the tracked
  # operator killer, scoped to cwd-under-workspace, self-tree-safe.
  local killer
  killer="$(cd "$(dirname "$0")" && pwd)/runner_kill_strays.sh"
  if [ ! -f "${killer}" ]; then
    echo "runner-hygiene: WARNING: stray killer not found at ${killer}; process-residue termination skipped" >&2
    return 0
  fi
  if SCAN_ROOT="${workspace}" bash "${killer}" kill; then
    return 0
  fi
  local rc=$?
  echo "::error::runner-hygiene: processes from a previous job still hold the workspace (killer exit ${rc}); refusing to build beside them (#1865 acceptance 7)." >&2
  return "${rc}"
}

clean_residue() {
  local removed=0
  local path
  local list err
  list="$(mktemp "${TMPDIR:-/tmp}/runner-hygiene.XXXXXX")"
  err="$(mktemp "${TMPDIR:-/tmp}/runner-hygiene.XXXXXX")"
  # mktemp-created, 0600, unguessable; removed on every exit path. The trap
  # binds the expanded paths now (double quotes) so it stays valid after the
  # function's locals go out of scope; mktemp names contain no quote chars.
  # shellcheck disable=SC2064
  trap "rm -f -- '${list}' '${err}'" EXIT
  printf '%s\n' "${workspace}/target" > "${list}"
  # node_modules discovery is best-effort but never silent: a find failure
  # is printed (and the heavy hitter, target/, does not depend on it).
  if ! find "${workspace}" -maxdepth 3 -name node_modules -type d -print \
      >> "${list}" 2> "${err}"; then
    echo "runner-hygiene: WARNING: node_modules residue discovery failed; continuing with target/ only:" >&2
    sed 's/^/  /' "${err}" >&2 || true
  fi
  while IFS= read -r path; do
    [ -n "${path}" ] || continue
    [ -e "${path}" ] || continue
    rm -rf -- "${path}"
    removed=$((removed + 1))
    echo "runner-hygiene: removed residue ${path}"
  done < "${list}"
  rm -f -- "${list}" "${err}"
  if [ "${removed}" -eq 0 ]; then
    echo "runner-hygiene: no prior-job residue found"
  fi
}

wipe_workspace() {
  # Remove the entire workspace contents (bash keeps an open fd on this
  # script, so unlinking it mid-run is safe). The next checkout rebuilds
  # the tree; between jobs the runner holds no per-job disk at all.
  echo "runner-hygiene: wiping workspace contents"
  if ! find "${workspace}" -mindepth 1 -maxdepth 1 -exec rm -rf -- {} +; then
    echo "runner-hygiene: WARNING: workspace wipe failed partway; the next preflight re-cleans" >&2
    return 0
  fi
}

case "${1:-}" in
  preflight)
    watermark="${2:?usage: runner_hygiene.sh preflight <watermark_gib>}"
    echo "runner-hygiene: preflight for workspace ${workspace}"
    echo "runner-hygiene: disk before cleanup:"
    report_disk
    kill_prior_job_strays
    clean_residue
    free_k="$(free_kib)"
    echo "runner-hygiene: free disk after cleanup: $(gib_display "${free_k}") GiB (watermark: ${watermark} GiB)"
    if awk -v b="${free_k}" -v w="${watermark}" 'BEGIN{exit !(b < w * 1024 * 1024)}'; then
      echo "::error::runner-hygiene: free disk $(gib_display "${free_k}") GiB is below the ${watermark} GiB start watermark; refusing this job loudly instead of building toward disk exhaustion (#1865 acceptance 6)." >&2
      exit 3
    fi
    ;;
  cleanup)
    echo "runner-hygiene: post-job cleanup for workspace ${workspace}"
    echo "runner-hygiene: disk before cleanup:"
    report_disk
    wipe_workspace
    echo "runner-hygiene: disk after cleanup:"
    report_disk
    ;;
  *)
    echo "usage: runner_hygiene.sh preflight <watermark_gib> | cleanup" >&2
    exit 2
    ;;
esac
