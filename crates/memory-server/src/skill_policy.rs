use serde_json::{json, Value};

pub(crate) const SUPERPOWER_BRAINSTORMING: &str = "skill:superpowers-brainstorming";
pub(crate) const SUPERPOWER_WRITING_PLANS: &str = "skill:superpowers-writing-plans";
pub(crate) const SUPERPOWER_EXECUTING_PLANS: &str = "skill:superpowers-executing-plans";
pub(crate) const SUPERPOWER_SUBAGENT_DRIVEN_DEVELOPMENT: &str =
    "skill:superpowers-subagent-driven-development";
pub(crate) const SUPERPOWER_REQUESTING_CODE_REVIEW: &str =
    "skill:superpowers-requesting-code-review";
pub(crate) const SUPERPOWER_VERIFICATION_BEFORE_COMPLETION: &str =
    "skill:superpowers-verification-before-completion";
pub(crate) const SUPERPOWER_FINISHING_BRANCH: &str =
    "skill:superpowers-finishing-a-development-branch";

pub(crate) const WAZA_CHECK: &str = "skill:waza-check";
pub(crate) const WAZA_DESIGN: &str = "skill:waza-design";
pub(crate) const WAZA_HEALTH: &str = "skill:waza-health";
pub(crate) const WAZA_HUNT: &str = "skill:waza-hunt";
pub(crate) const WAZA_LEARN: &str = "skill:waza-learn";
pub(crate) const WAZA_READ: &str = "skill:waza-read";
pub(crate) const WAZA_TACHI: &str = "skill:waza-tachi";
pub(crate) const WAZA_THINK: &str = "skill:waza-think";
pub(crate) const WAZA_WRITE: &str = "skill:waza-write";

pub(crate) const CODING_REFACTOR_CHECKLIST: &str = "skill:coding-refactor-checklist";
pub(crate) const CODING_TEST_STRATEGY: &str = "skill:coding-test-strategy";
pub(crate) const CODING_ARCHITECTURE_DECISION: &str = "skill:coding-architecture-decision";

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

pub(crate) fn shell_stage_skills(stage: &str) -> Vec<String> {
    match stage {
        "auto" | "execute" => Vec::new(),
        other => dispatch_stage_skills(other),
    }
}

pub(crate) fn worker_skills_for_task(task: &str) -> Vec<String> {
    let route = crate::copilot_ops::build_task_brief_routing(task, &[]);
    let mut skills = ids(&[WAZA_TACHI]);
    append_builtin_sops(&mut skills, route.selected_sops.into_iter());
    dedupe_preserve_order(&mut skills);
    skills
}

pub(crate) fn worker_skills_for_convoy_slice(parent_task: &str, slice_task: &str) -> Vec<String> {
    let combined = format!("{parent_task}\n{slice_task}");
    let mut skills = ids(&[
        SUPERPOWER_EXECUTING_PLANS,
        SUPERPOWER_REQUESTING_CODE_REVIEW,
    ]);
    skills.extend(worker_skills_for_task(&combined));
    dedupe_preserve_order(&mut skills);
    skills
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

pub(crate) fn native_policy_summary(stage: &str, required_skills: &[String]) -> Value {
    json!({
        "stage": stage,
        "required_skills": required_skills,
        "policy": "native",
        "leader_rule": "apply lifecycle skills before dispatching or shipping",
        "worker_rule": "apply role/task skills before substantive work and report skills used",
    })
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

    #[test]
    fn worker_policy_maps_gh_review_tasks_to_check() {
        let skills = worker_skills_for_task("look at the new GitHub PR and issues");
        assert!(skills.contains(&WAZA_CHECK.to_string()), "{skills:?}");
        assert!(skills.contains(&WAZA_TACHI.to_string()), "{skills:?}");
    }

    #[test]
    fn shell_ship_policy_requires_verification_gate() {
        let skills = shell_stage_skills("ship");
        assert!(skills.contains(&SUPERPOWER_VERIFICATION_BEFORE_COMPLETION.to_string()));
        assert!(skills.contains(&SUPERPOWER_FINISHING_BRANCH.to_string()));
    }

    #[test]
    fn shell_policy_rejects_dispatch_only_stages() {
        assert!(shell_stage_skills("auto").is_empty());
        assert!(shell_stage_skills("execute").is_empty());
    }
}
