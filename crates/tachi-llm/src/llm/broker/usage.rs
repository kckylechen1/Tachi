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
/// # Two invariants, both enforced in the constructor
///
/// The fields are private, and every way in — constructor or `Deserialize` —
/// goes through the same check, because both invariants are about *authority*
/// and a spend ceiling is the thing that reads them:
///
/// 1. **A token count is never negative.** `-1` is not a small number of
///    tokens, it is a number this process could not read; keeping it and
///    stamping it `provider_authoritative` would put a fiction in a durable
///    receipt and, worse, could offset a real cost downward.
/// 2. **`Unknown` and a number are mutually exclusive.** "Nothing usable was
///    observed" plus `prompt_tokens: 900` is two contradictory claims in one
///    record, and whichever one a later reader trusts, the other was a lie.
///    Constructors resolve it by *demoting* — an observation with no numbers
///    is `Unknown`, whatever tier it was asked for — and the deserialize path
///    refuses it outright rather than guessing which half to keep.
///
/// There is deliberately no invariant tying `total` to `prompt + completion`:
/// several providers bill a total that is not their sum, and deriving one
/// would manufacture authority the provenance tier does not have.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "UsageObservationV1Parts")]
pub struct UsageObservationV1 {
    /// Prompt-side tokens, when observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompt_tokens: Option<i64>,
    /// Completion-side tokens, when observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    completion_tokens: Option<i64>,
    /// Total tokens, when observed. Deliberately not derived from the other
    /// two: several providers bill a total that is not their sum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    total_tokens: Option<i64>,
    /// Where these numbers came from.
    provenance: UsageProvenanceV1,
    /// Opaque #1681 pricing-snapshot reference the numbers may later be priced
    /// against. Never interpreted in this slice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pricing_snapshot_ref: Option<String>,
}

/// The deserialization shadow of [`UsageObservationV1`]. One type for both
/// paths so a stored record cannot carry a shape the constructor refuses.
#[derive(Debug, Clone, Deserialize)]
pub struct UsageObservationV1Parts {
    /// Prompt-side tokens, when observed.
    #[serde(default)]
    pub prompt_tokens: Option<i64>,
    /// Completion-side tokens, when observed.
    #[serde(default)]
    pub completion_tokens: Option<i64>,
    /// Total tokens, when observed.
    #[serde(default)]
    pub total_tokens: Option<i64>,
    /// Where these numbers came from.
    pub provenance: UsageProvenanceV1,
    /// Opaque #1681 pricing-snapshot reference.
    #[serde(default)]
    pub pricing_snapshot_ref: Option<String>,
}

/// Why a usage observation was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageError {
    /// A token count was negative — unreadable, not small.
    NegativeTokenCount {
        /// Which field carried it.
        field: &'static str,
    },
    /// `Unknown` provenance arrived alongside a number.
    UnknownProvenanceWithNumbers,
    /// A number-bearing tier arrived with no numbers at all.
    NumberlessObservationClaimsAuthority,
}

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NegativeTokenCount { field } => {
                write!(f, "{field} was negative; a token count cannot be")
            }
            Self::UnknownProvenanceWithNumbers => f.write_str(
                "an unknown observation carried numbers; unknown means nothing was observed",
            ),
            Self::NumberlessObservationClaimsAuthority => f.write_str(
                "an observation with no numbers claimed a provenance other than unknown",
            ),
        }
    }
}

impl std::error::Error for UsageError {}

impl TryFrom<UsageObservationV1Parts> for UsageObservationV1 {
    type Error = UsageError;

