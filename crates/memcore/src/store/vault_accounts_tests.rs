//! Apply-side discriminators for tachi#1680 PR-C.
//!
//! The design's discrimination list for this slice, one test each: five tamper
//! shapes that must each produce **zero writes** (6), idempotent replay with
//! stable revisions (10), and the ruling that an alias-family merge suggestion
//! is never authority to fuse two account identities.
//!
//! Every refusal test asserts against a full before/after snapshot of the four
//! provider-account tables rather than against one row, because "zero writes"
//! is a statement about the database, not about the row the test was thinking
//! about.

use crate::db::vault_accounts::{
    get_account_custody, get_provider_account, insert_account_custody, insert_provider_account,
    list_provider_account_aliases, list_provider_account_events, list_provider_accounts,
    record_provider_account_alias,
};
use crate::error::{MemoryError, ProviderPlanRefusal};
use crate::vault::accounts::{
    AccountClass, AuthMode, CustodyKind, NewProviderAccount, EVENT_KIND_ACCOUNT_MERGED,
    EVENT_KIND_FINGERPRINT_OBSERVED, EVENT_KIND_PLAN_NOOP,
};
use crate::vault::apply::{
    plan_digest, AccountAction, AccountBinding, AliasSighting, BoundAccountPlan, CustodyBinding,
    MergeConfirmation, PlanBindings, PlanSourceDigests, PlannedAccount, SourceBinding,
    VaultEntryBinding,
};
use crate::vault::{VaultEntry, SECRET_TYPE_API_KEY};
use crate::MemoryStore;

use std::collections::HashMap;

const ACCOUNT_A: &str = "01JBACCOUNTAAAAAAAAAAAAAAA";
const ACCOUNT_B: &str = "01JBACCOUNTBBBBBBBBBBBBBBB";
const AUTH_REF_A: &str = "va1:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const AUTH_REF_B: &str = "va1:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const SOURCE_ID: &str = "env_file:/fixture/config.env";
const SOURCE_SHA: &str = "1111111111111111111111111111111111111111111111111111111111111111";

/// The reconcile pipeline's source re-reader, stubbed. Real callers hash files;
/// nothing about apply depends on where the bytes came from.
struct FixedSources(HashMap<String, String>);

impl FixedSources {
    fn with(source_id: &str, sha256: &str) -> Self {
        Self(HashMap::from([(source_id.to_string(), sha256.to_string())]))
    }

    fn empty() -> Self {
        Self(HashMap::new())
    }
}

impl PlanSourceDigests for FixedSources {
    fn current_digest(&self, source_id: &str) -> Result<Option<String>, String> {
        Ok(self.0.get(source_id).cloned())
    }
}

fn store() -> MemoryStore {
    MemoryStore::open_in_memory().expect("open in-memory store")
}

fn seed_vault_entry(store: &MemoryStore, name: &str, updated_at: &str) {
    store
        .vault_upsert_entry(&VaultEntry {
            name: name.to_string(),
            encrypted_value: format!("{name}-ciphertext"),
            nonce: format!("{name}-nonce"),
            secret_type: SECRET_TYPE_API_KEY.to_string(),
            description: "reconcile fixture".to_string(),
            allowed_agents: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: updated_at.to_string(),
            accessed_at: String::new(),
            access_count: 0,
        })
        .expect("seed vault entry");
}

fn seed_account(store: &MemoryStore, account_id: &str, auth_ref: &str, alias: &str) {
    let conn = store.connection();
    insert_provider_account(
        conn,
        &NewProviderAccount::api_key_pool(
            account_id,
            "siliconflow",
            auth_ref,
            "fpa1:kv:aaaaaaaaaaaa",
            AccountClass::ModelApi,
        ),
    )
    .expect("seed account");
    insert_account_custody(
        conn,
        auth_ref,
        account_id,
        CustodyKind::VaultEntry,
        "SILICONFLOW_API_KEY",
    )
    .expect("seed custody");
    record_provider_account_alias(conn, account_id, alias, "vault_entry").expect("seed alias");
}

