use super::*;
use clap::{CommandFactory, Parser};

#[test]
fn wiki_corpus_cli_is_nested_under_wiki_and_defaults_to_preview() {
    let preview =
        Cli::try_parse_from(["tachi", "wiki", "corpus"]).expect("wiki corpus preview should parse");
    assert!(matches!(
        preview.command,
        Some(Commands::Wiki {
            action: WikiAction::Corpus {
                apply: false,
                confirm: None,
                backup_dir: None,
                plan: None,
                repair_sibling_damage: false,
                adopt_legacy: false,
            }
        })
    ));

    let apply = Cli::try_parse_from([
        "tachi",
        "wiki",
        "corpus",
        "--apply",
        "--confirm",
        "MIGRATE_WIKI_CORPUS_V1",
        "--backup-dir",
        "/tmp/wiki-backups",
        "--plan",
        "/tmp/wiki-plan.json",
    ])
    .expect("wiki corpus apply flags should parse");
    assert!(matches!(
        apply.command,
        Some(Commands::Wiki {
            action: WikiAction::Corpus {
                apply: true,
                confirm: Some(confirm),
                backup_dir: Some(backup_dir),
                plan: Some(plan),
                repair_sibling_damage: false,
                adopt_legacy: false,
            }
        }) if confirm == "MIGRATE_WIKI_CORPUS_V1"
            && backup_dir == std::path::Path::new("/tmp/wiki-backups")
            && plan == std::path::Path::new("/tmp/wiki-plan.json")
    ));

    let repair = Cli::try_parse_from([
        "tachi",
        "wiki",
        "corpus",
        "--repair-sibling-damage",
        "--confirm",
        "REPAIR_WIKI_CORPUS_SIBLING_DAMAGE_V1",
        "--backup-dir",
        "/tmp/wiki-backups",
    ])
    .expect("wiki corpus sibling-damage repair flags should parse");
    assert!(matches!(
        repair.command,
        Some(Commands::Wiki {
            action: WikiAction::Corpus {
                apply: false,
                confirm: Some(confirm),
                backup_dir: Some(backup_dir),
                plan: None,
                repair_sibling_damage: true,
                adopt_legacy: false,
            }
        }) if confirm == "REPAIR_WIKI_CORPUS_SIBLING_DAMAGE_V1"
            && backup_dir == std::path::Path::new("/tmp/wiki-backups")
    ));
}

/// tachi#1624: `--adopt-legacy` is a third confirmed mode of the same
/// subcommand, so the flag must parse alongside its own token and must
/// default to `false` on every pre-existing invocation.
#[test]
fn wiki_corpus_cli_parses_adopt_legacy_mode() {
    let adopt = Cli::try_parse_from([
        "tachi",
        "wiki",
        "corpus",
        "--adopt-legacy",
        "--confirm",
        "ADOPT_WIKI_LEGACY_V1",
    ])
    .expect("wiki corpus legacy-adoption flags should parse");
    assert!(matches!(
        adopt.command,
        Some(Commands::Wiki {
            action: WikiAction::Corpus {
                apply: false,
                confirm: Some(confirm),
                backup_dir: None,
                plan: None,
                repair_sibling_damage: false,
                adopt_legacy: true,
            }
        }) if confirm == "ADOPT_WIKI_LEGACY_V1"
    ));

    let preview = Cli::try_parse_from(["tachi", "wiki", "corpus"])
        .expect("bare wiki corpus must keep parsing");
    assert!(matches!(
        preview.command,
        Some(Commands::Wiki {
            action: WikiAction::Corpus {
                adopt_legacy: false,
                ..
            }
        })
    ));

    let adopt_preview = Cli::try_parse_from(["tachi", "wiki", "corpus", "--adopt-legacy"])
        .expect("legacy-adoption preview takes no token");
    assert!(matches!(
        adopt_preview.command,
        Some(Commands::Wiki {
            action: WikiAction::Corpus {
                apply: false,
                confirm: None,
                backup_dir: None,
                plan: None,
                repair_sibling_damage: false,
                adopt_legacy: true,
            }
        })
    ));
}

