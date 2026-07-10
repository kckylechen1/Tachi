#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeSkillId {
    pub id: &'static str,
    pub name: &'static str,
}

macro_rules! define_native_skill_ids {
    ($collection:ident { $($constant:ident => ($id:literal, $name:literal)),+ $(,)? }) => {
        $(pub const $constant: &str = $id;)+

        pub const $collection: &[NativeSkillId] = &[
            $(NativeSkillId { id: $constant, name: $name },)+
        ];
    };
}

define_native_skill_ids!(SUPERPOWER_SKILL_IDS {
    SUPERPOWER_BRAINSTORMING => ("skill:superpowers-brainstorming", "brainstorming"),
    SUPERPOWER_WRITING_PLANS => ("skill:superpowers-writing-plans", "writing-plans"),
    SUPERPOWER_EXECUTING_PLANS => ("skill:superpowers-executing-plans", "executing-plans"),
    SUPERPOWER_SUBAGENT_DRIVEN_DEVELOPMENT => (
        "skill:superpowers-subagent-driven-development",
        "subagent-driven-development"
    ),
    SUPERPOWER_REQUESTING_CODE_REVIEW => (
        "skill:superpowers-requesting-code-review",
        "requesting-code-review"
    ),
    SUPERPOWER_VERIFICATION_BEFORE_COMPLETION => (
        "skill:superpowers-verification-before-completion",
        "verification-before-completion"
    ),
    SUPERPOWER_FINISHING_BRANCH => (
        "skill:superpowers-finishing-a-development-branch",
        "finishing-a-development-branch"
    ),
});

define_native_skill_ids!(WAZA_SKILL_IDS {
    WAZA_CHECK => ("skill:waza-check", "check"),
    WAZA_DESIGN => ("skill:waza-design", "design"),
    WAZA_HEALTH => ("skill:waza-health", "health"),
    WAZA_HUNT => ("skill:waza-hunt", "hunt"),
    WAZA_LEARN => ("skill:waza-learn", "learn"),
    WAZA_READ => ("skill:waza-read", "read"),
    WAZA_TACHI => ("skill:waza-tachi", "tachi"),
    WAZA_THINK => ("skill:waza-think", "think"),
    WAZA_WRITE => ("skill:waza-write", "write"),
});

define_native_skill_ids!(CODING_SKILL_IDS {
    CODING_REFACTOR_CHECKLIST => ("skill:coding-refactor-checklist", "refactor-checklist"),
    CODING_TEST_STRATEGY => ("skill:coding-test-strategy", "test-strategy"),
    CODING_ARCHITECTURE_DECISION => (
        "skill:coding-architecture-decision",
        "architecture-decision"
    ),
});

pub fn is_native_skill_id(id: &str) -> bool {
    SUPERPOWER_SKILL_IDS
        .iter()
        .chain(WAZA_SKILL_IDS)
        .chain(CODING_SKILL_IDS)
        .any(|skill| skill.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_skill_ids_are_unique_and_keep_expected_cardinality() {
        let ids = SUPERPOWER_SKILL_IDS
            .iter()
            .chain(WAZA_SKILL_IDS)
            .chain(CODING_SKILL_IDS)
            .map(|skill| skill.id)
            .collect::<std::collections::HashSet<_>>();

        assert_eq!(SUPERPOWER_SKILL_IDS.len(), 7);
        assert_eq!(WAZA_SKILL_IDS.len(), 9);
        assert_eq!(CODING_SKILL_IDS.len(), 3);
        assert_eq!(ids.len(), 19);
    }
}
