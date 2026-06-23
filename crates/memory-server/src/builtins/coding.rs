use super::helpers::make_skill_capability;
use super::*;

pub(super) fn builtin_coding_skills() -> Result<Vec<HubCapability>, String> {
    Ok(vec![
        make_skill_capability(
            "skill:coding-debug-pattern",
            "coding/debug-pattern",
            "Capture repeatable debug patterns for coding agents.",
            json!({
                "prompt": "Use this coding debug template to capture or apply a debugging pattern. Input:\n{{input}}",
                "content": "# /skills/coding/debug-pattern\n\n## Purpose\nCapture: symptom → likely causes → verification → fix.\n\n## Output\n- Error signature\n- Root cause shortlist\n- Fastest discriminators\n- Confirmed fix\n- Follow-up regression test\n\n## Retention\nEphemeral by default unless promoted into a gotcha or ADR.",
                "policy": { "visibility": "discoverable" },
                "domain": "coding",
                "skill_path": "/skills/coding/debug-pattern",
                "retention_policy": "ephemeral",
                "memory_path_templates": ["/coding/{repo_name}/pattern", "/coding/{repo_name}/debt"],
                "tags": ["coding", "debug", "preset"]
            }),
        )?,
        make_skill_capability(
            "skill:coding-gotcha-capture",
            "coding/gotcha-capture",
            "Promote a recurring coding pitfall into a permanent gotcha note.",
            json!({
                "prompt": "Convert this coding incident into a durable gotcha note. Input:\n{{input}}",
                "content": "# /skills/coding/gotcha-capture\n\n## Purpose\nTurn a one-off failure into a permanent gotcha record others can recall later.\n\n## Capture\n- Triggering condition\n- Misleading signal\n- Correct diagnosis\n- Preventive check\n\n## Storage\nWrite to `/coding/{repo_name}/gotcha`.\nRetention: permanent.",
                "policy": { "visibility": "discoverable" },
                "domain": "coding",
                "skill_path": "/skills/coding/gotcha-capture",
                "retention_policy": "permanent",
                "memory_path_templates": ["/coding/{repo_name}/gotcha"],
                "tags": ["coding", "gotcha", "preset"]
            }),
        )?,
        make_skill_capability(
            "skill:coding-architecture-decision",
            "coding/architecture-decision",
            "ADR template for architecture decisions and rejected options.",
            json!({
                "prompt": "Draft or apply an ADR using this template. Input:\n{{input}}",
                "content": "# /skills/coding/architecture-decision\n\n## Purpose\nRecord why a design was chosen, what alternatives were rejected, and what future signals should trigger re-evaluation.\n\n## Required sections\n- Context\n- Decision\n- Rejected alternatives\n- Validation / rollback\n- Drift signals\n\n## Storage\nWrite to `/coding/{repo_name}/decision`.\nRetention: permanent.",
                "policy": { "visibility": "discoverable" },
                "domain": "coding",
                "skill_path": "/skills/coding/architecture-decision",
                "retention_policy": "permanent",
                "memory_path_templates": ["/coding/{repo_name}/decision"],
                "tags": ["coding", "adr", "preset"]
            }),
        )?,
        make_skill_capability(
            "skill:coding-refactor-checklist",
            "coding/refactor-checklist",
            "Checklist for safe refactors.",
            json!({
                "prompt": "Apply this refactor checklist to the target change. Input:\n{{input}}",
                "content": "# /skills/coding/refactor-checklist\n\nBefore refactoring, confirm:\n1. Existing behavior is covered by regression tests\n2. Downstream dependencies are mapped\n3. Performance baseline is known\n4. Rollback path exists\n5. Diff is staged in reviewable slices",
                "policy": { "visibility": "discoverable" },
                "domain": "coding",
                "skill_path": "/skills/coding/refactor-checklist",
                "retention_policy": "durable",
                "tags": ["coding", "refactor", "preset"]
            }),
        )?,
        make_skill_capability(
            "skill:coding-code-review-lens",
            "coding/code-review-lens",
            "Four-lens code review scoring template.",
            json!({
                "prompt": "Review the target through the security / performance / readability / testability lenses. Input:\n{{input}}",
                "content": "# /skills/coding/code-review-lens\n\nScore the change across:\n- Security\n- Performance\n- Readability\n- Testability\n\nFor each lens, record risks, confidence, and required follow-up.",
                "policy": { "visibility": "hidden" },
                "domain": "coding",
                "skill_path": "/skills/coding/code-review-lens",
                "retention_policy": "durable",
                "tags": ["coding", "review", "preset"]
            }),
        )?,
        make_skill_capability(
            "skill:coding-test-strategy",
            "coding/test-strategy",
            "Testing strategy template by code type.",
            json!({
                "prompt": "Choose a test strategy using this template. Input:\n{{input}}",
                "content": "# /skills/coding/test-strategy\n\nMap the target into one of:\n- Pure function\n- Side-effectful integration\n- Async / concurrent workflow\n\nThen choose the minimum reliable test mix: unit, integration, snapshot, or replay.",
                "policy": { "visibility": "discoverable" },
                "domain": "coding",
                "skill_path": "/skills/coding/test-strategy",
                "retention_policy": "durable",
                "tags": ["coding", "testing", "preset"]
            }),
        )?,
    ])
}