#[test]
fn recall_coverage_cli_requires_db_and_exposes_explicit_options() {
    let parsed = Cli::try_parse_from([
        "tachi",
        "recall-coverage",
        "--db",
        "/tmp/coverage.db",
        "--top-k",
        "7",
        "--candidates-per-channel",
        "13",
        "--limit",
        "3",
        "--equivalence-file",
        "/tmp/equivalence.json",
        "--human",
    ])
    .expect("recall-coverage invocation should parse");
    assert!(matches!(
        parsed.command,
        Some(Commands::RecallCoverage(args))
            if args.db == std::path::Path::new("/tmp/coverage.db")
                && args.top_k == Some(7)
                && args.candidates_per_channel == Some(13)
                && args.limit == Some(3)
                && args.equivalence_file.as_deref()
                    == Some(std::path::Path::new("/tmp/equivalence.json"))
                && args.human
    ));

    let missing_db = Cli::try_parse_from(["tachi", "recall-coverage"])
        .expect_err("recall-coverage must not fall back to a default store");
    assert_eq!(
        missing_db.kind(),
        clap::error::ErrorKind::MissingRequiredArgument
    );

    let mut command = Cli::command();
    let help = command
        .find_subcommand_mut("recall-coverage")
        .expect("recall-coverage subcommand")
        .render_long_help()
        .to_string();
    for flag in [
        "--db",
        "--top-k",
        "--candidates-per-channel",
        "--limit",
        "--equivalence-file",
        "--human",
    ] {
        assert!(help.contains(flag), "help must advertise {flag}");
    }
}

#[test]
fn vault_exec_cli_parses_require_and_trailing_command() {
    let parsed = Cli::try_parse_from([
        "tachi",
        "vault",
        "exec",
        "--keychain",
        "--consumer",
        "clanker",
        "--require",
        "ZHIPUAI_API_KEY,XAI_API_KEY",
        "--",
        "opencode",
        "run",
        "--auto",
    ])
    .expect("vault exec invocation should parse");

    assert!(matches!(
        parsed.command,
        Some(Commands::Vault {
            action: VaultAction::Exec {
                keychain: true,
                consumer: Some(consumer),
                require,
                allow_unauthenticated: false,
                command,
                ..
            }
        }) if consumer == "clanker"
            && require == ["ZHIPUAI_API_KEY", "XAI_API_KEY"]
            && command == ["opencode", "run", "--auto"]
    ));
    assert!(Cli::try_parse_from(["tachi", "vault", "exec", "--keychain"]).is_err());
}

// #1413 concern 4: --allow-unauthenticated is an explicit opt-in to the
// inherited-environment fail-open path. Default is false (asserted above); the
// flag must parse to true and stay compatible with --require.
#[test]
fn vault_exec_cli_parses_allow_unauthenticated_opt_in() {
    let parsed = Cli::try_parse_from([
        "tachi",
        "vault",
        "exec",
        "--keychain",
        "--allow-unauthenticated",
        "--",
        "opencode",
        "run",
    ])
    .expect("--allow-unauthenticated should parse");

    assert!(matches!(
        parsed.command,
        Some(Commands::Vault {
            action: VaultAction::Exec {
                allow_unauthenticated: true,
                command,
                ..
            }
        }) if command == ["opencode", "run"]
    ));
}

#[test]
fn vault_providers_doctor_parses_explicit_report_only_password_source() {
    let parsed = Cli::try_parse_from([
        "tachi",
        "vault",
        "doctor",
        "--providers",
        "--opencode-config",
        "/tmp/opencode-fixture.json",
        "--password-file",
        "/tmp/vault-password-fixture",
    ])
    .expect("provider doctor comparison invocation should parse");

    assert!(matches!(
        parsed.command,
        Some(Commands::Vault {
            action: VaultAction::Doctor {
                providers: true,
                opencode_config: Some(config),
                password_file: Some(password),
                ..
            }
        }) if config == std::path::Path::new("/tmp/opencode-fixture.json")
            && password == std::path::Path::new("/tmp/vault-password-fixture")
    ));
    assert!(
        Cli::try_parse_from([
            "tachi",
            "vault",
            "doctor",
            "--profile",
            "fixture",
            "--consumer",
            "fixture",
            "--stdin-password"
        ])
        .is_err(),
        "provider comparison password sources must not attach to profile doctor mode"
    );
}

#[test]
fn vault_providers_doctor_rejects_multiple_password_sources() {
    for args in [
        vec!["--stdin-password", "--keychain"],
        vec![
            "--stdin-password",
            "--password-file",
            "/tmp/vault-password-fixture",
        ],
        vec![
            "--keychain",
            "--password-file",
            "/tmp/vault-password-fixture",
        ],
    ] {
        let mut argv = vec!["tachi", "vault", "doctor", "--providers"];
        argv.extend(args);
        let err = Cli::try_parse_from(argv)
            .expect_err("provider doctor password sources must be mutually exclusive");
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }
}

