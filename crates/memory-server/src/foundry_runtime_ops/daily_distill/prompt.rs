use serde_json::{json, Value};

use crate::llm::LlmClient;

use super::config::{scrub_agent_noise, MAX_BATCH_PAYLOAD_CHARS};
use super::types::{CandidateGroup, GroupPayload};

/// System prompt for the batch distill mega-call.
/// Enforces structured synthesis using decision markers for downstream SFT factory use.
pub(crate) const DISTILL_DAILY_SYSTEM_PROMPT: &str = r#"You are Tachi's batch memory distiller. You will receive a list of memory groups; each group contains 3+ related memories from a single coherence bucket (shared topic or entity, scoped to a single path prefix).

For EACH group, write a concise, faithful synthesis that:
- Preserves the most important durable facts (decisions, identifiers, file paths, commands, error signatures).
- Drops chit-chat, redundant restatements, time-sensitive scratch notes, tool JSON blobs, and thinking traces.
- Uses neutral third-person prose. Do NOT invent facts not present in the inputs.
- Stays under ~400 words per group.
- Structures key decisions using these markers when applicable:
  - [核心] for the central conclusion or architectural decision
  - [结论] for a derived insight or final determination
  - [方案] for a chosen implementation approach or solution
  - [重构/优化] for a refactoring or performance improvement decision

Return ONLY a JSON array. No prose before or after. Each element MUST be:
{
  "group_id": "<the group_id you were given>",
  "summary": "<one-line ≤120 chars>",
  "text": "<the full distilled synthesis>",
  "keywords": ["<lower-case tag>", ...]
}

If a group cannot be coherently distilled, return an object with an empty "text" and a "skip_reason" field; that group will be skipped.
"#;

pub(crate) fn build_batch_prompt(groups: &[CandidateGroup]) -> String {
    format!(
        "<system>\n{}\n</system>\n\n{}",
        DISTILL_DAILY_SYSTEM_PROMPT,
        build_batch_user_payload(groups)
    )
}

pub(crate) fn build_batch_user_payload(groups: &[CandidateGroup]) -> String {
    let mut payload = Vec::with_capacity(groups.len());
    for group in groups {
        let entries: Vec<Value> = group
            .entries
            .iter()
            .map(|entry| {
                json!({
                    "id": entry.id,
                    "topic": entry.topic,
                    "path": entry.path,
                    "importance": entry.importance,
                    "summary": entry.summary,
                    "text": scrub_agent_noise(&entry.text).chars().take(800).collect::<String>(),
                    "keywords": entry.keywords,
                    "entities": entry.entities,
                })
            })
            .collect();
        payload.push(json!({
            "group_id": group.group_id,
            "path_prefix": group.path_prefix,
            "coherence_key": group.coherence_key,
            "memories": entries,
        }));
    }
    let groups_json = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "[]".to_string());
    // If payload exceeds token budget, drop groups from the tail to stay within limits.
    let (groups_json, actual_count) = if groups_json.len() > MAX_BATCH_PAYLOAD_CHARS {
        let mut trimmed = payload;
        while trimmed.len() > 1
            && serde_json::to_string_pretty(&trimmed)
                .unwrap_or_default()
                .len()
                > MAX_BATCH_PAYLOAD_CHARS
        {
            trimmed.pop();
        }
        let count = trimmed.len();
        (
            serde_json::to_string_pretty(&trimmed).unwrap_or_else(|_| "[]".to_string()),
            count,
        )
    } else {
        (groups_json, groups.len())
    };
    format!(
        "Here are {} groups to distill:\n\n{}",
        actual_count, groups_json
    )
}

pub(crate) async fn fallback_distill(
    llm: &LlmClient,
    group: &CandidateGroup,
) -> Result<GroupPayload, String> {
    let user = build_fallback_user_payload(group);
    let text = llm
        .call_distill_llm(DISTILL_DAILY_SYSTEM_PROMPT_SINGLE, &user, None, 0.4, 600)
        .await?;
    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        return Err("fallback llm returned empty text".to_string());
    }
    let summary: String = trimmed.chars().take(120).collect();
    // Extract keywords from the distilled text and source group metadata
    let mut keywords: Vec<String> = Vec::new();
    for entry in &group.entries {
        keywords.extend(
            entry
                .keywords
                .iter()
                .filter(|k| !k.trim().is_empty())
                .cloned(),
        );
    }
    keywords.sort();
    keywords.dedup();
    keywords.truncate(12);
    Ok(GroupPayload {
        summary,
        text: trimmed,
        keywords,
        skip_reason: None,
    })
}

/// Same instructions as the batch prompt but for a single group — used as
/// the LLM fallback when Claude CLI fails or returns garbage.
pub(crate) const DISTILL_DAILY_SYSTEM_PROMPT_SINGLE: &str = r#"You are Tachi's memory distiller. Synthesize the provided related memories into a single concise, faithful summary under ~400 words. Drop chit-chat, keep durable facts (decisions, identifiers, file paths, commands, error signatures). Return ONLY the distilled prose, no JSON, no preamble."#;

fn build_fallback_user_payload(group: &CandidateGroup) -> String {
    let mut buf = String::new();
    buf.push_str(&format!(
        "path_prefix: {}\ncoherence_key: {}\n\n",
        group.path_prefix, group.coherence_key
    ));
    for (idx, entry) in group.entries.iter().enumerate() {
        buf.push_str(&format!(
            "[{}] topic={} importance={:.2}\nSummary: {}\nText: {}\n\n",
            idx + 1,
            if entry.topic.is_empty() {
                "unknown"
            } else {
                &entry.topic
            },
            entry.importance,
            entry.summary,
            entry.text.chars().take(600).collect::<String>()
        ));
    }
    buf
}
