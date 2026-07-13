//! Versioned, storage-driven Argon2id KDF parameters.
//!
//! Both Sigil/tachi-server and the HyperTachi fork write a `kdf_params` JSON
//! column (`{"m":65536,"t":3,"p":4}`) into `vault_config` at vault-init time
//! — and both deliberately ignore it at unlock, deriving with a
//! compile-time constant instead (tachi#1080 v1 comment). `KdfParams` gives
//! that column a first-class, versioned type: parse it, and refuse
//! (fail-closed) anything outside the known-supported set rather than
//! silently deriving with mismatched parameters — or, worse, letting a
//! decrypt failure caused by parameter drift get misread as "wrong
//! password" by a caller.
//!
//! This module is new, additive API. Nothing in either product calls it
//! yet — wiring a product's actual unlock path to read the stored value and
//! call [`crate::DerivedVaultKey::derive_with_params`] is deferred (see the
//! issue's "separately specced before adoption" list: legacy-vault
//! interpretation, the pinning contract, and the trading startup/health
//! policy all need an owner decision first). Existing `derive()` callers
//! are completely unaffected.

use serde::{Deserialize, Serialize};

/// Argon2id KDF parameters, matching the shape both products already write
/// to `vault_config.kdf_params` (`{"m":<memory_cost_kib>,"t":<time_cost>,"p":<parallelism>}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    #[serde(rename = "m")]
    pub memory_cost_kib: u32,
    #[serde(rename = "t")]
    pub time_cost: u32,
    #[serde(rename = "p")]
    pub parallelism: u32,
}

impl KdfParams {
    /// The only parameter set either product has ever actually written.
    /// Every healthy vault in the field today has exactly this value in its
    /// `kdf_params` column — enforcing against this set does not affect
    /// day-one unlock for a single existing vault.
    pub const PRODUCTION: KdfParams = KdfParams {
        memory_cost_kib: 65_536,
        time_cost: 3,
        parallelism: 4,
    };

    /// The parameter sets `derive_with_params`/`from_stored_json` accept.
    /// Deliberately a single entry today (the only value ever written);
    /// this is the seam a future format revision extends, not a place to
    /// widen speculatively.
    pub fn supported() -> &'static [KdfParams] {
        &[KdfParams::PRODUCTION]
    }

    /// Whether `self` is one of the [`KdfParams::supported`] profiles.
    pub fn is_supported(&self) -> bool {
        Self::supported().contains(self)
    }

    /// Fail-closed validation: `Ok(())` iff `self` is in the
    /// known-supported set, `Err` (naming the found value and the
    /// supported set) otherwise.
    pub fn validate(&self) -> Result<(), KdfParamsError> {
        if self.is_supported() {
            Ok(())
        } else {
            Err(KdfParamsError::Unsupported {
                found: *self,
                supported: Self::supported().to_vec(),
            })
        }
    }

    /// Parse the `vault_config.kdf_params` JSON column and validate it
    /// against the known-supported set. Fail-closed: malformed JSON or an
    /// unrecognized parameter combination is always `Err`, never silently
    /// coerced to a default.
    pub fn from_stored_json(json: &str) -> Result<KdfParams, KdfParamsError> {
        let parsed: KdfParams =
            serde_json::from_str(json).map_err(|source| KdfParamsError::Malformed {
                json: json.to_string(),
                source,
            })?;
        parsed.validate()?;
        Ok(parsed)
    }

    /// Format probe: report whether a stored `kdf_params` blob is
    /// recognized, without deriving a key. Useful for status/health
    /// surfaces that want to flag an unsupported vault format up front.
    pub fn probe_json(json: &str) -> KdfParamsProbe {
        match Self::from_stored_json(json) {
            Ok(params) => KdfParamsProbe::Supported(params),
            Err(err) => KdfParamsProbe::Unsupported(err),
        }
    }
}

/// Result of [`KdfParams::probe_json`].
#[derive(Debug)]
pub enum KdfParamsProbe {
    Supported(KdfParams),
    Unsupported(KdfParamsError),
}

/// Versioned KDF-parameter error. Distinct from the plain-`String` errors
/// the rest of this crate's (unchanged, pre-existing) API returns —
/// callers that want to distinguish "unrecognized vault format" from a
/// generic crypto failure can match on this type instead of string-sniffing
/// an error message.
#[derive(Debug, thiserror::Error)]
pub enum KdfParamsError {
    #[error("malformed KDF parameters JSON {json:?}: {source}")]
    Malformed {
        json: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("unsupported KDF parameters {found:?}; supported: {supported:?}")]
    Unsupported {
        found: KdfParams,
        supported: Vec<KdfParams>,
    },
    #[error("KDF derivation failed: {0}")]
    Derivation(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_default_parses_and_is_supported() {
        let json = r#"{"m":65536,"t":3,"p":4}"#;
        let parsed = KdfParams::from_stored_json(json).expect("production default must parse");
        assert_eq!(parsed, KdfParams::PRODUCTION);
    }

    #[test]
    fn forged_params_are_rejected_fail_closed() {
        let json = r#"{"m":1,"t":9,"p":9}"#;
        let err =
            KdfParams::from_stored_json(json).expect_err("forged params must not be accepted");
        assert!(
            matches!(err, KdfParamsError::Unsupported { .. }),
            "expected Unsupported, got {err:?}"
        );
    }

    #[test]
    fn malformed_json_is_a_distinct_error_from_unsupported_params() {
        let err = KdfParams::from_stored_json("not json")
            .expect_err("malformed JSON must not be accepted");
        assert!(
            matches!(err, KdfParamsError::Malformed { .. }),
            "expected Malformed, got {err:?}"
        );
    }

    #[test]
    fn probe_json_reports_supported_and_unsupported() {
        assert!(matches!(
            KdfParams::probe_json(r#"{"m":65536,"t":3,"p":4}"#),
            KdfParamsProbe::Supported(params) if params == KdfParams::PRODUCTION
        ));
        assert!(matches!(
            KdfParams::probe_json(r#"{"m":1,"t":9,"p":9}"#),
            KdfParamsProbe::Unsupported(KdfParamsError::Unsupported { .. })
        ));
    }
}
