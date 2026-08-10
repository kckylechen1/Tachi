//! Pipeline discriminators for tachi#1680 PR-C.
//!
//! The store-layer refusals are pinned in `memcore::store::vault_accounts_tests`.
//! What is pinned here is everything that only exists once discovery, the
//! registry and the Vault are in the picture: that a script-shaped candidate is
//! data and is never run, that two names of one registry family holding
//! different values stay two accounts, that the same names holding one value
//! collapse into one account with two aliases, that planning writes nothing,
//! and that plan → apply → replay is idempotent through the artifact file.

use super::*;

use memcore::MemoryStore;

const SILICONFLOW_VALUE: &str = "sk-tachi-1680-siliconflow-AAAA";
const EXTRACT_VALUE: &str = "sk-tachi-1680-extract-BBBB";
const MASTER_SALT: &[u8] = b"reconcile-fixture-salt";
const MASTER_PASSWORD: &str = "reconcile-fixture-password";

struct Fixture {
    home: tempfile::TempDir,
    cwd: tempfile::TempDir,
    db_path: PathBuf,
    key: crate::vault_crypto::DerivedVaultKey,
}

impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("home");
        let cwd = tempfile::tempdir().expect("cwd");
        let db_path = home.path().join(".tachi").join("global").join("memory.db");
        std::fs::create_dir_all(db_path.parent().expect("db parent")).expect("create db parent");
        let key = crate::vault_crypto::derive_cheap(MASTER_PASSWORD, MASTER_SALT)
            .expect("derive fixture key");
        // Touch the database so `MemoryStore::open` creates the schema once.
        drop(MemoryStore::open(db_path.to_str().expect("utf8 db")).expect("create db"));
        Self {
            home,
            cwd,
            db_path,
            key,
        }
    }

    fn store(&self) -> MemoryStore {
        MemoryStore::open(self.db_path.to_str().expect("utf8 db")).expect("open db")
    }

    fn read_only_store(&self) -> MemoryStore {
        MemoryStore::open_read_only(self.db_path.to_str().expect("utf8 db")).expect("open db ro")
    }

    fn seed_secret(&self, name: &str, value: &str) {
        let (encrypted_value, nonce) =
            crate::vault_crypto::encrypt(self.key.bytes(), value.as_bytes()).expect("encrypt");
        self.store()
            .vault_upsert_entry(&memcore::VaultEntry {
                name: name.to_string(),
                encrypted_value,
                nonce,
                secret_type: memcore::SECRET_TYPE_API_KEY.to_string(),
                description: "reconcile fixture".to_string(),
                allowed_agents: None,
                created_at: "2026-08-01T00:00:00Z".to_string(),
                updated_at: "2026-08-01T00:00:00Z".to_string(),
                accessed_at: String::new(),
                access_count: 0,
            })
            .expect("seed vault entry");
    }

    fn write_env(&self, contents: &str) -> PathBuf {
        let path = self.cwd.path().join(".env");
        std::fs::write(&path, contents).expect("write env fixture");
        path
    }

    fn plan(&self) -> PlanOutcome {
        build_plan(
            &self.read_only_store(),
            self.key.bytes(),
            self.home.path(),
            self.cwd.path(),
        )
        .expect("plan builds")
    }
}

/// `env_source_paths` reads `TACHI_HOME` from the process environment, so every
/// test here holds the global lock and pins that variable away from the
/// developer's real one.
fn env_guard() -> (
    std::sync::MutexGuard<'static, ()>,
    crate::test_support::EnvRestore,
) {
    let lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let guard = crate::test_support::EnvRestore::remove("TACHI_HOME");
    (lock, guard)
}

fn created_accounts(plan: &BoundAccountPlan) -> Vec<&PlannedAccount> {
    plan.actions
        .iter()
        .filter_map(|action| match action {
            AccountAction::CreateAccount { account } => Some(account.as_ref()),
            _ => None,
        })
        .collect()
}

