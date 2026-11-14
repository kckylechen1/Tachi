//! Canonical JSON for hashing already-applied route-policy state.

use serde_json::Value;
use std::collections::BTreeMap;

/// Sort object keys recursively while preserving array order. Shared with the
/// server's route-rule revisions and dispatch contract content digests.
pub fn canonical_json(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut sorted = BTreeMap::new();
            for (key, value) in map {
                sorted.insert(key.clone(), canonical_json(value));
            }
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical_json).collect()),
        scalar => scalar.clone(),
    }
}
