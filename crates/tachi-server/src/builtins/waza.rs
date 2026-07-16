use super::helpers::{make_skill_capability, resolve_skill_content_source, SkillContentSource};
use super::*;
use tachi_dispatch::{
    WAZA_CHECK, WAZA_DESIGN, WAZA_HEALTH, WAZA_HUNT, WAZA_LEARN, WAZA_READ, WAZA_TACHI, WAZA_THINK,
    WAZA_WRITE,
};

pub(super) fn builtin_waza_skills() -> Result<Vec<HubCapability>, String> {
    const WAZA_SKILLS: &[(&str, &str, &str, SkillContentSource)] = &[
        (
            WAZA_CHECK,
            "check",
            "Waza review, PR, issue, ship, release, and audit workflow.",
            SkillContentSource::Vendored("skill/waza/skills/check/SKILL.md"),
        ),
        (
            WAZA_DESIGN,
            "design",
            "Waza production-grade UI and frontend design workflow.",
            SkillContentSource::Vendored("skill/waza/skills/design/SKILL.md"),
        ),
        (
            WAZA_HEALTH,
            "health",
            "Waza agent/runtime configuration health audit workflow.",
            SkillContentSource::Vendored("skill/waza/skills/health/SKILL.md"),
        ),
        (
            WAZA_HUNT,
            "hunt",
            "Waza root-cause debugging and regression diagnosis workflow.",
            SkillContentSource::Vendored("skill/waza/skills/hunt/SKILL.md"),
        ),
        (
            WAZA_LEARN,
            "learn",
            "Waza research and source synthesis workflow.",
            SkillContentSource::Vendored("skill/waza/skills/learn/SKILL.md"),
        ),
        (
            WAZA_READ,
            "read",
            "Waza URL/PDF reading and clean source extraction workflow.",
            SkillContentSource::Vendored("skill/waza/skills/read/SKILL.md"),
        ),
        (
            WAZA_TACHI,
            "tachi",
            "Waza Tachi memory and task workflow.",
            SkillContentSource::Vendored("skill/waza/skills/tachi/SKILL.md"),
        ),
        (
            WAZA_THINK,
            "think",
            "Waza decision-complete planning and tradeoff workflow.",
            SkillContentSource::Vendored("skill/waza/skills/think/SKILL.md"),
        ),
        (
            WAZA_WRITE,
            "write",
            "Waza Chinese and English prose polishing workflow.",
            SkillContentSource::Vendored("skill/waza/skills/write/SKILL.md"),
        ),
    ];

    WAZA_SKILLS
        .iter()
        .map(|(id, name, description, source)| {
            let (content, _resolved_path, content_hash, source_path) =
                resolve_skill_content_source(name, source);
            make_skill_capability(
                id,
                &format!("waza/{name}"),
                description,
                json!({
                    "execution": "document",
                    "system": "You are applying a Waza workflow skill. Follow the embedded SKILL.md contract and keep the output grounded in the current task evidence.",
                    "prompt": "Apply the Waza skill `waza/{{skill_name}}` to the task below.\n\nTask:\n{{task}}\n\nContext:\n{{context}}\n\nReturn the skill-guided result without restating the full skill document.",
                    "content": content,
                    "content_hash": content_hash,
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
