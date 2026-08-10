// vault/fingerprint.rs — keyed, domain-separated credential fingerprints
// (tachi#1680 D2).
//
// # What a fingerprint is for here
//
// A fingerprint's job is to **merge and distinguish evidence at plan time** —
// "these two env-var names hold the same secret, so they are one account with
// two aliases" / "these two names hold different secrets, so they are not" —
// not to be an eternal identity. Identity across key rotation is carried by
// `provider_accounts.account_id` plus the append-only event history, which is
// what makes rotation-stable identity achievable without a rotation-stable
// hash of rotating key material.
//
// # Why keyed, and why the key is never stored
//
// An unkeyed digest of an API key is offline-enumerable whenever the value has
// low entropy or a known prefix structure, so both fingerprint families are
// **keyed** (BLAKE2b in MAC mode — the reason this module needs `blake2`
// rather than the crate's usual SHA-256: `sha2` alone cannot produce a MAC
// without adding `hmac`, so the "one hash family" preference recorded in
// `store::outbox` yields here, and the dependency is `admin`-only so the
// portable kernel never gains it).
//
// The MAC key ([`FingerprintKey`]) is **derived, never persisted**: it comes
// from the unlocked Vault master key through a domain-separated KDF step. A
// stored key would have to live in `vault_entries`, which has no internal/hidden
// flag — every row is exposed by `vault_list_entries` and counted by status —
// so it would surface as an unexplained secret in the operator's own listing.
// Deriving it gives availability exactly when it is needed (fingerprints are
// computed at plan/apply/materialization moments, all of which are already
// unlocked), zero new storage, and zero listing exposure.
//
// A Vault master-key rotation therefore rotates the fingerprint key too, and
// every stored fingerprint must be recomputed under the new key
// (`vault_accounts::record_account_fingerprint` with the `fingerprint_rekeyed`
// event kind). That is a re-fingerprint, not a new identity: `account_id` is
// untouched and the event row records the discontinuity. Locked-state
// consumers (status) read persisted fingerprint strings and never recompute.
//
// # Not normalization
//
// These functions hash exactly the bytes they are handed. Deciding that a
// trailing newline or surrounding whitespace does not change a credential is
// the reconcile pipeline's `normalize` step (#1680 D4), which runs *before*
// fingerprinting — putting it here would silently apply a normalization policy
// to callers that did not ask for one.

use std::fmt::Write as _;

use blake2::digest::{consts::U32, Mac};
use blake2::Blake2bMac;

/// Domain label separating the fingerprint key from every other key derived
/// from the same Vault master key.
const FP_KEY_DOMAIN: &[u8] = b"tachi.fp.v1";

/// Domain label for key fingerprints. Changing it invalidates every stored
/// `fp1:` value, so it is versioned in the label itself.
const KEY_FINGERPRINT_DOMAIN: &[u8] = b"tachi.provider-key.v1";

/// Domain label for account fingerprints. Distinct from
/// [`KEY_FINGERPRINT_DOMAIN`] so a key digest can never collide with — or be
/// replayed as — an account digest.
const ACCOUNT_FINGERPRINT_DOMAIN: &[u8] = b"tachi.provider-account.v1";

/// Scheme prefix of a key fingerprint: `fp1:<hex12>`.
pub const KEY_FINGERPRINT_SCHEME: &str = "fp1";

/// Scheme prefix of an account fingerprint: `fpa1:<class>:<hex12>`.
pub const ACCOUNT_FINGERPRINT_SCHEME: &str = "fpa1";

/// Hex characters kept from the MAC output. 12 hex = 48 bits: enough that an
/// accidental collision inside one install's handful of provider credentials
/// is not a practical concern, short enough to stay readable in a plan diff.
const FINGERPRINT_HEX_LEN: usize = 12;

/// Byte separating variable-length fields inside a MAC input. Every field that
/// precedes it in these constructions is either a fixed constant or a value
/// that cannot contain a NUL (`provider_kind` is a compile-time registry
/// identifier; member fingerprints are `fp1:<hex>` strings), so the encoding
/// is unambiguous.
const FIELD_SEPARATOR: u8 = 0x00;

type FingerprintMac = Blake2bMac<U32>;

