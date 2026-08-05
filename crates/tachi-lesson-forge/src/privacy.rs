//! Public-repository privacy screen for raw pilot sources.
//!
//! Raw text reaches this module only in memory, solely to decide whether a
//! bound row is eligible.  Errors intentionally name a category, never a
//! matching fragment, so diagnostics and reports cannot become a disclosure
//! channel.

use std::sync::OnceLock;

use regex::Regex;

use crate::contains_secret_like;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PilotPrivacyErrorV1 {
    SecretOrCredentialLike,
    PersonalData,
}

impl std::fmt::Display for PilotPrivacyErrorV1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SecretOrCredentialLike => write!(
                f,
                "pilot source is excluded because it appears to contain secret or credential material"
            ),
            Self::PersonalData => write!(
                f,
                "pilot source is excluded because it appears to contain personal data unsuitable for a public repository"
            ),
        }
    }
}

impl std::error::Error for PilotPrivacyErrorV1 {}

/// Conservative privacy gate for source text before any producer call.  This
/// is intentionally a reject-only classifier: ambiguous material is excluded
/// from the public pilot rather than redacted into an apparently safe row.
pub fn screen_source_for_public_pilot(text: &str) -> Result<(), PilotPrivacyErrorV1> {
    let lower = text.to_ascii_lowercase();
    if contains_secret_like(text) {
        return Err(PilotPrivacyErrorV1::SecretOrCredentialLike);
    }
    if email_pattern().is_match(text)
        || phone_pattern().is_match(text)
        || [
            "social security number",
            "ssn:",
            "date of birth",
            "home address",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
    {
        return Err(PilotPrivacyErrorV1::PersonalData);
    }
    Ok(())
}

/// Manifest reasons and decisions are committed public metadata. They must be
/// screened at freeze/load time, before spend, and may never contain an
/// excerpt label that invites raw source text into the artifact.
pub fn screen_manifest_metadata_for_public_pilot(text: &str) -> Result<(), PilotPrivacyErrorV1> {
    screen_source_for_public_pilot(text)?;
    let lower = text.to_ascii_lowercase();
    if ["source excerpt:", "raw source:", "verbatim source:"]
        .iter()
        .any(|marker| lower.contains(marker))
    {
        return Err(PilotPrivacyErrorV1::PersonalData);
    }
    Ok(())
}

fn email_pattern() -> &'static Regex {
    static EMAIL: OnceLock<Regex> = OnceLock::new();
    EMAIL.get_or_init(|| Regex::new(r"(?i)\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b").unwrap())
}

fn phone_pattern() -> &'static Regex {
    static PHONE: OnceLock<Regex> = OnceLock::new();
    PHONE.get_or_init(|| Regex::new(r"\b(?:\+?\d[ -]?){8,15}\b").unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_public_text_is_eligible() {
        assert!(screen_source_for_public_pilot("verification result: retry completed").is_ok());
    }

    #[test]
    fn credential_like_text_is_excluded_without_echoing_it() {
        assert_eq!(
            screen_source_for_public_pilot("credential-marker: api_key=example").unwrap_err(),
            PilotPrivacyErrorV1::SecretOrCredentialLike
        );
    }

    #[test]
    fn personal_data_is_excluded_without_echoing_it() {
        assert_eq!(
            screen_source_for_public_pilot("contact somebody@example.test for details")
                .unwrap_err(),
            PilotPrivacyErrorV1::PersonalData
        );
    }

    #[test]
    fn common_naked_token_shapes_are_excluded() {
        for token in [
            "sk-abcdefghijklmnopqrstuvwxyz123456",
            "voy-abcdefghijklmnopqrstuvwxyz123456",
            "xoxb-123456789012345678901234",
            "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghij",
            "AKIAIOSFODNN7EXAMPLE",
        ] {
            assert_eq!(
                screen_source_for_public_pilot(token).unwrap_err(),
                PilotPrivacyErrorV1::SecretOrCredentialLike
            );
        }
    }
}
