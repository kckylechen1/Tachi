//! Provider-account store + fingerprint discriminators (tachi#1680 PR-B).
//!
//! These are the tests the design's discrimination list names for this slice:
//! merge/distinguish (1), secret-negative serialization (8), rotation-stable
//! identity including the master-key rekey path, custody never travelling with
//! an account, and the proof that a keyed fingerprint costs the Vault listing
//! nothing because the key is derived rather than stored.

use super::*;

use crate::db::vault_accounts::{
    append_provider_account_event, find_provider_account_by_auth_ref,
    find_provider_accounts_by_fingerprint, get_account_custody, get_provider_account,
    insert_account_custody, insert_provider_account, list_provider_account_aliases,
    list_provider_account_events, list_provider_accounts, record_account_fingerprint,
    record_provider_account_alias, resolve_auth_ref, retire_provider_account_alias,
    update_custody_target, AliasObservation, FingerprintUpdate,
};
use crate::db::{vault_count_entries, vault_list_entries};
use crate::vault::accounts::{
    mint_account_id, mint_auth_ref, AccountClass, AuthMode, CustodyKind, NewProviderAccount,
    NewProviderAccountEvent, ProviderAccount, EVENT_KIND_ACCOUNT_CREATED,
    EVENT_KIND_ALIAS_OBSERVED, EVENT_KIND_FINGERPRINT_OBSERVED, EVENT_KIND_FINGERPRINT_REKEYED,
};
use crate::vault::fingerprint::FingerprintKey;
use crate::vault::VaultEntry;

/// A stand-in for the unlocked Vault master key. Real callers hand in
/// `DerivedVaultKey::bytes()`; nothing about the primitive depends on how the
/// 32 bytes were produced.
const MASTER_KEY: [u8; 32] = [0x5a; 32];
const REKEYED_MASTER_KEY: [u8; 32] = [0xa5; 32];

/// The secret values these tests fingerprint. Deliberately distinctive so a
/// leak into any serialized surface is unmissable.
const SECRET_A: &str = "sk-tachi-1680-secret-value-AAAA";
const SECRET_B: &str = "sk-tachi-1680-secret-value-BBBB";

fn fp_key() -> FingerprintKey {
    FingerprintKey::derive_from_master_key(&MASTER_KEY)
}

fn seed_account(
    conn: &Connection,
    provider_kind: &str,
    account_fingerprint: &str,
) -> (String, String) {
    let account_id = mint_account_id();
    let auth_ref = mint_auth_ref();
    insert_provider_account(
        conn,
        &NewProviderAccount::api_key_pool(
            account_id.clone(),
            provider_kind,
            auth_ref.clone(),
            account_fingerprint,
            AccountClass::ModelApi,
        ),
    )
    .expect("account inserts");
    (account_id, auth_ref)
}

// ── Discrimination 1: merge two names on one value, keep two values apart ────

/// Two env-var names of one provider family holding the *same* secret produce
/// one `fp1`, so the store carries a single account with two alias rows — the
/// merge half of discrimination 1. The alias rows are what keep "we saw it
/// under both names" recoverable after the merge.
#[test]
fn two_names_over_one_value_collapse_into_one_account_with_two_aliases() {
    let conn = make_conn();
    let key = fp_key();

    let under_canonical = key.key_fingerprint("deepseek", SECRET_A);
    let under_alias = key.key_fingerprint("deepseek", SECRET_A);
    assert_eq!(
        under_canonical, under_alias,
        "one value under two names must fingerprint identically"
    );

    let account_fingerprint = key.account_fingerprint_from_members([under_canonical.clone()]);
    let (account_id, _) = seed_account(&conn, "deepseek", &account_fingerprint);

    for name in ["DEEPSEEK_API_KEY", "REASONING_API_KEY"] {
        assert_eq!(
            record_provider_account_alias(&conn, &account_id, name, "config_env").unwrap(),
            AliasObservation::Created
        );
    }

    assert_eq!(list_provider_accounts(&conn).unwrap().len(), 1);
    let aliases = list_provider_account_aliases(&conn, &account_id).unwrap();
    assert_eq!(
        aliases
            .iter()
            .map(|alias| alias.alias_name.as_str())
            .collect::<Vec<_>>(),
        vec!["DEEPSEEK_API_KEY", "REASONING_API_KEY"]
    );
    assert!(aliases.iter().all(|alias| !alias.retired));
}

