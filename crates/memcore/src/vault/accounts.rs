// vault/accounts.rs — provider-account types (tachi#1680 D1/D5).
//
// The row types for the four tables added to the vault section of the baseline
// DDL. SQL lives beside them in `db::vault_accounts`, mirroring the split this
// table family already uses (`VaultKeyHealth` here, `db::vault_db` there).
//
// # The one invariant this module exists to hold
//
// A [`ProviderAccount`] is **public-safe metadata**: which vendor account a
// credential belongs to, how it authenticates, what it can do, how fresh the
// evidence is. It never carries the secret, and — just as deliberately — it
// never carries *where the secret lives*. Vault layout (rotation prefixes,
// member entry names) is custody, and custody lives in a physically separate
// row type ([`AccountCustody`]) reachable only through the opaque `auth_ref`.
//
// That separation is enforced by types, not by reviewer vigilance: no
// constructor and no serialization path on this file's account types can
// produce a struct holding both, and the custody types are neither
// `Serialize` nor plainly `Debug` — the two ways a struct usually escapes into
// a file or a log. A future field added to `ProviderAccount` is checked by
// `db::tests::vault_accounts_ops::account_serialized_field_set_is_frozen`,
// which pins the exact serialized key set.
//
// Types cannot check the *contents* of a caller-supplied string, so the one
// remaining way layout could reach these surfaces — writing a pool member name
// into an alias or a source descriptor — is refused at the store door instead;
// see [`names_rotation_pool_member`].

use serde::{Deserialize, Serialize};

/// How an account authenticates. Closed set — it must stay byte-identical to
/// the `CHECK (auth_mode IN (...))` clause on `provider_accounts`, which
/// `db::tests::vault_accounts_ops` pins from both directions (every variant
/// inserts; an unknown string is refused by the database).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    /// One or more interchangeable API keys held in a Vault rotation pool or a
    /// single Vault entry — everything `API_KEY_DEFS` describes today.
    ApiKeyPool,
    /// OAuth tokens minted and refreshed by a broker (#1684).
    BrokeredOauth,
    /// Cloud provider IAM (instance/workload identity); no stored secret.
    CloudIam,
    /// A session the provider itself owns (a CLI's own login state), which
    /// Tachi can observe but never materialize.
    ProviderOwnedSession,
    /// A local endpoint that takes no credential at all.
    LocalNoAuth,
    /// Recognized as an account, with an auth mechanism this build cannot
    /// operate. Explicit rather than absent: "we know and refuse" must be
    /// distinguishable from "we never looked".
    Unsupported,
}

impl AuthMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApiKeyPool => "api_key_pool",
            Self::BrokeredOauth => "brokered_oauth",
            Self::CloudIam => "cloud_iam",
            Self::ProviderOwnedSession => "provider_owned_session",
            Self::LocalNoAuth => "local_no_auth",
            Self::Unsupported => "unsupported",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "api_key_pool" => Some(Self::ApiKeyPool),
            "brokered_oauth" => Some(Self::BrokeredOauth),
            "cloud_iam" => Some(Self::CloudIam),
            "provider_owned_session" => Some(Self::ProviderOwnedSession),
            "local_no_auth" => Some(Self::LocalNoAuth),
            "unsupported" => Some(Self::Unsupported),
            _ => None,
        }
    }

    /// Every variant, in DDL order. The `CHECK` clause and this slice are the
    /// same closed set stated twice; a test compares them.
    pub const ALL: &'static [AuthMode] = &[
        Self::ApiKeyPool,
        Self::BrokeredOauth,
        Self::CloudIam,
        Self::ProviderOwnedSession,
        Self::LocalNoAuth,
        Self::Unsupported,
    ];

    /// Whether this mode has a credential Tachi itself holds — i.e. whether an
    /// account in this mode is expected to have an `auth_ref` and a custody
    /// row at all. The three "someone else holds it" modes have neither.
    pub fn has_tachi_held_credential(self) -> bool {
        matches!(self, Self::ApiKeyPool | Self::BrokeredOauth)
    }
}

