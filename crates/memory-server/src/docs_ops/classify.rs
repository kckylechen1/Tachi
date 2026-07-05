use crate::server_state::MemoryServer;
#[cfg(not(test))]
use serde_json::Value;
use std::path::Path;

/// LLM 辅助分类与元数据提取，支持在测试模式或 LLM 异常时的启发式 fallback
pub(super) async fn classify_and_extract_metadata(
    server: &MemoryServer,
    source_path: &str,
    content: &str,
) -> (String, String, String) {
    #[cfg(test)]
    {
        let _ = server;
        get_test_fallback_metadata(source_path, content)
    }

    #[cfg(not(test))]
    {
        let system_prompt = "You are an expert software engineer organizing a project wiki/documentation. \
Analyze the provided document source path and content. Determine the most appropriate category path, a concise title, and a brief summary (under 100 characters). \
\
Available standard categories:\
1. docs/engineering/architecture (for specs, system design, architectural decisions)\
2. docs/engineering/devops (for setups, deployments, SOPs, CI/CD)\
3. docs/engineering/code-review (for style guides, API contracts, ADRs)\
4. docs/engineering/debugging (for troubleshooting, incident reports, post-mortems)\
5. docs/product/<product_name> (PRDs, roadmaps, features. Replace <product_name> with actual name in lowercase, e.g. docs/product/hyperion)\
6. docs/agent/<agent_name> (Agent identity, profiles, handoff configs. Replace <agent_name> with actual name in lowercase, e.g. docs/agent/antigravity)\
\
Respond ONLY with a JSON object. No markdown wrapping except the raw JSON content:\
{\
  \"category_path\": \"docs/engineering/architecture\",\
  \"title\": \"Document Title\",\
  \"summary\": \"Short 1-sentence summary\"\
}";
        let user_prompt = format!(
            "Source Path: {}\n\nContent (preview):\n{}",
            source_path,
            content.chars().take(4000).collect::<String>()
        );

        match server
            .llm
            .call_extract_llm(system_prompt, &user_prompt, None, 0.2, 500)
            .await
        {
            Ok(resp) => {
                if let Ok(json_str) = tachi_llm::LlmClient::extract_json_payload(&resp) {
                    if let Ok(val) = serde_json::from_str::<Value>(json_str) {
                        let category_path = val
                            .get("category_path")
                            .and_then(|v| v.as_str())
                            .unwrap_or("docs/engineering/architecture")
                            .to_string();
                        let title = val
                            .get("title")
                            .and_then(|v| v.as_str())
                            .unwrap_or("Untitled Document")
                            .to_string();
                        let summary = val
                            .get("summary")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        return (category_path, title, summary);
                    }
                }
                get_test_fallback_metadata(source_path, content)
            }
            Err(e) => {
                tracing::warn!(
                    "[wiki_organize] LLM classification error: {}; falling back",
                    e
                );
                get_test_fallback_metadata(source_path, content)
            }
        }
    }
}

fn get_test_fallback_metadata(source_path: &str, content: &str) -> (String, String, String) {
    let path = Path::new(source_path);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("untitled");
    let title = stem.replace('-', " ").replace('_', " ");

    // 粗略 summary 提取
    let first_line = content
        .lines()
        .find(|line| !line.trim().is_empty() && !line.starts_with("---"))
        .unwrap_or("")
        .trim_start_matches(|c| c == '#' || c == ' ')
        .trim();
    let summary = if first_line.is_empty() {
        content.chars().take(80).collect::<String>()
    } else {
        first_line.chars().take(80).collect::<String>()
    };

    let stem_lower = stem.to_lowercase();
    let category_path = if stem_lower.contains("agent") {
        "docs/agent/test_agent".to_string()
    } else if stem_lower.contains("prd") || stem_lower.contains("product") {
        "docs/product/test_product".to_string()
    } else if stem_lower.contains("deploy")
        || stem_lower.contains("devops")
        || stem_lower.contains("setup")
    {
        "docs/engineering/devops".to_string()
    } else if stem_lower.contains("review")
        || stem_lower.contains("contract")
        || stem_lower.contains("adr")
    {
        "docs/engineering/code-review".to_string()
    } else if stem_lower.contains("debug")
        || stem_lower.contains("troubleshoot")
        || stem_lower.contains("fix")
    {
        "docs/engineering/debugging".to_string()
    } else {
        "docs/engineering/architecture".to_string()
    };

    (category_path, title, summary)
}
