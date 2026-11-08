use super::*;

const MASTER: [u8; 32] = [0x42; 32];

fn write_file(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
    std::fs::write(path, contents).expect("write fixture");
}

fn plan(home: &Path, cwd: &Path, db_path: &Path) -> PlanReport {
    let fp_key = FingerprintKey::derive_from_master_key(&MASTER);
    plan_report(home, cwd, db_path, HostFilter::Env, Vec::new(), &fp_key).expect("plan")
}

fn insert_custodied_api_account(
    store: &memcore::MemoryStore,
    account_id: &str,
    provider_kind: &str,
    secret: &str,
    custody_target: &str,
) {
    let fp_key = FingerprintKey::derive_from_master_key(&MASTER);
    let member = fp_key.key_fingerprint(provider_kind, secret);
    let account_fp = fp_key.account_fingerprint_from_members([member]);
    let auth_ref = format!("va1:{account_id}");
    memcore::db::insert_provider_account(
        store.connection(),
        &memcore::NewProviderAccount::api_key_pool(
            account_id,
            provider_kind,
            &auth_ref,
            account_fp,
            memcore::AccountClass::ModelApi,
        ),
    )
    .expect("account");
    memcore::db::insert_account_custody(
        store.connection(),
        &auth_ref,
        account_id,
        memcore::CustodyKind::VaultEntry,
        custody_target,
    )
    .expect("custody");
}

fn insert_rotation_pool_account(
    store: &memcore::MemoryStore,
    account_id: &str,
    provider_kind: &str,
    members: &[&str],
    custody_target: &str,
) {
    let fp_key = FingerprintKey::derive_from_master_key(&MASTER);
    let member_fingerprints: Vec<String> = members
        .iter()
        .map(|secret| fp_key.key_fingerprint(provider_kind, secret))
        .collect();
    let account_fingerprint = fp_key.account_fingerprint_from_members(&member_fingerprints);
    let auth_ref = format!("va1:{account_id}");
    memcore::db::insert_provider_account(
        store.connection(),
        &memcore::NewProviderAccount::api_key_pool(
            account_id,
            provider_kind,
            &auth_ref,
            account_fingerprint,
            memcore::AccountClass::ModelApi,
        ),
    )
    .expect("account");
    memcore::db::insert_account_custody(
        store.connection(),
        &auth_ref,
        account_id,
        memcore::CustodyKind::VaultRotationPool,
        custody_target,
    )
    .expect("custody");
    memcore::db::append_provider_account_event(
        store.connection(),
        &memcore::NewProviderAccountEvent::new(
            account_id,
            1,
            memcore::vault::accounts::EVENT_KIND_FINGERPRINT_OBSERVED,
        )
        .with_evidence(
            serde_json::json!({
                "member_count": member_fingerprints.len(),
                "member_fingerprints": member_fingerprints,
            })
            .to_string(),
        ),
    )
    .expect("member evidence");
}

fn assert_ineligible_exact_account_is_not_reused(
    configure: impl FnOnce(&mut memcore::NewProviderAccount),
) {
    let home = tempfile::tempdir().expect("home");
    let cwd = tempfile::tempdir().expect("cwd");
    let db_path = home.path().join(".tachi/global/tachi-global.db");
    std::fs::create_dir_all(db_path.parent().expect("parent")).expect("mkdir");
    let store = memcore::MemoryStore::open(db_path.to_str().expect("utf8")).expect("store");
    let fp_key = FingerprintKey::derive_from_master_key(&MASTER);
    let member = fp_key.key_fingerprint("deepseek", "same-fixture");
    let account_fp = fp_key.account_fingerprint_from_members([member]);
    let mut account = memcore::NewProviderAccount::api_key_pool(
        "account-ineligible",
        "deepseek",
        "va1:ineligible-fixture",
        account_fp,
        memcore::AccountClass::ModelApi,
    );
    configure(&mut account);
    memcore::db::insert_provider_account(store.connection(), &account).expect("account");
    drop(store);
    write_file(&cwd.path().join(".env"), "DEEPSEEK_API_KEY=same-fixture\n");

    let report = plan(home.path(), cwd.path(), &db_path);

    assert_eq!(report.actions.len(), 1, "{report:#?}");
    assert_eq!(report.actions[0].action, ACTION_CREATE_ACCOUNT);
    assert_ne!(report.actions[0].account_id, "account-ineligible");
}

