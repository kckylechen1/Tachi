use tachi_foundry::{build_fallback_user_payload, CandidateGroup, GroupPayload};
use tachi_llm::LlmClient;

pub(crate) use tachi_foundry::{
    build_batch_prompt, build_batch_user_payload, DISTILL_DAILY_SYSTEM_PROMPT,
};

pub(crate) async fn fallback_distill(
    llm: &LlmClient,
    group: &CandidateGroup,
) -> Result<GroupPayload, String> {
    let user = build_fallback_user_payload(group);
    let text = llm
        .call_distill_llm(
            tachi_foundry::DISTILL_DAILY_SYSTEM_PROMPT_SINGLE,
            &user,
            None,
            0.4,
            600,
        )
        .await?;
    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        return Err("fallback llm returned empty text".to_string());
    }
    let summary: String = trimmed.chars().take(120).collect();
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
