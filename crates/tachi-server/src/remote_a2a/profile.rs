//! Closed business input for the remote delegation extension. Parsing never
//! imports a caller's claim as local authority or expands a context reference.

use super::config::identifier;
use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::fmt;

pub(super) const MAX_REQUEST_BYTES: usize = 32 * 1024;
pub(super) const DELEGATION: &str = "urn:tachi:a2a:ext:delegation:v1";
pub(super) const PROVENANCE: &str = "urn:tachi:a2a:ext:provenance-receipt:v1";
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub(super) enum JobKind {
    ReviewRevision,
    VerifyPr,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReviewRevision => "review_revision",
            Self::VerifyPr => "verify_pr",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RpcRequest {
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct SendMessage {
    pub message: Message,
    pub configuration: SendConfiguration,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Message {
    pub message_id: String,
    pub role: String,
    pub parts: Vec<DataPart>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DataPart {
    pub data: RemoteJob,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct SendConfiguration {
    pub return_immediately: bool,
    #[serde(default)]
    pub history_length: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct GetTask {
    pub id: String,
    #[serde(default)]
    pub history_length: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TaskId {
    pub id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct ListTasks {
    #[serde(default = "default_page_size")]
    pub page_size: usize,
    #[serde(default, deserialize_with = "present")]
    pub page_token: Option<String>,
    #[serde(default)]
    pub history_length: u32,
    #[serde(default)]
    pub include_artifacts: bool,
}

fn default_page_size() -> usize {
    20
}

// An omitted optional field and an explicitly null field are different wire
// inputs. This profile accepts only the former, so no authority bytes vanish
// while producing the immutable digest.
fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OriginClaim {
    pub claim_id: String,
    pub transition_version: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StaticContext {
    pub source_ref: String,
    pub source_revision: String,
    pub text: String,
    pub sha256: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RemoteJob {
    pub kind: String,
    pub sender: String,
    pub recipient: String,
    pub trust_domain: String,
    pub idempotency_key: String,
    pub nonce: String,
    pub issued_at: String,
    pub expires_at: String,
    pub repository: String,
    pub head_sha: String,
    pub origin_claim: OriginClaim,
    pub job: JobKind,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub pull_request: Option<u32>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub question: Option<String>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub context: Option<StaticContext>,
}

pub(super) fn opaque_token(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
        && !tachi_runtime_core::secret_scan::contains_secret_like(value)
}

pub(super) fn repository_id(value: &str) -> bool {
    let Some((owner, name)) = value.split_once('/') else {
        return false;
    };
    value.len() <= 256
        && [owner, name].into_iter().all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
        && !tachi_runtime_core::secret_scan::contains_secret_like(value)
}

fn lower_hex(value: &str, lengths: &[usize]) -> bool {
    lengths.contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn safe_text(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    !tachi_runtime_core::secret_scan::contains_secret_like(value)
        && crate::memory_search_ops::scrub_think_tags(value) == value.trim()
        && ![
            "<think",
            "</think",
            "<analysis",
            "</analysis",
            "<|im_start|>",
            "<|im_sep|>",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
}

impl RemoteJob {
    /// Validate immutable content without applying the first-submission clock
    /// check. Storage does that after replay lookup in its admission transaction.
    pub fn validate(&self) -> Result<(), ()> {
        if self.kind != "tachi.remote_job/v1"
            || ![&self.sender, &self.recipient, &self.trust_domain]
                .into_iter()
                .all(|value| identifier(value))
            || !opaque_token(&self.idempotency_key, 128)
            || !opaque_token(&self.nonce, 128)
            || self.idempotency_key == self.nonce
            || !repository_id(&self.repository)
            || !lower_hex(&self.head_sha, &[40, 64])
            || !opaque_token(&self.origin_claim.claim_id, 256)
            || self.origin_claim.transition_version > MAX_SAFE_INTEGER
        {
            return Err(());
        }
        let timestamp = |text: &str| -> Result<jiff::Timestamp, ()> {
            if text.len() > 40 || !text.ends_with('Z') || !text.contains('T') {
                return Err(());
            }
            text.parse().map_err(|_| ())
        };
        let issued = timestamp(&self.issued_at)?;
        let expires = timestamp(&self.expires_at)?;
        if expires <= issued || expires - issued > jiff::SignedDuration::from_mins(15) {
            return Err(());
        }
        match self.job {
            JobKind::VerifyPr => {
                if self.pull_request.is_none_or(|number| number == 0)
                    || self.question.is_some()
                    || self.context.is_some()
                {
                    return Err(());
                }
            }
            JobKind::ReviewRevision => {
                if self.pull_request.is_some()
                    || self
                        .question
                        .as_ref()
                        .is_some_and(|text| text.len() > 4096 || !safe_text(text))
                {
                    return Err(());
                }
            }
        }
        if let Some(context) = &self.context {
            if !opaque_token(&context.source_ref, 256)
                || !opaque_token(&context.source_revision, 256)
                || !lower_hex(&context.sha256, &[64])
                || context.sha256 != format!("{:x}", Sha256::digest(context.text.as_bytes()))
                || !safe_text(&context.text)
                || serde_json::to_vec(context).map_err(|_| ())?.len() > 16 * 1024
            {
                return Err(());
            }
        }
        Ok(())
    }

    pub fn canonical_body(&self) -> Result<String, ()> {
        // This closed type has only fixed ASCII object keys, Unicode strings,
        // and nonnegative integers <= 2^53-1. Sorted-key JSON matches RFC 8785
        // for this domain; it is not a general-purpose JCS implementation.
        let mut value = serde_json::to_value(self).map_err(|_| ())?;
        value.sort_all_objects();
        let body = serde_json::to_string(&value).map_err(|_| ())?;
        if body.len() > MAX_REQUEST_BYTES {
            return Err(());
        }
        Ok(body)
    }
}

/// Preserve duplicate-key rejection before converting to typed objects. A
/// normal serde_json::Value would silently replace an earlier authority field.
struct UniqueValue(Value);

impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct UniqueVisitor;
        impl<'de> Visitor<'de> for UniqueVisitor {
            type Value = UniqueValue;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("JSON with unique object keys")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut object = Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if object.contains_key(&key) {
                        return Err(serde::de::Error::custom("duplicate JSON key"));
                    }
                    object.insert(key, map.next_value::<UniqueValue>()?.0);
                }
                Ok(UniqueValue(Value::Object(object)))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = seq.next_element::<UniqueValue>()? {
                    values.push(value.0);
                }
                Ok(UniqueValue(Value::Array(values)))
            }
            fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::Bool(value)))
            }
            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::Number(value.into())))
            }
            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::Number(value.into())))
            }
            fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Self::Value, E> {
                serde_json::Number::from_f64(value)
                    .map(|number| UniqueValue(Value::Number(number)))
                    .ok_or_else(|| E::custom("invalid number"))
            }
            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::String(value.into())))
            }
            fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::String(value)))
            }
            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::Null))
            }
        }
        deserializer.deserialize_any(UniqueVisitor)
    }
}