#[test]
fn deepseek_endpoint_creates_one_account_and_binds_lane_aliases() {
    let home = tempfile::tempdir().expect("home");
    let cwd = tempfile::tempdir().expect("cwd");
    let secret = "sk-fixture-deepseek-official";
    write_file(
        &cwd.path().join(".env"),
        &format!(
            "DISTILL_API_KEY2={secret}\nEXTRACT_API_KEY2={secret}\nDISTILL_BASE_URL=https://api.deepseek.com/chat/completions\nEXTRACT_BASE_URL=https://api.deepseek.com/chat/completions\n"
        ),
    );

    let report = plan(
        home.path(),
        cwd.path(),
        &home.path().join(".tachi/global/tachi-global.db"),
    );

    assert_eq!(
        report
            .actions
            .iter()
            .filter(|action| action.action == ACTION_CREATE_ACCOUNT)
            .count(),
        1,
        "{report:#?}"
    );
    let slots: Vec<&str> = report
        .actions
        .iter()
        .filter(|action| action.action == ACTION_BIND_SLOT)
        .filter_map(|action| action.slot.as_deref())
        .collect();
    assert_eq!(slots, vec!["DISTILL_API_KEY", "EXTRACT_API_KEY"]);
    assert!(report
        .actions
        .iter()
        .all(|action| action.provider_kind == "deepseek"));
    let encoded = serde_json::to_string(&report).expect("encode");
    assert!(!encoded.contains(secret));
    let encoded_actions = serde_json::to_string(&report.actions).expect("encode actions");
    assert!(!encoded_actions.contains("DISTILL_API_KEY2"));
    assert!(!encoded_actions.contains("EXTRACT_API_KEY2"));
    assert!(encoded.contains("fp1:"));
    assert!(encoded.contains("fpa1:"));
    let human = render_plan_human(&report);
    assert!(!human.contains(secret), "human plan leaked secret: {human}");
}

#[test]
fn same_bytes_under_two_lane_names_make_one_account_and_stable_digest() {
    let home = tempfile::tempdir().expect("home");
    let cwd = tempfile::tempdir().expect("cwd");
    write_file(
        &cwd.path().join(".env"),
        "DISTILL_API_KEY2=same-fixture\nEXTRACT_API_KEY2=same-fixture\nDISTILL_BASE_URL=api.deepseek.com\nEXTRACT_BASE_URL=api.deepseek.com\n",
    );

    let first = plan(
        home.path(),
        cwd.path(),
        &home.path().join(".tachi/global/tachi-global.db"),
    );
    let second = plan(
        home.path(),
        cwd.path(),
        &home.path().join(".tachi/global/tachi-global.db"),
    );

    assert_eq!(first.plan_digest, second.plan_digest);
    assert_eq!(
        first
            .actions
            .iter()
            .filter(|action| action.action == ACTION_CREATE_ACCOUNT)
            .count(),
        1
    );
    assert_eq!(
        first
            .actions
            .iter()
            .filter(|action| action.action == ACTION_BIND_SLOT)
            .count(),
        2
    );
}

#[test]
fn conflicting_fresh_accounts_for_one_slot_are_action_free_order_independently() {
    for second_endpoint in ["api.deepseek.com", "api.siliconflow.cn"] {
        let home = tempfile::tempdir().expect("home");
        let cwd = tempfile::tempdir().expect("cwd");
        let sources = [
            home.path().join(".tachi/config.env"),
            cwd.path().join(".env"),
        ];
        let sightings = [
            "DEEPSEEK_API_KEY=first-fixture\nDISTILL_API_KEY2=first-fixture\nDISTILL_BASE_URL=https://api.deepseek.com\n"
                .to_string(),
            format!(
                "DISTILL_API_KEY2=second-fixture\nDISTILL_BASE_URL=https://{second_endpoint}\n"
            ),
        ];
        let mut digests = Vec::new();
        for order in [[0, 1], [1, 0]] {
            for (source, index) in sources.iter().zip(order) {
                write_file(source, &sightings[index]);
            }
            let report = plan(home.path(), cwd.path(), &home.path().join("missing.db"));
            assert!(report.actions.is_empty(), "{report:#?}");
            let candidates: Vec<_> = report
                .candidates
                .iter()
                .filter(|candidate| {
                    matches!(
                        candidate.logical_name.as_str(),
                        "DISTILL_API_KEY2" | "DEEPSEEK_API_KEY"
                    )
                })
                .collect();
            assert_eq!(candidates.len(), 3);
            for candidate in candidates {
                assert_eq!(candidate.classification, "ambiguous");
                assert!(candidate.account_id.is_none());
                assert!(candidate.intended_slot_binds.is_empty());
            }
            digests.push(report.plan_digest);
        }
        assert_eq!(digests[0], digests[1]);
    }
}

#[test]
fn identical_fresh_account_for_one_slot_is_deduplicated_across_sources() {
    let home = tempfile::tempdir().expect("home");
    let cwd = tempfile::tempdir().expect("cwd");
    for source in [
        home.path().join(".tachi/config.env"),
        cwd.path().join(".env"),
    ] {
        write_file(
            &source,
            "DISTILL_API_KEY2=same-fixture\nDISTILL_BASE_URL=https://api.deepseek.com\n",
        );
    }
    let report = plan(home.path(), cwd.path(), &home.path().join("missing.db"));
    assert_eq!(report.actions.len(), 2, "{report:#?}");
    assert_eq!(report.actions[0].action, ACTION_CREATE_ACCOUNT);
    assert_eq!(report.actions[1].action, ACTION_BIND_SLOT);
    assert_eq!(report.actions[1].slot.as_deref(), Some("DISTILL_API_KEY"));
    assert_eq!(report.actions[0].account_id, report.actions[1].account_id);
    let candidates: Vec<_> = report
        .candidates
        .iter()
        .filter(|candidate| candidate.logical_name == "DISTILL_API_KEY2")
        .collect();
    assert_eq!(candidates.len(), 2);
    for candidate in candidates {
        assert_eq!(candidate.classification, "known");
        assert_eq!(
            candidate.account_id.as_deref(),
            Some(report.actions[0].account_id.as_str())
        );
    }
}