/// The distinguish half: one name, two different values must stay two
/// accounts, both visible. `find_provider_accounts_by_fingerprint` returning a
/// *list* is what lets the plan surface the ambiguity instead of a last-writer
/// silently winning.
#[test]
fn one_name_over_two_values_stays_two_accounts() {
    let conn = make_conn();
    let key = fp_key();

    let fp_a = key.account_fingerprint_from_members([key.key_fingerprint("deepseek", SECRET_A)]);
    let fp_b = key.account_fingerprint_from_members([key.key_fingerprint("deepseek", SECRET_B)]);
    assert_ne!(fp_a, fp_b, "different values must not share a fingerprint");

    let (first, _) = seed_account(&conn, "deepseek", &fp_a);
    let (second, _) = seed_account(&conn, "deepseek", &fp_b);
    record_provider_account_alias(&conn, &first, "DEEPSEEK_API_KEY", "config_env").unwrap();
    record_provider_account_alias(&conn, &second, "DEEPSEEK_API_KEY", "vault_entry").unwrap();

    assert_eq!(list_provider_accounts(&conn).unwrap().len(), 2);
    assert_eq!(
        find_provider_accounts_by_fingerprint(&conn, &fp_a)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        find_provider_accounts_by_fingerprint(&conn, &fp_b)
            .unwrap()
            .len(),
        1
    );
}

// ── Rotation-stable identity, including the master-key rekey path ────────────

/// Members rotate; the account does not. `account_id` and `auth_ref` survive,
/// the account fingerprint moves, the revision advances once, and the event
/// log says which pass did it.
#[test]
fn member_rotation_keeps_account_id_and_leaves_a_trail() {
    let conn = make_conn();
    let key = fp_key();

    let member_one = key.key_fingerprint("deepseek", SECRET_A);
    let member_two = key.key_fingerprint("deepseek", SECRET_B);
    let before = key.account_fingerprint_from_members([member_one.clone()]);
    let (account_id, auth_ref) = seed_account(&conn, "deepseek", &before);

    let after = key.account_fingerprint_from_members([member_one, member_two]);
    let update = record_account_fingerprint(
        &conn,
        &account_id,
        &after,
        EVENT_KIND_FINGERPRINT_OBSERVED,
        Some("pd1:rotation"),
        r#"{"members":2}"#,
    )
    .unwrap();
    assert!(matches!(
        update,
        FingerprintUpdate::Advanced { revision: 2, .. }
    ));

    let account = get_provider_account(&conn, &account_id).unwrap().unwrap();
    assert_eq!(account.account_id, account_id, "identity must survive");
    assert_eq!(account.auth_ref.as_deref(), Some(auth_ref.as_str()));
    assert_eq!(account.account_fingerprint, after);
    assert_eq!(account.revision, 2);

    let events = list_provider_account_events(&conn, &account_id).unwrap();
    assert_eq!(
        events
            .iter()
            .map(|event| event.event_kind.as_str())
            .collect::<Vec<_>>(),
        vec![EVENT_KIND_ACCOUNT_CREATED, EVENT_KIND_FINGERPRINT_OBSERVED]
    );
    assert_eq!(events[1].revision, 2);
    assert_eq!(events[1].plan_digest.as_deref(), Some("pd1:rotation"));
}

