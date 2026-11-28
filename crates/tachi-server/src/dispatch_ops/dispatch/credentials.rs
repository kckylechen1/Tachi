use super::*;
#[cfg(any(test, feature = "vault-test-api"))]
use tokio::process::Command;

#[cfg(any(test, feature = "vault-test-api"))]
pub(crate) fn apply_unlocked_vault_env(
    cmd: &mut Command,
    server: &MemoryServer,
    cwd: Option<&std::path::Path>,
) -> usize {
    let env = unlocked_vault_child_env_map(server, cwd);
    for (name, value) in &env {
        cmd.env(name, value);
    }
    env.len()
}

pub(super) fn unlocked_vault_child_env_map(
    server: &MemoryServer,
    cwd: Option<&std::path::Path>,
) -> HashMap<String, String> {
    let Ok(secrets) = server.unlocked_env_secrets_for_child_env(cwd) else {
        return HashMap::new();
    };

    let override_existing = std::env::var("TACHI_VAULT_CHILD_ENV")
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "all" | "full" | "legacy" | "legacy_all" | "override"
            )
        })
        .unwrap_or(false);
    let mut env = HashMap::new();
    for (name, value) in secrets {
        if !override_existing && std::env::var_os(&name).is_some() {
            continue;
        }
        env.insert(name, value);
    }
    env
}

/// Directories that may hold the credential profile of a dispatch in `cwd`,
/// nearest first. The walk up from `cwd`, through its canonical path, stops
/// at the enclosing checkout, so a nested checkout does not inherit the
/// profiles of the project around it, and at the first directory the world can
/// write: a profile there could be anyone's, and Tachi would materialize the
/// operator's secrets by it.
pub(super) fn credential_search_dirs(cwd: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut seen = HashSet::new();

    if let Some(cwd) = cwd {
        // Canonicalize an existing cwd so an unmanaged symlink/alias pointing
        // inside a nested checkout cannot walk lexically past the nested `.git`.
        let resolved_cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
        for ancestor in resolved_cwd.ancestors() {
            if !closed_to_others(ancestor) {
                break;
            }
            let dir = ancestor.join(default_credentials_dir());
            let key = dir.to_string_lossy().to_string();
            if seen.insert(key) {
                dirs.push(dir);
            }
            if std::fs::symlink_metadata(ancestor.join(".git")).is_ok() {
                break;
            }
        }
    }

    let default_dir = default_credentials_dir();
    let key = default_dir.to_string_lossy().to_string();
    if seen.insert(key) {
        dirs.push(default_dir);
    }

    dirs
}