/// Evidence strength of an account fingerprint. The class is part of the
/// stored string (`fpa1:<class>:<hex12>`) because a consumer must be able to
/// tell "the provider itself told us this is account X" from "we inferred one
/// account from the key material we can see".
///
/// A name-only grouping is deliberately **not** a class: names are aliases,
/// never evidence of account identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountFingerprintClass {
    /// Provider-attested account evidence (an account/organization id returned
    /// by the provider itself). Reserved: none of the APIs Tachi probes today
    /// returns one, so nothing mints this class yet — but persisted values
    /// must parse the moment a probe starts producing them, which is why the
    /// variant exists ahead of a constructor.
    ProviderAttested,
    /// Keyed digest of the account's current member key fingerprints. This is
    /// what reconcile mints today.
    KeyedMembers,
}

impl AccountFingerprintClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProviderAttested => "pa",
            Self::KeyedMembers => "kv",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pa" => Some(Self::ProviderAttested),
            "kv" => Some(Self::KeyedMembers),
            _ => None,
        }
    }

    /// Strength order: provider attestation beats inference from key material.
    pub fn outranks(self, other: Self) -> bool {
        matches!((self, other), (Self::ProviderAttested, Self::KeyedMembers))
    }
}

/// The class of a stored `fpa1:<class>:<hex12>` string, or `None` if the value
/// is not a well-formed account fingerprint of a class this build knows.
pub fn account_fingerprint_class(fingerprint: &str) -> Option<AccountFingerprintClass> {
    let rest = fingerprint.strip_prefix(ACCOUNT_FINGERPRINT_SCHEME)?;
    let rest = rest.strip_prefix(':')?;
    let (class, digest) = rest.split_once(':')?;
    if digest.len() != FINGERPRINT_HEX_LEN || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    AccountFingerprintClass::parse(class)
}

/// The MAC key for both fingerprint families. Derived from the unlocked Vault
/// master key; never serialized, never stored, zeroized on drop.
pub struct FingerprintKey {
    bytes: [u8; 32],
}

/// Hand-written, redacting `Debug` — deliberately not `#[derive]`d, for the
/// same reason `vault_kit::DerivedVaultKey` is hand-written: a derive would
/// print raw key material into any `{:?}`, panic message, or test failure.
impl std::fmt::Debug for FingerprintKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FingerprintKey([REDACTED; 32])")
    }
}

impl Drop for FingerprintKey {
    fn drop(&mut self) {
        for byte in self.bytes.iter_mut() {
            // SAFETY: `byte` is a unique `&mut u8` into a live, initialized
            // array; a volatile write of a fully-initialized value cannot be
            // elided by the optimizer as a dead store.
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }
}

impl FingerprintKey {
    /// Derive the fingerprint key from the unlocked Vault master key.
    ///
    /// The master key is already a uniformly random 32-byte Argon2id output,
    /// so the derivation step it needs is *domain separation*, not password
    /// stretching: a keyed BLAKE2b over a fixed domain label is exactly that,
    /// and re-running Argon2 here would buy nothing while making every plan
    /// pay hundreds of milliseconds.
    pub fn derive_from_master_key(master_key: &[u8; 32]) -> Self {
        let mut mac = new_mac(master_key);
        mac.update(FP_KEY_DOMAIN);
        let out = mac.finalize().into_bytes();
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&out);
        Self { bytes }
    }

    /// `fp1:<hex12>` for one secret value under one canonical provider kind.
    ///
    /// `provider_kind` is the registry's `ApiKeyDef::provider_kind` (#1680 D3),
    /// never an env-var name: two names of the same family holding the same
    /// value must produce the same fingerprint, which is exactly what makes
    /// the "two names, one account, two aliases" merge decidable.
    pub fn key_fingerprint(&self, provider_kind: &str, value: &str) -> String {
        let mut mac = new_mac(&self.bytes);
        mac.update(KEY_FINGERPRINT_DOMAIN);
        mac.update(provider_kind.as_bytes());
        mac.update(&[FIELD_SEPARATOR]);
        mac.update(value.as_bytes());
        format!(
            "{KEY_FINGERPRINT_SCHEME}:{}",
            truncated_hex(&mac.finalize().into_bytes())
        )
    }

    /// `fpa1:kv:<hex12>` over the account's current member key fingerprints.
    ///
    /// The input is the **sorted, de-duplicated set** of members, so the value
    /// depends on which keys the account holds and not on the order they were
    /// discovered in or on a member being seen twice in one sweep. Adding or
    /// removing a member changes it; that change is what a reconcile revision
    /// and a `fingerprint_observed` event record.
    pub fn account_fingerprint_from_members<I, S>(&self, members: I) -> String
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut members: Vec<String> = members
            .into_iter()
            .map(|member| member.as_ref().to_string())
            .collect();
        members.sort();
        members.dedup();