/// Everything the four provider-account tables hold, as a comparable string.
/// A refusal must leave this byte-identical.
fn snapshot(store: &MemoryStore) -> String {
    let conn = store.connection();
    let mut out = String::new();
    for account in list_provider_accounts(conn).expect("list accounts") {
        out.push_str(&serde_json::to_string(&account).expect("encode account"));
        out.push('\n');
        for alias in list_provider_account_aliases(conn, &account.account_id).expect("aliases") {
            out.push_str(&serde_json::to_string(&alias).expect("encode alias"));
            out.push('\n');
        }
        for event in list_provider_account_events(conn, &account.account_id).expect("events") {
            out.push_str(&serde_json::to_string(&event).expect("encode event"));
            out.push('\n');
        }
        // `AccountCustody` is deliberately neither `Serialize` nor plainly
        // `Debug`; its redacting `Debug` still carries the revision, which is
        // the part a drift test needs to see move.
        out.push_str(&format!(
            "{:?}\n",
            get_account_custody(conn, &account.account_id).expect("custody")
        ));
    }
    out
}

fn planned_account(account_id: &str, auth_ref: &str, alias: &str) -> PlannedAccount {
    PlannedAccount {
        account_id: account_id.to_string(),
        provider_kind: "siliconflow".to_string(),
        auth_mode: AuthMode::ApiKeyPool,
        account_class: AccountClass::ModelApi,
        account_fingerprint: "fpa1:kv:cccccccccccc".to_string(),
        auth_ref: auth_ref.to_string(),
        custody_kind: CustodyKind::VaultEntry,
        custody_logical_name: alias.to_string(),
        capabilities: Vec::new(),
        credential_policy_ref: None,
        source_refs: vec![SOURCE_ID.to_string()],
        aliases: vec![AliasSighting {
            alias_name: alias.to_string(),
            source_kind: "vault_entry".to_string(),
        }],
        evidence: "{\"members\":1}".to_string(),
    }
}

/// A plan that creates one account from one Vault-held credential, bound to the
/// source file it was discovered in and the Vault row it was read from.
fn create_plan() -> BoundAccountPlan {
    BoundAccountPlan {
        bindings: PlanBindings {
            sources: vec![SourceBinding {
                source_id: SOURCE_ID.to_string(),
                sha256: SOURCE_SHA.to_string(),
            }],
            vault_entries: vec![VaultEntryBinding {
                entry_name: "SILICONFLOW_API_KEY".to_string(),
                updated_at: Some("2026-08-01T00:00:00Z".to_string()),
            }],
            accounts: Vec::new(),
            custody: Vec::new(),
        },
        actions: vec![AccountAction::CreateAccount {
            account: Box::new(planned_account(
                ACCOUNT_A,
                AUTH_REF_A,
                "SILICONFLOW_API_KEY",
            )),
        }],
    }
}

fn apply(
    store: &mut MemoryStore,
    plan: &BoundAccountPlan,
    sources: &dyn PlanSourceDigests,
) -> Result<crate::store::vault_accounts::AccountApplyReport, MemoryError> {
    let digest = plan_digest(plan);
    store.apply_provider_account_plan(plan, &digest, sources)
}

fn refusal(err: &MemoryError) -> ProviderPlanRefusal {
    match err {
        MemoryError::ProviderAccountPlanRefused { reason, .. } => *reason,
        other => panic!("expected a typed plan refusal, got: {other}"),
    }
}

// ── The happy path the discriminators are measured against ──────────────────