/// The master-key rekey path (#1680 D2): rotating the Vault master key rotates
/// the derived fingerprint key, so every stored fingerprint changes even though
/// not one credential did. Identity must NOT break — same `account_id`, same
/// `auth_ref`, same aliases — and the discontinuity must be explained by a
/// `fingerprint_rekeyed` event rather than inferred from a mystery diff.
#[test]
fn master_key_rekey_refingerprints_without_reminting_identity() {
    let conn = make_conn();
    let old_key = fp_key();

    let before =
        old_key.account_fingerprint_from_members([old_key.key_fingerprint("deepseek", SECRET_A)]);
    let (account_id, auth_ref) = seed_account(&conn, "deepseek", &before);
    record_provider_account_alias(&conn, &account_id, "DEEPSEEK_API_KEY", "config_env").unwrap();

    // Same secret, new master key: the fingerprint MUST change (per-install
    // keying) while nothing about the credential did.
    let new_key = FingerprintKey::derive_from_master_key(&REKEYED_MASTER_KEY);
    let after =
        new_key.account_fingerprint_from_members([new_key.key_fingerprint("deepseek", SECRET_A)]);
    assert_ne!(before, after, "a rekey must actually rotate fingerprints");

    record_account_fingerprint(
        &conn,
        &account_id,
        &after,
        EVENT_KIND_FINGERPRINT_REKEYED,
        None,
        r#"{"reason":"vault_master_key_rotated"}"#,
    )
    .unwrap();

    let account = get_provider_account(&conn, &account_id).unwrap().unwrap();
    assert_eq!(account.account_id, account_id);
    assert_eq!(account.auth_ref.as_deref(), Some(auth_ref.as_str()));
    assert_eq!(account.account_fingerprint, after);
    assert_eq!(account.revision, 2);
    assert_eq!(
        list_provider_account_aliases(&conn, &account_id)
            .unwrap()
            .len(),
        1,
        "a rekey must not disturb aliases"
    );
    let events = list_provider_account_events(&conn, &account_id).unwrap();
    assert_eq!(
        events.last().unwrap().event_kind,
        EVENT_KIND_FINGERPRINT_REKEYED
    );
    assert_eq!(
        find_provider_account_by_auth_ref(&conn, &auth_ref)
            .unwrap()
            .unwrap()
            .account_id,
        account_id,
        "the custody handle must still reach the same account after a rekey"
    );
}

/// An unchanged fingerprint is a genuine no-op: no revision bump, no event.
/// Without this, every idle reconcile pass would inflate the revision the next
/// slice binds its apply-time preconditions to.
#[test]
fn recording_an_unchanged_fingerprint_writes_nothing() {
    let conn = make_conn();
    let key = fp_key();
    let fingerprint =
        key.account_fingerprint_from_members([key.key_fingerprint("deepseek", SECRET_A)]);
    let (account_id, _) = seed_account(&conn, "deepseek", &fingerprint);

    let update = record_account_fingerprint(
        &conn,
        &account_id,
        &fingerprint,
        EVENT_KIND_FINGERPRINT_OBSERVED,
        None,
        "{}",
    )
    .unwrap();
    assert_eq!(update, FingerprintUpdate::Unchanged { revision: 1 });
    assert_eq!(
        get_provider_account(&conn, &account_id)
            .unwrap()
            .unwrap()
            .revision,
        1
    );
    assert_eq!(
        list_provider_account_events(&conn, &account_id)
            .unwrap()
            .len(),
        1
    );
}

// ── Discrimination 8: nothing account-shaped ever carries a secret ───────────