#[test]
fn existing_fingerprint_binds_without_creating_another_account() {
    let home = tempfile::tempdir().expect("home");
    let cwd = tempfile::tempdir().expect("cwd");
    let db_path = home.path().join(".tachi/global/tachi-global.db");
    std::fs::create_dir_all(db_path.parent().expect("parent")).expect("mkdir");
    let store = memcore::MemoryStore::open(db_path.to_str().expect("utf8")).expect("store");
    let fp_key = FingerprintKey::derive_from_master_key(&MASTER);
    let member = fp_key.key_fingerprint("deepseek", "same-fixture");
    let account_fp = fp_key.account_fingerprint_from_members([member]);
    memcore::db::insert_provider_account(
        store.connection(),
        &memcore::NewProviderAccount::api_key_pool(
            "account-existing",
            "deepseek",
            "va1:fixture",
            account_fp,
            memcore::AccountClass::ModelApi,
        ),
    )
    .expect("account");
    drop(store);
    write_file(
        &cwd.path().join(".env"),
        "DISTILL_API_KEY2=same-fixture\nDISTILL_BASE_URL=api.deepseek.com\n",
    );

    let report = plan(home.path(), cwd.path(), &db_path);

    assert!(report
        .actions
        .iter()
        .all(|action| action.action != ACTION_CREATE_ACCOUNT));
    assert_eq!(report.actions.len(), 1, "{report:#?}");
    assert_eq!(report.actions[0].action, ACTION_BIND_SLOT);
    assert_eq!(report.actions[0].account_id, "account-existing");
}

#[test]
fn changed_fingerprint_for_canonical_custody_rotates_existing_account() {
    let home = tempfile::tempdir().expect("home");
    let cwd = tempfile::tempdir().expect("cwd");
    let db_path = home.path().join(".tachi/global/tachi-global.db");
    std::fs::create_dir_all(db_path.parent().expect("parent")).expect("mkdir");
    let store = memcore::MemoryStore::open(db_path.to_str().expect("utf8")).expect("store");
    let fp_key = FingerprintKey::derive_from_master_key(&MASTER);
    let old_member = fp_key.key_fingerprint("deepseek", "old-fixture");
    let old_account_fp = fp_key.account_fingerprint_from_members([old_member]);
    memcore::db::insert_provider_account(
        store.connection(),
        &memcore::NewProviderAccount::api_key_pool(
            "account-rotating",
            "deepseek",
            "va1:rotating-fixture",
            old_account_fp,
            memcore::AccountClass::ModelApi,
        ),
    )
    .expect("account");
    memcore::db::insert_account_custody(
        store.connection(),
        "va1:rotating-fixture",
        "account-rotating",
        memcore::CustodyKind::VaultEntry,
        "DEEPSEEK_API_KEY",
    )
    .expect("custody");
    drop(store);
    write_file(&cwd.path().join(".env"), "DEEPSEEK_API_KEY=new-fixture\n");

    let report = plan(home.path(), cwd.path(), &db_path);

    assert_eq!(report.actions.len(), 1, "{report:#?}");
    assert_eq!(report.actions[0].action, ACTION_ROTATE_ACCOUNT);
    assert_eq!(report.actions[0].account_id, "account-rotating");
}

#[test]
fn official_provider_hosts_remain_distinct() {
    assert_eq!(
        provider_kind_from_endpoint("api.deepseek.com").as_deref(),
        Some("deepseek")
    );
    assert_eq!(
        provider_kind_from_endpoint("https://api.siliconflow.cn/v1").as_deref(),
        Some("siliconflow")
    );
    assert_eq!(
        provider_kind_from_endpoint("https://api.z.ai/api/paas/v4").as_deref(),
        Some("zai")
    );
    assert_eq!(
        crate::provider_config::key_registry::provider_kind_for_env_name("ZHIPUAI_API_KEY"),
        Some("zhipuai")
    );
}

#[test]
fn identical_bytes_for_official_deepseek_siliconflow_and_glm_stay_distinct() {
    let home = tempfile::tempdir().expect("home");
    let cwd = tempfile::tempdir().expect("cwd");
    write_file(
        &cwd.path().join(".env"),
        "DEEPSEEK_API_KEY=same-fixture\nDEEPSEEK_BASE_URL=api.deepseek.com\n\
         SILICONFLOW_API_KEY=same-fixture\nSILICONFLOW_BASE_URL=api.siliconflow.cn\n\
         ZAI_API_KEY=same-fixture\nZAI_BASE_URL=api.z.ai\n",
    );

    let report = plan(
        home.path(),
        cwd.path(),
        &home.path().join(".tachi/global/tachi-global.db"),
    );

    let kinds: BTreeSet<&str> = report
        .actions
        .iter()
        .filter(|action| action.action == ACTION_CREATE_ACCOUNT)
        .map(|action| action.provider_kind.as_str())
        .collect();
    assert_eq!(kinds, BTreeSet::from(["deepseek", "siliconflow", "zai"]));
}

