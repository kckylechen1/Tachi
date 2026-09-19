use crate::server_state::MemoryServer;
use serde::Deserialize;
use std::path::{Component, Path};

#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone)]
pub(super) struct ClassifiedMetadata {
    pub(super) category_path: String,
    pub(super) title: String,
    pub(super) summary: String,
    pub(super) provenance: ClassificationProvenance,
}

#[derive(Debug, Clone)]
pub(super) enum ClassificationProvenance {
    Model(Box<tachi_llm::PersistedModelInvocationReceiptV1>),
    Heuristic,
}

impl ClassificationProvenance {
    pub(super) fn model_invocation_json(&self) -> Result<Option<String>, String> {
        match self {
            Self::Model(invocation) => serde_json::to_string(invocation)
                .map(Some)
                .map_err(|error| format!("serialize docs classification invocation: {error}")),
            Self::Heuristic => Ok(None),
        }
    }
}

#[derive(Deserialize)]
struct ModelClassification {
    category_path: String,
    title: String,
    summary: String,
}

/// LLM-assisted classification with an explicit provenance type. A model
/// receipt is returned only when one complete, parsed, validated response
/// supplied all three metadata fields; every fallback is typed heuristic.
pub(super) async fn classify_and_extract_metadata(
    server: &MemoryServer,
    source_path: &str,
    content: &str,
) -> ClassifiedMetadata {
    #[cfg(test)]
    if !MODEL_CLASSIFICATION_ENABLED.load(Ordering::SeqCst) {
        return fallback_metadata(source_path, content);
    }

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
        .call_extract_llm_with_receipt(system_prompt, &user_prompt, None, 0.2, 500)
        .await
    {
        Ok(response)
            if response.invocation.completion_status()
                == tachi_llm::CompletionStatusV1::Truncated =>
        {
            tracing::warn!(
                "[wiki_organize] LLM classification was truncated; falling back"
            );
            fallback_metadata(source_path, content)
        }
        Ok(response) => match parse_model_classification(&response.value) {
            Ok((category_path, title, summary)) => ClassifiedMetadata {
                category_path,
                title,
                summary,
                provenance: ClassificationProvenance::Model(Box::new(response.invocation)),
            },
            Err(error) => {
                tracing::warn!(
                    "[wiki_organize] LLM classification rejected: {}; falling back",
                    error
                );
                fallback_metadata(source_path, content)
            }
        },
        Err(error) => {
            tracing::warn!(
                "[wiki_organize] LLM classification error: {}; falling back",
                error
            );
            fallback_metadata(source_path, content)
        }
    }
}

fn parse_model_classification(response: &str) -> Result<(String, String, String), String> {
    let payload = tachi_llm::LlmClient::extract_json_payload(response)
        .map_err(|error| format!("invalid JSON payload: {error}"))?;
    let parsed: ModelClassification = serde_json::from_str(payload)
        .map_err(|error| format!("invalid classification JSON: {error}"))?;
    let category_path = normalize_model_category(&parsed.category_path)?;
    let title = normalize_model_scalar("title", &parsed.title, None)?;
    let summary = normalize_model_scalar("summary", &parsed.summary, Some(100))?;
    Ok((category_path, title, summary))
}

fn normalize_model_category(category: &str) -> Result<String, String> {
    let category = category.trim().trim_end_matches('/');
    let relative = category.strip_prefix("docs/").unwrap_or(category);
    let components = Path::new(relative)
        .components()
        .map(|component| match component {
            Component::Normal(value) => value
                .to_str()
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .ok_or_else(|| "category contains a non-UTF-8 or empty component".to_string()),
            _ => Err("category contains an unsafe path component".to_string()),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let allowed = match components.as_slice() {
        [engineering, leaf]
            if engineering == "engineering"
                && matches!(
                    leaf.as_str(),
                    "architecture" | "devops" | "code-review" | "debugging"
                ) => true,
        [family, name, ..]
            if matches!(family.as_str(), "product" | "agent") && !name.is_empty() => true,
        _ => false,
    };
    if !allowed || relative.contains('\\') {
        return Err("category is outside the supported docs taxonomy".to_string());
    }
    Ok(format!("docs/{}", components.join("/")))
}

fn normalize_model_scalar(
    field: &str,
    value: &str,
    max_chars: Option<usize>,
) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty()
        || value.chars().any(char::is_control)
        || value.contains(['"', '\''])
        || max_chars.is_some_and(|limit| value.chars().count() > limit)
    {
        return Err(format!("{field} is not a safe frontmatter scalar"));
    }
    Ok(value.to_string())
}

fn fallback_metadata(source_path: &str, content: &str) -> ClassifiedMetadata {
    let path = Path::new(source_path);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("untitled");
    let title = stem.replace(['-', '_'], " ");

    let (_, body) = super::frontmatter::parse_frontmatter(content);
    let first_line = body
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim_start_matches(['#', ' '])
        .trim();
    let summary = if first_line.is_empty() {
        body.chars().take(80).collect::<String>()
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

    ClassifiedMetadata {
        category_path,
        title,
        summary,
        provenance: ClassificationProvenance::Heuristic,
    }
}

#[cfg(test)]
static MODEL_CLASSIFICATION_ENABLED: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
pub(crate) struct ModelClassificationTestGuard;

#[cfg(test)]
impl Drop for ModelClassificationTestGuard {
    fn drop(&mut self) {
        MODEL_CLASSIFICATION_ENABLED.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
pub(crate) fn enable_model_classification_for_test() -> ModelClassificationTestGuard {
    assert!(
        !MODEL_CLASSIFICATION_ENABLED.swap(true, Ordering::SeqCst),
        "model classification test hook already enabled"
    );
    ModelClassificationTestGuard
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_metadata_validation_distinguishes_safe_and_unsafe_outputs() {
        let safe = r#"{"category_path":" docs/product/hyperion/decisions/ ","title":"Hyperion Design","summary":"A bounded summary"}"#;
        assert_eq!(
            parse_model_classification(safe).unwrap(),
            (
                "docs/product/hyperion/decisions".to_string(),
                "Hyperion Design".to_string(),
                "A bounded summary".to_string(),
            )
        );

        for unsafe_response in [
            r#"{"category_path":"docs/../outside","title":"Safe","summary":"Safe"}"#,
            r#"{"category_path":"docs/archive","title":"Safe","summary":"Safe"}"#,
            r#"{"category_path":"docs/engineering/devops","title":"line\nbreak","summary":"Safe"}"#,
            r#"{"category_path":"docs/engineering/devops","title":"Safe"}"#,
        ] {
            assert!(
                parse_model_classification(unsafe_response).is_err(),
                "unsafe or incomplete response must not be model-classified: {unsafe_response}"
            );
        }
    }
}
