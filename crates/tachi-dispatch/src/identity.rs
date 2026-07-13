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
        if observed.model_lineage_id != UNKNOWN_IDENTITY
            && !lineages_compatible(&observed.model_lineage_id, &self.planned.model_lineage_id)
            && !cross_lineage_authorized
        {
            return Err(format!(
                "cross-lineage carrier override '{}' -> '{}' is not authorized by the profile",
                self.planned.model_lineage_id, observed.model_lineage_id
            ));
        }
        let mismatch = identity_mismatch(&self.planned, &observed);
        self.observed = DispatchIdentityObserved {
            acknowledgement: acknowledgement.to_string(),
            effective: observed,
            mismatch,
            resolution_reason,
        };
        Ok(())
    }

    /// The identity attribution must copy: the carrier-acknowledged effective
    /// identity once an acknowledgement exists, otherwise the planned identity.
    /// After a substitution the planned identity is NOT what executed, so
    /// consumers reading through this accessor can never report a requested or
    /// planned identity as the effective one.
    pub fn attribution_identity(&self) -> &DispatchIdentityEffective {
        match self.observed.acknowledgement.as_str() {
            "acknowledged" | "substituted" | "ignored" => &self.observed.effective,
            _ => &self.planned,
        }
    }
}

/// A field the planner could not know at resolution time is recorded as
/// [`UNKNOWN_IDENTITY`]; absence of information is an explicit unknown state,
/// never evidence of substitution. Only two *known* values can disagree.
fn known_fields_disagree(planned: &str, observed: &str) -> bool {
    planned != UNKNOWN_IDENTITY && observed != UNKNOWN_IDENTITY && planned != observed
}

/// True when an identity-bearing field the planner committed to differs from
/// the observed value. Environment provenance (seat, transport, adapter and
/// carrier versions) is recorded on the receipt but is not identity, so it can
/// never flag a mismatch on its own.
fn identity_mismatch(
    planned: &DispatchIdentityEffective,
    observed: &DispatchIdentityEffective,
) -> bool {
    let model_disagrees = matches!(
        (planned.model.as_deref(), observed.model.as_deref()),
        (Some(p), Some(o)) if p != o
    );
    model_disagrees
        || known_fields_disagree(&planned.backend, &observed.backend)
        || known_fields_disagree(&planned.harness, &observed.harness)
        || known_fields_disagree(&planned.model_lineage_id, &observed.model_lineage_id)
        || known_fields_disagree(
            &planned.concrete_model_release,
            &observed.concrete_model_release,
        )
        || known_fields_disagree(&planned.provider_model, &observed.provider_model)
        || known_fields_disagree(
            &planned.provider_model_version,
            &observed.provider_model_version,
        )
        || known_fields_disagree(&planned.role, &observed.role)
}

/// Whether a requested/observed lineage stays inside a planned lineage.
/// Lineages are canonically `provider/family`. A profile that declares no
/// model resolves its lineage to the bare backend name; that constrains the
/// model *family*, not the provider, so `anthropic/claude` stays inside
/// `claude` while `openai/gpt` crosses it.
pub fn lineages_compatible(candidate: &str, planned: &str) -> bool {
    if candidate == planned {
        return true;
    }
    if !planned.contains('/') {
        return candidate
            .rsplit('/')
            .next()
            .is_some_and(|family| family == planned);
    }
    false
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
    let (release, version) = model.rsplit_once('@').unwrap_or((model, UNKNOWN_IDENTITY));
    let provider_model = release
        .split_once('/')
        .map(|(_, name)| name)
        .unwrap_or(release);
    (
        release.to_string(),
        provider_model.to_string(),
        version.to_string(),
    )
}
