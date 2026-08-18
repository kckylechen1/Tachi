// Stage/skill auto-derivation helpers (`dispatch_stage_skills`,
// `append_builtin_sops`) were retired in #1690 C3 S1: a dispatch's skills
// resolve ONLY from the explicit `params.skills` param plus the profile's
// STATIC reviewed list. The consts below survive for `is_native_skill` (used by
// the copilot briefing's static intent-map projection) and the profile tests.

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

#[cfg(test)]
mod tests {
    use super::*;

    /// #1690 C3 S1 re-anchor: the stage→skills map is retired, so the
    /// executor-vs-planner skill split the old map guarded now lives in the
    /// profiles' STATIC reviewed skill lists — the only surviving skill source
    /// for a dispatch. An executor profile carries the execution skill and NOT
    /// the subagent-factory skill; the planner carries the subagent factory.
    #[test]
    fn executor_profiles_carry_execution_skills_but_not_the_subagent_factory() {
        let impl_profile =
            tachi_dispatch::resolve_dispatch_profile("glm_impl").expect("glm_impl profile");
        let impl_skills = tachi_dispatch::profile_required_skill_ids(impl_profile);
        assert!(
            impl_skills.contains(&SUPERPOWER_EXECUTING_PLANS.to_string()),
            "executor static skills must carry the execution skill: {impl_skills:?}"
        );
        assert!(
            !impl_skills.contains(&SUPERPOWER_SUBAGENT_DRIVEN_DEVELOPMENT.to_string()),
            "executor static skills must NOT carry the subagent factory: {impl_skills:?}"
        );

        let plan_profile =
            tachi_dispatch::resolve_dispatch_profile("claude_plan").expect("claude_plan profile");
        let plan_skills = tachi_dispatch::profile_required_skill_ids(plan_profile);
        assert!(
            plan_skills.contains(&SUPERPOWER_SUBAGENT_DRIVEN_DEVELOPMENT.to_string()),
            "planner static skills must carry the subagent factory: {plan_skills:?}"
        );
    }
}