#[test]
fn apply_creates_the_account_its_custody_and_its_aliases() {
    let mut store = store();
    seed_vault_entry(&store, "SILICONFLOW_API_KEY", "2026-08-01T00:00:00Z");
    let plan = create_plan();

    let report = apply(
        &mut store,
        &plan,
        &FixedSources::with(SOURCE_ID, SOURCE_SHA),
    )
    .expect("plan applies");

    assert!(report.changed);
    assert_eq!(report.accounts_created, vec![ACCOUNT_A.to_string()]);
    assert_eq!(report.aliases_created.len(), 1);
    assert!(report.noop_event_ids.is_empty());

    let conn = store.connection();
    let account = get_provider_account(conn, ACCOUNT_A)
        .expect("read account")
        .expect("account exists");
    assert_eq!(account.revision, 1);
    assert_eq!(account.auth_ref.as_deref(), Some(AUTH_REF_A));

    let custody = get_account_custody(conn, ACCOUNT_A)
        .expect("read custody")
        .expect("custody exists");
    assert_eq!(custody.custody_target, "SILICONFLOW_API_KEY");

    let kinds: Vec<String> = list_provider_account_events(conn, ACCOUNT_A)
        .expect("events")
        .into_iter()
        .map(|event| event.event_kind)
        .collect();
    assert!(
        kinds
            .iter()
            .any(|kind| kind == EVENT_KIND_FINGERPRINT_OBSERVED),
        "the creating plan's evidence must be recorded: {kinds:?}"
    );
}

// ── Discrimination 10: replay is idempotent, revisions are stable ───────────

/// The same plan applied twice: the second pass verifies in full, finds the
/// account already exactly as planned, advances no revision, and says so with
/// a `plan_noop` event. This is the property that makes reconcile safe to run
/// from a cron job or a nervous operator.
#[test]
fn replaying_an_unchanged_plan_changes_nothing_and_holds_the_revision() {
    let mut store = store();
    seed_vault_entry(&store, "SILICONFLOW_API_KEY", "2026-08-01T00:00:00Z");
    let plan = create_plan();
    let sources = FixedSources::with(SOURCE_ID, SOURCE_SHA);

    apply(&mut store, &plan, &sources).expect("first apply");
    let revision_after_first = get_provider_account(store.connection(), ACCOUNT_A)
        .expect("read")
        .expect("exists")
        .revision;
    let aliases_after_first =
        list_provider_account_aliases(store.connection(), ACCOUNT_A).expect("aliases");

    let replay = apply(&mut store, &plan, &sources).expect("replay applies");
    assert!(!replay.changed, "replay must report no change");
    assert!(replay.accounts_created.is_empty());
    assert!(replay.aliases_created.is_empty());
    assert!(replay.fingerprints_advanced.is_empty());
    assert_eq!(
        replay.noop_event_ids.len(),
        1,
        "one plan_noop event for the one account the plan touches"
    );

    let account = get_provider_account(store.connection(), ACCOUNT_A)
        .expect("read")
        .expect("exists");
    assert_eq!(
        account.revision, revision_after_first,
        "an unchanged replay must not move the revision"
    );

    let aliases = list_provider_account_aliases(store.connection(), ACCOUNT_A).expect("aliases");
    assert_eq!(
        aliases.len(),
        aliases_after_first.len(),
        "replay must not add alias rows"
    );
    assert_eq!(
        aliases[0].first_seen, aliases_after_first[0].first_seen,
        "first_seen is history and never moves"
    );

    let noop_events: Vec<_> = list_provider_account_events(store.connection(), ACCOUNT_A)
        .expect("events")
        .into_iter()
        .filter(|event| event.event_kind == EVENT_KIND_PLAN_NOOP)
        .collect();
    assert_eq!(noop_events.len(), 1);
    assert_eq!(noop_events[0].revision, revision_after_first);
    assert_eq!(
        noop_events[0].plan_digest.as_deref(),
        Some(plan_digest(&plan).as_str())
    );
}

// ── Discrimination 6: five tampers, five refusals, zero writes each ─────────