fn advisory_codes(advisories: &[Advisory]) -> Vec<&str> {
    advisories
        .iter()
        .map(|advisory| advisory.code.as_str())
        .collect()
}

// ── Discrimination 3: a candidate that is a command is data, never a command ─

/// The shapes that matter, and — just as load-bearing — the shapes that must
/// **not** trip: `${vault:A|B}` is Tachi's own indirection syntax and a base64
/// key is allowed to contain punctuation.
#[test]
fn executable_shapes_are_recognized_without_flagging_ordinary_secrets() {
    assert!(looks_executable("$(curl https://evil.example)"));
    assert!(looks_executable("`id`"));
    assert!(looks_executable("cat <(echo hi)"));
    assert!(looks_executable("#!/bin/sh\nexit 0"));

    assert!(!looks_executable("sk-abc123+/=ABCdef"));
    assert!(!looks_executable("${vault:ZAI_API_KEY|BIGMODEL_API_KEY}"));
    assert!(!looks_executable("Bearer token; charset=utf-8"));
}

/// A candidate whose value is a command substitution is refused as data. The
/// proof that it was never executed is the side effect it would have had: the
/// fixture's command creates a file, and the file does not exist.
#[test]
fn a_script_bearing_candidate_is_refused_as_data_and_never_executed() {
    let (_lock, _tachi_home) = env_guard();
    let fixture = Fixture::new();
    let canary = fixture.cwd.path().join("pwned");
    fixture.write_env(&format!(
        "SILICONFLOW_API_KEY=$(touch {})\nDEEPSEEK_API_KEY=`touch {}`\n",
        canary.display(),
        canary.display()
    ));
    // A real credential in the Vault, so the pass has something to do and the
    // refusal is not just "nothing happened".
    fixture.seed_secret("SILICONFLOW_API_KEY", SILICONFLOW_VALUE);

    let outcome = fixture.plan();

    assert!(
        !canary.exists(),
        "a discovered value must never be executed; the canary file proves it was"
    );
    assert!(
        advisory_codes(&outcome.advisories)
            .into_iter()
            .filter(|code| *code == "refused_executable_candidate")
            .count()
            >= 2,
        "both script-shaped candidates must be refused: {:#?}",
        outcome.advisories
    );
    // The script text never becomes an alias of anything.
    let encoded = serde_json::to_string(&outcome.plan).expect("encode plan");
    assert!(
        !encoded.contains("touch") && !encoded.contains("$("),
        "no plan surface may carry the script text: {encoded}"
    );
}

// ── The alias-family ruling: names propose, fingerprints dispose ────────────

/// `EXTRACT_API_KEY` is a registry alias of the SiliconFlow family. That is a
/// naming fact, not evidence that two credentials are one account: when the
/// Vault holds both names with *different* values, reconcile plans two
/// independent accounts and no merge action at all.
#[test]
fn extract_and_siliconflow_with_different_values_stay_two_accounts() {
    let (_lock, _tachi_home) = env_guard();
    let fixture = Fixture::new();
    fixture.seed_secret("SILICONFLOW_API_KEY", SILICONFLOW_VALUE);
    fixture.seed_secret("EXTRACT_API_KEY", EXTRACT_VALUE);

    let outcome = fixture.plan();
    let created = created_accounts(&outcome.plan);
    assert_eq!(
        created.len(),
        2,
        "one account per distinct credential: {:#?}",
        outcome.plan.actions
    );

    for account in &created {
        let names: Vec<&str> = account
            .aliases
            .iter()
            .map(|alias| alias.alias_name.as_str())
            .collect();
        assert_eq!(
            names.len(),
            1,
            "a name must not join an account it shares no value with: {names:?}"
        );
    }
    let custody: Vec<&str> = created
        .iter()
        .map(|account| account.custody_logical_name.as_str())
        .collect();
    assert!(custody.contains(&"SILICONFLOW_API_KEY"), "{custody:?}");
    assert!(custody.contains(&"EXTRACT_API_KEY"), "{custody:?}");

    assert!(
        !outcome
            .plan
            .actions
            .iter()
            .any(|action| matches!(action, AccountAction::MergeAccounts { .. })),
        "alias-family similarity must never produce a merge action"
    );
}