#[test]
fn env_name_endpoint_mismatch_is_conflicted_and_plans_nothing() {
    let home = tempfile::tempdir().expect("home");
    let cwd = tempfile::tempdir().expect("cwd");
    write_file(
        &cwd.path().join(".env"),
        "DEEPSEEK_API_KEY=fixture\nDEEPSEEK_BASE_URL=https://api.siliconflow.cn/v1\n",
    );

    let report = plan(
        home.path(),
        cwd.path(),
        &home.path().join(".tachi/global/tachi-global.db"),
    );

    assert!(report.actions.is_empty());
    let candidate = report
        .candidates
        .iter()
        .find(|candidate| candidate.logical_name == "DEEPSEEK_API_KEY")
        .expect("candidate");
    assert_eq!(candidate.classification, "conflicted");
}

#[test]
fn unrelated_provider_endpoint_does_not_conflict_with_named_key() {
    let home = tempfile::tempdir().expect("home");
    let cwd = tempfile::tempdir().expect("cwd");
    write_file(
        &cwd.path().join(".env"),
        "DEEPSEEK_API_KEY=fixture\nSILICONFLOW_BASE_URL=https://api.siliconflow.cn/v1\n",
    );

    let report = plan(
        home.path(),
        cwd.path(),
        &home.path().join(".tachi/global/tachi-global.db"),
    );

    assert_eq!(report.actions.len(), 1, "{report:#?}");
    assert_eq!(report.actions[0].action, ACTION_CREATE_ACCOUNT);
    assert_eq!(report.actions[0].provider_kind, "deepseek");
    let candidate = report
        .candidates
        .iter()
        .find(|candidate| candidate.logical_name == "DEEPSEEK_API_KEY")
        .expect("candidate");
    assert_eq!(candidate.classification, "known");
}

#[test]
fn unrelated_secret_names_do_not_borrow_colocated_endpoint_identity() {
    let home = tempfile::tempdir().expect("home");
    let cwd = tempfile::tempdir().expect("cwd");
    write_file(
        &cwd.path().join(".env"),
        "DATABASE_PASSWORD=database-fixture\nAWS_ACCESS_KEY_ID=aws-fixture\nDEEPSEEK_BASE_URL=https://api.deepseek.com\n",
    );

    let report = plan(
        home.path(),
        cwd.path(),
        &home.path().join(".tachi/global/tachi-global.db"),
    );

    assert!(report.actions.is_empty(), "{report:#?}");
    for name in ["DATABASE_PASSWORD", "AWS_ACCESS_KEY_ID"] {
        let candidate = report
            .candidates
            .iter()
            .find(|candidate| candidate.logical_name == name)
            .expect("candidate");
        assert_eq!(candidate.classification, "unknown", "{report:#?}");
        assert!(candidate.provider_kind.is_none(), "{report:#?}");
    }
}

#[test]
fn unknown_api_key_name_does_not_gain_identity_from_matching_prefix_alone() {
    let home = tempfile::tempdir().expect("home");
    let cwd = tempfile::tempdir().expect("cwd");
    write_file(
        &cwd.path().join(".env"),
        "CUSTOM_API_KEY=custom-fixture\nCUSTOM_BASE_URL=https://api.deepseek.com\n",
    );

    let report = plan(
        home.path(),
        cwd.path(),
        &home.path().join(".tachi/global/tachi-global.db"),
    );

    assert!(report.actions.is_empty(), "{report:#?}");
    let candidate = report
        .candidates
        .iter()
        .find(|candidate| candidate.logical_name == "CUSTOM_API_KEY")
        .expect("candidate");
    assert_eq!(candidate.classification, "unknown", "{report:#?}");
    assert!(candidate.provider_kind.is_none(), "{report:#?}");
}

#[test]
fn unrelated_lane_endpoint_does_not_create_a_slot_bind() {
    let home = tempfile::tempdir().expect("home");
    let cwd = tempfile::tempdir().expect("cwd");
    write_file(
        &cwd.path().join(".env"),
        "DEEPSEEK_API_KEY=fixture\nEXTRACT_BASE_URL=https://api.deepseek.com\n",
    );

    let report = plan(
        home.path(),
        cwd.path(),
        &home.path().join(".tachi/global/tachi-global.db"),
    );

    assert_eq!(report.actions.len(), 1, "{report:#?}");
    assert_eq!(report.actions[0].action, ACTION_CREATE_ACCOUNT);
    assert!(report.actions[0].slot.is_none());
}

fn assert_rejected_endpoint_conflicts(endpoint: &str) {
    let home = tempfile::tempdir().expect("home");
    let cwd = tempfile::tempdir().expect("cwd");
    write_file(
        &cwd.path().join(".env"),
        &format!("DEEPSEEK_API_KEY=fixture\nDEEPSEEK_BASE_URL={endpoint}\n"),
    );

    let report = plan(
        home.path(),
        cwd.path(),
        &home.path().join(".tachi/global/tachi-global.db"),
    );

    assert!(report.actions.is_empty(), "{report:#?}");
    let candidate = report
        .candidates
        .iter()
        .find(|candidate| candidate.logical_name == "DEEPSEEK_API_KEY")
        .expect("candidate");
    assert_eq!(candidate.classification, "conflicted", "{report:#?}");
}

