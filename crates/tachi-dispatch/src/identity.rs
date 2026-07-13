//! Immutable identity receipts for a single dispatch lifecycle.
//!
//! The receipt is resolved once at the profile boundary and then copied by
//! runtime adapters.  Consumers must not reconstruct it from a mutable profile.

use serde::{Deserialize, Serialize};

pub const DISPATCH_IDENTITY_CONTRACT_ID: &str = "dispatch_identity_receipt/v1";
pub const UNKNOWN_IDENTITY: &str = "unknown";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchIdentityRequest {
    pub profile: Option<String>,
    pub model: Option<String>,
    pub agent: Option<String>,
    pub harness: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchIdentityEffective {
    pub profile: Option<String>,
    pub model: Option<String>,
    pub backend: String,
    pub harness: String,
    pub model_lineage_id: String,
    pub concrete_model_release: String,
    pub provider_model: String,
    pub provider_model_version: String,
    pub role: String,
    pub seat: String,
    pub transport: String,
    pub adapter_version: String,
    pub carrier_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchIdentityObserved {
    /// `acknowledged`, `substituted`, `ignored`, or `unconfirmed`.
    pub acknowledgement: String,
    pub effective: DispatchIdentityEffective,
    pub mismatch: bool,
    pub resolution_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchIdentityReceipt {
    pub contract_id: String,
    pub requested: DispatchIdentityRequest,
    pub planned: DispatchIdentityEffective,
    pub observed: DispatchIdentityObserved,
    pub resolution_reason: String,
}

impl DispatchIdentityReceipt {
    pub fn planned(
        requested: DispatchIdentityRequest,
        planned: DispatchIdentityEffective,
        resolution_reason: String,
    ) -> Self {
        let observed = DispatchIdentityEffective {
            profile: None,
            model: None,
            backend: UNKNOWN_IDENTITY.to_string(),
            harness: UNKNOWN_IDENTITY.to_string(),
            model_lineage_id: UNKNOWN_IDENTITY.to_string(),
            concrete_model_release: UNKNOWN_IDENTITY.to_string(),
            provider_model: UNKNOWN_IDENTITY.to_string(),
            provider_model_version: UNKNOWN_IDENTITY.to_string(),
            role: UNKNOWN_IDENTITY.to_string(),
            seat: UNKNOWN_IDENTITY.to_string(),
            transport: UNKNOWN_IDENTITY.to_string(),
            adapter_version: UNKNOWN_IDENTITY.to_string(),
            carrier_version: UNKNOWN_IDENTITY.to_string(),
        };
        Self {
            contract_id: DISPATCH_IDENTITY_CONTRACT_ID.to_string(),
            requested,
            planned,
            observed: DispatchIdentityObserved {
                acknowledgement: "unconfirmed".to_string(),
                effective: observed,
                mismatch: false,
                resolution_reason: "carrier acknowledgement unavailable".to_string(),
            },
            resolution_reason,
        }
    }

    /// Applies an adapter acknowledgement without ever relabeling a requested
    /// identity as observed. A cross-lineage substitution is rejected unless the
    /// selected profile explicitly permitted it at resolution time.
    pub fn acknowledge(
        &mut self,
        observed: DispatchIdentityEffective,
        acknowledgement: &str,
        resolution_reason: String,
        cross_lineage_authorized: bool,
    ) -> Result<(), String> {
        if observed.model_lineage_id != self.planned.model_lineage_id
            && observed.model_lineage_id != UNKNOWN_IDENTITY
            && !cross_lineage_authorized
        {
            return Err(format!(
                "cross-lineage carrier override '{}' -> '{}' is not authorized by the profile",
                self.planned.model_lineage_id, observed.model_lineage_id
            ));
        }
        let mismatch = observed != self.planned;
        self.observed = DispatchIdentityObserved {
            acknowledgement: acknowledgement.to_string(),
            effective: observed,
            mismatch,
            resolution_reason,
        };
        Ok(())
    }
}

pub fn model_lineage_id(model: Option<&str>, fallback: &str) -> String {
    let Some(model) = model.filter(|value| !value.trim().is_empty()) else {
        return fallback.to_string();
    };
    let release = model.split('@').next().unwrap_or(model);
    let Some((provider, model_name)) = release.split_once('/') else {
        return fallback.to_string();
    };
    let family = model_name.split('-').next().unwrap_or(model_name);
    if provider.is_empty() || family.is_empty() {
        fallback.to_string()
    } else {
        format!("{provider}/{family}")
    }
}

pub fn provider_model_parts(model: Option<&str>) -> (String, String, String) {
    let Some(model) = model.filter(|value| !value.trim().is_empty()) else {
        return (
            UNKNOWN_IDENTITY.to_string(),
            UNKNOWN_IDENTITY.to_string(),
            UNKNOWN_IDENTITY.to_string(),
        );
    };
    let (release, version) = model
        .rsplit_once('@')
        .unwrap_or((model, UNKNOWN_IDENTITY));
    (
        release.to_string(),
        release.to_string(),
        version.to_string(),
    )
}
