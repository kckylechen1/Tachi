use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const CHILD_ARGS: &str = "TACHI_NATIVE_SKILL_TEST_ARGS";

struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run_cli(root: &Path, args: &[&str]) -> Output {
    let mut command_args = vec![
        "tachi".to_string(),
        "--global-db".to_string(),
        root.join("unexpected.db").to_str().unwrap().to_string(),
        "--no-project-db".to_string(),
        "skill-surface".to_string(),
    ];
    command_args.extend(args.iter().map(|arg| arg.to_string()));
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "native_skill_cli_roundtrip", "--nocapture"])
        .env(CHILD_ARGS, serde_json::to_string(&command_args).unwrap())
        .env("HOME", root.join("home"))
        .env("USERPROFILE", root.join("home"))
        .env("TACHI_HOME", root.join("home/.tachi"))
        .env("TOKIO_WORKER_THREADS", "1")
        .current_dir(root)
        .output()
        .expect("run the real CLI in an isolated child process")
}

fn assert_success(output: Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn native_skill_cli_roundtrip() {
    // The same test entrypoint runs one real CLI invocation per isolated child.
    // HOME, TACHI_HOME, and cwd never point at a developer's directories.
    if let Ok(args) = std::env::var(CHILD_ARGS) {
        let args: Vec<String> = serde_json::from_str(&args).unwrap();
        tachi_server::bootstrap_test_api::run_native_skill_cli(&args).unwrap();
        return;
    }
    let f =
        Fixture(std::env::temp_dir().join(format!("tachi-native-skills-{}", uuid::Uuid::new_v4())));
    let root = &f.0;
    let source = root.join("central/example-skill");
    let target = root.join("native-skills");
    let conflict = root.join("another-host");
    let native = target.join("example-skill");
    fs::create_dir_all(source.join("references")).unwrap();
    fs::create_dir_all(source.join("scripts")).unwrap();
    fs::create_dir_all(root.join("home")).unwrap();
    fs::create_dir_all(&target).unwrap();
    fs::create_dir_all(conflict.join("example-skill")).unwrap();
    fs::write(
        source.join("SKILL.md"),
        "---\nname: example-skill\ndescription: Fixture\n---\nRead references/guide.md.",
    )
    .unwrap();
    fs::write(source.join("references/guide.md"), "original").unwrap();
    fs::write(source.join("scripts/helper.txt"), "asset, never executed").unwrap();
    fs::write(conflict.join("example-skill/valuable.txt"), "keep").unwrap();
    let source_path = source.to_str().unwrap();
    let target_path = target.to_str().unwrap();

    assert_success(run_cli(
        root,
        &["links", "--target-dir", target_path, "--json"],
    ));
    assert!(!target.join(".tachi-skill-links").exists());
    let install = [
        "link",
        "--source",
        source_path,
        "--target-dir",
        target_path,
        "--json",
    ];
    assert_success(run_cli(root, &install));
    assert!(fs::symlink_metadata(&native)
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(native.join("SKILL.md").is_file());
    assert_eq!(
        fs::read_to_string(native.join("scripts/helper.txt")).unwrap(),
        "asset, never executed"
    );
    fs::write(source.join("references/guide.md"), "edited once").unwrap();
    assert_eq!(
        fs::read_to_string(native.join("references/guide.md")).unwrap(),
        "edited once"
    );

    let receipt = target.join(".tachi-skill-links/example-skill/receipt.json");
    let installed_identity = fs::read(&receipt).unwrap();
    assert_success(run_cli(root, &install));
    assert_eq!(fs::read(&receipt).unwrap(), installed_identity);
    let rejected = run_cli(
        root,
        &[
            "link",
            "--source",
            source_path,
            "--target-dir",
            conflict.to_str().unwrap(),
        ],
    );
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("unmanaged"));
    assert_eq!(
        fs::read_to_string(conflict.join("example-skill/valuable.txt")).unwrap(),
        "keep"
    );

    let uninstall = [
        "unlink",
        "example-skill",
        "--target-dir",
        target_path,
        "--json",
    ];
    assert_success(run_cli(root, &uninstall));
    assert!(fs::symlink_metadata(&native).is_err());
    assert!(source.join("SKILL.md").is_file());
    assert_eq!(
        fs::read_to_string(source.join("references/guide.md")).unwrap(),
        "edited once"
    );
    assert_success(run_cli(root, &uninstall));
    assert!(
        !root.join("home/.tachi").exists(),
        "native CLI must bypass home/log startup writes"
    );
    assert!(
        !root.join("unexpected.db").exists(),
        "native CLI must bypass database startup"
    );
}
