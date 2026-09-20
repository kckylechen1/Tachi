use super::classify::{canonical_model_payload, ClassifiedMetadata};
use super::frontmatter::{serialize_representable_frontmatter, Frontmatter};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ExistingModelInvocationReceiptV1 {
    schema: String,
    lane: String,
    engine_kind: String,
    effective_provider: Option<String>,
    effective_model: Option<String>,
    effective_version: Option<String>,
    fallback_chain: Vec<String>,
    degraded: bool,
    completion_status: String,
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
    total_tokens: Option<i64>,
    latency_ms: Option<u64>,
    content_hash: String,
    memory_id: String,
    pub(super) revision: i64,
}

impl ExistingModelInvocationReceiptV1 {
    fn rebound_json(
        &self,
        frontmatter: &Frontmatter,
        object_id: &str,
        revision: i64,
    ) -> Result<String, String> {
        let payload = canonical_payload_from_frontmatter(frontmatter)?;
        let mut rebound = self.clone();
        rebound.content_hash =
            tachi_llm::PersistedModelInvocationReceiptV1::content_hash_for(&payload);
        rebound.memory_id = object_id.to_string();
        rebound.revision = revision;
        serde_json::to_string(&rebound)
            .map_err(|error| format!("serialize rebound docs classification invocation: {error}"))
    }
}

fn canonical_payload_from_frontmatter(frontmatter: &Frontmatter) -> Result<String, String> {
    let category = frontmatter.category.as_deref().ok_or_else(|| {
        "Refusing Wiki organize: protected invariant: bound model receipt has no category"
            .to_string()
    })?;
    let title = frontmatter.title.as_deref().ok_or_else(|| {
        "Refusing Wiki organize: protected invariant: bound model receipt has no title".to_string()
    })?;
    let summary = frontmatter.summary.as_deref().ok_or_else(|| {
        "Refusing Wiki organize: protected invariant: bound model receipt has no summary"
            .to_string()
    })?;
    canonical_model_payload(category, title, summary)
}

pub(super) fn validated_existing_model_receipt(
    frontmatter: Option<&Frontmatter>,
    object_id: &str,
) -> Result<Option<ExistingModelInvocationReceiptV1>, String> {
    let Some(frontmatter) = frontmatter else {
        return Ok(None);
    };
    let Some(receipt_json) = frontmatter.model_invocation_v1.as_deref() else {
        return Ok(None);
    };
    let value: Value = serde_json::from_str(receipt_json).map_err(|error| {
        format!(
            "Refusing Wiki organize: protected invariant: existing model receipt is not valid JSON: {error}"
        )
    })?;
    let object = value.as_object().ok_or_else(|| {
        "Refusing Wiki organize: protected invariant: existing model receipt is not an object"
            .to_string()
    })?;
    for field in [
        "schema",
        "lane",
        "engine_kind",
        "effective_provider",
        "effective_model",
        "effective_version",
        "fallback_chain",
        "degraded",
        "completion_status",
        "prompt_tokens",
        "completion_tokens",
        "total_tokens",
        "latency_ms",
        "content_hash",
        "memory_id",
        "revision",
    ] {
        if !object.contains_key(field) {
            return Err(format!(
                "Refusing Wiki organize: protected invariant: existing model receipt is incomplete (missing {field})"
            ));
        }
    }
    let receipt: ExistingModelInvocationReceiptV1 = serde_json::from_value(value).map_err(|error| {
        format!(
            "Refusing Wiki organize: protected invariant: existing model receipt violates the closed model-invocation-v1 wire contract: {error}"
        )
    })?;
    let nonblank_identity = |identity: &Option<String>| {
        identity
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty() && value.trim() == value)
    };
    if receipt.schema != tachi_llm::MODEL_INVOCATION_SCHEMA_V1
        || receipt.lane != "extract"
        || receipt.engine_kind != "provider_http"
        || !nonblank_identity(&receipt.effective_provider)
        || !nonblank_identity(&receipt.effective_model)
        || receipt
            .effective_version
            .as_deref()
            .is_some_and(|value| value.trim().is_empty() || value.trim() != value)
        || receipt.fallback_chain.len() > 4
        || receipt.fallback_chain.iter().any(|step| {
            !matches!(
                step.as_str(),
                "provider_http_fallback" | "claude_cli_to_provider_http"
            )
        })
        || receipt.completion_status != "complete"
        || [
            receipt.prompt_tokens,
            receipt.completion_tokens,
            receipt.total_tokens,
        ]
        .into_iter()
        .flatten()
        .any(|tokens| tokens < 0)
        || receipt.memory_id != object_id
        || receipt.revision < 1
    {
        return Err(
            "Refusing Wiki organize: protected invariant: existing model receipt provenance or binding identity is invalid"
                .to_string(),
        );
    }
    let payload = canonical_payload_from_frontmatter(frontmatter)?;
    let expected_hash = tachi_llm::PersistedModelInvocationReceiptV1::content_hash_for(&payload);
    if receipt.content_hash != expected_hash {
        return Err(
            "Refusing Wiki organize: protected invariant: existing model receipt content binding does not match its category/title/summary"
                .to_string(),
        );
    }
    Ok(Some(receipt))
}

pub(super) fn next_receipt_revision<'a>(
    receipts: impl IntoIterator<Item = &'a ExistingModelInvocationReceiptV1>,
) -> Result<i64, String> {
    receipts
        .into_iter()
        .map(|receipt| receipt.revision)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| {
            "Refusing Wiki organize: protected invariant: model receipt revision overflow"
                .to_string()
        })
}

pub(super) fn publication_has_model_receipt(
    classification: Option<&ClassifiedMetadata>,
    existing_receipt: Option<&ExistingModelInvocationReceiptV1>,
) -> bool {
    classification
        .map(ClassifiedMetadata::is_model_derived)
        .unwrap_or(existing_receipt.is_some())
}

pub(super) fn render_persisted_document(
    frontmatter: &Frontmatter,
    classification: Option<&ClassifiedMetadata>,
    existing_receipt: Option<&ExistingModelInvocationReceiptV1>,
    body: &str,
    object_id: &str,
    model_revision: Option<i64>,
) -> Result<String, String> {
    let mut frontmatter = frontmatter.clone();
    if let Some(classified) = classification {
        frontmatter.model_invocation_v1 =
            classified.bound_model_invocation_json(object_id, model_revision.unwrap_or(1))?;
    } else if let Some(receipt) = existing_receipt {
        frontmatter.model_invocation_v1 = Some(receipt.rebound_json(
            &frontmatter,
            object_id,
            model_revision.ok_or_else(|| {
                "Refusing Wiki organize: protected invariant: surviving model receipt has no committed revision"
                    .to_string()
            })?,
        )?);
    }
    Ok(format!(
        "{}{}",
        serialize_representable_frontmatter(&frontmatter)?,
        body
    ))
}

pub(super) fn render_preview_document(
    frontmatter: &Frontmatter,
    classification: Option<&ClassifiedMetadata>,
    body: &str,
) -> Result<String, String> {
    let mut frontmatter = frontmatter.clone();
    if let Some(classified) = classification {
        frontmatter.model_invocation_v1 = classified.unbound_model_invocation_json()?;
    }
    Ok(format!(
        "{}{}",
        serialize_representable_frontmatter(&frontmatter)?,
        body
    ))
}