#[test]
fn unknown_https_endpoint_is_conflicted_and_plans_nothing() {
    assert_rejected_endpoint_conflicts("https://evil.example/v1");
}

#[test]
fn malformed_endpoint_is_conflicted_and_plans_nothing() {
    assert_rejected_endpoint_conflicts("://not-a-url");
}

#[test]
fn http_official_endpoint_is_conflicted_and_plans_nothing() {
    assert_rejected_endpoint_conflicts("http://api.deepseek.com");
}

#[test]
fn explicit_empty_endpoints_are_conflicted_and_plan_nothing() {
    assert_rejected_endpoint_conflicts("");
    assert_rejected_endpoint_conflicts("\"\"");
}

#[test]
fn absent_endpoint_keeps_registry_name_classification_known() {
    let home = tempfile::tempdir().expect("home");
    let cwd = tempfile::tempdir().expect("cwd");
    write_file(&cwd.path().join(".env"), "DEEPSEEK_API_KEY=fixture\n");

    let report = plan(
        home.path(),
        cwd.path(),
        &home.path().join(".tachi/global/tachi-global.db"),
    );

    assert_eq!(report.actions.len(), 1, "{report:#?}");
    assert_eq!(report.actions[0].action, ACTION_CREATE_ACCOUNT);
    let candidate = report
        .candidates
        .iter()
        .find(|candidate| candidate.logical_name == "DEEPSEEK_API_KEY")
        .expect("candidate");
    assert_eq!(candidate.classification, "known");
}

#[test]
fn voyage_rerank_rotation_selects_rerank_account_when_both_accounts_exist() {
    let home = tempfile::tempdir().expect("home");
    let cwd = tempfile::tempdir().expect("cwd");
    let db_path = home.path().join(".tachi/global/tachi-global.db");
    std::fs::create_dir_all(db_path.parent().expect("parent")).expect("mkdir");
    let store = memcore::MemoryStore::open(db_path.to_str().expect("utf8")).expect("store");
    insert_custodied_api_account(
        &store,
        "account-voyage-embeddings",
        "voyage",
        "old-embeddings",
        "VOYAGE_API_KEY",
    );
    insert_custodied_api_account(
        &store,
        "account-voyage-rerank",
        "voyage",
        "old-rerank",
        "VOYAGE_RERANK_API_KEY",
    );
    drop(store);
    write_file(
        &cwd.path().join(".env"),
        "VOYAGE_RERANK_API_KEY=new-rerank\n",
    );

    let report = plan(home.path(), cwd.path(), &db_path);

    assert_eq!(report.actions.len(), 1, "{report:#?}");
    assert_eq!(report.actions[0].action, ACTION_ROTATE_ACCOUNT);
    assert_eq!(report.actions[0].account_id, "account-voyage-rerank");
}

#[test]
fn voyage_rerank_rotation_does_not_create_duplicate_when_only_rerank_exists() {
    let home = tempfile::tempdir().expect("home");
    let cwd = tempfile::tempdir().expect("cwd");
    let db_path = home.path().join(".tachi/global/tachi-global.db");
    std::fs::create_dir_all(db_path.parent().expect("parent")).expect("mkdir");
    let store = memcore::MemoryStore::open(db_path.to_str().expect("utf8")).expect("store");
    insert_custodied_api_account(
        &store,
        "account-voyage-rerank",
        "voyage",
        "old-rerank",
        "VOYAGE_RERANK_API_KEY",
    );
    drop(store);
    write_file(
        &cwd.path().join(".env"),
        "VOYAGE_RERANK_API_KEY=new-rerank\n",
    );

    let report = plan(home.path(), cwd.path(), &db_path);

    assert_eq!(report.actions.len(), 1, "{report:#?}");
    assert_eq!(report.actions[0].action, ACTION_ROTATE_ACCOUNT);
    assert_eq!(report.actions[0].account_id, "account-voyage-rerank");
}

#[test]
fn same_voyage_value_rotates_both_custody_accounts_order_independently() {
    let home = tempfile::tempdir().expect("home");
    let cwd = tempfile::tempdir().expect("cwd");
    let db_path = home.path().join(".tachi/global/tachi-global.db");
    std::fs::create_dir_all(db_path.parent().expect("parent")).expect("mkdir");
    let store = memcore::MemoryStore::open(db_path.to_str().expect("utf8")).expect("store");
    insert_custodied_api_account(
        &store,
        "account-voyage-embeddings",
        "voyage",
        "old-embeddings",
        "VOYAGE_API_KEY",
    );
    insert_custodied_api_account(
        &store,
        "account-voyage-rerank",
        "voyage",
        "old-rerank",
        "VOYAGE_RERANK_API_KEY",
    );
    drop(store);
    let env_path = cwd.path().join(".env");
    write_file(
        &env_path,
        "VOYAGE_API_KEY=same-new-value\nVOYAGE_RERANK_API_KEY=same-new-value\n",
    );

    let first = plan(home.path(), cwd.path(), &db_path);
    write_file(
        &env_path,
        "VOYAGE_RERANK_API_KEY=same-new-value\nVOYAGE_API_KEY=same-new-value\n",
    );
    let second = plan(home.path(), cwd.path(), &db_path);

    for report in [&first, &second] {
        let rotations: Vec<&str> = report
            .actions
            .iter()
            .filter(|action| action.action == ACTION_ROTATE_ACCOUNT)
            .map(|action| action.account_id.as_str())
            .collect();
        assert_eq!(
            rotations,
            vec!["account-voyage-embeddings", "account-voyage-rerank"],
            "{report:#?}"
        );
        assert_eq!(report.actions.len(), 2, "{report:#?}");
    }
    assert_eq!(first.plan_digest, second.plan_digest);
}

