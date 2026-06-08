//! Prompt envelope registry (#156) — structured instruction overlays per agent/mode.

use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Debug, Clone)]
pub(crate) struct PromptEnvelope {
    pub id: &'static str,
    pub identity: &'static str,
    pub constraints: &'static str,
    pub output_contract: &'static str,
}

fn registry() -> &'static HashMap<&'static str, PromptEnvelope> {
    static REG: OnceLock<HashMap<&'static str, PromptEnvelope>> = OnceLock::new();
    REG.get_or_init(|| {
        let envelopes = [
            PromptEnvelope {
                id: "deep_autonomous",
                identity:
                    "You are an autonomous implementation agent. Ship complete, verified work.",
                constraints:
                    "No scope creep. Prefer small diffs. Run verification before claiming done.",
                output_contract:
                    "Summarize changes, list commands run, call tachi_task(action=\"complete\") when finished.",
            },
            PromptEnvelope {
                id: "pair_programming",
                identity: "You are a pair-programming agent. Ask when requirements are ambiguous.",
                constraints: "Pause for user steering on architectural forks. Keep todos updated.",
                output_contract: "Short status updates; explicit next step.",
            },
            PromptEnvelope {
                id: "reviewer",
                identity: "You are a code reviewer. Find regressions and missing tests.",
                constraints: "Do not rewrite large sections without justification.",
                output_contract: "Findings by severity with file references.",
            },
            PromptEnvelope {
                id: "fallback_strict",
                identity: "You are a strict execution agent with minimal prose.",
                constraints: "Anti-slop: no filler. One action per step. Verify claims.",
                output_contract: "Bullet list: done / blocked / next.",
            },
        ];
        envelopes.into_iter().map(|e| (e.id, e)).collect()
    })
}

/// Resolve envelope id from agent + dispatch stage.
pub(crate) fn resolve_envelope_id(agent: &str, stage: Option<&str>) -> &'static str {
    let agent = agent.trim().to_ascii_lowercase();
    let stage = stage.unwrap_or("").trim().to_ascii_lowercase();
    match (agent.as_str(), stage.as_str()) {
        (_, "review") => "reviewer",
        ("codex", "execute") | ("codex", "") => "deep_autonomous",
        ("kimi", _) | ("grok", _) => "pair_programming",
        (_, "plan") | (_, "auto") => "pair_programming",
        ("claude", _) => "deep_autonomous",
        _ => "fallback_strict",
    }
}

pub(crate) fn render_envelope_overlay(agent: &str, stage: Option<&str>) -> Option<String> {
    let id = resolve_envelope_id(agent, stage);
    let envelope = registry().get(id)?;
    Some(format!(
        "## Prompt envelope: {id}\n\n### Identity\n{}\n\n### Constraints\n{}\n\n### Output contract\n{}",
        envelope.identity, envelope.constraints, envelope.output_contract
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_stage_selects_reviewer() {
        assert_eq!(resolve_envelope_id("claude", Some("review")), "reviewer");
    }
}
