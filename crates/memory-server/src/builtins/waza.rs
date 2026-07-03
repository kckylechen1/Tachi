use super::helpers::make_skill_capability;
use super::*;

pub(super) fn builtin_waza_skills() -> Result<Vec<HubCapability>, String> {
    const WAZA_SKILLS: &[(&str, &str, &str, &str)] = &[
        (
            "check",
            "Waza review, PR, issue, ship, release, and audit workflow.",
            "skill/waza/skills/check/SKILL.md",
            include_str!("../../../../skill/waza/skills/check/SKILL.md"),
        ),
        (
            "design",
            "Waza production-grade UI and frontend design workflow.",
            "skill/waza/skills/design/SKILL.md",
            include_str!("../../../../skill/waza/skills/design/SKILL.md"),
        ),
        (
            "health",
            "Waza agent/runtime configuration health audit workflow.",
            "skill/waza/skills/health/SKILL.md",
            include_str!("../../../../skill/waza/skills/health/SKILL.md"),
        ),
        (
            "hunt",
            "Waza root-cause debugging and regression diagnosis workflow.",
            "skill/waza/skills/hunt/SKILL.md",
            include_str!("../../../../skill/waza/skills/hunt/SKILL.md"),
        ),
        (
            "learn",
            "Waza research and source synthesis workflow.",
            "skill/waza/skills/learn/SKILL.md",
            include_str!("../../../../skill/waza/skills/learn/SKILL.md"),
        ),
        (
            "read",
            "Waza URL/PDF reading and clean source extraction workflow.",
            "skill/waza/skills/read/SKILL.md",
            include_str!("../../../../skill/waza/skills/read/SKILL.md"),
        ),
        (
            "tachi",
            "Waza Tachi memory and task workflow.",
            "skill/waza/skills/tachi/SKILL.md",
            include_str!("../../../../skill/waza/skills/tachi/SKILL.md"),
        ),
        (
            "think",
            "Waza decision-complete planning and tradeoff workflow.",
            "skill/waza/skills/think/SKILL.md",
            include_str!("../../../../skill/waza/skills/think/SKILL.md"),
        ),
        (
            "write",
            "Waza Chinese and English prose polishing workflow.",
            "skill/waza/skills/write/SKILL.md",
            include_str!("../../../../skill/waza/skills/write/SKILL.md"),
        ),
    ];

    WAZA_SKILLS
        .iter()
        .map(|(name, description, source_path, content)| {
            make_skill_capability(
                &format!("skill:waza-{name}"),
                &format!("waza/{name}"),
                description,
                json!({
                    "execution": "document",
                    "system": "You are applying a Waza workflow skill. Follow the embedded SKILL.md contract and keep the output grounded in the current task evidence.",
                    "prompt": "Apply the Waza skill `waza/{{skill_name}}` to the task below.\n\nTask:\n{{task}}\n\nContext:\n{{context}}\n\nReturn the skill-guided result without restating the full skill document.",
                    "content": content,
                    "policy": { "visibility": "discoverable" },
                    "tags": ["waza", "workflow", "builtin", "skill-set"],
                    "source_path": source_path,
                    "skill_path": format!("/skills/waza/{name}"),
                    "retention_policy": "permanent",
                    "inputSchema": {
                        "type": "object",
                        "required": ["task"],
                        "properties": {
                            "task": {"type": "string"},
                            "context": {"type": "string"},
                            "skill_name": {"type": "string", "default": name}
                        }
                    }
                }),
            )
        })
        .collect()
}