/// The secret-scan fixture. Every serialization surface an account row has —
/// the row itself, the list projection, its aliases, its events — is scanned
/// for the secret values that produced its fingerprints and for the Vault
/// layout its custody row points at. A hit on any of them is the leak this
/// whole table split exists to prevent.
#[test]
fn account_serialization_is_secret_negative_and_custody_free() {
    let conn = make_conn();
    let key = fp_key();
    let fingerprint =
        key.account_fingerprint_from_members([key.key_fingerprint("deepseek", SECRET_A)]);
    let (account_id, auth_ref) = seed_account(&conn, "deepseek", &fingerprint);
    record_provider_account_alias(&conn, &account_id, "DEEPSEEK_API_KEY", "config_env").unwrap();
    append_provider_account_event(
        &conn,
        &NewProviderAccountEvent::new(account_id.as_str(), 1, EVENT_KIND_ALIAS_OBSERVED)
            .with_evidence(r#"{"alias":"DEEPSEEK_API_KEY"}"#),
    )
    .unwrap();
    insert_account_custody(
        &conn,
        &auth_ref,
        &account_id,
        CustodyKind::VaultRotationPool,
        "DEEPSEEK_API_KEY",
    )
    .unwrap();

    let surfaces = vec![
        serde_json::to_string(&get_provider_account(&conn, &account_id).unwrap().unwrap()).unwrap(),
        serde_json::to_string(&list_provider_accounts(&conn).unwrap()).unwrap(),
        serde_json::to_string(&list_provider_account_aliases(&conn, &account_id).unwrap()).unwrap(),
        serde_json::to_string(&list_provider_account_events(&conn, &account_id).unwrap()).unwrap(),
    ];

    for surface in &surfaces {
        for secret in [SECRET_A, SECRET_B] {
            assert!(!surface.contains(secret), "secret leaked into {surface}");
        }
        // Custody: neither the pointer's shape nor its target may appear on an
        // account-shaped surface. (`DEEPSEEK_API_KEY` is legitimately present
        // as an *alias* on the alias/event surfaces, so the assertion that
        // custody stays away is made against the account surfaces below.)
        for custody_marker in ["custody_kind", "custody_target", "vault_rotation_pool"] {
            assert!(
                !surface.contains(custody_marker),
                "custody marker {custody_marker} leaked into {surface}"
            );
        }
    }

    let account_surface = &surfaces[0];
    assert!(
        !account_surface.contains("DEEPSEEK_API_KEY"),
        "an account row must not carry the Vault name its secret lives under"
    );
    assert!(
        account_surface.contains(&auth_ref),
        "the opaque handle is the only custody-adjacent thing an account carries"
    );
}

/// The serialized key set of an account is pinned. A future field cannot be
/// added to `ProviderAccount` without a human deciding, in this test, that it
/// is public-safe — which is the only durable defence against custody or
/// secret material arriving by way of a "harmless" new column.
#[test]
fn account_serialized_field_set_is_frozen() {
    let conn = make_conn();
    let key = fp_key();
    let fingerprint =
        key.account_fingerprint_from_members([key.key_fingerprint("deepseek", SECRET_A)]);
    let (account_id, _) = seed_account(&conn, "deepseek", &fingerprint);

    let account = get_provider_account(&conn, &account_id).unwrap().unwrap();
    let value = serde_json::to_value(&account).unwrap();
    let mut keys: Vec<&str> = value
        .as_object()
        .expect("an account serializes as an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "account_class",
            "account_fingerprint",
            "account_id",
            "auth_mode",
            "auth_ref",
            "capabilities",
            "created_at",
            "credential_policy_ref",
            "provider_kind",
            "refresh_authority",
            "revision",
            "source_refs",
            "status",
            "updated_at",
        ]
    );

    let round_trip: ProviderAccount = serde_json::from_value(value).unwrap();
    assert_eq!(round_trip, account);
}

/// The other half of "the fingerprint key is not stored": computing
/// fingerprints for a whole set of accounts must leave the Vault listing and
/// its entry count exactly as they were. If `fp_key` were ever persisted as a
/// vault entry, this test would see an extra, unexplained secret — which is
/// precisely how an operator would have discovered it.
#[test]
fn fingerprinting_adds_no_vault_entries() {
    let conn = make_conn();

    vault_upsert_entry(
        &conn,
        &VaultEntry {
            name: "DEEPSEEK_API_KEY".to_string(),
            encrypted_value: "ciphertext".to_string(),
            nonce: "nonce".to_string(),
            ..VaultEntry::default()
        },
    )
    .unwrap();
    let before_count = vault_count_entries(&conn).unwrap();
    let before_names: Vec<String> = vault_list_entries(&conn)
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();

    let key = fp_key();
    for (kind, secret) in [("deepseek", SECRET_A), ("openai", SECRET_B)] {
        let fingerprint = key.account_fingerprint_from_members([key.key_fingerprint(kind, secret)]);
        let (account_id, auth_ref) = seed_account(&conn, kind, &fingerprint);
        insert_account_custody(
            &conn,
            &auth_ref,
            &account_id,
            CustodyKind::VaultEntry,
            "DEEPSEEK_API_KEY",
        )
        .unwrap();
    }

    assert_eq!(vault_count_entries(&conn).unwrap(), before_count);
    let after_names: Vec<String> = vault_list_entries(&conn)
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert_eq!(after_names, before_names);
    assert_eq!(list_provider_accounts(&conn).unwrap().len(), 2);
}

// ── Custody: resolvable, repointable, never merged into the account ──────────

