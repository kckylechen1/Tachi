//! `tachi-runtime-core` dependency allowlist (#2077 PR C).
//!
//! The crate may depend only on `memcore`, `tachi-params`,
//! `vault-kit`, and third-party crates already in the workspace lock.
//! A dependency on `tachi-server` or on any other workspace crate must
//! RED here — that would break the "leaf utilities only" contract.
//!
//! Dependencies come from `cargo metadata`, so every manifest form
//! (dotted tables, target tables, `package` renames, comments, quoting)
//! resolves exactly as Cargo resolves it.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

const ALLOWED_WORKSPACE_DEPS: &[&str] = &["memcore", "tachi-params", "vault-kit"];
const CRATE: &str = "tachi-runtime-core";

fn workspace_metadata() -> Value {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    let output = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
        ])
        .arg("--manifest-path")
        .arg(&manifest)
        .output()
        .expect("run cargo metadata");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("cargo metadata emits JSON")
}

#[test]
fn runtime_core_dependencies_stay_within_allowlist() {
    let metadata = workspace_metadata();
    let packages = metadata["packages"].as_array().expect("packages array");
    let workspace_names: BTreeSet<&str> = packages
        .iter()
        .filter_map(|package| package["name"].as_str())
        .collect();
    assert!(
        workspace_names.contains(CRATE),
        "{CRATE} is not a workspace member"
    );
    let core = packages
        .iter()
        .find(|package| package["name"] == CRATE)
        .expect("tachi-runtime-core package");

    // `name` is the real package name even when the manifest renames it.
    for dep in core["dependencies"].as_array().expect("dependencies array") {
        let name = dep["name"].as_str().expect("dependency name");
        assert_ne!(name, CRATE, "{CRATE} declares a self-dependency");
        if workspace_names.contains(name) {
            assert!(
                ALLOWED_WORKSPACE_DEPS.contains(&name),
                "{CRATE} depends on workspace crate '{name}' (kind {}, target {}), outside \
                 the #2077 allowlist {ALLOWED_WORKSPACE_DEPS:?}",
                dep["kind"],
                dep["target"]
            );
        }
    }
}
