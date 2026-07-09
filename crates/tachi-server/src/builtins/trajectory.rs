use super::helpers::make_skill_capability;
use super::*;

pub(super) fn builtin_trajectory_distiller() -> Result<HubCapability, String> {
    let content = r#"# trajectory-distiller

Turn a successful execution trajectory into a reusable skill document.

## Inputs
- task_description
- execution_trace
- final_outcome
- agent_id
- skill_path
- domain

## Output contract
Return Markdown with exactly these sections:
1. 适用场景
2. 核心步骤
3. 踩坑记录
4. 验证标准
5. 适用域标签

Keep it reusable, concrete, and procedural. Prefer SOP-style bullets over narrative."#;

    make_skill_capability(
        "skill:trajectory-distiller",
        "trajectory-distiller",
        "Distill execution traces into reusable skill documents.",
        json!({
            "system": "You turn execution trajectories into concise, reusable skill playbooks. Output Markdown only.",
            "prompt": "Distill the following execution trajectory into a reusable skill document.\n\nTask Description:\n{{task_description}}\n\nExecution Trace:\n{{execution_trace}}\n\nFinal Outcome:\n{{final_outcome}}\n\nAgent ID:\n{{agent_id}}\n\nSkill Path:\n{{skill_path}}\n\nDomain:\n{{domain}}\n\nReturn Markdown with these exact top-level sections:\n1. 适用场景\n2. 核心步骤\n3. 踩坑记录\n4. 验证标准\n5. 适用域标签",
            "content": content,
            "policy": { "visibility": "discoverable" },
            "tags": ["distillation", "trajectory", "builtin"],
            "retention_policy": "permanent",
            "skill_path": "/skills/general/trajectory-distiller",
            "inputSchema": {
                "type": "object",
                "required": ["task_description", "execution_trace", "final_outcome", "skill_path"],
                "properties": {
                    "task_description": {"type": "string"},
                    "execution_trace": {},
                    "final_outcome": {},
                    "agent_id": {"type": "string"},
                    "skill_path": {"type": "string"},
                    "domain": {"type": "string"}
                }
            }
        }),
    )
}
