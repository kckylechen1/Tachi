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
//! # Why the parsing is `url::Url`
//!
//! The request path parses with this crate (reqwest builds on it), so the
//! gate sees the URL the provider receives. A hand scan drifted from it in
//! the direction that matters: the parser removes tabs and newlines before it
//! reads a URL, so `?api_<TAB>key=` reached a provider as `api_key` and
//! passed the gate. A string that is not an absolute URL is still scanned by
//! hand: nothing sends it, but the catalog may store it.

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
    match url::Url::parse(endpoint) {
        Ok(url) => {
            if !url.username().is_empty() || url.password().is_some() {
                return Some(EndpointCredentialLeak::Userinfo);
            }
            url.query_pairs()
                .find_map(|(key, _)| credential_shaped_key(key.as_ref()))
        }
        Err(_) => scan_by_hand(endpoint),
    }
}

fn credential_shaped_key(key: &str) -> Option<EndpointCredentialLeak> {
    CREDENTIAL_SHAPED_QUERY_KEYS
        .iter()
        .any(|needle| key.eq_ignore_ascii_case(needle))
        .then(|| EndpointCredentialLeak::QueryKey {
            key: key.to_string(),
        })
}

/// The same rule for a string `url` refuses: not an absolute URL, or not a
/// URL at all.
fn scan_by_hand(endpoint: &str) -> Option<EndpointCredentialLeak> {
    if endpoint_authority(endpoint).contains('@') {
        return Some(EndpointCredentialLeak::Userinfo);
    }
    query_keys(endpoint)
        .iter()
        .find_map(|key| credential_shaped_key(key))
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
    fn a_tab_or_newline_inside_a_key_does_not_slip_past_the_gate() {
        // The parser the request uses removes tabs and newlines before it
        // reads the URL, so both of these reach the provider as `api_key`.
        for separator in ["\t", "\n", "\r"] {
            let endpoint =
                format!("https://api.example.invalid/v1?api_{separator}key=sk-live-secret");
            assert_eq!(
                endpoint_credential_leak(&endpoint),
                Some(EndpointCredentialLeak::QueryKey {
                    key: "api_key".to_string()
                }),
                "{endpoint:?}"
            );
        }
    }

    #[test]
    fn a_backslash_ends_the_authority_the_way_the_request_path_reads_it() {
        // Host `api.example.invalid`, path `/@x/v1`: there is no userinfo.
        assert_eq!(
            endpoint_credential_leak("https://api.example.invalid\\@x/v1"),
            None
        );
    }

    #[test]
    fn a_string_that_is_not_an_absolute_url_is_still_scanned_by_hand() {
        assert_eq!(
            endpoint_credential_leak("api.example.invalid/v1?api_key=sk-live-secret"),
            Some(EndpointCredentialLeak::QueryKey {
                key: "api_key".to_string()
            })
        );
        assert_eq!(
            endpoint_credential_leak("https://user:pass@api.example.invalid:notaport/v1"),
            Some(EndpointCredentialLeak::Userinfo)
        );
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
