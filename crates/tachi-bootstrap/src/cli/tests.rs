use super::*;
use clap::{CommandFactory, Parser};

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
fn backfill_vectors_rejects_db_and_project_together() {
    let parsed = Cli::try_parse_from([
        "tachi",
        "backfill-vectors",
        "--db",
        "/tmp/memory.db",
        "--project",
        "sigil",
    ]);
    assert!(parsed.is_err());
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
            action:
                WorktreeAction::Open {
                    repo,
                    task,
                    role,
                    dry_run,
                    json,
                    ..
                },
        } => {
            assert_eq!(repo, std::path::PathBuf::from("/tmp/repo"));
            assert_eq!(task.as_deref(), Some("484"));
            assert_eq!(role.as_deref(), Some("executor"));
            assert!(dry_run);
            assert!(json);
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
            action:
                WorktreeAction::Open {
                    env_class,
                    approve_private_target,
                    reserve_bytes,
                    ..
                },
        } => {
            assert_eq!(env_class, "edit-only");
            assert!(approve_private_target.is_none());
            assert!(reserve_bytes.is_none());
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
