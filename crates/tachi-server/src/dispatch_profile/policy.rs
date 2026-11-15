//! Content-addressing primitives for route-policy state. The route-policy
//! proposal/review/apply/simulate handlers were retired with `tachi_tune`;
//! these hashes still stamp recommendations, dispatch contracts, and eval
//! projections against already-applied `ROUTE_POLICY_RULE_NS` rows.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tachi_dispatch::policy::canonical_json;

/// SHA-256 hex of the canonical form of `identity_payload`. `serde_json` does
/// not guarantee key order across rebuilds, so the value is canonicalized
/// first and the hash is stable regardless of how the caller assembled it.
pub(crate) fn content_digest_hex(identity_payload: &Value) -> String {
    let canonical = canonical_json(identity_payload).to_string();
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let bytes = hasher.finalize();
    let mut out = String::with_capacity(2 * bytes.len());
    for byte in bytes {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

/// Deterministic revision of every active route-rule row. It deliberately
/// includes each hard-state version as well as canonical value content: a
/// third party writing the same JSON still advances the revision.
pub(crate) fn route_policy_source_revision(rows: &[memcore::db::StateRow]) -> String {
    let mut snapshot = rows
        .iter()
        .map(|row| {
            let value = serde_json::from_str::<Value>(&row.value_json)
                .map(|value| canonical_json(&value))
                .unwrap_or_else(|_| Value::String(row.value_json.clone()));
            json!({
                "key": row.key,
                "version": row.version,
                "value": value,
            })
        })
        .collect::<Vec<_>>();
    snapshot.sort_by(|left, right| left["key"].as_str().cmp(&right["key"].as_str()));
    content_digest_hex(&Value::Array(snapshot))
}