/// The resolver (#1680 D5) is the only path from the opaque handle to Vault
/// layout, and repointing custody must leave the handle — and therefore every
/// upper-layer reference — untouched.
#[test]
fn custody_resolves_and_repoints_without_changing_the_auth_ref() {
    let conn = make_conn();
    let key = fp_key();
    let fingerprint =
        key.account_fingerprint_from_members([key.key_fingerprint("deepseek", SECRET_A)]);
    let (account_id, auth_ref) = seed_account(&conn, "deepseek", &fingerprint);
    insert_account_custody(
        &conn,
        &auth_ref,
        &account_id,
        CustodyKind::VaultEntry,
        "DEEPSEEK_API_KEY",
    )
    .unwrap();

    let resolved = resolve_auth_ref(&conn, &auth_ref).unwrap().unwrap();
    assert_eq!(resolved.account_id, account_id);
    assert_eq!(resolved.custody_kind, CustodyKind::VaultEntry);
    assert_eq!(resolved.custody_target, "DEEPSEEK_API_KEY");
    assert_eq!(resolved.revision, 1);

    // Restructure the pool: same account, same handle, new custody revision.
    let updated = update_custody_target(
        &conn,
        &auth_ref,
        CustodyKind::VaultRotationPool,
        "DEEPSEEK_API_KEY",
    )
    .unwrap()
    .unwrap();
    assert_eq!(updated.auth_ref, auth_ref);
    assert_eq!(updated.revision, 2);
    assert_eq!(updated.custody_kind, CustodyKind::VaultRotationPool);

    // A repeat of the same restructuring is a no-op, not a revision inflator.
    let repeated = update_custody_target(
        &conn,
        &auth_ref,
        CustodyKind::VaultRotationPool,
        "DEEPSEEK_API_KEY",
    )
    .unwrap()
    .unwrap();
    assert_eq!(repeated.revision, 2);

    // The account row is untouched by all of it.
    let account = get_provider_account(&conn, &account_id).unwrap().unwrap();
    assert_eq!(account.revision, 1);
    assert_eq!(account.auth_ref.as_deref(), Some(auth_ref.as_str()));
    assert_eq!(
        get_account_custody(&conn, &account_id)
            .unwrap()
            .unwrap()
            .custody_target,
        "DEEPSEEK_API_KEY"
    );
}

#[test]
fn an_unknown_auth_ref_resolves_to_nothing_rather_than_guessing() {
    let conn = make_conn();
    assert!(resolve_auth_ref(&conn, "va1:deadbeef").unwrap().is_none());
    assert!(update_custody_target(
        &conn,
        "va1:deadbeef",
        CustodyKind::VaultEntry,
        "DEEPSEEK_API_KEY"
    )
    .unwrap()
    .is_none());
}

// ── Table-level contracts: closed sets, append-only, uniqueness ──────────────

/// Every `AuthMode` this build knows must satisfy the DDL's `CHECK`, and a
/// mode outside it must be refused by the database rather than stored. The two
/// halves together are what make the closed set real instead of decorative.
#[test]
fn auth_mode_check_clause_matches_the_type_exactly() {
    let conn = make_conn();

    for (index, mode) in AuthMode::ALL.iter().enumerate() {
        let account_id = mint_account_id();
        let auth_ref = mode.has_tachi_held_credential().then(mint_auth_ref);
        insert_provider_account(
            &conn,
            &NewProviderAccount {
                account_id: account_id.clone(),
                provider_kind: "deepseek".to_string(),
                auth_mode: *mode,
                auth_ref,
                account_fingerprint: format!("fpa1:kv:{index:012x}"),
                account_class: AccountClass::ModelApi,
                capabilities: Vec::new(),
                credential_policy_ref: None,
                refresh_authority: "none".to_string(),
                status: "active".to_string(),
                source_refs: Vec::new(),
            },
        )
        .unwrap_or_else(|err| panic!("auth_mode {} must be storable: {err}", mode.as_str()));
        assert_eq!(
            get_provider_account(&conn, &account_id)
                .unwrap()
                .unwrap()
                .auth_mode,
            *mode
        );
    }

    let refused = conn.execute(
        "INSERT INTO provider_accounts
            (account_id, provider_kind, auth_mode, account_fingerprint, created_at, updated_at)
         VALUES ('rogue', 'deepseek', 'password_login', 'fpa1:kv:0123456789ab', '', '')",
        [],
    );
    assert!(
        refused.is_err(),
        "the database must refuse an auth_mode outside the closed set"
    );

    let refused_custody = conn.execute(
        "INSERT INTO account_custody
            (auth_ref, account_id, custody_kind, custody_target, revision, updated_at)
         VALUES ('va1:x', 'rogue', 'keychain', 'DEEPSEEK_API_KEY', 1, '')",
        [],
    );
    assert!(
        refused_custody.is_err(),
        "the database must refuse a custody_kind outside the closed set"
    );
}

