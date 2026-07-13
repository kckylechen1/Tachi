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

/// Carrier acknowledgement state. A closed vocabulary on purpose: an
/// unrecognized token in a persisted receipt fails deserialization, which the
/// loader surfaces as a corrupt (explicitly unattributable) receipt — an
/// unknown acknowledgement can never fall through to planned attribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchAcknowledgement {
    Unconfirmed,
    Acknowledged,
    Substituted,
    Ignored,
}

/// Evidentiary basis of a flat identity attribution (#1065 option D). Frozen
/// beside the attribution columns so every reader can tell planned routing
/// intent from carrier-observed execution fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityAttributionBasis {
    /// Planned identity; no carrier acknowledgement exists.
    PlannedUnconfirmed,
    /// Planned identity overlaid with carrier-reported fields.
    AcknowledgedOverlay,
    /// Carrier-observed identity verbatim (substituted or ignored override).
    Observed,
    /// No receipt existed; attribution reconstructed from profile/agent.
    FallbackUnreceipted,
    /// Explicitly unattributable (e.g. corrupt receipt).
    Unknown,
}

impl IdentityAttributionBasis {
    pub fn as_str(&self) -> &'static str {
        match self {
            IdentityAttributionBasis::PlannedUnconfirmed => "planned_unconfirmed",
            IdentityAttributionBasis::AcknowledgedOverlay => "acknowledged_overlay",
            IdentityAttributionBasis::Observed => "observed",
            IdentityAttributionBasis::FallbackUnreceipted => "fallback_unreceipted",
            IdentityAttributionBasis::Unknown => "unknown",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "planned_unconfirmed" => Some(IdentityAttributionBasis::PlannedUnconfirmed),
            "acknowledged_overlay" => Some(IdentityAttributionBasis::AcknowledgedOverlay),
            "observed" => Some(IdentityAttributionBasis::Observed),
            "fallback_unreceipted" => Some(IdentityAttributionBasis::FallbackUnreceipted),
            "unknown" => Some(IdentityAttributionBasis::Unknown),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchIdentityObserved {
    pub acknowledgement: DispatchAcknowledgement,
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
                acknowledgement: DispatchAcknowledgement::Unconfirmed,
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
        acknowledgement: DispatchAcknowledgement,
        resolution_reason: String,
    ) -> Result<(), String> {
        if acknowledgement == DispatchAcknowledgement::Unconfirmed {
            return Err(
                "'unconfirmed' is the absence of an acknowledgement, not one a carrier can send"
                    .to_string(),
            );
        }
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
            acknowledgement,
            effective: observed,
            mismatch,
            resolution_reason,
        };
        Ok(())
    }

    /// The canonical attribution projection: the identity a consumer must
    /// copy, PAIRED with the evidentiary basis it rests on. Consumers persist
    /// both — an attribution without its basis is how planned routing intent
    /// gets read back as execution fact (#1065 option D).
    ///
    /// - `Unconfirmed`: the planned identity (resolution output, not the
    ///   caller's request) with basis `PlannedUnconfirmed`.
    /// - `Acknowledged`: the planned identity enriched with every field the
    ///   carrier positively reported (a partial acknowledgement must not
    ///   launder known planned identity into `unknown`), basis
    ///   `AcknowledgedOverlay`.
    /// - `Substituted` / `Ignored`: the observed identity verbatim — what the
    ///   carrier did not report stays `unknown`; back-filling from the plan
    ///   would relabel a planned identity as executed. Basis `Observed`.
    pub fn attribution(&self) -> (DispatchIdentityEffective, IdentityAttributionBasis) {
        match self.observed.acknowledgement {
            DispatchAcknowledgement::Acknowledged => (
                merge_known_over(&self.planned, &self.observed.effective),
                IdentityAttributionBasis::AcknowledgedOverlay,
            ),
            DispatchAcknowledgement::Substituted | DispatchAcknowledgement::Ignored => (
                self.observed.effective.clone(),
                IdentityAttributionBasis::Observed,
            ),
            DispatchAcknowledgement::Unconfirmed => (
                self.planned.clone(),
                IdentityAttributionBasis::PlannedUnconfirmed,
            ),
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
/// `claude` while `openai/gpt` crosses it. Shape is validated BEFORE any
/// comparison: two identical malformed values are still malformed, and
/// malformed input always fails closed.
pub fn lineages_compatible(candidate: &str, planned: &str) -> bool {
    if !lineage_is_canonical(candidate) || !lineage_is_canonical(planned) {
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

/// A canonical lineage is either a bare non-empty family or exactly
/// `provider/family` with both segments non-empty.
fn lineage_is_canonical(lineage: &str) -> bool {
    let mut segments = lineage.split('/');
    match (segments.next(), segments.next(), segments.next()) {
        (Some(family), None, None) => !family.is_empty(),
        (Some(provider), Some(family), None) => !provider.is_empty() && !family.is_empty(),
        _ => false,
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