#[test]
fn exact_dedupe_cli_is_nested_under_repair_dedupe() {
    let parsed = Cli::try_parse_from([
        "tachi",
        "repair",
        "dedupe",
        "exact",
        "--db",
        "project:test",
        "--output",
        "plan.json",
        "--limit",
        "1",
        "--path-prefix",
        "/wiki",
    ])
    .expect("nested exact-dedupe plan should parse");
    assert!(matches!(
        parsed.command,
        Some(Commands::Repair {
            action: Some(RepairAction::Dedupe {
                action: DedupeAction::Exact { db, output, limit: Some(1), path_prefix: Some(prefix) }
            }), ..
        }) if db == "project:test" && output == std::path::Path::new("plan.json") && prefix == "/wiki"
    ));
    assert!(
        Cli::try_parse_from(["tachi", "repair", "exact", "--db", "x", "--output", "p"]).is_err()
    );
    assert!(
        Cli::try_parse_from(["tachi", "repair", "apply", "--db", "x", "--plan", "p", "--yes"])
            .is_err()
    );

    let parsed = Cli::try_parse_from([
        "tachi",
        "repair",
        "dedupe",
        "apply",
        "--db",
        "project:test",
        "--plan",
        "plan.json",
        "--yes",
        "--receipt-out",
        "receipt.json",
    ])
    .expect("nested exact-dedupe apply should parse");
    assert!(matches!(
        parsed.command,
        Some(Commands::Repair {
            action: Some(RepairAction::Dedupe {
                action: DedupeAction::Apply { db, plan, yes: true, receipt_out }
            }), ..
        }) if db == "project:test"
            && plan == std::path::Path::new("plan.json")
            && receipt_out == std::path::Path::new("receipt.json")
    ));

    let parsed = Cli::try_parse_from([
        "tachi",
        "repair",
        "dedupe",
        "restore",
        "--db",
        "project:test",
        "--receipt",
        "receipt.json",
        "--yes",
    ])
    .expect("nested exact-dedupe restore should parse");
    assert!(matches!(
        parsed.command,
        Some(Commands::Repair {
            action: Some(RepairAction::Dedupe {
                action: DedupeAction::Restore { db, receipt, yes: true }
            }), ..
        }) if db == "project:test" && receipt == std::path::Path::new("receipt.json")
    ));
}

#[test]
fn lifecycle_consistency_cli_exposes_plan_apply_restore_boundaries() {
    let parsed = Cli::try_parse_from([
        "tachi",
        "repair",
        "lifecycle",
        "plan",
        "--db",
        "project:test",
        "--output",
        "plan.json",
        "--json",
    ])
    .expect("lifecycle plan should parse");
    assert!(matches!(
        parsed.command,
        Some(Commands::Repair {
            action: Some(RepairAction::Lifecycle {
                action: LifecycleConsistencyAction::Plan { db, output, json: true }
            }), ..
        }) if db == "project:test" && output == std::path::Path::new("plan.json")
    ));

    let parsed = Cli::try_parse_from([
        "tachi",
        "repair",
        "lifecycle",
        "apply",
        "--db",
        "project:test",
        "--plan",
        "plan.json",
        "--receipt-out",
        "receipt.json",
    ])
    .expect("lifecycle apply parses without confirmation for runtime refusal");
    assert!(matches!(
        parsed.command,
        Some(Commands::Repair {
            action: Some(RepairAction::Lifecycle {
                action: LifecycleConsistencyAction::Apply { db, plan, yes: false, receipt_out }
            }), ..
        }) if db == "project:test"
            && plan == std::path::Path::new("plan.json")
            && receipt_out == std::path::Path::new("receipt.json")
    ));

    let parsed = Cli::try_parse_from([
        "tachi",
        "repair",
        "lifecycle",
        "restore",
        "--db",
        "project:test",
        "--receipt",
        "receipt.json",
        "--yes",
    ])
    .expect("lifecycle restore should parse");
    assert!(matches!(
        parsed.command,
        Some(Commands::Repair {
            action: Some(RepairAction::Lifecycle {
                action: LifecycleConsistencyAction::Restore { db, receipt, yes: true }
            }), ..
        }) if db == "project:test" && receipt == std::path::Path::new("receipt.json")
    ));
}