        let mut mac = new_mac(&self.bytes);
        mac.update(ACCOUNT_FINGERPRINT_DOMAIN);
        mac.update(AccountFingerprintClass::KeyedMembers.as_str().as_bytes());
        for member in &members {
            mac.update(&[FIELD_SEPARATOR]);
            mac.update(member.as_bytes());
        }
        format!(
            "{ACCOUNT_FINGERPRINT_SCHEME}:{}:{}",
            AccountFingerprintClass::KeyedMembers.as_str(),
            truncated_hex(&mac.finalize().into_bytes())
        )
    }
}

fn new_mac(key: &[u8; 32]) -> FingerprintMac {
    // BLAKE2b accepts any key up to its 128-byte block size; a fixed 32-byte
    // key can never be rejected, so this cannot panic in practice.
    <FingerprintMac as Mac>::new_from_slice(key)
        .expect("BLAKE2b accepts a 32-byte MAC key by construction")
}

fn truncated_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(FINGERPRINT_HEX_LEN);
    for byte in bytes.iter().take(FINGERPRINT_HEX_LEN / 2) {
        write!(&mut out, "{byte:02x}").expect("writing to String cannot fail");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MASTER: [u8; 32] = [7u8; 32];

    fn key() -> FingerprintKey {
        FingerprintKey::derive_from_master_key(&MASTER)
    }

    /// Pinned vector. The construction is a stored format: every persisted
    /// `fp1:` value in every database was minted by exactly these bytes in
    /// exactly this order, so a refactor that "harmlessly" reorders the MAC
    /// input, drops the separator, or widens the truncation silently
    /// invalidates them all. This is the assertion that makes that loud.
    #[test]
    fn key_fingerprint_matches_the_pinned_construction() {
        let expected = {
            let mut mac = <FingerprintMac as Mac>::new_from_slice(&MASTER).unwrap();
            mac.update(b"tachi.fp.v1");
            let fp_key = mac.finalize().into_bytes();

            let mut mac = <FingerprintMac as Mac>::new_from_slice(&fp_key).unwrap();
            mac.update(b"tachi.provider-key.v1");
            mac.update(b"deepseek");
            mac.update(&[0x00]);
            mac.update(b"sk-pinned-value");
            let digest = mac.finalize().into_bytes();
            let mut hex = String::new();
            for byte in digest.iter().take(6) {
                write!(&mut hex, "{byte:02x}").expect("writing to String cannot fail");
            }
            format!("fp1:{hex}")
        };

        assert_eq!(
            key().key_fingerprint("deepseek", "sk-pinned-value"),
            expected
        );
    }

    #[test]
    fn key_fingerprint_is_stable_and_shaped() {
        let fp = key().key_fingerprint("deepseek", "sk-value-a");
        assert_eq!(fp, key().key_fingerprint("deepseek", "sk-value-a"));
        let digest = fp.strip_prefix("fp1:").expect("fp1 scheme prefix");
        assert_eq!(digest.len(), FINGERPRINT_HEX_LEN);
        assert!(digest.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    /// The merge half of discrimination 1, at the primitive level: the
    /// fingerprint is over (provider kind, value) and knows nothing about the
    /// env-var name a value was found under, so two names holding one value
    /// are indistinguishable here — which is what lets the plan collapse them
    /// into one account with two aliases.
    #[test]
    fn one_value_under_two_names_produces_one_fingerprint() {
        let key = key();
        let under_canonical = key.key_fingerprint("deepseek", "sk-shared-value");
        let under_alias = key.key_fingerprint("deepseek", "sk-shared-value");
        assert_eq!(under_canonical, under_alias);
    }

    /// The distinguish half: same name, different values must not collapse.
    #[test]
    fn different_values_produce_different_fingerprints() {
        let key = key();
        assert_ne!(
            key.key_fingerprint("deepseek", "sk-value-a"),
            key.key_fingerprint("deepseek", "sk-value-b")
        );
    }

    /// Domain separation across provider families: the same leaked value
    /// stored for two vendors must not correlate.
    #[test]
    fn same_value_under_two_provider_kinds_does_not_correlate() {
        let key = key();
        assert_ne!(
            key.key_fingerprint("deepseek", "sk-same"),
            key.key_fingerprint("openai", "sk-same")
        );
    }

    /// The separator is load-bearing: without it, ("ab", "cd") and ("a",
    /// "bcd") would hash the same bytes and one vendor's fingerprint could be
    /// forged from another's.
    #[test]
    fn provider_kind_and_value_cannot_be_re_split() {
        let key = key();
        assert_ne!(
            key.key_fingerprint("ab", "cd"),
            key.key_fingerprint("a", "bcd")
        );
    }

    /// Per-install keying (leader ruling in D2): two Vaults with different
    /// master keys must not produce comparable fingerprints for one value.
    #[test]
    fn fingerprints_are_per_install_keyed() {
        let other = FingerprintKey::derive_from_master_key(&[9u8; 32]);
        assert_ne!(
            key().key_fingerprint("deepseek", "sk-same"),
            other.key_fingerprint("deepseek", "sk-same")
        );
    }

    /// The fingerprint key must not be the master key: a leaked fingerprint
    /// key must not be a Vault decryption key.
    #[test]
    fn fingerprint_key_is_domain_separated_from_the_master_key() {
        let derived = key();
        assert_ne!(derived.bytes, MASTER);
    }

    #[test]
    fn fingerprint_key_debug_never_prints_key_material() {
        let rendered = format!("{:?}", key());
        assert_eq!(rendered, "FingerprintKey([REDACTED; 32])");
        assert!(!rendered.contains('7'));
    }

    #[test]
    fn account_fingerprint_ignores_member_order_and_duplicates() {
        let key = key();
        let a = key.key_fingerprint("deepseek", "sk-a");
        let b = key.key_fingerprint("deepseek", "sk-b");
        let sorted = key.account_fingerprint_from_members([a.clone(), b.clone()]);
        let reversed = key.account_fingerprint_from_members([b.clone(), a.clone()]);
        let duplicated = key.account_fingerprint_from_members([b, a.clone(), a]);
        assert_eq!(sorted, reversed);
        assert_eq!(sorted, duplicated);
    }

    #[test]
    fn account_fingerprint_changes_when_membership_changes() {
        let key = key();
        let a = key.key_fingerprint("deepseek", "sk-a");
        let b = key.key_fingerprint("deepseek", "sk-b");
        assert_ne!(
            key.account_fingerprint_from_members([a.clone()]),
            key.account_fingerprint_from_members([a, b])
        );
    }

    #[test]
    fn account_fingerprint_carries_its_evidence_class() {
        let key = key();
        let fp = key.account_fingerprint_from_members([key.key_fingerprint("deepseek", "sk-a")]);
        assert!(fp.starts_with("fpa1:kv:"));
        assert_eq!(
            account_fingerprint_class(&fp),
            Some(AccountFingerprintClass::KeyedMembers)
        );
    }

    /// The reserved `pa` class must round-trip through the parser even though
    /// nothing mints it yet — otherwise the day a probe starts returning
    /// provider-attested ids, every stored value reads as malformed.
    #[test]
    fn provider_attested_class_parses_though_nothing_mints_it_yet() {
        assert_eq!(
            account_fingerprint_class("fpa1:pa:0123456789ab"),
            Some(AccountFingerprintClass::ProviderAttested)
        );
        assert!(AccountFingerprintClass::ProviderAttested
            .outranks(AccountFingerprintClass::KeyedMembers));
        assert!(!AccountFingerprintClass::KeyedMembers
            .outranks(AccountFingerprintClass::ProviderAttested));
    }

    #[test]
    fn malformed_account_fingerprints_are_rejected_rather_than_guessed() {
        for bad in [
            "",
            "fpa1:kv",
            "fpa1:kv:",
            "fpa1:name:0123456789ab",
            "fpa1:kv:0123456789",     // too short
            "fpa1:kv:0123456789abcd", // too long
            "fpa1:kv:zzzzzzzzzzzz",   // not hex
            "fp1:0123456789ab",       // key fingerprint, not account
        ] {
            assert_eq!(account_fingerprint_class(bad), None, "accepted {bad:?}");
        }
    }

    /// A key fingerprint and an account fingerprint over the same bytes must
    /// not be interchangeable.
    #[test]
    fn key_and_account_domains_do_not_collide() {
        let key = key();
        let member = key.key_fingerprint("deepseek", "sk-a");
        let account = key.account_fingerprint_from_members([member.clone()]);
        assert_ne!(
            member.trim_start_matches("fp1:"),
            account.trim_start_matches("fpa1:kv:")
        );
    }
}
