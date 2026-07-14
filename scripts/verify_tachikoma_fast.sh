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

# ─── Fast test selection (Refs #1095) ─────────────────────────────────────────
# The six serial `cargo test -p tachi-server <name> -- --test-threads=1`
# commands converged into ONE `cargo nextest run` filterset. The test SELECTION
# SET is byte-identical (see https://nexte.st/docs/selecting):
#   * `cargo test <name>` matches the full `module::test_name` by SUBSTRING;
#   * `cargo nextest run -E 'test(<name>)'` does the same substring ("contains")
#     match on the same full name, and `+` is the set-union (OR) operator.
#   * None of the filter strings contain filterset metacharacters
#     (=, *, ?, [, ], /), so each `test(...)` is a plain substring predicate
#     identical to the corresponding `cargo test <name>` positional argument.
#   * `-j 1` preserves the original `--test-threads=1` serial execution; no
#     parallelism is widened.
# Verbatim mapping (original cargo test filter -> term in the union):
#   1. bootstrap::cli_tool::cards::tests                     -> test(bootstrap::cli_tool::cards::tests)
#   2. bootstrap::skill_surface_cli                          -> test(bootstrap::skill_surface_cli)
#   3. poke_smoke_suite_writes_report_and_probe_artifacts   -> test(poke_smoke_suite_writes_report_and_probe_artifacts)
#   4. proposal_evolution                                    -> test(proposal_evolution)
#   5. TACHI_FAST_DEEP=0: dispatch_ops::acpx::tests          -> test(dispatch_ops::acpx::tests)
#      TACHI_FAST_DEEP=1: acpx                               -> test(acpx)   (broader substring, a strict superset)
#   6. cycle_status                                          -> test(cycle_status)
#
# Note (inherited, non-impactful): nextest's repo profiles carry a slow-timeout
# cap (terminate-after 6 x 20s = 120s) that bare `cargo test` lacks; all six
# selected groups are known-fast (sub-second), so this never fires. It is a
# ceiling, not a timeout expansion.
if [[ "${TACHI_FAST_DEEP:-0}" == "1" ]]; then
  ACPX_FILTER="test(acpx)"
else
  ACPX_FILTER="test(dispatch_ops::acpx::tests)"
fi
TEST_FILTER="test(bootstrap::cli_tool::cards::tests) \
+ test(bootstrap::skill_surface_cli) \
+ test(poke_smoke_suite_writes_report_and_probe_artifacts) \
+ test(proposal_evolution) \
+ ${ACPX_FILTER} \
+ test(cycle_status)"
run cargo nextest run -p tachi-server --locked -j 1 -E "$TEST_FILTER"

run cargo clippy -p tachi-server --all-targets --locked -- -D warnings