/// Tamper 1 — the plan. An action edited after the digest was computed.
#[test]
fn a_tampered_action_is_refused_with_zero_writes() {
    let mut store = store();
    seed_vault_entry(&store, "SILICONFLOW_API_KEY", "2026-08-01T00:00:00Z");
    let plan = create_plan();
    let honest_digest = plan_digest(&plan);

    let mut tampered = plan.clone();
    if let AccountAction::CreateAccount { account } = &mut tampered.actions[0] {
        account.account_fingerprint = "fpa1:kv:deadbeefdead".to_string();
    }

    let before = snapshot(&store);
    let err = store
        .apply_provider_account_plan(
            &tampered,
            &honest_digest,
            &FixedSources::with(SOURCE_ID, SOURCE_SHA),
        )
        .expect_err("a tampered action must be refused");
    assert_eq!(refusal(&err), ProviderPlanRefusal::PlanDigestMismatch);
    assert_eq!(snapshot(&store), before, "refusal must write nothing");
}

/// Tamper 2 — the digest. Content untouched, the approved digest swapped.
#[test]
fn a_tampered_digest_is_refused_with_zero_writes() {
    let mut store = store();
    seed_vault_entry(&store, "SILICONFLOW_API_KEY", "2026-08-01T00:00:00Z");
    let plan = create_plan();

    let before = snapshot(&store);
    let err = store
        .apply_provider_account_plan(
            &plan,
            "pd1:0000000000000000000000000000000000000000000000000000000000000000",
            &FixedSources::with(SOURCE_ID, SOURCE_SHA),
        )
        .expect_err("a tampered digest must be refused");
    assert_eq!(refusal(&err), ProviderPlanRefusal::PlanDigestMismatch);
    assert_eq!(snapshot(&store), before, "refusal must write nothing");
}

/// Tamper 3 — `auth_ref`. The account now points at different custody than the
/// plan was decided against.
#[test]
fn a_drifted_auth_ref_is_refused_with_zero_writes() {
    let mut store = store();
    seed_account(&store, ACCOUNT_A, AUTH_REF_A, "SILICONFLOW_API_KEY");

    let plan = BoundAccountPlan {
        bindings: PlanBindings {
            accounts: vec![AccountBinding {
                account_id: ACCOUNT_A.to_string(),
                revision: 1,
                // What the plan bound is not what the account carries.
                auth_ref: Some(AUTH_REF_B.to_string()),
                credential_policy_ref: None,
            }],
            ..PlanBindings::default()
        },
        actions: vec![AccountAction::ObserveAlias {
            account_id: ACCOUNT_A.to_string(),
            alias_name: "EXTRACT_API_KEY".to_string(),
            source_kind: "config_env".to_string(),
        }],
    };

    let before = snapshot(&store);
    let err = apply(&mut store, &plan, &FixedSources::empty()).expect_err("auth_ref drift");
    assert_eq!(refusal(&err), ProviderPlanRefusal::AuthRefDrift);
    assert_eq!(snapshot(&store), before, "refusal must write nothing");
}

/// Tamper 4 — the credential policy. The ref the plan was authorized under is
/// not the ref in force.
#[test]
fn a_drifted_credential_policy_ref_is_refused_with_zero_writes() {
    let mut store = store();
    seed_account(&store, ACCOUNT_A, AUTH_REF_A, "SILICONFLOW_API_KEY");

    let plan = BoundAccountPlan {
        bindings: PlanBindings {
            accounts: vec![AccountBinding {
                account_id: ACCOUNT_A.to_string(),
                revision: 1,
                auth_ref: Some(AUTH_REF_A.to_string()),
                // The seeded account carries no policy ref at all.
                credential_policy_ref: Some("cp1:default@3".to_string()),
            }],
            ..PlanBindings::default()
        },
        actions: vec![AccountAction::ObserveAlias {
            account_id: ACCOUNT_A.to_string(),
            alias_name: "EXTRACT_API_KEY".to_string(),
            source_kind: "config_env".to_string(),
        }],
    };

    let before = snapshot(&store);
    let err = apply(&mut store, &plan, &FixedSources::empty()).expect_err("policy drift");
    assert_eq!(refusal(&err), ProviderPlanRefusal::PolicyRefDrift);
    assert_eq!(snapshot(&store), before, "refusal must write nothing");
}