#[test]
fn missing_canonical_custody_never_rotates_custodyless_account() {
    let home = tempfile::tempdir().expect("home");
    let cwd = tempfile::tempdir().expect("cwd");
    let db_path = home.path().join(".tachi/global/tachi-global.db");
    std::fs::create_dir_all(db_path.parent().expect("parent")).expect("mkdir");
    let store = memcore::MemoryStore::open(db_path.to_str().expect("utf8")).expect("store");
    let fp_key = FingerprintKey::derive_from_master_key(&MASTER);
    let old_member = fp_key.key_fingerprint("deepseek", "old-fixture");
    let old_account_fp = fp_key.account_fingerprint_from_members([old_member]);
    memcore::db::insert_provider_account(
        store.connection(),
        &memcore::NewProviderAccount::api_key_pool(
            "account-without-custody",
            "deepseek",
            "va1:missing-custody",
            old_account_fp,
            memcore::AccountClass::ModelApi,
        ),
    )
    .expect("account");
    drop(store);
    write_file(
        &cwd.path().join(".env"),
        "EXTRACT_API_KEY2=new-fixture\nEXTRACT_BASE_URL=https://api.deepseek.com\n",
    );

    let report = plan(home.path(), cwd.path(), &db_path);

    assert!(report
        .actions
        .iter()
        .all(|action| action.account_id != "account-without-custody"));
    assert_eq!(
        report
            .actions
            .iter()
            .filter(|action| action.action == ACTION_CREATE_ACCOUNT)
            .count(),
        1,
        "{report:#?}"
    );
    assert!(report.actions.iter().any(|action| {
        action.action == ACTION_BIND_SLOT && action.slot.as_deref() == Some("EXTRACT_API_KEY")
    }));
}

#[test]
fn config_env_alias_does_not_suppress_missing_lane_slot_binding() {
    let home = tempfile::tempdir().expect("home");
    let cwd = tempfile::tempdir().expect("cwd");
    let db_path = home.path().join(".tachi/global/tachi-global.db");
    std::fs::create_dir_all(db_path.parent().expect("parent")).expect("mkdir");
    let store = memcore::MemoryStore::open(db_path.to_str().expect("utf8")).expect("store");
    insert_custodied_api_account(
        &store,
        "account-deepseek",
        "deepseek",
        "same-fixture",
        "DEEPSEEK_API_KEY",
    );
    memcore::db::record_provider_account_alias(
        store.connection(),
        "account-deepseek",
        "DISTILL_API_KEY",
        "config_env",
    )
    .expect("config alias");
    drop(store);
    write_file(
        &cwd.path().join(".env"),
        "DISTILL_API_KEY=same-fixture\nDISTILL_BASE_URL=https://api.deepseek.com\n",
    );

    let without_binding = plan(home.path(), cwd.path(), &db_path);
    assert_eq!(without_binding.actions.len(), 1, "{without_binding:#?}");
    assert_eq!(without_binding.actions[0].action, ACTION_BIND_SLOT);
    assert_eq!(
        without_binding.actions[0].slot.as_deref(),
        Some("DISTILL_API_KEY")
    );

    let store = memcore::MemoryStore::open(db_path.to_str().expect("utf8")).expect("store");
    memcore::db::record_provider_account_alias(
        store.connection(),
        "account-deepseek",
        "DISTILL_API_KEY",
        "lane_slot",
    )
    .expect("slot binding");
    drop(store);

    let with_binding = plan(home.path(), cwd.path(), &db_path);
    assert!(with_binding.actions.is_empty(), "{with_binding:#?}");
}

#[test]
fn unchanged_rotation_pool_member_is_a_noop() {
    let home = tempfile::tempdir().expect("home");
    let cwd = tempfile::tempdir().expect("cwd");
    let db_path = home.path().join(".tachi/global/tachi-global.db");
    std::fs::create_dir_all(db_path.parent().expect("parent")).expect("mkdir");
    let store = memcore::MemoryStore::open(db_path.to_str().expect("utf8")).expect("store");
    insert_rotation_pool_account(
        &store,
        "account-deepseek-pool",
        "deepseek",
        &["member-a", "member-b"],
        "DEEPSEEK_API_KEY",
    );
    drop(store);
    write_file(&cwd.path().join(".env"), "DEEPSEEK_API_KEY=member-a\n");

    let report = plan(home.path(), cwd.path(), &db_path);

    assert!(report.actions.is_empty(), "{report:#?}");
    let candidate = report
        .candidates
        .iter()
        .find(|candidate| candidate.logical_name == "DEEPSEEK_API_KEY")
        .expect("candidate");
    assert_eq!(
        candidate.account_id.as_deref(),
        Some("account-deepseek-pool")
    );
}

