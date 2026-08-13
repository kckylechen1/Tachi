//! One rule for "this endpoint URL is smuggling a credential" (tachi#1681
//! PR-D debt (c); the same rule as #1682 slice-1's `EndpointUrl::new`).
//!
//! # Why the rule lives here and not beside its first caller
//!
//! Two independent surfaces refuse a credential-bearing endpoint: the catalog
//! import, which must not write one into a durable operator-visible row, and
//! the request path, which must not send one. They were written in different
//! leaves and had drifted apart in exactly the way that matters — the catalog
//! side checked userinfo only, so `?api_key=sk-live-…` walked straight into
//! the `endpoint_ref` column, into status output, and into anything that later
//! read the row.
//!
//! A rule that two callers each keep their own copy of is a rule that will be
//! extended on one side only. memcore is the node below both, and the deny
//! list is data, so the list and the parser live here and both sides refuse
//! the same strings.
//!
//! # Refuse, never strip
//!
//! Both callers **refuse** rather than redacting, and that is deliberate on
//! the request side in a way worth restating: quietly removing the credential
//! would send an unauthenticated request to an endpoint whose operator plainly
//! expected one, turning a configuration mistake into a confusing 401. On the
//! catalog side, an earlier revision scrubbed userinfo out of the derived
//! account handle and left it in `endpoint_ref` — proof that partial
//! redaction is the shape that fails.
//!
//! # Why the parsing is by hand
//!
//! memcore is a storage leaf and resolves no URL crate; adding one for a
//! substring scan would be a dependency for a parser we can state in twenty
//! lines. The scan is deliberately narrow — the authority for userinfo, the
//! query string for credential-shaped keys — and it percent-decodes keys
//! before comparing, because `?%61pi_key=` is the same key spelled to slip
//! past a literal match.

/// Query keys whose presence means "a credential is being smuggled in the
/// query string", matched case-insensitively and against the **whole** key
/// (never a substring, so `api-version` never collides with `key`).
///
/// Frozen in one place so #1682's request-path check and this crate's catalog
/// check cannot diverge. Adding an entry tightens both at once, which is the
/// entire point.
///
/// That is the intended end state and not yet the shipped one: #1682 slice-1
/// still carries its own copy of this list on its branch. The crate-root doc
/// on `memcore::catalog` states which merge owes the deletion; until then,
/// anyone adding an entry here adds it there too.
pub const CREDENTIAL_SHAPED_QUERY_KEYS: &[&str] = &[
    "api_key",
    "apikey",
    "key",
    "token",
    "secret",
    "access_token",
    "bearer",
    "authorization",
];

/// How an endpoint URL is carrying a credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointCredentialLeak {
    /// `https://user:password@host/…`.
    Userinfo,
    /// `…?api_key=…`. Carries the offending **key**, never the value: this
    /// verdict is reported on status surfaces and in logs, which is precisely
    /// the audience the value must never reach.
    QueryKey { key: String },
}

impl std::fmt::Display for EndpointCredentialLeak {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Userinfo => f.write_str("the URL authority carries userinfo credentials"),
            Self::QueryKey { key } => write!(
                f,
                "the URL query string carries a credential-shaped key '{key}'"
            ),
        }
    }
}

/// Whether this endpoint smuggles a credential, and how.
///
/// `None` is the ordinary case, including a URL with a perfectly normal query
/// string: this is a credential-key rule, not a ban on query strings.
pub fn endpoint_credential_leak(endpoint: &str) -> Option<EndpointCredentialLeak> {
    if endpoint_authority(endpoint).contains('@') {
        return Some(EndpointCredentialLeak::Userinfo);
    }
    for key in query_keys(endpoint) {
        if CREDENTIAL_SHAPED_QUERY_KEYS
            .iter()
            .any(|needle| key.eq_ignore_ascii_case(needle))
        {
            return Some(EndpointCredentialLeak::QueryKey { key });
        }
    }
    None
}