/// Tamper 5 — the source revision. The config file the plan reasoned from has
/// been rewritten since.
#[test]
fn a_drifted_source_digest_is_refused_with_zero_writes() {
    let mut store = store();
    seed_vault_entry(&store, "SILICONFLOW_API_KEY", "2026-08-01T00:00:00Z");
    let plan = create_plan();

    let before = snapshot(&store);
    let err = apply(
        &mut store,
        &plan,
        &FixedSources::with(SOURCE_ID, &"2".repeat(64)),
    )
    .expect_err("source drift");
    assert_eq!(refusal(&err), ProviderPlanRefusal::SourceDigestMismatch);
    assert_eq!(snapshot(&store), before, "refusal must write nothing");

    // A vanished source is drift too: absent evidence is not unchanged evidence.
    let err = apply(&mut store, &plan, &FixedSources::empty()).expect_err("source vanished");
    assert_eq!(refusal(&err), ProviderPlanRefusal::SourceDigestMismatch);
    assert_eq!(snapshot(&store), before, "refusal must write nothing");
}

/// The account-revision binding: somebody else advanced the account between
/// plan and apply.
#[test]
fn a_drifted_account_revision_is_refused_with_zero_writes() {
    let mut store = store();
    seed_account(&store, ACCOUNT_A, AUTH_REF_A, "SILICONFLOW_API_KEY");

    let plan = BoundAccountPlan {
        bindings: PlanBindings {
            accounts: vec![AccountBinding {
                account_id: ACCOUNT_A.to_string(),
                revision: 7,
                auth_ref: Some(AUTH_REF_A.to_string()),
                credential_policy_ref: None,
            }],
            ..PlanBindings::default()
        },
        actions: vec![AccountAction::RecordFingerprint {
            account_id: ACCOUNT_A.to_string(),
            account_fingerprint: "fpa1:kv:ffffffffffff".to_string(),
            event_kind: EVENT_KIND_FINGERPRINT_OBSERVED.to_string(),
            evidence: "{}".to_string(),
        }],
    };

    let before = snapshot(&store);
    let err = apply(&mut store, &plan, &FixedSources::empty()).expect_err("revision drift");
    assert_eq!(refusal(&err), ProviderPlanRefusal::AccountRevisionDrift);
    assert_eq!(snapshot(&store), before, "refusal must write nothing");
}

/// The Vault-entry binding: the credential itself was rewritten under the plan.
#[test]
fn a_rewritten_vault_entry_is_refused_with_zero_writes() {
    let mut store = store();
    seed_vault_entry(&store, "SILICONFLOW_API_KEY", "2026-08-09T00:00:00Z");
    let plan = create_plan(); // binds updated_at = 2026-08-01

    let before = snapshot(&store);
    let err = apply(
        &mut store,
        &plan,
        &FixedSources::with(SOURCE_ID, SOURCE_SHA),
    )
    .expect_err("vault entry drift");
    assert_eq!(refusal(&err), ProviderPlanRefusal::VaultEntryDrift);
    assert_eq!(snapshot(&store), before, "refusal must write nothing");
}

/// The custody binding: custody was repointed between plan and apply.
#[test]
fn a_drifted_custody_revision_is_refused_with_zero_writes() {
    let mut store = store();
    seed_account(&store, ACCOUNT_A, AUTH_REF_A, "SILICONFLOW_API_KEY");

    let plan = BoundAccountPlan {
        bindings: PlanBindings {
            accounts: vec![AccountBinding {
                account_id: ACCOUNT_A.to_string(),
                revision: 1,
                auth_ref: Some(AUTH_REF_A.to_string()),
                credential_policy_ref: None,
            }],
            custody: vec![CustodyBinding {
                auth_ref: AUTH_REF_A.to_string(),
                revision: 4,
            }],
            ..PlanBindings::default()
        },
        actions: vec![AccountAction::RepointCustody {
            auth_ref: AUTH_REF_A.to_string(),
            custody_kind: CustodyKind::VaultRotationPool,
            custody_logical_name: "SILICONFLOW_API_KEY".to_string(),
        }],
    };

    let before = snapshot(&store);
    let err = apply(&mut store, &plan, &FixedSources::empty()).expect_err("custody drift");
    assert_eq!(refusal(&err), ProviderPlanRefusal::CustodyRevisionDrift);
    assert_eq!(snapshot(&store), before, "refusal must write nothing");
}