#[test]
fn unchanged_rotation_pool_member_binds_missing_lane_slot() {
    let home = tempfile::tempdir().expect("home");
    let cwd = tempfile::tempdir().expect("cwd");
    let db_path = home.path().join(".tachi/global/tachi-global.db");
    std::fs::create_dir_all(db_path.parent().expect("parent")).expect("mkdir");
    let store = memcore::MemoryStore::open(db_path.to_str().expect("utf8")).expect("store");
    insert_rotation_pool_account(
        &store,
        "account-deepseek-pool",
        "deepseek",
        &["member-a", "member-b"],
        "DEEPSEEK_API_KEY",
    );
    drop(store);
    write_file(
        &cwd.path().join(".env"),
        "DISTILL_API_KEY=member-a\nDISTILL_BASE_URL=https://api.deepseek.com\n",
    );

    let report = plan(home.path(), cwd.path(), &db_path);

    assert_eq!(report.actions.len(), 1, "{report:#?}");
    assert_eq!(report.actions[0].action, ACTION_BIND_SLOT);
    assert_eq!(report.actions[0].account_id, "account-deepseek-pool");
    assert_eq!(report.actions[0].slot.as_deref(), Some("DISTILL_API_KEY"));
}

#[test]
fn changed_rotation_pool_value_rotates_existing_account() {
    let home = tempfile::tempdir().expect("home");
    let cwd = tempfile::tempdir().expect("cwd");
    let db_path = home.path().join(".tachi/global/tachi-global.db");
    std::fs::create_dir_all(db_path.parent().expect("parent")).expect("mkdir");
    let store = memcore::MemoryStore::open(db_path.to_str().expect("utf8")).expect("store");
    insert_rotation_pool_account(
        &store,
        "account-deepseek-pool",
        "deepseek",
        &["member-a", "member-b"],
        "DEEPSEEK_API_KEY",
    );
    drop(store);
    write_file(
        &cwd.path().join(".env"),
        "DEEPSEEK_API_KEY=replacement-member\n",
    );

    let report = plan(home.path(), cwd.path(), &db_path);

    assert_eq!(report.actions.len(), 1, "{report:#?}");
    assert_eq!(report.actions[0].action, ACTION_ROTATE_ACCOUNT);
    assert_eq!(report.actions[0].account_id, "account-deepseek-pool");
}

#[test]
fn conflicting_rotation_sightings_are_order_independent_and_action_free() {
    let home = tempfile::tempdir().expect("home");
    let cwd = tempfile::tempdir().expect("cwd");
    let db_path = home.path().join(".tachi/global/tachi-global.db");
    std::fs::create_dir_all(db_path.parent().expect("parent")).expect("mkdir");
    let store = memcore::MemoryStore::open(db_path.to_str().expect("utf8")).expect("store");
    insert_custodied_api_account(
        &store,
        "account-deepseek",
        "deepseek",
        "old-fixture",
        "DEEPSEEK_API_KEY",
    );
    drop(store);
    let home_config = home.path().join(".tachi/config.env");
    let cwd_config = cwd.path().join(".env");
    write_file(&home_config, "DEEPSEEK_API_KEY=new-fixture-a\n");
    write_file(&cwd_config, "DEEPSEEK_API_KEY=new-fixture-b\n");

    let first = plan(home.path(), cwd.path(), &db_path);
    write_file(&home_config, "DEEPSEEK_API_KEY=new-fixture-b\n");
    write_file(&cwd_config, "DEEPSEEK_API_KEY=new-fixture-a\n");
    let second = plan(home.path(), cwd.path(), &db_path);

    for report in [&first, &second] {
        assert!(report.actions.is_empty(), "{report:#?}");
        let candidates: Vec<&Candidate> = report
            .candidates
            .iter()
            .filter(|candidate| candidate.logical_name == "DEEPSEEK_API_KEY")
            .collect();
        assert_eq!(candidates.len(), 2, "{report:#?}");
        assert!(candidates
            .iter()
            .all(|candidate| candidate.classification == "ambiguous"));
    }
    assert_eq!(first.plan_digest, second.plan_digest);
}

#[test]
fn retired_exact_account_is_not_eligible_for_intake_reuse() {
    assert_ineligible_exact_account_is_not_reused(|account| {
        account.status = memcore::vault::accounts::ACCOUNT_STATUS_RETIRED.to_string();
    });
}

#[test]
fn non_api_key_exact_account_is_not_eligible_for_intake_reuse() {
    assert_ineligible_exact_account_is_not_reused(|account| {
        account.auth_mode = memcore::AuthMode::BrokeredOauth;
    });
}

#[test]
fn wrong_class_exact_account_is_not_eligible_for_intake_reuse() {
    assert_ineligible_exact_account_is_not_reused(|account| {
        account.account_class = memcore::AccountClass::SearchApi;
    });
}

