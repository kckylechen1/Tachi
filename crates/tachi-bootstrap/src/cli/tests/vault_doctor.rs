use super::*;

#[test]
fn vault_profile_doctor_accepts_optional_run_directory() {
    for run_dir in [None, Some("/tmp/credential-run-fixture")] {
        let mut args = vec![
            "tachi",
            "vault",
            "doctor",
            "--profile",
            "fixture",
            "--consumer",
            "worker",
        ];
        if let Some(path) = run_dir {
            args.extend(["--run-dir", path]);
        }
        let parsed = Cli::try_parse_from(args).expect("profile doctor should parse");
        assert!(matches!(parsed.command,
            Some(Commands::Vault {
                action: VaultAction::Doctor { providers: false, run_dir: parsed_run_dir, .. }
            }) if parsed_run_dir.as_deref() == run_dir.map(std::path::Path::new)
        ));
    }
}

#[test]
fn vault_providers_doctor_rejects_profile_run_directory() {
    let error = Cli::try_parse_from([
        "tachi",
        "vault",
        "doctor",
        "--providers",
        "--run-dir",
        "/tmp/credential-run-fixture",
    ])
    .expect_err("provider doctor has no credential-profile run directory");
    assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
}
