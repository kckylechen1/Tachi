use super::super::SkillSourceMetadata;
use super::types::{SkillChangeRisk, SkillSourceAffectedCard};

pub(in crate::bootstrap::skill_surface_cli) fn classify_skill_patch(
    source: &SkillSourceMetadata,
    path: &str,
    patch: &str,
) -> SkillChangeRisk {
    let mut classes = Vec::<String>::new();
    let mut reasons = Vec::<String>::new();
    let lower = patch.to_ascii_lowercase();
    let path_lower = path.to_ascii_lowercase();

    if path_lower.contains("/scripts/")
        || path_lower.ends_with(".sh")
        || lower.contains("```bash")
        || lower.contains("command")
        || lower.contains("subprocess")
    {
        push_unique(&mut classes, "shell_or_scripts");
        reasons.push("change may alter shell/script/tool execution guidance".to_string());
    }
    if contains_any(
        &lower,
        &[
            "allowed_tools",
            "permission",
            "permissions",
            "sandbox",
            "approval",
            "approve",
            "github_write",
            "write actions",
        ],
    ) {
        push_unique(&mut classes, "tool_permissions");
        reasons.push("change mentions permission or approval boundaries".to_string());
    }
    if contains_any(
        &lower,
        &[
            "rm -rf",
            "git clean",
            "delete",
            "destructive",
            "force",
            "cleanup",
            "clean up",
        ],
    ) {
        push_unique(&mut classes, "destructive_safety");
        reasons.push("change touches destructive or cleanup safety language".to_string());
    }
    if contains_any(
        &lower,
        &[
            "evidence",
            "verification",
            "verify",
            "review",
            "reviewer",
            "tests",
            "test ",
            "report",
            "done:",
        ],
    ) {
        push_unique(&mut classes, "evidence_contract");
        reasons.push("change may alter completion, review, or verification evidence".to_string());
    }
    if contains_any(
        &lower,
        &[
            "plan", "dispatch", "subagent", "task", "worker", "ledger", "todo", "workflow",
        ],
    ) {
        push_unique(&mut classes, "lifecycle_guidance");
        reasons.push("change may alter lifecycle or subagent orchestration guidance".to_string());
    }
    if contains_any(
        &lower,
        &["description:", "when_to_use:", "dispatch_intent:", "name:"],
    ) {
        push_unique(&mut classes, "routing_metadata");
        reasons.push("change may alter skill routing or discovery metadata".to_string());
    }
    if path_lower.contains("example") || lower.contains("example") {
        push_unique(&mut classes, "examples");
        reasons.push("change touches examples or sample workflow text".to_string());
    }
    if source.local_overlay.is_some() {
        push_unique(&mut classes, "local_overlay");
        reasons.push("local overlay must be checked against upstream text changes".to_string());
    }
    if source.kind.as_deref() == Some("tachi_native_contract") {
        push_unique(&mut classes, "native_contract");
        reasons.push(
            "Tachi native wrapper must be checked against upstream-equivalent changes".to_string(),
        );
    }
    if classes.is_empty() {
        classes.push("documentation_guidance".to_string());
        reasons.push("skill text changed without a more specific classifier hit".to_string());
    }

    let risk_level = if classes.iter().any(|class| {
        matches!(
            class.as_str(),
            "shell_or_scripts" | "tool_permissions" | "destructive_safety"
        )
    }) {
        "high"
    } else if classes.iter().any(|class| {
        matches!(
            class.as_str(),
            "evidence_contract"
                | "lifecycle_guidance"
                | "routing_metadata"
                | "local_overlay"
                | "native_contract"
        )
    }) {
        "medium"
    } else {
        "low"
    }
    .to_string();

    SkillChangeRisk {
        change_classes: classes,
        risk_level,
        risk_reasons: reasons,
    }
}

pub(in crate::bootstrap::skill_surface_cli) fn affected_cards_for_skill(
    skill_id: &str,
) -> Vec<SkillSourceAffectedCard> {
    crate::dispatch_profile::DISPATCH_PROFILES
        .iter()
        .filter(|profile| {
            crate::dispatch_profile::profile_required_skill_ids(profile)
                .iter()
                .any(|required| required == skill_id)
        })
        .map(|profile| {
            let card = crate::dispatch_profile::profile_json(profile);
            SkillSourceAffectedCard {
                profile: profile.name.to_string(),
                display_name: profile.display_name.to_string(),
                role: profile.role.to_string(),
                stage: profile.stage.map(str::to_string),
                archetype: card
                    .get("card_archetype")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
            }
        })
        .collect()
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn push_unique(values: &mut Vec<String>, value: &str) {
    if !values.iter().any(|item| item == value) {
        values.push(value.to_string());
    }
}