/// The authority substring: scheme off, and the first `/`, `?` or `#` ends it.
///
/// Exposed so a caller deriving an account handle from the host parses the
/// authority the *same* way the userinfo gate does — a gate and a derivation
/// that disagree about which substring is the authority is how a credential
/// ends up on one side of the boundary and not the other. A later `@` in a
/// path or query (`/v1/@scope/model`) is not userinfo and is not refused.
pub fn endpoint_authority(endpoint: &str) -> &str {
    let without_scheme = endpoint
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(endpoint);
    without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(without_scheme)
}

/// Every query key in the URL, percent-decoded.
fn query_keys(endpoint: &str) -> Vec<String> {
    let Some((_, after_question)) = endpoint.split_once('?') else {
        return Vec::new();
    };
    let query = after_question
        .split_once('#')
        .map(|(query, _)| query)
        .unwrap_or(after_question);
    query
        .split('&')
        .map(|pair| pair.split_once('=').map(|(key, _)| key).unwrap_or(pair))
        .filter(|key| !key.is_empty())
        .map(percent_decode_key)
        .collect()
}

/// Percent-decode a query key, ASCII only, and treat `+` as a space the way a
/// form-encoded query does.
///
/// A byte that is not valid UTF-8 after decoding is left as its literal escape
/// rather than replaced: the result is only ever compared against an ASCII
/// deny list, and a lossy replacement could turn two different keys into one.
fn percent_decode_key(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            // Read the two escape digits as bytes, not by slicing the `str`:
            // a `%` followed by the middle of a multi-byte character would
            // panic on a non-boundary slice, and a malformed escape is a thing
            // to pass through, never a thing to crash on.
            b'%' if index + 2 < bytes.len() => {
                match std::str::from_utf8(&bytes[index + 1..index + 3])
                    .ok()
                    .and_then(|digits| u8::from_str_radix(digits, 16).ok())
                {
                    Some(byte) => {
                        out.push(byte);
                        index += 3;
                    }
                    None => {
                        out.push(b'%');
                        index += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn userinfo_in_the_authority_is_a_leak_and_a_later_at_sign_is_not() {
        assert_eq!(
            endpoint_credential_leak("https://user:pass@api.example.invalid/v1"),
            Some(EndpointCredentialLeak::Userinfo)
        );
        assert_eq!(
            endpoint_credential_leak("https://api.example.invalid/v1/@scope/model"),
            None
        );
        assert_eq!(
            endpoint_credential_leak("https://api.example.invalid/v1?model=@latest"),
            None
        );
    }

    #[test]
    fn every_frozen_credential_key_is_refused_case_insensitively() {
        for key in CREDENTIAL_SHAPED_QUERY_KEYS {
            let upper = key.to_uppercase();
            assert_eq!(
                endpoint_credential_leak(&format!(
                    "https://api.example.invalid/v1?{upper}=sk-live-secret"
                )),
                Some(EndpointCredentialLeak::QueryKey { key: upper.clone() }),
                "{key} must be refused however it is cased"
            );
        }
    }

    #[test]
    fn a_percent_encoded_key_does_not_slip_past_the_literal_match() {
        assert_eq!(
            endpoint_credential_leak("https://api.example.invalid/v1?%61pi_key=sk-live-secret"),
            Some(EndpointCredentialLeak::QueryKey {
                key: "api_key".to_string()
            })
        );
    }

    #[test]
    fn an_ordinary_query_key_is_not_a_credential_key() {
        // The rule is about credential keys, not about query strings, and it
        // matches whole keys: `api-version` is not `key`, and `monkey` is not
        // `key` either.
        for endpoint in [
            "https://api.example.invalid/v1?api-version=2026-01-01",
            "https://api.example.invalid/v1?monkey=1&keyboard=2",
            "https://api.example.invalid/v1",
        ] {
            assert_eq!(endpoint_credential_leak(endpoint), None, "{endpoint}");
        }
    }

    #[test]
    fn the_key_is_reported_but_the_value_never_is() {
        let leak = endpoint_credential_leak("https://api.example.invalid/v1?token=sk-live-secret")
            .expect("a credential-shaped key");
        let rendered = leak.to_string();
        assert!(rendered.contains("token"));
        assert!(
            !rendered.contains("sk-live-secret"),
            "the refusal is logged: it may name the key and never the value"
        );
    }
}