/// A structural refusal that is not drift: an action nobody bound a
/// precondition for. An unbound account is an unverified write, so it is
/// refused even when the world happens to match.
#[test]
fn an_action_on_an_unbound_account_is_refused_with_zero_writes() {
    let mut store = store();
    seed_account(&store, ACCOUNT_A, AUTH_REF_A, "SILICONFLOW_API_KEY");

    let plan = BoundAccountPlan {
        bindings: PlanBindings::default(),
        actions: vec![AccountAction::ObserveAlias {
            account_id: ACCOUNT_A.to_string(),
            alias_name: "EXTRACT_API_KEY".to_string(),
            source_kind: "config_env".to_string(),
        }],
    };

    let before = snapshot(&store);
    let err = apply(&mut store, &plan, &FixedSources::empty()).expect_err("unbound account");
    assert_eq!(refusal(&err), ProviderPlanRefusal::UnboundAccount);
    assert_eq!(snapshot(&store), before, "refusal must write nothing");
}

// ── The merge ruling: evidence proposes, only an operator disposes ──────────

/// A merge action with no confirmation is refused outright, and nothing else
/// in the plan runs either — a plan is applied whole or not at all, so an
/// unconfirmed merge cannot be smuggled in behind actions that would have been
/// fine on their own.
#[test]
fn an_unconfirmed_merge_is_refused_and_the_rest_of_the_plan_does_not_run() {
    let mut store = store();
    seed_account(&store, ACCOUNT_A, AUTH_REF_A, "SILICONFLOW_API_KEY");
    seed_account(&store, ACCOUNT_B, AUTH_REF_B, "EXTRACT_API_KEY");

    let plan = BoundAccountPlan {
        bindings: PlanBindings {
            accounts: vec![
                AccountBinding {
                    account_id: ACCOUNT_A.to_string(),
                    revision: 1,
                    auth_ref: Some(AUTH_REF_A.to_string()),
                    credential_policy_ref: None,
                },
                AccountBinding {
                    account_id: ACCOUNT_B.to_string(),
                    revision: 1,
                    auth_ref: Some(AUTH_REF_B.to_string()),
                    credential_policy_ref: None,
                },
            ],
            ..PlanBindings::default()
        },
        actions: vec![
            AccountAction::ObserveAlias {
                account_id: ACCOUNT_A.to_string(),
                alias_name: "SUMMARY_API_KEY".to_string(),
                source_kind: "config_env".to_string(),
            },
            AccountAction::MergeAccounts {
                from_account_id: ACCOUNT_B.to_string(),
                into_account_id: ACCOUNT_A.to_string(),
                confirmation: None,
            },
        ],
    };

    let before = snapshot(&store);
    let err = apply(&mut store, &plan, &FixedSources::empty()).expect_err("unconfirmed merge");
    assert_eq!(refusal(&err), ProviderPlanRefusal::UnconfirmedMerge);
    assert_eq!(
        snapshot(&store),
        before,
        "the alias observation ahead of the merge must not have run either"
    );
}

