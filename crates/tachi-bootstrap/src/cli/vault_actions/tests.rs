use crate::cli::{Cli, Commands, ConnectionAction, VaultAction};
use clap::Parser;
use std::path::Path;

fn parse_connection(args: &[&str]) -> ConnectionAction {
    let argv = [vec!["tachi", "vault", "connection"], args.to_vec()].concat();
    let cli = Cli::try_parse_from(argv).expect("parse connection action");
    let Some(Commands::Vault {
        action: VaultAction::Connection { action },
    }) = cli.command
    else {
        panic!("expected connection command");
    };
    action
}

#[test]
fn connection_cli_registers_a_single_file_with_explicit_replacement() {
    assert!(matches!(
        parse_connection(&["put", "--file", "/tmp/exa.json"]),
        ConnectionAction::Put { file, replace: false }
            if file == Path::new("/tmp/exa.json")
    ));
    assert!(matches!(
        parse_connection(&["put", "--file", "/tmp/exa.json", "--replace"]),
        ConnectionAction::Put { replace: true, .. }
    ));
    Cli::try_parse_from(["tachi", "vault", "connection", "put", "--replace"])
        .expect_err("put requires an explicit JSON file");
}

#[test]
fn connection_cli_keeps_child_arguments_after_explicit_separator() {
    let action = parse_connection(&[
        "run",
        "exa",
        "--expect-revision",
        "7",
        "--cwd",
        "/tmp/project",
        "--password-file",
        "/tmp/password",
        "--",
        "node",
        "search.js",
        "--keychain",
        "query with spaces",
    ]);
    assert!(matches!(
        action,
        ConnectionAction::Run {
            name,
            expect_revision: Some(7),
            cwd: Some(cwd),
            password_file: Some(password_file),
            stdin_password: false,
            keychain: false,
            insecure_password_file: false,
            command,
        } if name == "exa"
            && cwd == Path::new("/tmp/project")
            && password_file == Path::new("/tmp/password")
            && command == ["node", "search.js", "--keychain", "query with spaces"]
    ));
    Cli::try_parse_from(["tachi", "vault", "connection", "run", "exa", "node"])
        .expect_err("run requires an explicit command separator");
}

#[test]
fn connection_cli_rejects_self_asserted_identity_and_conflicting_unlock_sources() {
    for args in [
        vec!["--consumer", "agent-a"],
        vec!["--agent", "agent-a"],
        vec!["--project", "other-project"],
        vec!["--keychain", "--stdin-password"],
        vec!["--keychain", "--password-file", "/tmp/password"],
        vec!["--insecure-password-file"],
    ] {
        let argv = [
            vec!["tachi", "vault", "connection", "run", "exa"],
            args,
            vec!["--", "true"],
        ]
        .concat();
        Cli::try_parse_from(argv).expect_err("unsupported authority/unlock flags must fail");
    }
}

#[test]
fn connection_cli_ssh_and_context_need_no_unlock_flags() {
    assert!(matches!(
        parse_connection(&["run", "dgx", "--", "hostname"]),
        ConnectionAction::Run {
            stdin_password: false,
            keychain: false,
            password_file: None,
            ..
        }
    ));
    assert!(matches!(
        parse_connection(&["context", "--cwd", "/tmp/project"]),
        ConnectionAction::Context { cwd: Some(cwd) }
            if cwd == Path::new("/tmp/project")
    ));
}