/// An auth mode with no Tachi-held credential must not carry a handle to
/// custody that cannot exist, and one that does hold a credential must not be
/// storable without a handle. Both directions are refused before SQL.
#[test]
fn auth_ref_presence_must_agree_with_the_auth_mode() {
    let conn = make_conn();

    let missing = insert_provider_account(
        &conn,
        &NewProviderAccount {
            account_id: mint_account_id(),
            provider_kind: "deepseek".to_string(),
            auth_mode: AuthMode::ApiKeyPool,
            auth_ref: None,
            account_fingerprint: "fpa1:kv:0123456789ab".to_string(),
            account_class: AccountClass::ModelApi,
            capabilities: Vec::new(),
            credential_policy_ref: None,
            refresh_authority: "none".to_string(),
            status: "active".to_string(),
            source_refs: Vec::new(),
        },
    );
    assert!(
        missing.is_err(),
        "an api_key_pool account needs an auth_ref"
    );

    let spurious = insert_provider_account(
        &conn,
        &NewProviderAccount {
            account_id: mint_account_id(),
            provider_kind: "ollama".to_string(),
            auth_mode: AuthMode::LocalNoAuth,
            auth_ref: Some(mint_auth_ref()),
            account_fingerprint: "fpa1:kv:0123456789ab".to_string(),
            account_class: AccountClass::ModelApi,
            capabilities: Vec::new(),
            credential_policy_ref: None,
            refresh_authority: "none".to_string(),
            status: "active".to_string(),
            source_refs: Vec::new(),
        },
    );
    assert!(
        spurious.is_err(),
        "a credential-free auth mode must not carry a custody handle"
    );
}

/// Aliases are append/retire, never delete: retiring keeps the row and its
/// `first_seen`, and observing the name again revives it rather than minting a
/// second row with a fresh history.
#[test]
fn aliases_retire_and_revive_without_losing_first_seen() {
    let conn = make_conn();
    let key = fp_key();
    let fingerprint =
        key.account_fingerprint_from_members([key.key_fingerprint("deepseek", SECRET_A)]);
    let (account_id, _) = seed_account(&conn, "deepseek", &fingerprint);

    record_provider_account_alias(&conn, &account_id, "DEEPSEEK_API_KEY", "config_env").unwrap();
    let first_seen = list_provider_account_aliases(&conn, &account_id).unwrap()[0]
        .first_seen
        .clone();

    assert_eq!(
        record_provider_account_alias(&conn, &account_id, "DEEPSEEK_API_KEY", "config_env")
            .unwrap(),
        AliasObservation::Refreshed
    );
    // Snapshot the last *observation* time immediately before retiring, so the
    // assertion below is about retirement not stamping it, not about clock
    // resolution.
    let last_seen = list_provider_account_aliases(&conn, &account_id).unwrap()[0]
        .last_seen
        .clone();
    assert!(retire_provider_account_alias(&conn, &account_id, "DEEPSEEK_API_KEY").unwrap());
    assert!(
        !retire_provider_account_alias(&conn, &account_id, "DEEPSEEK_API_KEY").unwrap(),
        "retiring an already-retired alias changes nothing"
    );

    let after_retire = list_provider_account_aliases(&conn, &account_id).unwrap();
    let retired = &after_retire[0];
    assert!(retired.retired);
    assert_eq!(retired.first_seen, first_seen, "history is not rewritten");
    assert_eq!(
        retired.last_seen, last_seen,
        "retiring is the moment we stopped seeing a name, not a sighting — \
         stamping last_seen here would make the column mean nothing"
    );

    assert_eq!(
        record_provider_account_alias(&conn, &account_id, "DEEPSEEK_API_KEY", "vault_entry")
            .unwrap(),
        AliasObservation::Revived
    );
    let aliases = list_provider_account_aliases(&conn, &account_id).unwrap();
    assert_eq!(aliases.len(), 1, "a revived alias is not a second row");
    assert!(!aliases[0].retired);
    assert_eq!(aliases[0].first_seen, first_seen);
    assert_eq!(aliases[0].source_kind, "vault_entry");
}

