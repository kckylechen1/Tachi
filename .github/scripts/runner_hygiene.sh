#!/usr/bin/env bash
# Trusted acceptance-runner hygiene gate (#1865).
#
# One tracked script serves both ends of every acceptance-lane job:
#   runner_hygiene.sh preflight <watermark_gib>   -- first step after checkout
#   runner_hygiene.sh cleanup                     -- last step, marked if: always()
#
# Contract (issue #1865, "Runner lifecycle / disk hygiene"):
#   * pre-job: remove residue a cancelled or failed predecessor left in the
#     checkout, then refuse the job loudly when free disk on the workspace
#     volume is below the watermark. A skip below the watermark is a clear
#     failure with a non-zero exit, never a silent build into disk exhaustion.
#   * post-job: remove workspace build residue (target/, node_modules/) so a
#     single-slot self-hosted runner does not squat tens of GB between runs,
#     and report free disk so the run log carries a disk ledger.
#   * Caches that legitimately live outside the disposable workspace root
#     (~/.rustup toolchains, ~/.cargo registry, the runner's _tool cache,
#     ~/.npm) are bounded by the toolchain/dependency universe, not by job
#     count; they are reported by the operator runbook, not deleted here.
#
# Watermarks (#1865 activation gate): first activation requires >= 80 GiB
# free; heavy Rust work must refuse below 60 GiB free. Light lanes pass a
# smaller sanity watermark at their call site.
#
# Bash-3.2 compatible: the macOS acceptance host resolves /bin/bash.

set -euo pipefail

workspace="${GITHUB_WORKSPACE:-$(pwd)}"

free_gib() {
  df -k "${workspace}" | awk 'NR==2 {printf "%.1f", $4 / 1024 / 1024}'
}

report_disk() {
  df -h "${workspace}" | tail -1
}

clean_residue() {
  local removed=0
  local path
  # Large untracked build outputs a previous job may have left behind. The
  # residue list is deliberately explicit: workspace-local target/ and
  # node_modules/ trees are per-job ephemeral by policy (#1865), never a
  # shared unbounded build cache.
  while IFS= read -r path; do
    [ -n "${path}" ] || continue
    [ -e "${path}" ] || continue
    rm -rf -- "${path}"
    removed=$((removed + 1))
    echo "runner-hygiene: removed residue ${path}"
  done <<EOF
$(printf '%s\n' "${workspace}/target"
  find "${workspace}" -maxdepth 3 -name node_modules -type d -print 2>/dev/null || true)
EOF
  if [ "${removed}" -eq 0 ]; then
    echo "runner-hygiene: no prior-job residue found"
  fi
}

case "${1:-}" in
  preflight)
    watermark="${2:?usage: runner_hygiene.sh preflight <watermark_gib>}"
    echo "runner-hygiene: preflight for workspace ${workspace}"
    echo "runner-hygiene: disk before residue cleanup:"
    report_disk
    clean_residue
    free="$(free_gib)"
    echo "runner-hygiene: free disk after residue cleanup: ${free} GiB (watermark: ${watermark} GiB)"
    if awk "BEGIN{exit !(${free} < ${watermark})}"; then
      echo "::error::runner-hygiene: free disk ${free} GiB is below the ${watermark} GiB start watermark; refusing this job loudly instead of building toward disk exhaustion (#1865 acceptance 6)." >&2
      exit 3
    fi
    ;;
  cleanup)
    echo "runner-hygiene: post-job cleanup for workspace ${workspace}"
    echo "runner-hygiene: disk before cleanup:"
    report_disk
    clean_residue
    echo "runner-hygiene: disk after cleanup:"
    report_disk
    ;;
  *)
    echo "usage: runner_hygiene.sh preflight <watermark_gib> | cleanup" >&2
    exit 2
    ;;
esac
