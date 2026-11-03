//! Host-defined, immutable record contracts. Domain names and writer grants
//! belong to the embedding host; neither is inferred from a record request.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{canonical_json_digest_hex, MemoryError, SupersessionReceipt};

pub(crate) const RECORD_KINDS_NAMESPACE: &str = "typed_record_kinds";
pub(crate) const MAX_RECORD_BYTES: usize = 1_048_576;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordKindId {
    pub name: String,
    pub version: u32,
}

impl RecordKindId {
    pub(crate) fn validate(&self) -> Result<(), MemoryError> {
        validate_token(&self.name, "record kind")?;
        if self.version == 0 {
            return Err(invalid("record kind version must be positive"));
        }
        Ok(())
    }

    pub(crate) fn stamp_key(&self) -> String {
        format!("{}@{}", self.name, self.version)
    }
}

/// Both schemas use JSON Schema draft 2020-12. Key schemas must explicitly
/// close the top-level object, so an undeclared identity key always refuses.
/// All kinds in this API are append-only; mutable notes use MemoryEntry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordKind {
    pub id: RecordKindId,
    pub key_schema: Value,
    pub payload_schema: Value,
}

struct CompiledKind {
    definition: RecordKind,
    keys: jsonschema::Validator,
    payload: jsonschema::Validator,
}

/// Compiled once from trusted host configuration, never from tool arguments.
#[derive(Clone)]
pub struct RecordRegistry {
    kinds: Arc<BTreeMap<RecordKindId, CompiledKind>>,
}

impl RecordRegistry {
    pub fn new(kinds: Vec<RecordKind>) -> Result<Self, MemoryError> {
        if kinds.is_empty() || kinds.len() > 128 {
            return Err(invalid("record registry requires between 1 and 128 kinds"));
        }
        let mut compiled = BTreeMap::new();
        for kind in kinds {
            kind.id.validate()?;
            if kind.key_schema.get("type").and_then(Value::as_str) != Some("object")
                || kind.key_schema.get("additionalProperties") != Some(&Value::Bool(false))
                || !kind
                    .key_schema
                    .get("properties")
                    .is_some_and(Value::is_object)
            {
                return Err(invalid(
                    "record key schema must be an explicitly closed object",
                ));
            }
            let entry = CompiledKind {
                keys: compile_schema(&kind.key_schema)?,
                payload: compile_schema(&kind.payload_schema)?,
                definition: kind.clone(),
            };
            if compiled.insert(kind.id, entry).is_some() {
                return Err(invalid("duplicate record kind/version in host registry"));
            }
        }
        Ok(Self {
            kinds: Arc::new(compiled),
        })
    }

    pub fn definitions(&self) -> impl Iterator<Item = &RecordKind> {
        self.kinds.values().map(|kind| &kind.definition)
    }

    pub(crate) fn validate_keys(&self, id: &RecordKindId, keys: &Value) -> Result<(), MemoryError> {
        let kind = self
            .kinds
            .get(id)
            .ok_or_else(|| invalid(format!("unregistered record kind {}", id.stamp_key())))?;
        kind.keys
            .validate(keys)
            .map_err(|error| invalid(format!("invalid record keys: {error}")))
    }

    pub(crate) fn validate_input(&self, input: &TypedRecordInput) -> Result<(), MemoryError> {
        validate_token(&input.id, "record id")?;
        let size = serde_json::to_vec(input)?.len();
        if size > MAX_RECORD_BYTES || input.links.len() > 128 {
            return Err(invalid("typed record exceeds the size or link limit"));
        }
        self.validate_keys(&input.kind, &input.keys)?;
        let kind = self
            .kinds
            .get(&input.kind)
            .expect("key validation resolved kind");
        kind.payload
            .validate(&input.payload)
            .map_err(|error| invalid(format!("invalid record payload: {error}")))?;
        let mut links = BTreeSet::new();
        for link in &input.links {
            if link.relation != link.relation.trim() {
                return Err(invalid(
                    "typed record relations must use canonical ontology tokens",
                ));
            }
            crate::relation_ontology::validate_relation_for_write(&link.relation)?;
            if link.relation == "supersedes" {
                return Err(invalid("supersedes requires a checked predecessor claim"));
            }
            let (kind, keys) = match &link.target {
                TypedRecordTarget::Local { kind, keys, .. }
                | TypedRecordTarget::External { kind, keys } => (kind, keys),
            };
            self.validate_keys(kind, keys)?;
            if !links.insert(canonical_json_digest_hex(&serde_json::to_value(link)?)) {
                return Err(invalid("duplicate typed record link"));
            }
        }
        Ok(())
    }
}