/// One account, one custody row, one handle: the uniqueness constraints that
/// stop two accounts from quietly sharing a Vault secret behind two handles.
#[test]
fn custody_and_auth_ref_are_one_to_one() {
    let conn = make_conn();
    let key = fp_key();
    let fingerprint =
        key.account_fingerprint_from_members([key.key_fingerprint("deepseek", SECRET_A)]);
    let (first, first_ref) = seed_account(&conn, "deepseek", &fingerprint);
    let (second, second_ref) = seed_account(&conn, "openai", &fingerprint);

    insert_account_custody(
        &conn,
        &first_ref,
        &first,
        CustodyKind::VaultEntry,
        "DEEPSEEK_API_KEY",
    )
    .unwrap();

    assert!(
        insert_account_custody(
            &conn,
            &first_ref,
            &second,
            CustodyKind::VaultEntry,
            "OPENAI_API_KEY"
        )
        .is_err(),
        "one auth_ref must not map to two accounts"
    );
    insert_account_custody(
        &conn,
        &second_ref,
        &first,
        CustodyKind::VaultEntry,
        "OPENAI_API_KEY",
    )
    .expect_err("one account must not have two custody rows");
}

/// Creating an account writes its own creation event, so no account exists
/// whose origin is unexplained; events then only ever accumulate.
#[test]
fn every_account_starts_with_a_creation_event_and_events_only_append() {
    let conn = make_conn();
    let key = fp_key();
    let fingerprint =
        key.account_fingerprint_from_members([key.key_fingerprint("deepseek", SECRET_A)]);
    let (account_id, _) = seed_account(&conn, "deepseek", &fingerprint);

    let created = list_provider_account_events(&conn, &account_id).unwrap();
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].event_kind, EVENT_KIND_ACCOUNT_CREATED);
    assert_eq!(created[0].revision, 1);
    assert_eq!(created[0].evidence, "{}");

    append_provider_account_event(
        &conn,
        &NewProviderAccountEvent::new(account_id.as_str(), 1, EVENT_KIND_ALIAS_OBSERVED),
    )
    .unwrap();
    let after = list_provider_account_events(&conn, &account_id).unwrap();
    assert_eq!(after.len(), 2);
    assert!(
        after[0].id < after[1].id,
        "events must come back in append order"
    );
}

/// A duplicate `account_id` is refused rather than silently overwriting an
/// existing account — the id is minted once and never reused.
#[test]
fn account_ids_cannot_be_reused() {
    let conn = make_conn();
    let key = fp_key();
    let fingerprint =
        key.account_fingerprint_from_members([key.key_fingerprint("deepseek", SECRET_A)]);
    let (account_id, _) = seed_account(&conn, "deepseek", &fingerprint);

    let duplicate = insert_provider_account(
        &conn,
        &NewProviderAccount::api_key_pool(
            account_id,
            "deepseek",
            mint_auth_ref(),
            fingerprint.clone(),
            AccountClass::ModelApi,
        ),
    );
    assert!(duplicate.is_err());
}

/// Recording a fingerprint for an account that does not exist is a typed
/// not-found, not a silent no-op that would let a caller believe it wrote.
#[test]
fn recording_a_fingerprint_for_an_unknown_account_is_an_error() {
    let conn = make_conn();
    let err = record_account_fingerprint(
        &conn,
        "01ARZ3NDEKTSV4RRFFQ69G5FAV",
        "fpa1:kv:0123456789ab",
        EVENT_KIND_FINGERPRINT_OBSERVED,
        None,
        "{}",
    )
    .expect_err("an unknown account must not be silently ignored");
    assert!(format!("{err}").contains("does not exist"), "{err}");
}