/// A confirmed merge is the only path that fuses two identities, and it moves
/// the names before it retires the source so no alias is ever reachable from
/// neither side.
#[test]
fn a_confirmed_merge_moves_the_aliases_then_retires_the_source() {
    let mut store = store();
    seed_account(&store, ACCOUNT_A, AUTH_REF_A, "SILICONFLOW_API_KEY");
    seed_account(&store, ACCOUNT_B, AUTH_REF_B, "EXTRACT_API_KEY");

    let plan = BoundAccountPlan {
        bindings: PlanBindings {
            accounts: vec![
                AccountBinding {
                    account_id: ACCOUNT_A.to_string(),
                    revision: 1,
                    auth_ref: Some(AUTH_REF_A.to_string()),
                    credential_policy_ref: None,
                },
                AccountBinding {
                    account_id: ACCOUNT_B.to_string(),
                    revision: 1,
                    auth_ref: Some(AUTH_REF_B.to_string()),
                    credential_policy_ref: None,
                },
            ],
            ..PlanBindings::default()
        },
        actions: vec![AccountAction::MergeAccounts {
            from_account_id: ACCOUNT_B.to_string(),
            into_account_id: ACCOUNT_A.to_string(),
            confirmation: Some(MergeConfirmation {
                confirmed_by: "operator:kc".to_string(),
                confirmed_at: "2026-08-10T00:00:00Z".to_string(),
            }),
        }],
    };

    let report = apply(&mut store, &plan, &FixedSources::empty()).expect("confirmed merge applies");
    assert!(report.changed);
    assert_eq!(report.accounts_retired.len(), 1);

    let conn = store.connection();
    let target_aliases: Vec<String> = list_provider_account_aliases(conn, ACCOUNT_A)
        .expect("target aliases")
        .into_iter()
        .filter(|alias| !alias.retired)
        .map(|alias| alias.alias_name)
        .collect();
    assert!(
        target_aliases.contains(&"EXTRACT_API_KEY".to_string()),
        "the merged name must be reachable from the target: {target_aliases:?}"
    );

    let source = get_provider_account(conn, ACCOUNT_B)
        .expect("read source")
        .expect("source row survives the merge");
    assert_eq!(source.status, "retired");
    assert!(
        list_provider_account_aliases(conn, ACCOUNT_B)
            .expect("source aliases")
            .iter()
            .all(|alias| alias.retired),
        "the source keeps its alias rows, retired"
    );

    // Both ends carry the merge event: a merge read from one side is unreadable.
    for account_id in [ACCOUNT_A, ACCOUNT_B] {
        assert!(
            list_provider_account_events(conn, account_id)
                .expect("events")
                .iter()
                .any(|event| event.event_kind == EVENT_KIND_ACCOUNT_MERGED),
            "{account_id} must carry the merge event"
        );
    }
}

/// A merge action naming an account that no longer exists is refused rather
/// than half-applied.
#[test]
fn a_merge_into_a_missing_account_is_refused_with_zero_writes() {
    let mut store = store();
    seed_account(&store, ACCOUNT_B, AUTH_REF_B, "EXTRACT_API_KEY");

    let plan = BoundAccountPlan {
        bindings: PlanBindings {
            accounts: vec![
                AccountBinding {
                    account_id: ACCOUNT_B.to_string(),
                    revision: 1,
                    auth_ref: Some(AUTH_REF_B.to_string()),
                    credential_policy_ref: None,
                },
                AccountBinding {
                    account_id: ACCOUNT_A.to_string(),
                    revision: 1,
                    auth_ref: Some(AUTH_REF_A.to_string()),
                    credential_policy_ref: None,
                },
            ],
            ..PlanBindings::default()
        },
        actions: vec![AccountAction::MergeAccounts {
            from_account_id: ACCOUNT_B.to_string(),
            into_account_id: ACCOUNT_A.to_string(),
            confirmation: Some(MergeConfirmation {
                confirmed_by: "operator:kc".to_string(),
                confirmed_at: "2026-08-10T00:00:00Z".to_string(),
            }),
        }],
    };

    let before = snapshot(&store);
    let err = apply(&mut store, &plan, &FixedSources::empty()).expect_err("missing merge target");
    assert_eq!(refusal(&err), ProviderPlanRefusal::UnknownAccount);
    assert_eq!(snapshot(&store), before, "refusal must write nothing");
}
