use serde_json::Value;

pub(crate) const SUPERPOWER_BRAINSTORMING: &str = tachi_dispatch::SUPERPOWER_BRAINSTORMING;
pub(crate) const SUPERPOWER_WRITING_PLANS: &str = tachi_dispatch::SUPERPOWER_WRITING_PLANS;
pub(crate) const SUPERPOWER_EXECUTING_PLANS: &str = tachi_dispatch::SUPERPOWER_EXECUTING_PLANS;
pub(crate) const SUPERPOWER_SUBAGENT_DRIVEN_DEVELOPMENT: &str =
    tachi_dispatch::SUPERPOWER_SUBAGENT_DRIVEN_DEVELOPMENT;
pub(crate) const SUPERPOWER_REQUESTING_CODE_REVIEW: &str =
    tachi_dispatch::SUPERPOWER_REQUESTING_CODE_REVIEW;
pub(crate) const SUPERPOWER_VERIFICATION_BEFORE_COMPLETION: &str =
    tachi_dispatch::SUPERPOWER_VERIFICATION_BEFORE_COMPLETION;
pub(crate) const SUPERPOWER_FINISHING_BRANCH: &str = tachi_dispatch::SUPERPOWER_FINISHING_BRANCH;

pub(crate) const WAZA_CHECK: &str = tachi_dispatch::WAZA_CHECK;
pub(crate) const WAZA_DESIGN: &str = tachi_dispatch::WAZA_DESIGN;
pub(crate) const WAZA_HEALTH: &str = tachi_dispatch::WAZA_HEALTH;
pub(crate) const WAZA_HUNT: &str = tachi_dispatch::WAZA_HUNT;
pub(crate) const WAZA_LEARN: &str = tachi_dispatch::WAZA_LEARN;
pub(crate) const WAZA_READ: &str = tachi_dispatch::WAZA_READ;
pub(crate) const WAZA_TACHI: &str = tachi_dispatch::WAZA_TACHI;
pub(crate) const WAZA_THINK: &str = tachi_dispatch::WAZA_THINK;
pub(crate) const WAZA_WRITE: &str = tachi_dispatch::WAZA_WRITE;

pub(crate) const CODING_REFACTOR_CHECKLIST: &str = tachi_dispatch::CODING_REFACTOR_CHECKLIST;
pub(crate) const CODING_TEST_STRATEGY: &str = tachi_dispatch::CODING_TEST_STRATEGY;
pub(crate) const CODING_ARCHITECTURE_DECISION: &str = tachi_dispatch::CODING_ARCHITECTURE_DECISION;

pub(crate) fn dispatch_stage_key(stage: Option<&str>) -> String {
    stage
        .unwrap_or("")
        .to_ascii_lowercase()
        .split(':')
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

pub(crate) fn dispatch_stage_instruction(stage_key: &str) -> Option<String> {
    if stage_key == "auto" {
        Some(
            "IMPORTANT: Produce a plan first. Do NOT execute directly. \
             Wait for the operator to review the plan and trigger the execute stage."
                .to_string(),
        )
    } else {
        None
    }
}

pub(crate) fn dispatch_stage_skills(stage_key: &str) -> Vec<String> {
    match stage_key {
        "brainstorm" => ids(&[SUPERPOWER_BRAINSTORMING, WAZA_THINK]),
        "plan" | "auto" => ids(&[SUPERPOWER_WRITING_PLANS, WAZA_THINK]),
        "dispatch" => ids(&[
            SUPERPOWER_SUBAGENT_DRIVEN_DEVELOPMENT,
            SUPERPOWER_EXECUTING_PLANS,
            WAZA_TACHI,
        ]),
        "execute" => ids(&[SUPERPOWER_EXECUTING_PLANS]),
        "review" => ids(&[
            SUPERPOWER_REQUESTING_CODE_REVIEW,
            SUPERPOWER_VERIFICATION_BEFORE_COMPLETION,
            WAZA_CHECK,
        ]),
        "ship" => ids(&[
            SUPERPOWER_VERIFICATION_BEFORE_COMPLETION,
            SUPERPOWER_FINISHING_BRANCH,
            WAZA_CHECK,
        ]),
        _ => Vec::new(),
    }
}

pub(crate) fn is_native_skill(id: &str) -> bool {
    matches!(
        id,
        SUPERPOWER_BRAINSTORMING
            | SUPERPOWER_WRITING_PLANS
            | SUPERPOWER_EXECUTING_PLANS
            | SUPERPOWER_SUBAGENT_DRIVEN_DEVELOPMENT
            | SUPERPOWER_REQUESTING_CODE_REVIEW
            | SUPERPOWER_VERIFICATION_BEFORE_COMPLETION
            | SUPERPOWER_FINISHING_BRANCH
            | WAZA_CHECK
            | WAZA_DESIGN
            | WAZA_HEALTH
            | WAZA_HUNT
            | WAZA_LEARN
            | WAZA_READ
            | WAZA_TACHI
            | WAZA_THINK
            | WAZA_WRITE
            | CODING_REFACTOR_CHECKLIST
            | CODING_TEST_STRATEGY
            | CODING_ARCHITECTURE_DECISION
    )
}

pub(crate) fn dedupe_preserve_order(items: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    items.retain(|item| seen.insert(item.clone()));
}

pub(crate) fn append_builtin_sops(skills: &mut Vec<String>, sops: impl Iterator<Item = Value>) {
    for sop in sops {
        let Some(id) = sop.get("id").and_then(|value| value.as_str()) else {
            continue;
        };
        if id.starts_with("skill:") && is_native_skill(id) {
            skills.push(id.to_string());
        }
    }
    dedupe_preserve_order(skills);
}

fn ids(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_policy_keeps_subagent_factory_on_parent_dispatch() {
        let dispatch = dispatch_stage_skills("dispatch");
        assert!(dispatch.contains(&SUPERPOWER_SUBAGENT_DRIVEN_DEVELOPMENT.to_string()));

        let execute = dispatch_stage_skills("execute");
        assert!(!execute.contains(&SUPERPOWER_SUBAGENT_DRIVEN_DEVELOPMENT.to_string()));
        assert!(execute.contains(&SUPERPOWER_EXECUTING_PLANS.to_string()));
    }
}
