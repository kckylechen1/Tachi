# Actions capacity and quota recovery

Status: operational guidance for the delivery doctrine. Checked: 2026-09-20.
Owner request: “然后你记得给 agent md，文档这些都更新一下。然后好像我的 minutes 不够了。”

[`dispatch-lifecycle.md` §8–§9](../architecture/dispatch-lifecycle.md#8-acceptance-evidence)
owns acceptance and authority. This runbook diagnoses capacity; it neither changes
billing nor waives a required check. Read live job and billing evidence before acting.

## Separate the two execution paths

The workflow definitions at recovery parent `800d417ed96d3d26e9d10ebbd55025bbcec52ad5`
route work as follows. Re-read the live workflow before using this inventory.

| Defined job | Runner | Evidence scope |
| --- | --- | --- |
| Rust workspace | self-hosted, tachi-acceptance | Rust commands on the actual host OS/architecture |
| Build seat setup policy | self-hosted, tachi-acceptance | Tooling and supply-chain policy |
| Secret scan | self-hosted, tachi-acceptance | gitleaks |
| Node packages | self-hosted, tachi-acceptance | Two package matrix members |
| CI acceptance | self-hosted, tachi-acceptance | Automated job-family aggregation only |
| Physical DB identity fallback | windows-latest | Three Windows crate matrix members |
| Separate fmt.yml format gate | ubuntu-24.04 | Hosted formatting if that workflow is enabled and triggered |

A self-hosted label does not establish Linux. Record actual runner name, OS, and
architecture from the run. macOS acceptance cannot certify Linux-only process
behavior, and neither can substitute for Windows-specific checks.

GitHub's [Actions billing documentation](https://docs.github.com/en/billing/concepts/product-billing/github-actions)
states that self-hosted execution is free of GitHub Actions minute charges, while
private-repository GitHub-hosted execution uses the repository owner's included
allowance and then paid usage. Artifact/cache storage is a separate billing axis;
self-hosted compute still uses the owner's hardware and capacity. Recheck that
source when diagnosing a later incident; do not hard-code a perpetual price promise.

## Read before retrying

1. Record repository, PR, requested head/base, actual checkout/tree, run id/attempt,
   job names, raw status/conclusion, and whether steps ran. Preserve existing logs.
2. Inspect the run's own annotation. Zero steps proves no test execution, not a
   particular cause. Billing, payment, budget, service, or permission failures need
   their own evidence. No annotation access means the cause remains unconfirmed.
3. For hosted jobs, inspect the owner's Billing and licensing overview, Actions
   usage for the current billing cycle, and Budgets and alerts. Check allowance,
   applicable hard-stop budgets, payment failures, and storage separately. A
   quota/budget/payment failure is not repaired by an unchanged workflow rerun.
4. For self-hosted queues, inspect repository Settings → Actions → Runners for the
   registered runner's status and matching labels. Inspect its service and `_diag`
   logs on the host when authorized. Offline, busy, incompatible labels, network,
   disk, and account/service restrictions are distinct possibilities. Do not infer
   a specific one just from `queued`, or assume purchasing minutes fixes the queue.

Billing guidance: [usage](https://docs.github.com/en/billing/how-tos/products/view-productlicense-use)
and [budgets](https://docs.github.com/en/billing/how-tos/set-up-budgets).
Runner guidance: [monitoring and troubleshooting](https://docs.github.com/en/actions/how-tos/manage-runners/self-hosted-runners/monitor-and-troubleshoot).

Repository write permission is not proof of billing read/write access. If the
connection cannot read account usage or runner administration, report the missing
visibility; request a redacted usage/status screenshot, not a token or payment data.

## Conserve capacity without false acceptance

- Batch a coherent repair and documentation synchronization into one push. Run
  narrow local checks before pushing when the required tooling exists. Do not
  repeatedly edit PR heads merely to create another full acceptance run.
- Keep the existing per-PR concurrency cancellation of superseded runs. Cancel an
  additional run only after identifying its exact obsolete candidate and ownership;
  never indiscriminately cancel unrelated active work or discard retained evidence.
- While a failure cause is unchanged, stop manual reruns. Once readiness is restored,
  collect the missing evidence on the intended candidate. A changed head needs a
  fresh review verdict and acceptance for that object; earlier results retain their
  original candidate attribution.
- Use an existing admitted self-hosted execution seat for appropriate work. New
  runners, trust-label changes, toolchain provisioning, or spending changes require
  their corresponding authorization. Do not expose owner credentials to PR code.
- Preserve every required Windows matrix member. New Windows hardware or an
  explicitly reviewed platform-scope change is a separate delivery, not an automatic
  Linux substitution or a late `not_applicable` label for a failed Windows run.
- Do not use `[skip ci]`, blanket path exclusions, `continue-on-error`, advisory
  suppression, or success-shaped receipts to bypass this candidate's obligations.
  Markdown that changes governance is not automatically a low-risk docs-only change.
- Hosted budgets, payment methods, private-repository visibility, branch protection,
  artifact retention/deletion, and live services are not changed by this runbook.
  An owner request to save minutes is not authority to purchase more or delete proof.

These are operating instructions, not claims of new scheduler, retry, or billing
machinery. The current workflow may still start CI automatically on a branch update;
a single batched push is not a promise of zero new runs.

## Return contract

Report what changed, what actually executed, which exact required items remain
blocked, the observed cause and its source (or unknown), and the next readiness check.
Keep `implemented`, `reviewed`, `accepted`, `merged`, and `deployed` separate. A
successful CI aggregate cannot establish independent review or an authenticated canary.