    fn try_from(parts: UsageObservationV1Parts) -> Result<Self, Self::Error> {
        for (field, value) in [
            ("prompt_tokens", parts.prompt_tokens),
            ("completion_tokens", parts.completion_tokens),
            ("total_tokens", parts.total_tokens),
        ] {
            if value.is_some_and(|value| value < 0) {
                return Err(UsageError::NegativeTokenCount { field });
            }
        }
        let has_number = parts.prompt_tokens.is_some()
            || parts.completion_tokens.is_some()
            || parts.total_tokens.is_some();
        match (parts.provenance, has_number) {
            (UsageProvenanceV1::Unknown, true) => Err(UsageError::UnknownProvenanceWithNumbers),
            // Lossless: an unknown observation can still name the pricing
            // snapshot it *would* have been priced against.
            (UsageProvenanceV1::Unknown, false) => Ok(Self {
                pricing_snapshot_ref: parts.pricing_snapshot_ref,
                ..Self::unknown()
            }),
            (_, false) => Err(UsageError::NumberlessObservationClaimsAuthority),
            (provenance, true) => Ok(Self {
                prompt_tokens: parts.prompt_tokens,
                completion_tokens: parts.completion_tokens,
                total_tokens: parts.total_tokens,
                provenance,
                pricing_snapshot_ref: parts.pricing_snapshot_ref,
            }),
        }
    }
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
        prompt_tokens: Option<u64>,
        completion_tokens: Option<u64>,
        total_tokens: Option<u64>,
    ) -> Self {
        Self::observed(
            UsageProvenanceV1::ProviderAuthoritative,
            prompt_tokens,
            completion_tokens,
            total_tokens,
        )
    }

    /// An observation derived from the catalog's tokenizer/pricing metadata.
    pub fn catalog_calculated(
        prompt_tokens: Option<u64>,
        completion_tokens: Option<u64>,
        total_tokens: Option<u64>,
    ) -> Self {
        Self::observed(
            UsageProvenanceV1::CatalogCalculated,
            prompt_tokens,
            completion_tokens,
            total_tokens,
        )
    }

    /// A locally approximated observation.
    pub fn estimated(
        prompt_tokens: Option<u64>,
        completion_tokens: Option<u64>,
        total_tokens: Option<u64>,
    ) -> Self {
        Self::observed(
            UsageProvenanceV1::Estimated,
            prompt_tokens,
            completion_tokens,
            total_tokens,
        )
    }

    /// The one place a numbered observation is built.
    ///
    /// Counts arrive unsigned, so "negative" cannot even be expressed by a
    /// caller; a value above `i64::MAX` is not a token count either, and is
    /// recorded as unobserved rather than wrapped into a negative one. An
    /// observation left with no numbers demotes to [`Self::unknown`] — the
    /// tier it asked for would be authority over nothing.
    fn observed(
        provenance: UsageProvenanceV1,
        prompt_tokens: Option<u64>,
        completion_tokens: Option<u64>,
        total_tokens: Option<u64>,
    ) -> Self {
        let count = |value: Option<u64>| value.and_then(|value| i64::try_from(value).ok());
        let prompt_tokens = count(prompt_tokens);
        let completion_tokens = count(completion_tokens);
        let total_tokens = count(total_tokens);
        if prompt_tokens.is_none() && completion_tokens.is_none() && total_tokens.is_none() {
            return Self::unknown();
        }
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens,
            provenance,
            pricing_snapshot_ref: None,
        }
    }

    /// Attach the #1681 pricing snapshot these numbers may later be priced
    /// against. Never interpreted in this slice.
    pub fn with_pricing_snapshot_ref(mut self, pricing_snapshot_ref: impl Into<String>) -> Self {
        self.pricing_snapshot_ref = Some(pricing_snapshot_ref.into());
        self
    }

    /// Prompt-side tokens, when observed.
    pub fn prompt_tokens(&self) -> Option<i64> {
        self.prompt_tokens
    }

    /// Completion-side tokens, when observed.
    pub fn completion_tokens(&self) -> Option<i64> {
        self.completion_tokens
    }

    /// Total tokens, when observed. Never derived from the other two.
    pub fn total_tokens(&self) -> Option<i64> {
        self.total_tokens
    }

    /// Where these numbers came from.
    pub fn provenance(&self) -> UsageProvenanceV1 {
        self.provenance
    }

    /// The opaque #1681 pricing-snapshot reference, when one was attached.
    pub fn pricing_snapshot_ref(&self) -> Option<&str> {
        self.pricing_snapshot_ref.as_deref()
    }

    /// Whether any number at all was observed.
    pub fn has_any_number(&self) -> bool {
        self.prompt_tokens.is_some()
            || self.completion_tokens.is_some()
            || self.total_tokens.is_some()
    }
}