/// The other half of discrimination 1: the *same* value under two names is one
/// account with two aliases, and it is the fingerprint that decides — the
/// env-file name joins the Vault-held account because the values match.
#[test]
fn one_value_under_two_names_collapses_into_one_account_with_two_aliases() {
    let (_lock, _tachi_home) = env_guard();
    let fixture = Fixture::new();
    fixture.seed_secret("SILICONFLOW_API_KEY", SILICONFLOW_VALUE);
    fixture.write_env(&format!("EXTRACT_API_KEY={SILICONFLOW_VALUE}\n"));

    let outcome = fixture.plan();
    let created = created_accounts(&outcome.plan);
    assert_eq!(created.len(), 1, "{:#?}", outcome.plan.actions);

    let mut names: Vec<&str> = created[0]
        .aliases
        .iter()
        .map(|alias| alias.alias_name.as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(names, vec!["EXTRACT_API_KEY", "SILICONFLOW_API_KEY"]);
    assert_eq!(created[0].custody_logical_name, "SILICONFLOW_API_KEY");
}

/// An env-file value the Vault does not hold is reported and left alone.
/// Reconcile never imports a secret, so it never mints an account whose
/// credential nobody has custody of.
#[test]
fn an_env_only_credential_is_advised_and_never_becomes_an_account() {
    let (_lock, _tachi_home) = env_guard();
    let fixture = Fixture::new();
    fixture.write_env("DEEPSEEK_API_KEY=sk-only-in-a-file\n");

    let outcome = fixture.plan();
    assert!(
        created_accounts(&outcome.plan).is_empty(),
        "{:#?}",
        outcome.plan.actions
    );
    assert!(
        advisory_codes(&outcome.advisories).contains(&"not_under_vault_custody"),
        "{:#?}",
        outcome.advisories
    );
}

// ── Planning is read-only ──────────────────────────────────────────────────

/// Planning decrypts, fingerprints and decides — and writes nothing. Asserted
/// on the database file's bytes, the same contract shape `providers doctor`
/// holds and for the same reason: a read-only stage that quietly writes is a
/// read-only stage nobody can trust.
#[test]
fn planning_leaves_the_vault_db_and_the_source_files_byte_identical() {
    let (_lock, _tachi_home) = env_guard();
    let fixture = Fixture::new();
    fixture.seed_secret("SILICONFLOW_API_KEY", SILICONFLOW_VALUE);
    let env_path = fixture.write_env(&format!("EXTRACT_API_KEY={SILICONFLOW_VALUE}\n"));

    let db_before = tachi_params::sha256_hex(&std::fs::read(&fixture.db_path).expect("db bytes"));
    let env_before = tachi_params::sha256_hex(&std::fs::read(&env_path).expect("env bytes"));

    let outcome = fixture.plan();
    assert!(!outcome.plan.actions.is_empty());

    assert_eq!(
        tachi_params::sha256_hex(&std::fs::read(&fixture.db_path).expect("db bytes")),
        db_before,
        "planning must not write to the vault DB"
    );
    assert_eq!(
        tachi_params::sha256_hex(&std::fs::read(&env_path).expect("env bytes")),
        env_before,
        "planning must not rewrite a source file"
    );
}

// ── Discrimination 10, end to end through the artifact ─────────────────────

/// Plan, write the artifact, apply it, then apply the same file again: the
/// second pass verifies in full, changes nothing, and holds every revision.
#[test]
fn plan_apply_replay_is_idempotent_through_the_artifact_file() {
    let (_lock, _tachi_home) = env_guard();
    let fixture = Fixture::new();
    fixture.seed_secret("SILICONFLOW_API_KEY", SILICONFLOW_VALUE);
    fixture.write_env(&format!("EXTRACT_API_KEY={SILICONFLOW_VALUE}\n"));

    let outcome = fixture.plan();
    let artifact = ReconcilePlanArtifact {
        schema: PLAN_SCHEMA.to_string(),
        generated_at: "2026-08-10T00:00:00Z".to_string(),
        plan_digest: memcore::plan_digest(&outcome.plan),
        bound: outcome.plan,
    };
    let artifact_path = fixture.cwd.path().join("plan.json");
    std::fs::write(
        &artifact_path,
        serde_json::to_string_pretty(&artifact).expect("encode artifact"),
    )
    .expect("write artifact");

    let reloaded: ReconcilePlanArtifact =
        serde_json::from_str(&std::fs::read_to_string(&artifact_path).expect("read artifact"))
            .expect("decode artifact");
    assert_eq!(
        memcore::plan_digest(&reloaded.bound),
        reloaded.plan_digest,
        "the digest must survive the artifact round trip"
    );

    let mut store = fixture.store();
    let first = store
        .apply_provider_account_plan(&reloaded.bound, &reloaded.plan_digest, &EnvFileSources)
        .expect("first apply");
    assert!(first.changed);
    assert_eq!(first.accounts_created.len(), 1);

    let revisions_after_first: Vec<(String, i64)> =
        memcore::list_provider_accounts(store.connection())
            .expect("accounts")
            .into_iter()
            .map(|account| (account.account_id, account.revision))
            .collect();

    let replay = store
        .apply_provider_account_plan(&reloaded.bound, &reloaded.plan_digest, &EnvFileSources)
        .expect("replay applies");
    assert!(
        !replay.changed,
        "replaying the same artifact must be a no-op"
    );
    assert!(!replay.noop_event_ids.is_empty());

    let revisions_after_replay: Vec<(String, i64)> =
        memcore::list_provider_accounts(store.connection())
            .expect("accounts")
            .into_iter()
            .map(|account| (account.account_id, account.revision))
            .collect();
    assert_eq!(revisions_after_first, revisions_after_replay);

    // And a re-plan against the applied state proposes nothing further.
    let second_plan = fixture.plan();
    assert!(
        second_plan.plan.actions.is_empty(),
        "a converged world must plan no further actions: {:#?}",
        second_plan.plan.actions
    );
}

/// Hand-editing the artifact between plan and apply is refused with zero
/// writes — the file is the operator's to read, not the plan's authority.
#[test]
fn a_hand_edited_artifact_is_refused_with_zero_writes() {
    let (_lock, _tachi_home) = env_guard();
    let fixture = Fixture::new();
    fixture.seed_secret("SILICONFLOW_API_KEY", SILICONFLOW_VALUE);

    let outcome = fixture.plan();
    let mut artifact = ReconcilePlanArtifact {
        schema: PLAN_SCHEMA.to_string(),
        generated_at: "2026-08-10T00:00:00Z".to_string(),
        plan_digest: memcore::plan_digest(&outcome.plan),
        bound: outcome.plan,
    };
    // The tamper an attacker actually wants: point custody at a different
    // Vault object while leaving the approved digest in place.
    if let Some(AccountAction::CreateAccount { account }) = artifact.bound.actions.first_mut() {
        account.custody_logical_name = "DEEPSEEK_API_KEY".to_string();
    }

    let mut store = fixture.store();
    let err = store
        .apply_provider_account_plan(&artifact.bound, &artifact.plan_digest, &EnvFileSources)
        .expect_err("a hand-edited artifact must be refused");
    assert!(
        matches!(
            err,
            memcore::MemoryError::ProviderAccountPlanRefused {
                reason: memcore::ProviderPlanRefusal::PlanDigestMismatch,
                ..
            }
        ),
        "unexpected error: {err}"
    );
    assert!(
        memcore::list_provider_accounts(store.connection())
            .expect("accounts")
            .is_empty(),
        "a refused apply must create no account"
    );
}

/// A source file rewritten between plan and apply is drift: the evidence the
/// plan reasoned from is gone, so nothing is written.
#[test]
fn a_source_file_rewritten_after_planning_refuses_the_apply() {
    let (_lock, _tachi_home) = env_guard();
    let fixture = Fixture::new();
    fixture.seed_secret("SILICONFLOW_API_KEY", SILICONFLOW_VALUE);
    fixture.write_env(&format!("EXTRACT_API_KEY={SILICONFLOW_VALUE}\n"));

    let outcome = fixture.plan();
    let digest = memcore::plan_digest(&outcome.plan);

    fixture.write_env(&format!(
        "EXTRACT_API_KEY={SILICONFLOW_VALUE}\n# an edit after the plan was approved\n"
    ));

    let mut store = fixture.store();
    let err = store
        .apply_provider_account_plan(&outcome.plan, &digest, &EnvFileSources)
        .expect_err("source drift must refuse");
    assert!(
        matches!(
            err,
            memcore::MemoryError::ProviderAccountPlanRefused {
                reason: memcore::ProviderPlanRefusal::SourceDigestMismatch,
                ..
            }
        ),
        "unexpected error: {err}"
    );
    assert!(
        memcore::list_provider_accounts(store.connection())
            .expect("accounts")
            .is_empty(),
        "a refused apply must create no account"
    );
}

// ── Layout stays in custody ────────────────────────────────────────────────

/// A rotation pool is planned as one account under its prefix, and no member
/// name appears anywhere in the artifact — not as an alias, not as a source
/// ref, not as a binding.
#[test]
fn a_rotation_pool_plans_one_account_and_leaks_no_member_name() {
    let (_lock, _tachi_home) = env_guard();
    let fixture = Fixture::new();
    fixture.seed_secret("DEEPSEEK_API_KEY_1", "sk-deepseek-member-one");
    fixture.seed_secret("DEEPSEEK_API_KEY_2", "sk-deepseek-member-two");
    fixture
        .store()
        .vault_set_rotation(&memcore::VaultKeyRotation {
            prefix: "DEEPSEEK_API_KEY".to_string(),
            current_index: 1,
            total_keys: 2,
            rotation_strategy: "round_robin".to_string(),
            created_at: "2026-08-01T00:00:00Z".to_string(),
            updated_at: "2026-08-01T00:00:00Z".to_string(),
        })
        .expect("seed rotation");

    let outcome = fixture.plan();
    let created = created_accounts(&outcome.plan);
    assert_eq!(created.len(), 1, "{:#?}", outcome.plan.actions);
    assert_eq!(created[0].custody_logical_name, "DEEPSEEK_API_KEY");
    assert_eq!(
        created[0].custody_kind,
        memcore::CustodyKind::VaultRotationPool
    );

    let encoded = serde_json::to_string(&outcome.plan).expect("encode plan");
    assert!(
        !encoded.contains("DEEPSEEK_API_KEY_1") && !encoded.contains("DEEPSEEK_API_KEY_2"),
        "no plan surface may carry a rotation-pool member name: {encoded}"
    );
    assert!(
        outcome
            .plan
            .bindings
            .vault_pools
            .iter()
            .any(|binding| binding.prefix == "DEEPSEEK_API_KEY"),
        "the pool must be bound by prefix + membership digest"
    );

    let mut store = fixture.store();
    let report = store
        .apply_provider_account_plan(
            &outcome.plan,
            &memcore::plan_digest(&outcome.plan),
            &EnvFileSources,
        )
        .expect("pool plan applies");
    assert_eq!(report.accounts_created.len(), 1);
}