#[test]
fn unsupported_host_does_not_scan_env_or_codex_auth() {
    let home = tempfile::tempdir().expect("home");
    let cwd = tempfile::tempdir().expect("cwd");
    write_file(&cwd.path().join(".env"), "DEEPSEEK_API_KEY=fixture\n");
    write_file(
        &home.path().join(".codex/auth.json"),
        r#"{"tokens":{"access":"must-not-be-read"}}"#,
    );

    let (filter, notes) = parse_host_filter(Some("openclaw"));
    assert_eq!(filter, HostFilter::Unsupported);
    let discovery = discover_report(
        home.path(),
        cwd.path(),
        &home.path().join("missing.db"),
        filter,
        notes,
        None,
    )
    .expect("unsupported discovery");
    let report = build_plan(discovery);

    assert!(report.candidates.is_empty());
    assert!(report.actions.is_empty());
    assert_eq!(report.notes[0].code, "unsupported_source");
}

#[test]
fn plan_is_read_only_without_explicit_artifact_write() {
    let home = tempfile::tempdir().expect("home");
    let cwd = tempfile::tempdir().expect("cwd");
    let db_path = home.path().join(".tachi/global/tachi-global.db");
    std::fs::create_dir_all(db_path.parent().expect("parent")).expect("mkdir");
    let store = memcore::MemoryStore::open(db_path.to_str().expect("utf8")).expect("store");
    insert_custodied_api_account(
        &store,
        "account-deepseek",
        "deepseek",
        "same-fixture",
        "DEEPSEEK_API_KEY",
    );
    drop(store);
    let env_path = cwd.path().join(".env");
    write_file(&env_path, "DEEPSEEK_API_KEY=same-fixture\n");
    let db_before = std::fs::read(&db_path).expect("db before");
    let env_before = std::fs::read(&env_path).expect("env before");

    let report = plan(home.path(), cwd.path(), &db_path);

    assert!(report.actions.is_empty(), "{report:#?}");
    assert_eq!(std::fs::read(&db_path).expect("db after"), db_before);
    assert_eq!(std::fs::read(&env_path).expect("env after"), env_before);
    assert!(!cwd.path().join(".tachi/intake-plan.json").exists());
}

#[test]
fn env_parser_uses_caller_bytes_without_reopening_attribution_path() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join(".env");
    write_file(&path, "ON_DISK_API_KEY=from-disk\n");

    let parsed = parse_env_content(&path, "IN_HAND_API_KEY=from-hand\n");

    assert_eq!(
        parsed,
        vec![(path, "IN_HAND_API_KEY".to_string(), "from-hand".to_string())]
    );
}

#[test]
fn env_parser_strips_matching_quotes_and_source_paths_are_deduplicated() {
    let home = tempfile::tempdir().expect("home");
    let path = home.path().join(".env");
    let parsed = parse_env_content(
        &path,
        "DOUBLE_API_KEY=\"same-fixture\"\nSINGLE_API_KEY='same-fixture'\nPLAIN_API_KEY=same-fixture\n",
    );
    assert!(parsed.iter().all(|(_, _, value)| value == "same-fixture"));

    let paths = env_source_paths(home.path(), home.path());
    let unique: HashSet<&PathBuf> = paths.iter().collect();
    assert_eq!(paths.len(), unique.len(), "duplicate paths: {paths:#?}");
}

#[test]
fn codex_host_reads_inert_metadata_but_never_reports_auth_bytes() {
    let home = tempfile::tempdir().expect("home");
    let cwd = tempfile::tempdir().expect("cwd");
    let secret = "codex-session-fixture-secret";
    let raw = format!(r#"{{"tokens":{{"access":"{secret}"}}}}"#);
    write_file(&home.path().join(".codex/auth.json"), &raw);

    let report = discover_report(
        home.path(),
        cwd.path(),
        &home.path().join("missing.db"),
        HostFilter::Codex,
        Vec::new(),
        None,
    )
    .expect("codex discovery");

    let candidate = report
        .candidates
        .iter()
        .find(|candidate| candidate.logical_name == "codex.auth")
        .expect("codex metadata candidate");
    assert_eq!(candidate.secret_type, SECRET_TYPE_JSON_BLOB);
    assert_eq!(candidate.classification, "unknown");
    let json = serde_json::to_string(&report).expect("json");
    let human = render_discovery_human(&report);
    for forbidden in [raw.as_str(), secret] {
        assert!(!json.contains(forbidden), "JSON leaked Codex auth: {json}");
        assert!(
            !human.contains(forbidden),
            "human report leaked Codex auth: {human}"
        );
    }
    let plan = build_plan(report);
    assert!(plan.actions.is_empty(), "{plan:#?}");
}

#[test]
fn env_parser_treats_command_substitution_as_data() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join(".env");
    let marker = dir.path().join("must-not-exist");
    let value = format!("$(touch {})", marker.display());
    let parsed = parse_env_content(&path, &format!("DEEPSEEK_API_KEY={value}\n"));
    assert_eq!(parsed[0].2, value);
    assert!(!marker.exists());
}