/// What kind of surface the account serves. Mirrors the registry's `KeyClass`
/// (#1680 D3) — the boundary that keeps a search credential out of the LLM
/// provider cache — restated as stored data because the account row outlives
/// any one process's registry view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountClass {
    #[default]
    ModelApi,
    SearchApi,
    Infra,
}

impl AccountClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ModelApi => "model_api",
            Self::SearchApi => "search_api",
            Self::Infra => "infra",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "model_api" => Some(Self::ModelApi),
            "search_api" => Some(Self::SearchApi),
            "infra" => Some(Self::Infra),
            _ => None,
        }
    }
}

/// Where the secret behind an `auth_ref` physically lives. Closed set, pinned
/// by the `CHECK` clause on `account_custody`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CustodyKind {
    /// `custody_target` is a `vault_key_rotations.prefix`.
    VaultRotationPool,
    /// `custody_target` is a `vault_entries.name`.
    VaultEntry,
}

impl CustodyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::VaultRotationPool => "vault_rotation_pool",
            Self::VaultEntry => "vault_entry",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "vault_rotation_pool" => Some(Self::VaultRotationPool),
            "vault_entry" => Some(Self::VaultEntry),
            _ => None,
        }
    }
}

/// Default `provider_accounts.status`.
pub const ACCOUNT_STATUS_ACTIVE: &str = "active";
/// Default `provider_accounts.refresh_authority` — nobody refreshes this
/// credential on Tachi's behalf. #1684's broker is what changes it.
pub const REFRESH_AUTHORITY_NONE: &str = "none";

/// Event kinds written to `provider_account_events`. Not an enum: the table
/// column is free TEXT so a later slice can add a kind without a schema
/// change, and an unknown kind read back from an older/newer build must stay
/// readable rather than fail to parse.
pub const EVENT_KIND_ACCOUNT_CREATED: &str = "account_created";
pub const EVENT_KIND_ALIAS_OBSERVED: &str = "alias_observed";
pub const EVENT_KIND_ALIAS_RETIRED: &str = "alias_retired";
/// A reconcile pass saw the account's member key material change (rotation,
/// member added/removed) and recomputed its account fingerprint.
pub const EVENT_KIND_FINGERPRINT_OBSERVED: &str = "fingerprint_observed";
/// The Vault master key rotated, so the fingerprint key rotated with it and
/// every fingerprint was recomputed. Identity is explicitly *not* broken by
/// this: same `account_id`, new fingerprints, one event saying why (#1680 D2).
pub const EVENT_KIND_FINGERPRINT_REKEYED: &str = "fingerprint_rekeyed";
/// The custody pointer moved (pool restructured, entry renamed) while
/// `auth_ref` stayed the same.
pub const EVENT_KIND_CUSTODY_UPDATED: &str = "custody_updated";

/// Scheme prefix of an `auth_ref`. Versioned so a second custody addressing
/// scheme can coexist with `va1:` handles already stored.
pub const AUTH_REF_SCHEME: &str = "va1";

/// A provider account: public-safe metadata only.
///
/// `capabilities` and `source_refs` are stored as JSON arrays. `source_refs`
/// stays opaque at this layer — the reconcile pipeline (#1680 D4) owns the
/// descriptor grammar and the redaction rule that keeps a source descriptor
/// from becoming a path/secret leak.
///
/// What this layer does enforce is a floor, not a grammar: because the account
/// row *is* a serialized public surface, `db::vault_accounts` refuses a
/// `source_ref` (or an alias name) that carries rotation-pool layout, per
/// [`names_rotation_pool_member`]. Without it "public-safe metadata" would rest
/// on every future caller remembering the rule, which is exactly the kind of
/// field discipline the custody table split was introduced to replace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderAccount {
    pub account_id: String,
    pub provider_kind: String,
    pub auth_mode: AuthMode,
    /// `None` for auth modes with no Tachi-held credential. Opaque by
    /// construction: it encodes no Vault name, so it can appear in plans,
    /// events and status without leaking layout.
    pub auth_ref: Option<String>,
    pub account_fingerprint: String,
    pub account_class: AccountClass,
    pub capabilities: Vec<String>,
    pub credential_policy_ref: Option<String>,
    pub refresh_authority: String,
    pub status: String,
    pub revision: i64,
    pub source_refs: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// The caller-supplied half of a new account row. Separate from
