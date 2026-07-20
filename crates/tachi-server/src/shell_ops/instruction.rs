use super::*;

// ─── instruction.md generation ───────────────────────────────────────────────

pub(super) fn build_instruction_md(
    flow_id: &str,
    stage: &str,
    task: &str,
    injection: &InjectionResult,
    notes: Option<&str>,
    validation: &[String],
    allowed_scope: &[String],
) -> String {
    let mut s = String::new();
    s.push_str(&format!("# Tachi Flow Instruction — {}\n\n", flow_id));
    s.push_str(&format!("Stage: **{}**\n\n", stage));
    s.push_str("## Task\n\n");
    s.push_str(task.trim());
    s.push_str("\n\n");

    let lifecycle_skills = crate::skill_policy::shell_stage_skills(stage);
    s.push_str("## Native Lifecycle Policy\n\n");
    s.push_str(
        "- Treat Superpowers and Waza as native workflow gates, not optional Hub suggestions.\n",
    );
    s.push_str("- Leader owns mode choice, worker slicing, integration, final verification, and the user-facing completion claim.\n");
    s.push_str("- Use native child agents or external lanes only for bounded, independent, verifiable work; max 6 concurrent workers.\n");
    s.push_str("- Give every worker goal, allowed scope, forbidden scope when relevant, required skills, validation commands, and report-back contract.\n");
    s.push_str("- Keep dependent work serial: plan-before-code, same-file edits, chained transforms, and reviewer gates.\n");
    s.push_str("- Inject MCP/tool permissions by worker role/profile; fail or report impact when required MCP access is missing.\n");
    s.push_str("- Workers must report back; child output is draft evidence until the leader reviews and verifies it.\n\n");
    if lifecycle_skills.is_empty() {
        s.push_str("Required leader skills: (none)\n\n");
    } else {
        s.push_str("Required leader skills:\n");
        for skill in &lifecycle_skills {
            s.push_str(&format!("- `{}`\n", skill));
        }
        s.push('\n');
    }

    s.push_str("## Required Reading (Injected SOP)\n\n");
    if let Some(p) = injection.injected_path.as_deref() {
        s.push_str(&format!("- `{}`", p));
        if let Some(rel) = injection.rel_path.as_deref() {
            s.push_str(&format!(" (source: `{}`)", rel));
        }
        if let Some(hash) = injection.content_hash.as_deref() {
            s.push_str(&format!(" fingerprint={}", &hash[..16]));
        }
        s.push('\n');
    } else if injection.required {
        s.push_str(&format!(
            "- WARNING: required meta skill `{}` could not be loaded: {}\n",
            injection.rel_path.as_deref().unwrap_or("?"),
            injection.warning.as_deref().unwrap_or("unknown error")
        ));
    } else {
        s.push_str("- (no meta skill required for this stage)\n");
    }
    s.push('\n');

    s.push_str("## Allowed Scope\n\n");
    if allowed_scope.is_empty() {
        s.push_str("- (unspecified — keep changes minimal and reversible)\n");
    } else {
        for a in allowed_scope {
            s.push_str(&format!("- {}\n", a));
        }
    }
    s.push('\n');

    s.push_str("## Validation Commands\n\n");
    if validation.is_empty() {
        s.push_str("- `cargo test -p tachi-server`\n");
    } else {
        for v in validation {
            s.push_str(&format!("- `{}`\n", v));
        }
    }
    s.push('\n');

    s.push_str("## Expected Outputs\n\n");
    s.push_str("Write all artifacts under the run directory:\n\n");
    s.push_str("- `result.md` — what was done, decisions taken, blockers\n");
    s.push_str("- `validation.md` — verification evidence (logs, test summaries)\n");
    s.push_str("- `status.json` — updated status (handled by tachi_shell when stage advances)\n");
    s.push_str("- `events.jsonl` — append-only event log\n");
    s.push_str("- `artifacts/` — anything else (diffs, gitleaks output, PR body, etc.)\n\n");

    if let Some(n) = notes {
        s.push_str("## Notes\n\n");
        s.push_str(n.trim());
        s.push_str("\n\n");
    }

    s
}
