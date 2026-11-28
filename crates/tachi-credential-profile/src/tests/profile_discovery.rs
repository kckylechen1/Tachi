#[test]
fn credential_profile_json_parse_retains_legacy_kinds_for_diagnostics() {
    let raw = r#"{
      "credential_profiles": {
        "codex_shared": {
          "provider": "openai_codex",
          "description": "Shared Codex auth",
          "entries": {
            "api_key": "OPENAI_API_KEY",
            "auth_json": "CODEX_AUTH_JSON"
          },
          "allowed_consumers": {
            "agents": ["codex_cli"],
            "profiles": ["codex_55_review"]
          },
          "materializers": [
            {"type": "env", "source": "api_key", "target": "OPENAI_API_KEY"},
            {"type": "file_copy", "source": "auth_json", "target": "~/.codex/auth.json", "chmod": "0600"},
            {"type": "config_content_env", "source": "api_key", "target": "OPENCODE_CONFIG_CONTENT", "template": {"provider": "openai"}}
          ]
        }
      }
    }"#;

    let doc: crate::CredentialProfileDocument =
        serde_json::from_str(raw).expect("profile document parses");
    let profile = doc
        .credential_profiles
        .get("codex_shared")
        .expect("profile exists");
    assert_eq!(profile.provider.as_deref(), Some("openai_codex"));
    assert_eq!(profile.materializers.len(), 3);
    assert_eq!(profile.materializers[1].chmod.as_deref(), Some("0600"));
}

#[cfg(unix)]
#[test]
fn credential_profile_discovery_skips_unrelated_malformed_json() {
    let dir = tempfile::tempdir().expect("temp credential dir");
    std::fs::write(dir.path().join("broken.json"), "{not valid json")
        .expect("write malformed profile");
    std::fs::write(
        dir.path().join("valid.json"),
        r#"{
          "credential_profiles": {
            "codex_shared": {
              "entries": {"api_key": "OPENAI_API_KEY"},
              "materializers": [
                {"type": "env", "source": "api_key", "target": "OPENAI_API_KEY"}
              ]
            }
          }
        }"#,
    )
    .expect("write valid profile");

    let (path, profile) = crate::find_credential_profile(dir.path(), "codex_shared")
        .expect("valid profile should be found despite malformed sibling");
    assert_eq!(
        path.file_name().and_then(|name| name.to_str()),
        Some("valid.json")
    );
    assert_eq!(profile.materializers[0].kind, "env");
}

#[cfg(not(unix))]
#[test]
fn credential_profile_discovery_requires_descriptor_anchoring() {
    let dir = tempfile::tempdir().expect("temp credential dir");
    let error = crate::find_credential_profile(dir.path(), "profile").unwrap_err();
    assert!(error.contains("descriptor-anchored filesystem operations are unavailable"));
}

#[cfg(unix)]
#[test]
fn credential_profile_discovery_refuses_symlinked_components_and_leaf() {
    use std::fs;
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().expect("project");
    let project = dir.path().join("project");
    let outside = dir.path().join("outside");
    let credentials = project.join(".tachi/credentials");
    fs::create_dir_all(outside.join("credentials")).unwrap();
    fs::create_dir(&project).unwrap();
    let outside_profile = outside.join("credentials/profile.json");
    fs::write(
        &outside_profile,
        r#"{"credential_profiles":{"borrowed":{"provider":"outside"}}}"#,
    )
    .unwrap();

    symlink(&outside, project.join(".tachi")).unwrap();
    assert!(crate::find_credential_profile(&credentials, "borrowed").is_err());
    fs::remove_file(project.join(".tachi")).unwrap();

    fs::create_dir(project.join(".tachi")).unwrap();
    symlink(outside.join("credentials"), &credentials).unwrap();
    assert!(crate::find_credential_profile(&credentials, "borrowed").is_err());
    fs::remove_file(&credentials).unwrap();

    fs::create_dir(&credentials).unwrap();
    let leaf = credentials.join("profile.json");
    symlink(&outside_profile, &leaf).unwrap();
    assert!(crate::find_credential_profile(&credentials, "borrowed").is_err());
    assert!(crate::load_credential_profile_from_path(&leaf, "borrowed").is_err());

    fs::remove_file(&leaf).unwrap();
    fs::copy(outside_profile, &leaf).unwrap();
    assert_eq!(
        crate::find_credential_profile(&credentials, "borrowed")
            .unwrap()
            .0,
        leaf
    );
}