/// [`ProviderAccount`] because `revision`, `created_at` and `updated_at` are
/// the store's to assign — a caller that could pass its own `revision` could
/// silently rewind the optimistic-concurrency counter every later slice binds
/// its preconditions to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewProviderAccount {
    pub account_id: String,
    pub provider_kind: String,
    pub auth_mode: AuthMode,
    pub auth_ref: Option<String>,
    pub account_fingerprint: String,
    pub account_class: AccountClass,
    pub capabilities: Vec<String>,
    pub credential_policy_ref: Option<String>,
    pub refresh_authority: String,
    pub status: String,
    pub source_refs: Vec<String>,
}

impl NewProviderAccount {
    /// The common case: an API-key account, active, no policy, no capability
    /// claims yet.
    pub fn api_key_pool(
        account_id: impl Into<String>,
        provider_kind: impl Into<String>,
        auth_ref: impl Into<String>,
        account_fingerprint: impl Into<String>,
        account_class: AccountClass,
    ) -> Self {
        Self {
            account_id: account_id.into(),
            provider_kind: provider_kind.into(),
            auth_mode: AuthMode::ApiKeyPool,
            auth_ref: Some(auth_ref.into()),
            account_fingerprint: account_fingerprint.into(),
            account_class,
            capabilities: Vec::new(),
            credential_policy_ref: None,
            refresh_authority: REFRESH_AUTHORITY_NONE.to_string(),
            status: ACCOUNT_STATUS_ACTIVE.to_string(),
            source_refs: Vec::new(),
        }
    }
}

/// One env-var name an account has been seen under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderAccountAlias {
    pub account_id: String,
    pub alias_name: String,
    /// Where the name was observed (`config_env`, `vault_entry`, `process_env`
    /// …). Free text at this layer; #1680 D4's discover step owns the
    /// vocabulary.
    pub source_kind: String,
    pub first_seen: String,
    pub last_seen: String,
    pub retired: bool,
}

/// One append-only account audit row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderAccountEvent {
    pub id: i64,
    pub account_id: String,
    /// The account revision this event produced.
    pub revision: i64,
    pub event_kind: String,
    /// The `pd1:` reconcile plan digest that caused it, when there was one.
    pub plan_digest: Option<String>,
    /// JSON. Public-safe by the same rule as the account row: fingerprints and
    /// counts, never key material.
    pub evidence: String,
    pub created_at: String,
}

/// The caller-supplied half of an event row (`id` is the store's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewProviderAccountEvent {
    pub account_id: String,
    pub revision: i64,
    pub event_kind: String,
    pub plan_digest: Option<String>,
    pub evidence: String,
}

impl NewProviderAccountEvent {
    pub fn new(
        account_id: impl Into<String>,
        revision: i64,
        event_kind: impl Into<String>,
    ) -> Self {
        Self {
            account_id: account_id.into(),
            revision,
            event_kind: event_kind.into(),
            plan_digest: None,
            evidence: "{}".to_string(),
        }
    }

    pub fn with_plan_digest(mut self, plan_digest: impl Into<String>) -> Self {
        self.plan_digest = Some(plan_digest.into());
        self
    }

    pub fn with_evidence(mut self, evidence: impl Into<String>) -> Self {
        self.evidence = evidence.into();
        self
    }
}

/// The custody row: the only place that says where an account's secret lives.
///
/// Deliberately **not** `Serialize`. Every other type in this module is
/// serializable because it is public-safe; this one is not public-safe, and
/// leaving the derive off means a future "return it in the API response"
/// cannot compile rather than quietly shipping Vault layout to a plan file or
/// an MCP response.
///
/// `Debug` is hand-written and redacts `custody_target` for the other half of
/// that rule: a *derived* `Debug` would have made `tracing::debug!("{custody:?}")`
/// — and every `assert_eq!` failure, panic message and `unwrap` backtrace —
/// print the rotation prefix or entry name that the missing `Serialize` was
/// there to keep off log surfaces. The pointer's shape (`custody_kind`) stays
/// visible because it is the part with diagnostic value and no layout in it.
#[derive(Clone, PartialEq, Eq)]
pub struct AccountCustody {
    pub auth_ref: String,
    pub account_id: String,
    pub custody_kind: CustodyKind,
    pub custody_target: String,
    pub revision: i64,
    pub updated_at: String,
}