fn compile_schema(schema: &Value) -> Result<jsonschema::Validator, MemoryError> {
    if serde_json::to_vec(schema)?.len() > 65_536 {
        return Err(invalid("record schema exceeds 64 KiB"));
    }
    if let Some(draft) = schema.get("$schema") {
        if draft.as_str() != Some("https://json-schema.org/draft/2020-12/schema") {
            return Err(invalid("record schemas must use JSON Schema draft 2020-12"));
        }
    }
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .offline()
        .should_validate_formats(true)
        .should_ignore_unknown_formats(false)
        .build(schema)
        .map_err(|error| invalid(format!("invalid record schema: {error}")))
}

/// A trusted host's grant for one store role. This type deliberately has no
/// Deserialize implementation. Transport adapters bind it before requests.
#[derive(Debug, Clone)]
pub struct RecordWriter {
    pub(crate) actor: String,
    pub(crate) role: String,
    allowed: BTreeSet<RecordKindId>,
}

impl RecordWriter {
    pub fn from_host_config(
        actor: &str,
        role: &str,
        allowed: Vec<RecordKindId>,
    ) -> Result<Self, MemoryError> {
        validate_token(actor, "record writer")?;
        validate_token(role, "record store role")?;
        if role == crate::path_router::UNKNOWN_DB_LABEL {
            return Err(invalid("record writer requires an explicit store role"));
        }
        for id in &allowed {
            id.validate()?;
        }
        Ok(Self {
            actor: actor.into(),
            role: role.into(),
            allowed: allowed.into_iter().collect(),
        })
    }

    pub(crate) fn require(&self, role: &str, kind: &RecordKindId) -> Result<(), MemoryError> {
        if self.role != role || !self.allowed.contains(kind) {
            return Err(invalid(format!(
                "writer {} is not authorized for {} in role {role}",
                self.actor,
                kind.stamp_key()
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypedRecordInput {
    pub id: String,
    pub kind: RecordKindId,
    pub keys: Value,
    pub payload: Value,
    #[serde(default)]
    pub links: Vec<TypedRecordLink>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypedRecordLink {
    pub relation: String,
    pub target: TypedRecordTarget,
}

/// Local references are resolved and digest-checked in the write transaction.
/// External references assert shape only; the host owns their truth validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum TypedRecordTarget {
    Local {
        record_id: String,
        kind: RecordKindId,
        keys: Value,
        content_sha256: String,
    },
    External {
        kind: RecordKindId,
        keys: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedRecord {
    pub id: String,
    pub kind: RecordKindId,
    pub keys: Value,
    pub payload: Value,
    pub links: Vec<TypedRecordLink>,
    pub revision: i64,
    pub content_sha256: String,
    pub writer: String,
    pub created_at: String,
    /// Resolved from the append-only lineage ledger, outside immutable content.
    pub superseded_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypedRecordWrite {
    pub record: TypedRecord,
    pub prior_idempotent: bool,
    pub receipt: Option<SupersessionReceipt>,
}

pub(crate) fn validate_token(value: &str, field: &str) -> Result<(), MemoryError> {
    if value.is_empty()
        || value.len() > 256
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:/-".contains(&b))
    {
        return Err(invalid(format!(
            "invalid {field}: use 1..256 ASCII identifier characters"
        )));
    }
    Ok(())
}

pub(crate) fn invalid(message: impl Into<String>) -> MemoryError {
    MemoryError::InvalidArg(message.into())
}

#[cfg(test)]
mod tests;
