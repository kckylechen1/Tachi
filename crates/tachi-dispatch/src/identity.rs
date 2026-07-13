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
    /// Frozen at resolution from the selected profile's declaration; a later
    /// acknowledgement cannot widen it. No caller-supplied flag exists: the
    /// profile is the only authority for a cross-lineage exception.
    #[serde(default)]
    pub cross_lineage_authorized: bool,
}

impl DispatchIdentityReceipt {
    pub fn planned(
        requested: DispatchIdentityRequest,
        planned: DispatchIdentityEffective,
        resolution_reason: String,
        cross_lineage_authorized: bool,
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
            cross_lineage_authorized,
        }
    }

    /// Applies an adapter acknowledgement without ever relabeling a requested
    /// identity as observed. A cross-lineage substitution is rejected unless the
    /// selected profile explicitly permitted it at resolution time — the
    /// authorization was frozen into the receipt then, so no acknowledgement
    /// caller can grant itself the exception.
    pub fn acknowledge(
        &mut self,
        observed: DispatchIdentityEffective,
        acknowledgement: &str,
        resolution_reason: String,
    ) -> Result<(), String> {
        if observed.model_lineage_id != UNKNOWN_IDENTITY
            && !lineages_compatible(&observed.model_lineage_id, &self.planned.model_lineage_id)
            && !self.cross_lineage_authorized
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

    /// The identity attribution must copy — never reconstruct — for outcome
    /// rows and signature evidence:
    ///
    /// - `unconfirmed` (no acknowledgement): the planned identity, which is
    ///   resolution output, not the caller's request; the receipt persisted
    ///   next to the attribution keeps the unconfirmed state explicit.
    /// - `acknowledged`: the planned identity enriched with every field the
    ///   carrier positively reported — a partial acknowledgement must not
    ///   launder known planned identity into `unknown`.
    /// - `substituted` / `ignored`: the observed identity verbatim. What the
    ///   carrier did not report stays `unknown`; back-filling from the plan
    ///   would relabel a planned identity as executed, which the contract
    ///   forbids.
    pub fn attribution_identity(&self) -> DispatchIdentityEffective {
        match self.observed.acknowledgement.as_str() {
            "acknowledged" => merge_known_over(&self.planned, &self.observed.effective),
            "substituted" | "ignored" => self.observed.effective.clone(),
            _ => self.planned.clone(),
        }
    }
}

/// `base` overlaid with every field of `overlay` that carries a known value.
fn merge_known_over(
    base: &DispatchIdentityEffective,
    overlay: &DispatchIdentityEffective,
) -> DispatchIdentityEffective {
    fn pick(base: &str, overlay: &str) -> String {
        if overlay == UNKNOWN_IDENTITY {
            base.to_string()
        } else {
            overlay.to_string()
        }
    }
    DispatchIdentityEffective {
        profile: overlay.profile.clone().or_else(|| base.profile.clone()),
        model: overlay.model.clone().or_else(|| base.model.clone()),
        backend: pick(&base.backend, &overlay.backend),
        harness: pick(&base.harness, &overlay.harness),
        model_lineage_id: pick(&base.model_lineage_id, &overlay.model_lineage_id),
        concrete_model_release: pick(
            &base.concrete_model_release,
            &overlay.concrete_model_release,
        ),
        provider_model: pick(&base.provider_model, &overlay.provider_model),
        provider_model_version: pick(
            &base.provider_model_version,
            &overlay.provider_model_version,
        ),
        role: pick(&base.role, &overlay.role),
        seat: pick(&base.seat, &overlay.seat),
        transport: pick(&base.transport, &overlay.transport),
        adapter_version: pick(&base.adapter_version, &overlay.adapter_version),
        carrier_version: pick(&base.carrier_version, &overlay.carrier_version),
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
    let profile_disagrees = matches!(
        (planned.profile.as_deref(), observed.profile.as_deref()),
        (Some(p), Some(o)) if p != o
    );
    model_disagrees
        || profile_disagrees
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
/// `claude` while `openai/gpt` crosses it. Anything not shaped like a
/// canonical lineage (empty, extra separators, empty segments) never
/// matches: malformed input fails closed.
pub fn lineages_compatible(candidate: &str, planned: &str) -> bool {
    if candidate.is_empty() || planned.is_empty() {
        return false;
    }
    if candidate == planned {
        return true;
    }
    if !planned.contains('/') {
        let mut segments = candidate.split('/');
        return matches!(
            (segments.next(), segments.next(), segments.next()),
            (Some(provider), Some(family), None) if !provider.is_empty() && family == planned
        );
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
