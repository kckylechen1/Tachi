use super::*;
use clap::Parser;

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
