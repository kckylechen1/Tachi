//! Token usage, with its provenance attached.
//!
//! # Why provenance is part of the type
//!
//! Today `parse_usage_tokens` returns three `Option<i64>`s and nothing records
//! where they came from, so a number the provider reported and a number this
//! process guessed are indistinguishable once stored. That is fine while usage
//! is only a log line and fatal once it is the input to a spend ceiling: an
//! estimate silently promoted to authority is how a budget gets enforced
//! against a fiction.
//!
//! So a usage observation always carries its [`UsageProvenanceV1`], and the
//! four tiers are ordered by how much weight they can bear. Cost is
//! deliberately **not** computed here: pricing is #1681's catalog, and
//! [`UsageObservationV1::pricing_snapshot_ref`] is an opaque forward reference
//! kept so the structure does not have to change when that lands. Computing a
//! cost against a table that does not exist would put a fabricated number in a
//! durable receipt, so this slice abstains.
//!
//! Like the disposition vocabulary, this reaches durable storage through the
//! broker-side wrapper, not by amending `model-invocation-v1` (#1519).

use serde::{Deserialize, Serialize};

/// Where a usage number came from, in descending order of authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UsageProvenanceV1 {
    /// The provider reported it in its own response. The only tier that may
    /// be reconciled against an invoice.
    #[serde(rename = "provider_authoritative")]
    ProviderAuthoritative,
    /// Derived from the catalog's tokenizer/pricing metadata rather than
    /// reported. Good enough to enforce a ceiling, not to bill.
    #[serde(rename = "catalog_calculated")]
    CatalogCalculated,
    /// Approximated locally (character heuristics and the like).
    #[serde(rename = "estimated")]
    Estimated,
    /// Nothing usable was observed. Not zero — *unknown*.
    #[serde(rename = "unknown")]
    Unknown,
}

impl UsageProvenanceV1 {
    /// The frozen wire spelling. Must equal the variant's `serde(rename)`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProviderAuthoritative => "provider_authoritative",
            Self::CatalogCalculated => "catalog_calculated",
            Self::Estimated => "estimated",
            Self::Unknown => "unknown",
        }
    }

    /// Parse a frozen wire spelling; `None` for anything unrecognised.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "provider_authoritative" => Self::ProviderAuthoritative,
            "catalog_calculated" => Self::CatalogCalculated,
            "estimated" => Self::Estimated,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }

    /// Every variant, in declaration order.
    pub const ALL: &'static [UsageProvenanceV1] = &[
        Self::ProviderAuthoritative,
        Self::CatalogCalculated,
        Self::Estimated,
        Self::Unknown,
    ];

    /// Whether a number of this provenance may be reconciled against a
    /// provider invoice. Only the reported tier may.
    pub fn is_billable_authority(self) -> bool {
        matches!(self, Self::ProviderAuthoritative)
    }
}

/// One invocation's token usage.
///
/// Fields are public: there is no cross-field invariant to protect (a
/// provider that reports `prompt` but not `total` is a real and legal
/// observation, and inventing the sum would manufacture authority the
/// provenance tier does not have). What the type *does* enforce, by making
/// [`UsageObservationV1::provenance`] non-optional, is that no usage number
/// exists in this system without a stated source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageObservationV1 {
    /// Prompt-side tokens, when observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<i64>,
    /// Completion-side tokens, when observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<i64>,
    /// Total tokens, when observed. Deliberately not derived from the other
    /// two: several providers bill a total that is not their sum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<i64>,
    /// Where these numbers came from.
    pub provenance: UsageProvenanceV1,
    /// Opaque #1681 pricing-snapshot reference the numbers may later be priced
    /// against. Never interpreted in this slice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing_snapshot_ref: Option<String>,
}

impl UsageObservationV1 {
    /// The "we observed nothing" observation.
    ///
    /// Note it is *not* zeros: zero tokens and unknown tokens are different
    /// claims, and only one of them is honest when the provider sent no usage
    /// block.
    pub fn unknown() -> Self {
        Self {
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            provenance: UsageProvenanceV1::Unknown,
            pricing_snapshot_ref: None,
        }
    }

    /// A provider-reported observation.
    pub fn provider_authoritative(
        prompt_tokens: Option<i64>,
        completion_tokens: Option<i64>,
        total_tokens: Option<i64>,
    ) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens,
            provenance: UsageProvenanceV1::ProviderAuthoritative,
            pricing_snapshot_ref: None,
        }
    }

    /// Whether any number at all was observed.
    pub fn has_any_number(&self) -> bool {
        self.prompt_tokens.is_some()
            || self.completion_tokens.is_some()
            || self.total_tokens.is_some()
    }
}
