#[test]
fn credential_profile_json_parse_supports_core_materializers() {
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
            {"type": "config_overlay", "source": "api_key", "target": "OPENCODE_CONFIG_CONTENT", "template": {"provider": "openai"}}
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

    let (path, profile) =
        crate::find_credential_profile(dir.path(), "codex_shared")
            .expect("valid profile should be found despite malformed sibling");
    assert_eq!(
        path.file_name().and_then(|name| name.to_str()),
        Some("valid.json")
    );
    assert_eq!(profile.materializers[0].kind, "env");
}