/// Whether others can write into this directory.
#[cfg(unix)]
fn closed_to_others(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(dir)
        .map(|metadata| metadata.is_dir() && metadata.mode() & 0o002 == 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn closed_to_others(_dir: &Path) -> bool {
    true
}

pub(super) fn find_dispatch_credential_profile(
    profile_name: &str,
    cwd: Option<&Path>,
) -> Result<(PathBuf, tachi_credential_profile::CredentialProfile), String> {
    let mut searched = Vec::new();
    let mut skipped = Vec::new();
    for dir in credential_search_dirs(cwd) {
        searched.push(dir.display().to_string());
        if !dir.exists() {
            continue;
        }
        match find_credential_profile(&dir, profile_name) {
            Ok(found) => return Ok(found),
            Err(err) => skipped.push(err),
        }
    }

    let mut msg = format!(
        "Credential profile '{}' not found. Searched: {}",
        profile_name,
        searched.join(", ")
    );
    if !skipped.is_empty() {
        msg.push_str(&format!("; skipped: {}", skipped.join(" | ")));
    }
    Err(msg)
}

pub(super) fn dispatch_credential_consumer(
    agent_norm: &str,
    selected_profile: Option<&str>,
    profile: &tachi_credential_profile::CredentialProfile,
) -> String {
    let allowed = &profile.allowed_consumers;
    if allowed.agents.is_empty() && allowed.profiles.is_empty() {
        return agent_norm.to_string();
    }
    if allowed.agents.iter().any(|agent| agent == agent_norm) {
        return agent_norm.to_string();
    }
    if let Some(selected_profile) = selected_profile {
        if allowed
            .profiles
            .iter()
            .any(|profile| profile == selected_profile)
        {
            return selected_profile.to_string();
        }
    }
    selected_profile.unwrap_or(agent_norm).to_string()
}

pub(super) struct DispatchCredentialMaterialization {
    pub(super) reports: Vec<CredentialMaterializeReport>,
    pub(super) env: HashMap<String, String>,
}

pub(super) fn credential_report_ready(report: &CredentialMaterializeReport) -> bool {
    report.allowed
        && report.missing_secrets.is_empty()
        && report.denied_secrets.is_empty()
        && report.steps.iter().all(|step| step.status == "ready")
}

pub(super) fn materialize_dispatch_credentials(
    server: &MemoryServer,
    grant: &tachi_params::ExecutionGrant,
    agent_norm: &str,
    selected_profile: Option<&str>,
    run_dir: &Path,
    dispatch_id: &str,
) -> Result<DispatchCredentialMaterialization, String> {
    if grant.credential_profiles.is_empty() {
        return Ok(DispatchCredentialMaterialization {
            reports: Vec::new(),
            env: HashMap::new(),
        });
    }

    let dispatch_id = dispatch_id.trim();
    if dispatch_id.is_empty() {
        return Err("dispatch credential materialization requires a non-empty dispatch ID".into());
    }

    let cwd = grant.allowed_cwd.as_deref();
    let mut profile_names = grant
        .credential_profiles
        .iter()
        .map(|profile| profile.trim().to_string())
        .filter(|profile| !profile.is_empty())
        .collect::<Vec<_>>();
    crate::skill_policy::dedupe_preserve_order(&mut profile_names);

    let mut reports = Vec::new();
    let mut env = HashMap::new();
    let apply = (|| -> Result<(), String> {
        for profile_name in profile_names {
            let (_, profile) = find_dispatch_credential_profile(&profile_name, cwd)?;
            let consumer = dispatch_credential_consumer(agent_norm, selected_profile, &profile);
            let plan = server.with_global_store(|store| {
                plan_credential_materialization_with_run_dir(
                    &profile_name,
                    &profile,
                    &consumer,
                    store,
                    Some(run_dir),
                )
            })?;
            if !credential_report_ready(&plan) {
                let plan_json = serde_json::to_string(&credential_materialize_report_json(&plan))
                    .map_err(|e| format!("serialize credential plan: {e}"))?;
                return Err(format!(
                    "Credential profile '{}' is not ready for consumer '{}': {}",
                    profile_name, consumer, plan_json
                ));
            }

            let secret_names = profile_secret_names(&profile);
            let mut secret_values = HashMap::new();
            for secret_name in secret_names {
                let value = read_unlocked_vault_secret(server, &secret_name, Some(&consumer), false)
                .map_err(|err| {
                    format!(
                        "Credential profile '{}' requires unlocked Vault secret '{}' for consumer '{}': {}",
                        profile_name, secret_name, consumer, err
                    )
                })?;
                secret_values.insert(secret_name, value);
            }

            let result = server.with_global_store(|store| {
                apply_credential_materialization(
                    &profile_name,
                    &profile,
                    &consumer,
                    store,
                    &secret_values,
                    &CredentialApplyOptions {
                        allow_existing: false,
                        run_dir: Some(run_dir.to_path_buf()),
                        run_generation: Some(dispatch_id.to_owned()),
                    },
                )
            })?;
            env.extend(result.env);
            reports.push(result.report);
        }

        Ok(())
    })();
    if let Err(mut error) = apply {
        if !reports.is_empty() {
            append_dispatch_credential_cleanup_error(server, run_dir, dispatch_id, &mut error);
        }
        return Err(error);
    }

    Ok(DispatchCredentialMaterialization { reports, env })
}

pub(super) fn append_dispatch_credential_cleanup_error(
    server: &MemoryServer,
    run_dir: &Path,
    dispatch_id: &str,
    error: &mut String,
) {
    match server.with_global_store(|store| {
        cleanup_ephemeral_credential_materializations_for_run(
            store,
            run_dir,
            Some(dispatch_id),
            false,
        )
    }) {
        Ok(report) if report.errors.is_empty() => {}
        Ok(report) => error.push_str(&format!(
            "; temporary credential cleanup incomplete: {}",
            report.errors.join("; ")
        )),
        Err(cleanup_error) => error.push_str(&format!(
            "; temporary credential cleanup could not be confirmed: {cleanup_error}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p3_typed_grant_rejects_missing_credential_selector_without_flat_dispatch_params() {
        let server = crate::tests::make_server();
        let run_dir = tempfile::tempdir().expect("credential run dir");
        let missing_selector = "p3-typed-missing-credential-selector";
        let grant = tachi_params::ExecutionGrant {
            grant_id: "p3-credential-grant".to_string(),
            env_id: None,
            unmanaged_cwd_allowed: false,
            allowed_cwd: None,
            credential_profiles: vec![missing_selector.to_string()],
            mcp_access: None,
            allowed_tools: Vec::new(),
            permission_profile: None,
            sandbox: None,
            max_turns: None,
            timeout_secs: 5,
        };
        let err = match materialize_dispatch_credentials(
            &server,
            &grant,
            "custom",
            Some("typed-profile"),
            run_dir.path(),
            "p3-credential-dispatch",
        ) {
            Err(err) => err,
            Ok(_) => {
                panic!("typed missing selector must fail in the production credential consumer")
            }
        };
        assert!(
            err.contains(missing_selector),
            "production materializer must report the independently minted selector: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn credential_failure_on_second_profile_cleans_first_overlay() {
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join("run");
        let workspace = root.path().join("workspace");
        let profiles = workspace.join(default_credentials_dir());
        std::fs::create_dir(&run).unwrap();
        std::fs::create_dir_all(&profiles).unwrap();
        let server = MemoryServer::new(root.path().join("global.db"), None).unwrap();
        crate::vault_test_api::seed_vault(&server, "credential-test-password").unwrap();
        server
            .seed_vault_secret(crate::vault_test_api::VaultSecretFixture {
                name: "FIRST_SECRET".into(),
                value: "fixture-value".into(),
                secret_type: "api_key".into(),
                description: "fixture".into(),
                allowed_agents: None,
            })
            .unwrap();
        std::fs::write(profiles.join("profiles.json"), json!({
            "credential_profiles": {
                "first": { "materializers": [
                    { "type": "config_overlay", "source": "FIRST_SECRET", "target": "{credentials_dir}/probe.key" }
                ] },
                "second": { "materializers": [
                    { "type": "config_overlay", "source": "MISSING_SECRET", "target": "{credentials_dir}/second.key" }
                ] }
            }
        }).to_string()).unwrap();
        let grant = tachi_params::ExecutionGrant {
            grant_id: "fixture-grant".into(),
            allowed_cwd: Some(workspace),
            credential_profiles: vec!["first".into(), "second".into()],
            env_id: None,
            unmanaged_cwd_allowed: false,
            mcp_access: None,
            allowed_tools: Vec::new(),
            permission_profile: None,
            sandbox: None,
            max_turns: None,
            timeout_secs: 5,
        };
        let error =
            materialize_dispatch_credentials(&server, &grant, "codex", None, &run, "fixture-run")
                .err()
                .expect("second profile must fail");
        assert!(
            error.contains("second") && error.contains("MISSING_SECRET"),
            "{error}"
        );
        assert!(!run.join("credentials/probe.key").exists());
        server
            .with_global_store(|store| {
                let rows = store
                    .list_state(tachi_credential_profile::CREDENTIAL_MATERIALIZATION_NAMESPACE)
                    .map_err(|err| err.to_string())?;
                assert_eq!(rows.len(), 1);
                let metadata: serde_json::Value =
                    serde_json::from_str(&rows[0].value_json).unwrap();
                assert_eq!(metadata["profile"], "first");
                assert_eq!(metadata["cleanup_status"], "cleaned");
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn credential_search_dirs_stops_at_enclosing_git_checkout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outer = dir.path().join("outer");
        let nested = outer.join("vendor/nested");
        let nested_src = nested.join("src");
        std::fs::create_dir_all(&nested_src).expect("nested src");
        std::fs::create_dir_all(nested.join(".git")).expect("nested .git");

        let canonical_nested = std::fs::canonicalize(&nested).expect("canonical nested");
        let canonical_outer = std::fs::canonicalize(&outer).expect("canonical outer");
        let dirs = credential_search_dirs(Some(&nested_src));
        assert!(
            dirs.contains(&canonical_nested.join(default_credentials_dir())),
            "canonical nested credentials dir missing: {dirs:?}"
        );
        assert!(
            !dirs.contains(&outer.join(default_credentials_dir()))
                && !dirs.contains(&canonical_outer.join(default_credentials_dir())),
            "credential search must not cross out of the nested checkout: {dirs:?}"
        );

        let linked = outer.join("vendor/linked");
        std::fs::create_dir_all(linked.join("pkg")).expect("linked pkg");
        std::fs::write(linked.join(".git"), "gitdir: /elsewhere/worktrees/linked\n")
            .expect("linked .git file");
        let linked_dirs = credential_search_dirs(Some(&linked.join("pkg")));
        assert!(
            !linked_dirs.contains(&outer.join(default_credentials_dir()))
                && !linked_dirs.contains(&canonical_outer.join(default_credentials_dir())),
            "linked worktree .git file must also stop credential search: {linked_dirs:?}"
        );

        let alias = outer.join("alias-to-nested-src");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&nested_src, &alias).expect("symlink alias");
        #[cfg(unix)]
        {
            let alias_dirs = credential_search_dirs(Some(&alias));
            assert!(
                alias_dirs.contains(&canonical_nested.join(default_credentials_dir())),
                "canonical nested credentials dir missing: {alias_dirs:?}"
            );
            assert!(
                !alias_dirs.contains(&outer.join(default_credentials_dir()))
                    && !alias_dirs.contains(&canonical_outer.join(default_credentials_dir())),
                "cwd alias must not escape nested checkout boundary: {alias_dirs:?}"
            );
        }
    }
}

#[cfg(test)]
mod search_dirs_tests {
    use super::*;

    fn directories(cwd: &Path) -> Vec<PathBuf> {
        credential_search_dirs(Some(cwd))
    }

    /// The candidates of the walk itself: `<ancestor>/.tachi/credentials`
    /// for every ancestor of `cwd`'s canonical path that was searched.
    fn project_directories(cwd: &Path) -> Vec<PathBuf> {
        let canonical = std::fs::canonicalize(cwd).expect("canonical cwd");
        directories(cwd)
            .into_iter()
            .filter(|dir| {
                dir.parent().and_then(Path::parent).is_some_and(|ancestor| {
                    ancestor.is_absolute() && canonical.starts_with(ancestor)
                })
            })
            .collect()
    }

    #[test]
    fn a_nested_checkout_does_not_inherit_the_profiles_of_the_project_around_it() {
        let root = tempfile::tempdir().expect("tempdir");
        let base = std::fs::canonicalize(root.path()).expect("canonical root");
        let outer = base.join("outer");
        let inner = outer.join("inner");
        let sub = inner.join("sub");
        std::fs::create_dir_all(&sub).expect("nested directories");
        std::fs::create_dir(outer.join(".git")).expect("outer checkout");
        std::fs::create_dir(inner.join(".git")).expect("inner checkout");

        let dirs = project_directories(&sub);

        assert!(dirs.contains(&sub.join(".tachi/credentials")), "{dirs:?}");
        assert!(dirs.contains(&inner.join(".tachi/credentials")), "{dirs:?}");
        assert!(
            !dirs.contains(&outer.join(".tachi/credentials")),
            "{dirs:?}"
        );
    }

    /// A profile in a directory the world can write may be anyone's; Tachi
    /// would materialize the operator's secrets by it.
    #[cfg(unix)]
    #[test]
    fn the_lookup_stops_at_a_directory_the_world_can_write() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().expect("tempdir");
        let base = std::fs::canonicalize(root.path()).expect("canonical root");
        let open = base.join("open");
        std::fs::create_dir(&open).expect("open directory");
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777))
            .expect("world-writable directory");
        let project = open.join("project");
        std::fs::create_dir(&project).expect("project directory");

        assert_eq!(
            project_directories(&project),
            vec![project.join(".tachi/credentials")],
            "nothing above the open directory is searched"
        );
        assert!(
            project_directories(&open).is_empty(),
            "a dispatch directory the world can write does not trust its own profiles"
        );
    }

    #[test]
    fn a_project_without_a_checkout_keeps_looking_up_through_its_ancestors() {
        let root = tempfile::tempdir().expect("tempdir");
        let base = std::fs::canonicalize(root.path()).expect("canonical root");
        let project = base.join("project");
        let sub = project.join("a").join("b");
        std::fs::create_dir_all(&sub).expect("directories");

        let dirs = project_directories(&sub);

        assert!(dirs.contains(&sub.join(".tachi/credentials")), "{dirs:?}");
        assert!(
            dirs.contains(&project.join(".tachi/credentials")),
            "{dirs:?}"
        );
    }
}
