#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

run() {
  printf '\n==> %s\n' "$*"
  "$@"
}

# Keep cargo invocations serialized. Parallel cargo commands contend on the
# package cache and target directory locks, which makes narrow checks look slow.
run cargo fmt --all --check
run git diff --check
run cargo test -p tachi-server bootstrap::cli_tool::cards::tests --locked -- --test-threads=1
run cargo test -p tachi-server bootstrap::skill_surface_cli --locked -- --test-threads=1
run cargo test -p tachi-server poke_smoke_suite_writes_report_and_probe_artifacts --locked -- --test-threads=1
run cargo test -p tachi-server proposal_evolution --locked -- --test-threads=1
if [[ "${TACHI_FAST_DEEP:-0}" == "1" ]]; then
  run cargo test -p tachi-server acpx --locked -- --test-threads=1
else
  run cargo test -p tachi-server dispatch_ops::acpx::tests --locked -- --test-threads=1
fi
run cargo test -p tachi-server cycle_status --locked -- --test-threads=1
run cargo clippy -p tachi-server --all-targets --locked -- -D warnings