#[test]
fn capture_archive_commands_parse_and_mutations_require_confirm() {
    let plan = Cli::try_parse_from([
        "tachi",
        "foundry",
        "capture-archive-plan",
        "--db",
        "/tmp/a.db",
        "--as-of",
        "2026-07-20T00:00:00Z",
        "--output",
        "plan.json",
    ])
    .expect("capture archive plan should parse");
    assert!(matches!(
        plan.command,
        Some(Commands::Foundry { action: FoundryAction::CaptureArchivePlan { db: Some(db), as_of: Some(as_of), output: Some(output) } })
            if db == std::path::Path::new("/tmp/a.db")
                && as_of == "2026-07-20T00:00:00Z"
                && output == std::path::Path::new("plan.json")
    ));

    let apply = Cli::try_parse_from([
        "tachi",
        "foundry",
        "capture-archive-apply",
        "--plan",
        "plan.json",
        "--confirm",
    ])
    .expect("confirmed capture archive apply should parse");
    assert!(matches!(
        apply.command,
        Some(Commands::Foundry { action: FoundryAction::CaptureArchiveApply { plan, confirm: true, .. } })
            if plan == std::path::Path::new("plan.json")
    ));
    assert!(Cli::try_parse_from([
        "tachi",
        "foundry",
        "capture-archive-apply",
        "--plan",
        "plan.json"
    ])
    .is_err());

    let restore = Cli::try_parse_from([
        "tachi",
        "foundry",
        "capture-archive-restore",
        "--receipt",
        "receipt.json",
        "--confirm",
    ])
    .expect("confirmed capture archive restore should parse");
    assert!(matches!(
        restore.command,
        Some(Commands::Foundry { action: FoundryAction::CaptureArchiveRestore { receipt, confirm: true, .. } })
            if receipt == std::path::Path::new("receipt.json")
    ));
    assert!(Cli::try_parse_from([
        "tachi",
        "foundry",
        "capture-archive-restore",
        "--receipt",
        "receipt.json"
    ])
    .is_err());
}

