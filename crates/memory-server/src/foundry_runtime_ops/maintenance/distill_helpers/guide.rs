use super::text::{fragments_contain_any, guide_text_fragments, has_numbered_steps};
use memory_core::MemoryEntry;

pub(in crate::foundry_runtime_ops::maintenance::distill_helpers) const GUIDE_TYPE_CONSTRAINT: &str =
    "constraint";
pub(in crate::foundry_runtime_ops::maintenance::distill_helpers) const GUIDE_TYPE_FIX_PATTERN:
    &str = "fix_pattern";
pub(in crate::foundry_runtime_ops::maintenance::distill_helpers) const GUIDE_TYPE_DECISION: &str =
    "decision";
pub(in crate::foundry_runtime_ops::maintenance::distill_helpers) const GUIDE_TYPE_RUNBOOK: &str =
    "runbook";

pub(in crate::foundry_runtime_ops) fn classify_distill_guide_type(
    distill_text: &str,
    source_entries: &[MemoryEntry],
) -> &'static str {
    let fragments = || guide_text_fragments(distill_text, source_entries);

    if fragments_contain_any(
        fragments(),
        &[
            "fix",
            "fixed",
            "repair",
            "bug",
            "error",
            "failure",
            "failed",
            "panic",
            "exception",
            "regression",
            "linker",
            "修复",
            "报错",
            "错误",
            "失败",
        ],
    ) {
        return GUIDE_TYPE_FIX_PATTERN;
    }
    if fragments_contain_any(
        fragments(),
        &[
            "must",
            "must not",
            "never",
            "required",
            "constraint",
            "invariant",
            "policy",
            "do not",
            "don't",
            "不得",
            "必须",
            "禁止",
            "约束",
        ],
    ) {
        return GUIDE_TYPE_CONSTRAINT;
    }
    if fragments_contain_any(
        fragments(),
        &[
            "decided", "decision", "choose", "chosen", "accepted", "rejected", "tradeoff", "adr",
            "决定", "取舍", "拒绝",
        ],
    ) || source_entries
        .iter()
        .any(|entry| entry.category.eq_ignore_ascii_case("decision"))
    {
        return GUIDE_TYPE_DECISION;
    }
    if fragments_contain_any(
        fragments(),
        &[
            "runbook",
            "checklist",
            "procedure",
            "step",
            "steps",
            "playbook",
            "how to",
            "操作",
            "步骤",
            "流程",
        ],
    ) || has_numbered_steps(distill_text)
    {
        return GUIDE_TYPE_RUNBOOK;
    }
    GUIDE_TYPE_RUNBOOK
}