impl std::fmt::Debug for AccountCustody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountCustody")
            .field("auth_ref", &self.auth_ref)
            .field("account_id", &self.account_id)
            .field("custody_kind", &self.custody_kind)
            .field("custody_target", &RedactedCustodyTarget)
            .field("revision", &self.revision)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

/// What an `auth_ref` resolves to. Same non-`Serialize` and same redacting
/// `Debug` as [`AccountCustody`], for the same reason: this is the resolver's
/// answer, and the resolver's answer is Vault layout.
#[derive(Clone, PartialEq, Eq)]
pub struct CustodyResolution {
    pub account_id: String,
    pub custody_kind: CustodyKind,
    pub custody_target: String,
    pub revision: i64,
}

impl std::fmt::Debug for CustodyResolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CustodyResolution")
            .field("account_id", &self.account_id)
            .field("custody_kind", &self.custody_kind)
            .field("custody_target", &RedactedCustodyTarget)
            .field("revision", &self.revision)
            .finish()
    }
}

/// What a redacted `custody_target` renders as. A unit struct rather than a
/// string literal so `{:?}` prints it bare (`custody_target: [REDACTED]`)
/// instead of quoting it like a value that could be mistaken for a real target.
struct RedactedCustodyTarget;

impl std::fmt::Debug for RedactedCustodyTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

/// Separators a source descriptor might use around the identifiers it embeds.
/// Deliberately **not** a grammar — #1680 D4 owns the descriptor grammar, and
/// this module must not pre-empt it. This is only wide enough to find a layout
/// token hiding inside whatever shape D4 eventually picks
/// (`"vault_entry:GEMINI_API_KEY_2"`, `"config_env GEMINI_API_KEY_2"`, …).
const DESCRIPTOR_SEPARATORS: &[char] = &[':', ',', ';', ' ', '\t', '\n', '/', '\\', '=', '#', '|'];

/// Whether a string names a Vault **rotation-pool member** — the one shape
/// D5 says must never appear on an account surface (`"DEEPSEEK_API_KEY_2"`,
/// where the member entry name is also the `vault_key_health.key_id`).
///
/// The logical name itself (`DEEPSEEK_API_KEY`) is deliberately *not* layout:
/// it is exactly what `provider_account_aliases.alias_name` stores. What leaks
/// is the member index, because that is the pool's internal structure — the
/// thing `auth_ref` + `account_custody` exist to keep on one side of a table
/// boundary.
///
/// Shape-based rather than a lookup against `vault_key_rotations`, on purpose:
/// a DB lookup would make the same string legal before a pool is registered and
/// illegal after, so a descriptor's admissibility would depend on when it was
/// written. This predicate is a pure function of the string.
pub fn names_rotation_pool_member(value: &str) -> bool {
    value
        .split(DESCRIPTOR_SEPARATORS)
        .any(is_rotation_member_token)
}

/// `PREFIX_<n>` where `PREFIX` is env-var-shaped (upper-case ASCII, digits and
/// underscores, containing at least one letter) and `n` is a positive decimal
/// without a leading zero — i.e. what [`api_key_pool_member_index`] would
/// accept for the pool named `PREFIX`.
///
/// [`api_key_pool_member_index`]: crate::vault::api_key_pool_member_index
fn is_rotation_member_token(token: &str) -> bool {
    let Some((prefix, index)) = token.rsplit_once('_') else {
        return false;
    };
    let index_ok =
        !index.is_empty() && index.bytes().all(|b| b.is_ascii_digit()) && !index.starts_with('0');
    let prefix_ok = prefix.bytes().any(|b| b.is_ascii_uppercase())
        && prefix
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
    index_ok && prefix_ok
}