#[test]
fn card_cli_parses_list_and_show() {
    let list =
        Cli::try_parse_from(["tachi", "card", "list", "--json"]).expect("card list should parse");
    match list.command.expect("command") {
        Commands::Card {
            action: CardAction::List { json },
        } => assert!(json),
        other => panic!("unexpected command: {other:?}"),
    }

    let show = Cli::try_parse_from(["tachi", "card", "show", "codex_55_review"])
        .expect("card show should parse");
    match show.command.expect("command") {
        Commands::Card {
            action: CardAction::Show { id, json },
        } => {
            assert_eq!(id, "codex_55_review");
            assert!(!json);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

/// tachi#1202: `cards` (plural, dispatch-ledger lane-card ingest) must parse
/// as a DISTINCT command from `card` (singular, Tachikoma dispatch-profile
/// projection) above — regression guard against the two verbs colliding or
/// clap accidentally treating one as an alias of the other.
#[test]
fn cards_cli_parses_sync_and_list_distinct_from_singular_card() {
    let sync = Cli::try_parse_from([
        "tachi",
        "cards",
        "sync",
        "--dir",
        "/tmp/fixture-cards",
        "--json",
    ])
    .expect("cards sync should parse");
    match sync.command.expect("command") {
        Commands::Cards {
            action: CardsAction::Sync { dir, json },
        } => {
            assert_eq!(dir, Some(std::path::PathBuf::from("/tmp/fixture-cards")));
            assert!(json);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let list = Cli::try_parse_from(["tachi", "cards", "list"]).expect("cards list should parse");
    match list.command.expect("command") {
        Commands::Cards {
            action: CardsAction::List { json },
        } => assert!(!json),
        other => panic!("unexpected command: {other:?}"),
    }

    // The pre-existing singular `card` command must still parse unaffected.
    let card_list =
        Cli::try_parse_from(["tachi", "card", "list"]).expect("card list should still parse");
    match card_list.command.expect("command") {
        Commands::Card {
            action: CardAction::List { json },
        } => assert!(!json),
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn cards_governance_cli_parses_explicit_json_artifacts() {
    let draft =
        Cli::try_parse_from(["tachi", "cards", "draft", "--input", "request.json"]).unwrap();
    assert!(
        matches!(draft.command, Some(Commands::Cards { action: CardsAction::Draft { input } }) if input == std::path::Path::new("request.json"))
    );
    let apply = Cli::try_parse_from([
        "tachi",
        "cards",
        "apply",
        "--approval",
        "approval.json",
        "--evidence",
        "fresh.json",
        "--dir",
        "/cards",
    ])
    .unwrap();
    assert!(
        matches!(apply.command, Some(Commands::Cards { action: CardsAction::Apply { approval, evidence, dir } }) if approval == std::path::Path::new("approval.json") && evidence == std::path::Path::new("fresh.json") && dir.as_deref() == Some(std::path::Path::new("/cards")))
    );
}

#[test]
fn poke_cli_parses_smoke_run() {
    let parsed = Cli::try_parse_from(["tachi", "poke", "run", "--suite", "smoke", "--json"])
        .expect("poke run should parse");
    match parsed.command.expect("command") {
        Commands::Poke {
            action: PokeAction::Run { suite, json },
        } => {
            assert_eq!(suite, "smoke");
            assert!(json);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn eval_cli_parses_recall_gate() {
    let parsed = Cli::try_parse_from([
        "tachi",
        "eval",
        "recall",
        "--top-k",
        "12",
        "--min-recall",
        "0.95",
        "--enable-rerank",
        "--json",
    ])
    .expect("eval recall should parse");
    match parsed.command.expect("command") {
        Commands::Eval {
            action:
                EvalAction::Recall {
                    cases,
                    top_k,
                    min_recall,
                    min_mrr,
                    enable_rerank,
                    json,
                },
        } => {
            assert!(cases.is_none());
            assert_eq!(top_k, 12);
            assert_eq!(min_recall, 0.95);
            assert_eq!(min_mrr, 0.0);
            assert!(enable_rerank);
            assert!(json);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn mcp_cli_parses_add_with_vault_header() {
    let parsed = Cli::try_parse_from([
        "tachi",
        "mcp",
        "add",
        "context7",
        "https://mcp.context7.com/mcp",
        "--transport",
        "http",
        "--header",
        "Authorization: Bearer ${vault:CONTEXT7_API_KEY}",
        "--json",
    ])
    .expect("mcp add should parse");

    match parsed.command.expect("command") {
        Commands::Mcp {
            action:
                McpAction::Add {
                    name,
                    url,
                    transport,
                    headers,
                    key,
                    stdin_password,
                    keychain,
                    password_file,
                    insecure_password_file,
                    json,
                },
        } => {
            assert_eq!(name, "context7");
            assert_eq!(url, "https://mcp.context7.com/mcp");
            assert_eq!(transport, "http");
            assert_eq!(headers, ["Authorization: Bearer ${vault:CONTEXT7_API_KEY}"]);
            assert!(key.is_none());
            assert!(!stdin_password);
            assert!(!keychain);
            assert!(password_file.is_none());
            assert!(!insecure_password_file);
            assert!(json);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn mcp_cli_parses_key_flag_with_vault_unlock_options() {
    let parsed = Cli::try_parse_from([
        "tachi",
        "mcp",
        "add",
        "context7",
        "https://mcp.context7.com/mcp",
        "--key",
        "TEST_SENTINEL_VALUE_DO_NOT_STORE",
        "--keychain",
    ])
    .expect("--key parses with vault unlock options");

    match parsed.command.expect("command") {
        Commands::Mcp {
            action: McpAction::Add { key, keychain, .. },
        } => {
            assert_eq!(key.as_deref(), Some("TEST_SENTINEL_VALUE_DO_NOT_STORE"));
            assert!(keychain);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn skill_surface_cli_parses_sources() {
    let parsed = Cli::try_parse_from(["tachi", "skill-surface", "sources", "--json"])
        .expect("skill-surface sources should parse");
    match parsed.command.expect("command") {
        Commands::SkillSurface {
            action: SkillSurfaceAction::Sources { json },
        } => assert!(json),
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn skill_surface_cli_parses_sync_plan() {
    let parsed = Cli::try_parse_from(["tachi", "skill-surface", "sync-plan", "--json"])
        .expect("skill-surface sync-plan should parse");
    match parsed.command.expect("command") {
        Commands::SkillSurface {
            action: SkillSurfaceAction::SyncPlan { json },
        } => assert!(json),
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn injection_surface_cli_parses_doctor() {
    let parsed = Cli::try_parse_from([
        "tachi",
        "injection-surface",
        "doctor",
        "--registry",
        "/tmp/fleet.json",
        "--home",
        "/tmp/fixture-home",
        "--json",
    ])
    .expect("injection-surface doctor should parse");
    match parsed.command.expect("command") {
        Commands::InjectionSurface {
            action:
                InjectionSurfaceAction::Doctor {
                    registry,
                    home,
                    json,
                },
        } => {
            assert_eq!(registry, std::path::PathBuf::from("/tmp/fleet.json"));
            assert_eq!(home, Some(std::path::PathBuf::from("/tmp/fixture-home")));
            assert!(json);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn backfill_vectors_accepts_named_project() {
    let parsed = Cli::try_parse_from([
        "tachi",
        "backfill-vectors",
        "--project",
        "sigil",
        "--dry-run",
    ])
    .expect("backfill-vectors --project should parse");
    match parsed.command.expect("command") {
        Commands::BackfillVectors {
            db,
            project,
            dry_run,
            ..
        } => {
            assert!(db.is_none());
            assert_eq!(project.as_deref(), Some("sigil"));
            assert!(dry_run);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn backfill_vectors_all_projects_is_exclusive_and_cache_is_opt_in() {
    let parsed = Cli::try_parse_from(["tachi", "backfill-vectors", "--all-projects", "--dry-run"])
        .expect("all-projects dry run should parse");
    match parsed.command.expect("command") {
        Commands::BackfillVectors {
            db,
            project,
            all_projects,
            dry_run,
            include_cache,
            ..
        } => {
            assert!(db.is_none());
            assert!(project.is_none());
            assert!(all_projects);
            assert!(dry_run);
            assert!(!include_cache, "recall-cache rows must be explicit opt-in");
        }
        other => panic!("unexpected command: {other:?}"),
    }

    for conflicting_args in [
        vec!["--db", "/tmp/memory.db", "--all-projects"],
        vec!["--project", "sigil", "--all-projects"],
        vec!["--db", "/tmp/memory.db", "--project", "sigil"],
    ] {
        let mut args = vec!["tachi", "backfill-vectors"];
        args.extend(conflicting_args);
        assert!(Cli::try_parse_from(args).is_err());
    }
}

#[test]
fn clean_sweep_uses_cli_default_max_age() {
    let parsed =
        Cli::try_parse_from(["tachi", "clean", "sweep"]).expect("clean sweep default should parse");
    match parsed.command.expect("command") {
        Commands::Clean {
            action:
                CleanAction::Sweep {
                    max_age_days,
                    dry_run,
                    ..
                },
        } => {
            assert_eq!(max_age_days, DEFAULT_WORKTREE_SWEEP_MAX_AGE_DAYS);
            assert!(!dry_run);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn clean_orphans_defaults_to_preview_and_the_shared_age_gate() {
    let parsed = Cli::try_parse_from(["tachi", "clean", "orphans"])
        .expect("clean orphans default should parse");
    match parsed.command.expect("command") {
        Commands::Clean {
            action:
                CleanAction::Orphans {
                    root,
                    max_age_days,
                    force,
                    ..
                },
        } => {
            assert!(root.is_empty(), "roots default to the scratch volumes");
            assert_eq!(max_age_days, DEFAULT_ORPHAN_REAP_MAX_AGE_DAYS);
            // Deleting gigabytes is opt-in: the default run must be a preview.
            assert!(!force, "clean orphans must not delete without --force");
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn clean_orphans_rejects_force_with_dry_run() {
    let parsed = Cli::try_parse_from(["tachi", "clean", "orphans", "--force", "--dry-run"]);
    assert!(
        parsed.is_err(),
        "--force and --dry-run are contradictory and must not both parse"
    );
}

#[test]
fn worktree_open_parses_managed_flags() {
    use crate::cli::WorktreeAction;
    let parsed = Cli::try_parse_from([
        "tachi",
        "worktree",
        "open",
        "--repo",
        "/tmp/repo",
        "--task",
        "484",
        "--role",
        "executor",
        "--dry-run",
        "--json",
    ])
    .expect("worktree open should parse");
    match parsed.command.expect("command") {
        Commands::Worktree {
            action: WorktreeAction::Open(args),
        } => {
            assert_eq!(args.repo, std::path::PathBuf::from("/tmp/repo"));
            assert_eq!(args.task.as_deref(), Some("484"));
            assert_eq!(args.role.as_deref(), Some("executor"));
            assert!(args.dry_run);
            assert!(args.json);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

/// #894 S2c: the DEFAULT provisioning class is `edit-only` — a worktree that
/// gets no build target dir. If this flips to a build class by default, every
/// dispatched lane silently starts allocating target dirs again.
#[test]
fn worktree_open_defaults_to_the_edit_only_class() {
    use crate::cli::WorktreeAction;
    let parsed = Cli::try_parse_from(["tachi", "worktree", "open", "--repo", "/tmp/repo"])
        .expect("worktree open should parse");
    match parsed.command.expect("command") {
        Commands::Worktree {
            action: WorktreeAction::Open(args),
        } => {
            assert_eq!(args.env_class, "edit-only");
            assert!(args.approve_private_target.is_none());
            assert!(args.reserve_bytes.is_none());
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

/// #894 S2c round-2: `build cancel` exists and is how a queued ticket leaves the
/// queue. Without it, a ticket the seat cannot run has no exit that is not a
/// dead letter three failures later.
#[test]
fn build_cancel_parses_and_defaults_its_reason() {
    use crate::cli::BuildAction;
    let parsed = Cli::try_parse_from(["tachi", "build", "cancel", "--ticket-id", "bt-7"])
        .expect("build cancel should parse");
    match parsed.command.expect("command") {
        Commands::Build {
            action: BuildAction::Cancel {
                ticket_id, reason, ..
            },
        } => {
            assert_eq!(ticket_id, "bt-7");
            assert!(
                !reason.is_empty(),
                "a cancellation is always recorded WITH a reason"
            );
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn build_submit_parses_a_trailing_command_and_defaults_head_to_head() {
    use crate::cli::BuildAction;
    let parsed = Cli::try_parse_from([
        "tachi",
        "build",
        "submit",
        "--repo",
        "/tmp/repo",
        "--env-id",
        "env-7",
        "--",
        "cargo",
        "test",
        "-p",
        "memcore",
    ])
    .expect("build submit should parse");
    match parsed.command.expect("command") {
        Commands::Build {
            action:
                BuildAction::Submit {
                    repo,
                    head,
                    env_id,
                    command,
                    ..
                },
        } => {
            assert_eq!(repo, std::path::PathBuf::from("/tmp/repo"));
            assert_eq!(head, "HEAD");
            assert_eq!(env_id.as_deref(), Some("env-7"));
            assert_eq!(command, vec!["cargo", "test", "-p", "memcore"]);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn vault_sync_help_names_offline_guessing_risk() {
    let mut export_cmd = Cli::command();
    let export_help = export_cmd
        .find_subcommand_mut("vault")
        .expect("vault command")
        .find_subcommand_mut("sync-export")
        .expect("sync-export command")
        .render_long_help()
        .to_string();
    assert!(
        export_help.contains("offline password guessing"),
        "{export_help}"
    );
    assert!(export_help.contains("--allow-cloud"), "{export_help}");
    // #576 residual surface reduction path must remain discoverable.
    assert!(
        export_help.contains("--entries-only") || export_help.contains("entries-only"),
        "export help must document --entries-only: {export_help}"
    );

    let mut import_cmd = Cli::command();
    let import_help = import_cmd
        .find_subcommand_mut("vault")
        .expect("vault command")
        .find_subcommand_mut("sync-import")
        .expect("sync-import command")
        .render_long_help()
        .to_string();
    assert!(
        import_help.contains("offline password guessing"),
        "{import_help}"
    );
    assert!(import_help.contains("--allow-unsigned"), "{import_help}");
}

#[test]
fn help_reports_api_only_distill_and_canonical_db_paths() {
    let mut root_cmd = Cli::command();
    let root_help = root_cmd.render_long_help().to_string();
    assert!(
        root_help.contains("Run batch memory distill through the configured API lane"),
        "{root_help}"
    );
    assert!(
        root_help.contains("never launches Claude CLI"),
        "{root_help}"
    );
    assert!(
        !root_help.contains("Claude CLI when configured"),
        "{root_help}"
    );
    assert!(root_help.contains(".tachi/tachi-memory.db"), "{root_help}");

    let mut backfill_cmd = Cli::command();
    let backfill_help = backfill_cmd
        .find_subcommand_mut("backfill-vectors")
        .expect("backfill-vectors command")
        .render_long_help()
        .to_string();
    assert!(
        backfill_help.contains("~/.tachi/projects/<name>/tachi-memory.db"),
        "{backfill_help}"
    );
    assert!(
        !backfill_help.contains("~/.tachi/projects/<name>/memory.db"),
        "{backfill_help}"
    );
}

// Regression coverage for the `save`-alias / `--text` CLI ergonomics bug: a
// user ran `tachi save --path X --text "content"` and hit
// `error: unexpected argument '--text' found` with a misleading `-- --text`
// tip and a `Usage: tachi remember ...` line that never explained `save` is
// an alias of `remember`. `--text` is now an accepted named form of the
// positional TEXT argument, and `save` is a *visible* alias so it shows up
// in `--help` output.

fn effective_remember_text(cli: &Cli) -> Option<String> {
    match &cli.command {
        Some(Commands::Remember {
            text, text_flag, ..
        }) => text.clone().or_else(|| text_flag.clone()),
        other => panic!("expected Commands::Remember, got {other:?}"),
    }
}

#[test]
fn save_text_flag_is_equivalent_to_remember_positional_text() {
    // Exact repro from the bug report, modulo the actual note text.
    let via_save_flag = Cli::try_parse_from([
        "tachi",
        "save",
        "--path",
        "/scratch/x",
        "--text",
        "hello world",
    ])
    .expect("`tachi save --path X --text Y` must parse: --text is now an accepted named form");

    let via_remember_positional =
        Cli::try_parse_from(["tachi", "remember", "--path", "/scratch/x", "hello world"])
            .expect("`tachi remember --path X Y` positional form must still parse");

    assert_eq!(
        effective_remember_text(&via_save_flag),
        effective_remember_text(&via_remember_positional),
        "save --text and remember <TEXT> must resolve to the same effective text"
    );
    assert_eq!(
        effective_remember_text(&via_save_flag).as_deref(),
        Some("hello world")
    );

    // `save` really did resolve through the `Remember` variant (i.e. it is
    // dispatching as the alias, not some separate command), and --path
    // still parses correctly alongside --text.
    assert!(matches!(
        via_save_flag.command,
        Some(Commands::Remember { ref path, .. }) if path.as_deref() == Some("/scratch/x")
    ));
}

#[test]
fn remember_positional_still_works_without_text_flag() {
    let parsed = Cli::try_parse_from(["tachi", "remember", "plain positional note"])
        .expect("bare positional TEXT must still parse");
    assert_eq!(
        effective_remember_text(&parsed).as_deref(),
        Some("plain positional note")
    );
}

#[test]
fn remember_text_flag_alone_works_without_positional() {
    let parsed = Cli::try_parse_from(["tachi", "remember", "--text", "flag-only note"])
        .expect("`--text` alone (no positional) must parse");
    assert_eq!(
        effective_remember_text(&parsed).as_deref(),
        Some("flag-only note")
    );
}

#[test]
fn remember_rejects_both_positional_and_text_flag() {
    let err = Cli::try_parse_from([
        "tachi",
        "remember",
        "--text",
        "flag note",
        "positional note",
    ])
    .expect_err(
        "passing both the positional TEXT and --text must be a clear conflict, not a silent pick",
    );
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn remember_rejects_missing_text_entirely() {
    let err = Cli::try_parse_from(["tachi", "remember"])
        .expect_err("remember with neither positional TEXT nor --text must fail clearly");
    assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
}

#[test]
fn save_is_a_discoverable_visible_alias_of_remember() {
    // Before this fix `save` was a hidden `alias`: it parsed, but nothing in
    // `--help` output told the user `save` and `remember` are the same
    // command, so the Usage line "changing name" on error looked like a bug
    // rather than a documented alias.
    //
    // Only the *parent's* subcommand list renders `[aliases: ...]`
    // (clap_builder-4.6.0 output/help_template.rs: `sc_spec_vals` is reached
    // solely from `write_subcommand`/`will_subcommands_wrap`), so the root
    // long help is the surface that discriminates `visible_alias` from a
    // hidden `alias`. Asserting on `remember --help` would NOT: a subcommand's
    // own `render_long_help()` never prints its own aliases, and the string
    // "save" appears there anyway via the `--text` arg's doc comment.
    let root_help = Cli::command().render_long_help().to_string();
    assert!(
        root_help.contains("save"),
        "top-level --help should surface the `save` alias next to `remember`: {root_help}"
    );
}