pub(super) fn parse_request(bytes: &[u8]) -> Result<RpcRequest, ()> {
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(());
    }
    let unique: UniqueValue = serde_json::from_slice(bytes).map_err(|_| ())?;
    let request: RpcRequest = serde_json::from_value(unique.0).map_err(|_| ())?;
    let valid_id = match &request.id {
        Value::String(value) => opaque_token(value, 128),
        Value::Number(value) => value
            .as_u64()
            .is_some_and(|number| number <= MAX_SAFE_INTEGER),
        _ => false,
    };
    if request.jsonrpc != "2.0" || !valid_id || !opaque_token(&request.method, 64) {
        return Err(());
    }
    Ok(request)
}

pub(super) fn parse_job(bytes: &[u8]) -> Result<RemoteJob, ()> {
    let value = parse_unique(bytes)?;
    let job: RemoteJob = serde_json::from_value(value).map_err(|_| ())?;
    job.validate()?;
    Ok(job)
}

pub(super) fn parse_unique(bytes: &[u8]) -> Result<Value, ()> {
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(());
    }
    serde_json::from_slice::<UniqueValue>(bytes)
        .map(|value| value.0)
        .map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn job() -> Value {
        json!({
            "kind":"tachi.remote_job/v1", "sender":"origin", "recipient":"destination",
            "trust_domain":"owner", "idempotency_key":"request-1", "nonce":"nonce-1",
            "issued_at":"2026-11-22T00:00:00Z", "expires_at":"2026-11-22T00:15:00Z",
            "repository":"owner/repo", "head_sha":"a".repeat(40),
            "origin_claim":{"claim_id":"claim-1","transition_version":2}, "job":"review_revision"
        })
    }

    #[test]
    fn profile_digest_preserves_authority_and_matches_closed_jcs_domain() {
        let mut input = job();
        input["question"] = json!("Unicode 雪😀, slash /, newline\n, formfeed\u{c}, control\u{f}");
        let parsed: RemoteJob = serde_json::from_value(input.clone()).unwrap();
        parsed.validate().unwrap();
        let canonical = parsed.canonical_body().unwrap();
        assert!(canonical.starts_with("{\"expires_at\":"));
        assert!(
            canonical.contains("Unicode 雪😀, slash /, newline\\n, formfeed\\f, control\\u000f")
        );
        let first_digest = Sha256::digest(canonical.as_bytes());
        input["origin_claim"]["transition_version"] = json!(3);
        let changed: RemoteJob = serde_json::from_value(input).unwrap();
        assert_ne!(
            first_digest,
            Sha256::digest(changed.canonical_body().unwrap().as_bytes())
        );
    }

    #[test]
    fn profile_rejects_duplicate_authority_fields_and_float_revisions() {
        assert!(parse_request(br#"{"jsonrpc":"2.0","id":1,"method":"SendMessage","params":{"data":{"sender":"a","sender":"b"}}}"#).is_err());
        let mut input = job();
        input["origin_claim"]["transition_version"] = json!(1.0);
        assert!(serde_json::from_value::<RemoteJob>(input).is_err());
    }

    #[test]
    fn dgx_job_cannot_carry_memory_or_worker_selected_commands() {
        let mut input = job();
        input["job"] = json!("verify_pr");
        input["pull_request"] = json!(12);
        serde_json::from_value::<RemoteJob>(input.clone())
            .unwrap()
            .validate()
            .unwrap();
        input["context"] = Value::Null;
        assert!(serde_json::from_value::<RemoteJob>(input.clone()).is_err());
        input.as_object_mut().unwrap().remove("context");
        input["question"] = json!("");
        assert!(serde_json::from_value::<RemoteJob>(input.clone())
            .unwrap()
            .validate()
            .is_err());
        input.as_object_mut().unwrap().remove("question");
        input["command"] = json!("whoami");
        assert!(serde_json::from_value::<RemoteJob>(input).is_err());
    }

    #[test]
    fn review_context_is_bounded_digest_bound_and_secret_negative() {
        let mut input = job();
        let text = "Selected public design notes.";
        input["context"] = json!({"source_ref":"note:design", "source_revision":"v2",
            "text":text, "sha256":format!("{:x}",Sha256::digest(text.as_bytes()))});
        serde_json::from_value::<RemoteJob>(input.clone())
            .unwrap()
            .validate()
            .unwrap();
        input["context"]["text"] = json!("Changed input");
        assert!(serde_json::from_value::<RemoteJob>(input)
            .unwrap()
            .validate()
            .is_err());
        for unsafe_text in [
            "Authorization: Bearer hidden-value",
            "<think>private reasoning</think>",
        ] {
            let mut input = job();
            input["question"] = json!(unsafe_text);
            assert!(serde_json::from_value::<RemoteJob>(input)
                .unwrap()
                .validate()
                .is_err());
        }
    }
}