/// Mint an opaque `auth_ref`.
///
/// Opaque is the whole point (#1680 D5): encoding the logical name or rotation
/// prefix into the ref would leak layout everywhere the ref travels, and would
/// force the ref to change whenever custody was restructured — exactly the
/// coupling the indirection exists to remove. 128 random bits from the same
/// OS CSPRNG `Uuid::new_v4` uses.
pub fn mint_auth_ref() -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(AUTH_REF_SCHEME.len() + 1 + 32);
    out.push_str(AUTH_REF_SCHEME);
    out.push(':');
    for byte in uuid::Uuid::new_v4().as_bytes() {
        write!(&mut out, "{byte:02x}").expect("writing to String cannot fail");
    }
    out
}

const CROCKFORD_BASE32: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Mint an account id: a ULID (48-bit millisecond timestamp + 80 random bits,
/// Crockford base32, 26 chars).
///
/// ULID rather than the crate's usual UUIDv4 because this id is the one thing
/// about an account that must never change — through key rotation, alias
/// churn, custody restructuring and master-key rekey — so it is worth having
/// it sort by creation time, which makes an account's first event and its id
/// order the same story. Encoded locally rather than by adding a `ulid`
/// dependency: it is thirty lines and one pinned test.
pub fn mint_account_id() -> String {
    let millis = chrono::Utc::now().timestamp_millis().max(0) as u64;
    let entropy = *uuid::Uuid::new_v4().as_bytes();
    let mut random = [0u8; 10];
    random.copy_from_slice(&entropy[..10]);
    encode_ulid(millis, random)
}

/// Mask keeping the low 48 bits of the millisecond timestamp — a ULID's time
/// field. Beyond it (year 10889) the encoding would silently wrap, so it is
/// masked rather than allowed to collide with the random field.
const ULID_TIME_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

fn encode_ulid(millis: u64, random: [u8; 10]) -> String {
    let mut value = u128::from(millis & ULID_TIME_MASK) << 80;
    let mut low: u128 = 0;
    for byte in random {
        low = (low << 8) | u128::from(byte);
    }
    value |= low;

    let mut out = [0u8; 26];
    for slot in out.iter_mut().rev() {
        *slot = CROCKFORD_BASE32[(value & 0x1f) as usize];
        value >>= 5;
    }
    String::from_utf8(out.to_vec()).expect("Crockford base32 is ASCII")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn auth_mode_strings_round_trip_and_cover_every_variant() {
        for mode in AuthMode::ALL {
            assert_eq!(AuthMode::parse(mode.as_str()), Some(*mode));
        }
        assert_eq!(AuthMode::ALL.len(), 6);
        assert_eq!(AuthMode::parse("api-key-pool"), None);
        assert_eq!(AuthMode::parse(""), None);
    }

    /// The stored string and the serialized string must be the same string.
    /// `as_str` is what reaches SQLite (and the `CHECK` clauses); serde's
    /// `rename_all` is what reaches JSON. Nothing but this test stops the two
    /// from drifting into a state where a plan says `apiKeyPool` while the
    /// database says `api_key_pool` and every join by that value silently
    /// misses.
    #[test]
    fn serde_rendering_equals_the_stored_string_for_every_closed_set() {
        for mode in AuthMode::ALL {
            assert_eq!(
                serde_json::to_string(mode).unwrap(),
                format!("\"{}\"", mode.as_str())
            );
        }
        for class in [
            AccountClass::ModelApi,
            AccountClass::SearchApi,
            AccountClass::Infra,
        ] {
            assert_eq!(
                serde_json::to_string(&class).unwrap(),
                format!("\"{}\"", class.as_str())
            );
        }
        for kind in [CustodyKind::VaultRotationPool, CustodyKind::VaultEntry] {
            assert_eq!(
                serde_json::to_string(&kind).unwrap(),
                format!("\"{}\"", kind.as_str())
            );
        }
    }

    #[test]
    fn account_class_and_custody_kind_strings_round_trip() {
        for class in [
            AccountClass::ModelApi,
            AccountClass::SearchApi,
            AccountClass::Infra,
        ] {
            assert_eq!(AccountClass::parse(class.as_str()), Some(class));
        }
        assert_eq!(AccountClass::default(), AccountClass::ModelApi);
        for kind in [CustodyKind::VaultRotationPool, CustodyKind::VaultEntry] {
            assert_eq!(CustodyKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(CustodyKind::parse("keychain"), None);
    }

    /// The three "somebody else holds the credential" modes must not be
    /// expected to carry custody — that is what makes `auth_ref` nullable
    /// meaningful rather than an accident.
    #[test]
    fn only_tachi_held_modes_expect_custody() {
        assert!(AuthMode::ApiKeyPool.has_tachi_held_credential());
        assert!(AuthMode::BrokeredOauth.has_tachi_held_credential());
        for mode in [
            AuthMode::CloudIam,
            AuthMode::ProviderOwnedSession,
            AuthMode::LocalNoAuth,
            AuthMode::Unsupported,
        ] {
            assert!(!mode.has_tachi_held_credential(), "{mode:?}");
        }
    }

    #[test]
    fn auth_refs_are_opaque_and_unique() {
        let first = mint_auth_ref();
        let second = mint_auth_ref();
        assert_ne!(first, second);
        for ref_value in [&first, &second] {
            let body = ref_value
                .strip_prefix("va1:")
                .expect("auth_ref carries its scheme");
            assert_eq!(body.len(), 32);
            assert!(body.bytes().all(|b| b.is_ascii_hexdigit()));
        }
    }

    /// The layout-leak guard for D5: an `auth_ref` minted for an account whose
    /// secret lives in `DEEPSEEK_API_KEY_2` must contain no trace of that name,
    /// because the ref is minted without ever seeing it.
    #[test]
    fn auth_refs_encode_nothing_about_vault_layout() {
        let refs: Vec<String> = (0..64).map(|_| mint_auth_ref()).collect();
        for ref_value in &refs {
            let upper = ref_value.to_ascii_uppercase();
            for leak in ["DEEPSEEK", "API_KEY", "ROTATION", "VAULT"] {
                assert!(!upper.contains(leak), "{ref_value} leaks {leak}");
            }
        }
        assert_eq!(refs.iter().collect::<HashSet<_>>().len(), refs.len());
    }

    #[test]
    fn account_ids_are_ulid_shaped_and_unique() {
        let ids: Vec<String> = (0..64).map(|_| mint_account_id()).collect();
        for id in &ids {
            assert_eq!(id.len(), 26, "{id}");
            assert!(
                id.bytes().all(|b| CROCKFORD_BASE32.contains(&b)),
                "{id} is not Crockford base32"
            );
        }
        assert_eq!(ids.iter().collect::<HashSet<_>>().len(), ids.len());
    }

    /// Pinned encoder vector plus the property that motivates ULID at all:
    /// ids minted later sort after ids minted earlier, lexicographically.
    #[test]
    fn ulid_encoding_is_pinned_and_time_ordered() {
        assert_eq!(encode_ulid(0, [0u8; 10]), "00000000000000000000000000");
        assert_eq!(encode_ulid(1, [0u8; 10]), "00000000010000000000000000");
        assert_eq!(
            encode_ulid((1u64 << 48) - 1, [0xff; 10]),
            "7ZZZZZZZZZZZZZZZZZZZZZZZZZ"
        );

        let earlier = encode_ulid(1_700_000_000_000, [0xff; 10]);
        let later = encode_ulid(1_700_000_000_001, [0x00; 10]);
        assert!(earlier < later, "{earlier} !< {later}");
    }

    /// Custody must not be serializable — the compile-time half of "custody is
    /// never serialized with an account". This test states the rule; the
    /// enforcement is the absent derive, which a `serde_json::to_string` on
    /// `AccountCustody` would fail to compile against.
    #[test]
    fn account_metadata_serializes_without_any_custody_field() {
        let account = ProviderAccount {
            account_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
            provider_kind: "deepseek".to_string(),
            auth_mode: AuthMode::ApiKeyPool,
            auth_ref: Some("va1:0123".to_string()),
            account_fingerprint: "fpa1:kv:0123456789ab".to_string(),
            account_class: AccountClass::ModelApi,
            capabilities: vec!["chat".to_string()],
            credential_policy_ref: None,
            refresh_authority: REFRESH_AUTHORITY_NONE.to_string(),
            status: ACCOUNT_STATUS_ACTIVE.to_string(),
            revision: 1,
            source_refs: vec!["config_env".to_string()],
            created_at: "2026-08-09T00:00:00.000Z".to_string(),
            updated_at: "2026-08-09T00:00:00.000Z".to_string(),
        };
        let json = serde_json::to_string(&account).expect("account serializes");
        for forbidden in ["custody_kind", "custody_target", "DEEPSEEK_API_KEY"] {
            assert!(!json.contains(forbidden), "{json} contains {forbidden}");
        }
        let round_trip: ProviderAccount = serde_json::from_str(&json).expect("account round-trips");
        assert_eq!(round_trip, account);
    }

    /// The `Debug` half of the same rule, at the type level: re-deriving
    /// `Debug` on either custody type would put the Vault target back into
    /// every log line, panic message and assertion failure. The store-level
    /// guard is
    /// `db::tests::vault_accounts_ops::custody_debug_output_carries_no_vault_target`;
    /// this one fails at the type that owns the decision.
    #[test]
    fn custody_types_redact_their_target_in_debug() {
        let custody = AccountCustody {
            auth_ref: "va1:0123".to_string(),
            account_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
            custody_kind: CustodyKind::VaultRotationPool,
            custody_target: "DEEPSEEK_API_KEY".to_string(),
            revision: 3,
            updated_at: "2026-08-09T00:00:00.000Z".to_string(),
        };
        let resolution = CustodyResolution {
            account_id: custody.account_id.clone(),
            custody_kind: custody.custody_kind,
            custody_target: custody.custody_target.clone(),
            revision: custody.revision,
        };

        for rendered in [format!("{custody:?}"), format!("{resolution:?}")] {
            assert!(
                !rendered.contains("DEEPSEEK_API_KEY"),
                "custody target leaked into Debug: {rendered}"
            );
            assert!(
                rendered.contains("custody_target: [REDACTED]"),
                "the field must stay visible as redacted, not vanish: {rendered}"
            );
            assert!(
                rendered.contains("VaultRotationPool"),
                "the pointer kind carries no layout and stays: {rendered}"
            );
        }
    }

    /// The layout predicate the store refuses writes with. A member index is
    /// pool structure; the logical name it is built from is not, because that
    /// is precisely what an alias row stores.
    #[test]
    fn rotation_member_names_are_recognized_without_catching_logical_names() {
        for layout in [
            "DEEPSEEK_API_KEY_2",
            "GEMINI_API_KEY_10",
            "vault_entry:DEEPSEEK_API_KEY_2",
            "config_env DEEPSEEK_API_KEY_2",
            "pool=GOOGLE_SEARCH_API_KEY_3",
        ] {
            assert!(names_rotation_pool_member(layout), "{layout}");
        }
        for safe in [
            "DEEPSEEK_API_KEY",
            "config_env",
            "vault_entry:DEEPSEEK_API_KEY",
            "file:~/.config/tachi/env#L12",
            // Not a member index: a leading zero is not how members are named,
            // and `api_key_pool_member_index` would not accept index 0 either.
            "DEEPSEEK_API_KEY_02",
            "DEEPSEEK_API_KEY_0",
            // Lower-case is a descriptor word, not an env-var name.
            "revision_2",
            "",
        ] {
            assert!(!names_rotation_pool_member(safe), "{safe}");
        }
    }

    #[test]
    fn event_builder_defaults_are_empty_not_absent() {
        let event = NewProviderAccountEvent::new("acct", 2, EVENT_KIND_FINGERPRINT_OBSERVED);
        assert_eq!(event.plan_digest, None);
        assert_eq!(event.evidence, "{}");
        let bound = event
            .clone()
            .with_plan_digest("pd1:abc")
            .with_evidence(r#"{"members":2}"#);
        assert_eq!(bound.plan_digest.as_deref(), Some("pd1:abc"));
        assert_eq!(bound.evidence, r#"{"members":2}"#);
        assert_eq!(bound.revision, 2);
    }
}
