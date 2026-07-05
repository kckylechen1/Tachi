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
