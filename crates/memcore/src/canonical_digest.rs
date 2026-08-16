//! Canonical-JSON content digest — the memcore-resident half of the
//! content-addressing rule (tachi#1681 D1, cross-vendor review finding 3).
//!
//! `tune_ops::route_policy::content_digest_hex` in `tachi-server` is the same
//! rule, but it is `pub(crate)` there and `tachi-server` sits *above* memcore:
//! a memcore type whose primary key is its own content digest cannot import
//! it without inverting the dependency direction. Finding 3's disposition was
//! explicit — add the small primitive here rather than reach across the crate
//! boundary — so this module is that primitive, with the same two rules the
//! route-policy one has:
//!
//! 1. **Canonicalize before hashing.** `serde_json`'s map ordering is a
//!    feature-flag away from changing, so key order is made a property of the
//!    code ([`canonical_json`]), not of a cargo feature staying off.
//! 2. **SHA-256, lower hex, no prefix.** Callers that want a scheme prefix
//!    (`ps1:`, `pd1:`, …) add their own, so the prefix stays the caller's
//!    versioning knob rather than something baked into the hash.
//!
//! Deliberately **not** admin-gated and deliberately dependency-free beyond
//! `serde_json` + `sha2` (both unconditional in this crate), so the portable
//! kernel can use it too.

use std::collections::BTreeMap;

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// Sort every object key, recursively, leaving arrays in caller order (array
/// order is content, not layout).
pub fn canonical_json(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted = map
                .iter()
                .map(|(key, value)| (key.clone(), canonical_json(value)))
                .collect::<BTreeMap<_, _>>();
            Value::Object(sorted.into_iter().collect::<Map<_, _>>())
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical_json).collect()),
        scalar => scalar.clone(),
    }
}

/// SHA-256 hex of the canonical serialization of `value`.
pub fn canonical_json_digest_hex(value: &Value) -> String {
    let canonical = canonical_json(value).to_string();
    let bytes = Sha256::digest(canonical.as_bytes());
    let mut out = String::with_capacity(2 * bytes.len());
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Whether two JSON values represent the same content under the canonical
/// form the digest uses. The only sanctioned "are these the same JSON" test —
/// `Value == Value` compares map iteration order for equal keys only by luck.
pub fn canonical_json_eq(left: &Value, right: &Value) -> bool {
    canonical_json(left) == canonical_json(right)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn key_order_does_not_change_the_digest() {
        let a = json!({"b": 1, "a": {"d": 4, "c": 3}});
        let b = json!({"a": {"c": 3, "d": 4}, "b": 1});
        assert_eq!(canonical_json_digest_hex(&a), canonical_json_digest_hex(&b));
        assert!(canonical_json_eq(&a, &b));
    }

    #[test]
    fn array_order_does_change_the_digest() {
        let a = json!({"prices": [1, 2]});
        let b = json!({"prices": [2, 1]});
        assert_ne!(canonical_json_digest_hex(&a), canonical_json_digest_hex(&b));
        assert!(!canonical_json_eq(&a, &b));
    }

    #[test]
    fn any_value_change_changes_the_digest() {
        let a = json!({"input_per_mtok": "0.14"});
        let b = json!({"input_per_mtok": "0.15"});
        assert_ne!(canonical_json_digest_hex(&a), canonical_json_digest_hex(&b));
    }

    #[test]
    fn digest_is_lower_hex_sha256_width() {
        let digest = canonical_json_digest_hex(&json!({}));
        assert_eq!(digest.len(), 64);
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(!digest.chars().any(|c| c.is_ascii_uppercase()));
    }
}
