use super::*;
use crate::test_support::{CwdRestore, EnvRestore};
use memcore::anchored_fs::AnchoredDirectory;
use std::os::unix::fs::PermissionsExt;

const HEADER_SECRET: &str = "fixture-only-codex-header";
const POLICY: &str = "[mcp_servers.tachi]\ncommand = 'existing-tachi'\nenabled = false\nenabled_tools = ['tachi_search']\n";

struct Fixture {
    server: MemoryServer,
    root: PathBuf,
    daemon: PathBuf,
    workspace: PathBuf,
    run: PathBuf,
    config: PathBuf,
    assignment: tachi_params::ResolvedStaffAssignment,
    request: StaffAssignmentRequest,
    _cwd: CwdRestore,
    _env: Vec<EnvRestore>,
    _temp: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for name in [
            "daemon",
            "workspace",
            "run",
            "bin",
            "codex-home",
            "home",
            "tachi-home",
        ] {
            std::fs::create_dir(root.join(name)).unwrap();
        }
        let python = std::process::Command::new("python3")
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
            .unwrap();
        assert!(python.status.success());
        let executable = root.join("bin/codex");
        std::fs::write(
            &executable,
            format!(
                "#!{}\n{}",
                String::from_utf8(python.stdout).unwrap().trim(),
                r#"
import json, os, pathlib, sys
mode = sys.argv[1]
if mode == 'mcp':
    paths = [pathlib.Path.cwd() / '.codex/config.toml']
    codex_home = os.environ.get('CODEX_HOME')
    if codex_home:
        paths.append(pathlib.Path(codex_home) / 'config.toml')
    else:
        paths.append(pathlib.Path(os.environ['HOME']) / '.codex/config.toml')
    collision = any(p.exists() and '[mcp_servers.tachi]' in p.read_text() for p in paths)
    observation = {'cwd': os.getcwd(), 'inode': os.stat('.').st_ino,
        'home': codex_home, 'inherited': os.environ.get('CODEX_CONTEXT_INHERITED_TEST'),
        'removed': os.environ.get('CODEX_CONTEXT_REMOVED_TEST'),
        'cargo_target': os.environ.get('CARGO_TARGET_DIR')}
    pathlib.Path(os.environ['CODEX_CONTEXT_PROBE_MARKER']).write_text(json.dumps(observation))
    print(json.dumps([{'name': 'tachi'}] if collision else []))
elif mode == 'exec':
    args = sys.argv[2:]
    if '-C' in args:
        os.chdir(args[args.index('-C') + 1])
    observation = {'cwd': os.getcwd(), 'inode': os.stat('.').st_ino, 'args': args,
        'home': os.environ.get('CODEX_HOME'),
        'inherited': os.environ.get('CODEX_CONTEXT_INHERITED_TEST'),
        'removed': os.environ.get('CODEX_CONTEXT_REMOVED_TEST'),
        'cargo_target': os.environ.get('CARGO_TARGET_DIR')}
    pathlib.Path(os.environ['CODEX_CONTEXT_WORKER_MARKER']).write_text(json.dumps(observation))
else:
    raise RuntimeError('unexpected fake Codex command')
"#
            ),
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = std::env::join_paths(
            std::iter::once(root.join("bin")).chain(
                std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                    .filter(|entry| entry.is_absolute()),
            ),
        )
        .unwrap();
        let daemon = root.join("daemon");
        let workspace = root.join("workspace");
        let run = root.join("run");
        let env = vec![
            EnvRestore::set_os("PATH", &path),
            EnvRestore::set_path("HOME", &root.join("home")),
            EnvRestore::set_path("TACHI_HOME", &root.join("tachi-home")),
            EnvRestore::set_path("TACHI_RUN_ROOT", &root.join("runs")),
            EnvRestore::set_path("TACHI_WORKTREES_ROOT", &root.join("worktrees")),
            EnvRestore::remove("TACHI_PROJECT_ROOT"),
            EnvRestore::set_path("CODEX_HOME", &root.join("codex-home")),
            EnvRestore::set_path("CODEX_CONTEXT_PROBE_MARKER", &root.join("probe.json")),
            EnvRestore::set_path("CODEX_CONTEXT_WORKER_MARKER", &root.join("worker.json")),
            EnvRestore::set("CODEX_CONTEXT_INHERITED_TEST", "inherited-fixture"),
            EnvRestore::set("CODEX_CONTEXT_REMOVED_TEST", "remove-fixture"),
            EnvRestore::set("CARGO_TARGET_DIR", "must-stay-removed"),
        ];
        let cwd = CwdRestore::set(&daemon);
        let server = MemoryServer::new(root.join("global.db"), None).unwrap();
        let config = root.join("injected.json");
        std::fs::write(
            &config,
            serde_json::to_vec(&json!({"mcpServers":{"tachi":{
                "type":"http", "url":"http://127.0.0.1:6919/mcp",
                "headers":{"Authorization": HEADER_SECRET}
            }}}))
            .unwrap(),
        )
        .unwrap();
        Self {
            server,
            root,
            daemon,
            workspace,
            run,
            config,
            assignment: tachi_params::ResolvedStaffAssignment::new(
                "codex-context",
                tachi_params::TachiDispatchReason::ExplicitUserRequest,
                "codex",
                "codex",
            ),
            request: StaffAssignmentRequest::new(
                tachi_params::TachiDispatchReason::ExplicitUserRequest,
                "context regression",
            ),
            _cwd: cwd,
            _env: env,
            _temp: temp,
        }
    }

    fn grant(&self) -> tachi_params::ExecutionGrant {
        tachi_params::ExecutionGrant {
            grant_id: "codex-context-grant".into(),
            env_id: None,
            unmanaged_cwd_allowed: false,
            allowed_cwd: Some(self.workspace.clone()),
            credential_profiles: Vec::new(),
            mcp_access: None,
            allowed_tools: Vec::new(),
            permission_profile: Some("default".into()),
            sandbox: Some("read-only".into()),
            max_turns: Some(3),
            timeout_secs: 5,
        }
    }

    fn prepare(&self, grant: &tachi_params::ExecutionGrant) -> PreparedDispatchBackend {
        let prepared = prepare_dispatch_backend(DispatchBackendContext {
            server: &self.server,
            trajectory_path: &self.run.join("trajectory.jsonl"),
            workspace_dir: &self.run,
            dispatch_id: "codex-context",
            request: &self.request,
            assignment: &self.assignment,
            grant,
            command: &[],
            prompt: "bounded task",
            custom_launch_spec: None,
            prompt_md_path: &self.run.join("prompt.md"),
            mcp_config_path: Some(&self.config),
            v2: true,
            plan_generated_at: None,
            plan_duration_ms: None,
            harness_transport: "cli",
            harness_server_url: &None,
            timeout_secs_for_status: 5,
        })
        .unwrap();
        assert_eq!(
            prepared.pending_codex_mcp.as_deref(),
            Some(self.config.as_path())
        );
        assert!(
            !self.root.join("probe.json").exists(),
            "backend construction must not probe before per-run credentials"
        );
        prepared
    }

    fn finalize(
        &self,
        prepared: &mut PreparedDispatchBackend,
        grant: &tachi_params::ExecutionGrant,
        authority: Option<&AnchoredDirectory>,
    ) -> Result<(), String> {
        finalize_dispatch_backend(
            &mut prepared.execution,
            prepared.pending_codex_mcp.as_deref(),
            grant,
            authority,
        )
    }

    fn observe(&self, name: &str) -> Value {
        serde_json::from_slice(&std::fs::read(self.root.join(name)).unwrap()).unwrap()
    }

    fn worker(&self, prepared: &mut PreparedDispatchBackend) {
        let DispatchExecution::Subprocess(command) = &mut prepared.execution else {
            panic!("Codex CLI")
        };
        let args: Vec<_> = command
            .as_std()
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert!(args.windows(2).any(|p| p == ["--sandbox", "read-only"]));
        assert!(!args
            .iter()
            .any(|s| s.contains("bypass") || s.contains(HEADER_SECRET)));
        assert!(command.as_std_mut().status().unwrap().success());
    }

    fn failure(&self, error: &str) -> String {
        super::super::backend_failure::record_execution_backend_finalize_failure(
            &self.server,
            ExecutionBackendPrepareFailure {
                trajectory_path: &self.run.join("trajectory.jsonl"),
                workspace_dir: &self.run,
                dispatch_id: "codex-context",
                assignment: &self.assignment,
                request: &self.request,
                backend: "codex",
                error,
                v2: true,
                plan_generated_at: None,
                plan_duration_ms: None,
                harness_transport: "cli",
                harness_server_url: &None,
                timeout_secs_for_status: 5,
            },
            Some(&self.config),
        )
    }
}

fn policy(directory: &Path) -> PathBuf {
    let path = directory.join(".codex/config.toml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, POLICY).unwrap();
    path
}

#[test]
fn codex_managed_workspace_policy_is_observed_before_worker_start() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let fixture = Fixture::new();
    let policy = policy(&fixture.workspace);
    let authority = AnchoredDirectory::open_absolute(&fixture.workspace).unwrap();
    let grant = carrier_execution_grant(&fixture.grant(), true);
    let mut prepared = fixture.prepare(&grant);
    let error = fixture
        .finalize(&mut prepared, &grant, Some(&authority))
        .unwrap_err();
    assert!(
        error.starts_with("Codex already configures MCP server 'tachi'"),
        "{error}"
    );
    assert_eq!(
        fixture.observe("probe.json")["cwd"],
        fixture.workspace.to_string_lossy().as_ref()
    );
    assert!(!fixture.root.join("worker.json").exists());
    assert_eq!(std::fs::read_to_string(policy).unwrap(), POLICY);
    let error = fixture.failure(&error);
    assert!(!error.contains("cleanup incomplete"));
    assert!(!fixture.config.exists());
    let status: Value =
        serde_json::from_slice(&std::fs::read(fixture.run.join("status.json")).unwrap()).unwrap();
    assert_eq!(status["state"], "TASK_STATE_FAILED");
    assert_eq!(status["exit_code"], 1);
    assert_eq!(std::env::current_dir().unwrap(), fixture.daemon);
}

#[test]
fn codex_daemon_only_policy_does_not_refuse_a_clean_managed_workspace() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let fixture = Fixture::new();
    let policy = policy(&fixture.daemon);
    let authority = AnchoredDirectory::open_absolute(&fixture.workspace).unwrap();
    let grant = carrier_execution_grant(&fixture.grant(), true);
    let mut prepared = fixture.prepare(&grant);
    fixture
        .finalize(&mut prepared, &grant, Some(&authority))
        .unwrap();
    fixture.worker(&mut prepared);
    assert_eq!(
        fixture.observe("probe.json")["cwd"],
        fixture.observe("worker.json")["cwd"]
    );
    assert_eq!(
        fixture.observe("worker.json")["cwd"],
        fixture.workspace.to_string_lossy().as_ref()
    );
    assert_eq!(std::fs::read_to_string(policy).unwrap(), POLICY);
}

#[test]
fn codex_probe_and_worker_keep_the_pinned_directory_after_path_replacement() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let fixture = Fixture::new();
    let authority = AnchoredDirectory::open_absolute(&fixture.workspace).unwrap();
    let identity = authority.identity().unwrap();
    let grant = carrier_execution_grant(&fixture.grant(), true);
    let mut prepared = fixture.prepare(&grant);
    let moved = fixture.root.join("original-workspace");
    std::fs::rename(&fixture.workspace, &moved).unwrap();
    std::fs::create_dir(&fixture.workspace).unwrap();
    let replacement = policy(&fixture.workspace);
    fixture
        .finalize(&mut prepared, &grant, Some(&authority))
        .unwrap();
    fixture.worker(&mut prepared);
    for name in ["probe.json", "worker.json"] {
        let observation = fixture.observe(name);
        assert_eq!(observation["inode"], identity.inode);
        assert_eq!(observation["cwd"], moved.to_string_lossy().as_ref());
    }
    assert_eq!(std::fs::read_to_string(replacement).unwrap(), POLICY);
}

#[test]
fn codex_inventory_preserves_inheritance_and_explicit_env_removals() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let fixture = Fixture::new();
    let authority = AnchoredDirectory::open_absolute(&fixture.workspace).unwrap();
    let grant = carrier_execution_grant(&fixture.grant(), true);
    let mut prepared = fixture.prepare(&grant);
    let DispatchExecution::Subprocess(command) = &mut prepared.execution else {
        panic!("Codex CLI")
    };
    command.env_remove("CODEX_CONTEXT_REMOVED_TEST");
    command.env_remove("CARGO_TARGET_DIR");
    command.env_remove("CODEX_HOME");
    command.env("HOME", fixture.root.join("home"));
    fixture
        .finalize(&mut prepared, &grant, Some(&authority))
        .unwrap();
    fixture.worker(&mut prepared);
    for name in ["probe.json", "worker.json"] {
        let observation = fixture.observe(name);
        assert_eq!(observation["inherited"], "inherited-fixture");
        assert!(observation["removed"].is_null());
        assert!(observation["cargo_target"].is_null());
        assert!(observation["home"].is_null());
    }
    let DispatchExecution::Subprocess(command) = &prepared.execution else {
        panic!("Codex CLI")
    };
    assert!(
        command
            .as_std()
            .get_envs()
            .any(|(key, value)| key == "CARGO_TARGET_DIR" && value.is_none()),
        "containment wrappers must retain the explicit removal"
    );
}

#[test]
fn codex_unmanaged_inventory_uses_the_admitted_config_directory() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let fixture = Fixture::new();
    policy(&fixture.daemon);
    let grant = fixture.grant();
    let mut prepared = fixture.prepare(&grant);
    fixture.finalize(&mut prepared, &grant, None).unwrap();
    fixture.worker(&mut prepared);
    assert_eq!(
        fixture.observe("probe.json")["cwd"],
        fixture.workspace.to_string_lossy().as_ref()
    );
    assert_eq!(
        fixture.observe("worker.json")["cwd"],
        fixture.workspace.to_string_lossy().as_ref()
    );
}

#[test]
fn codex_unmanaged_ambiguous_relative_configuration_refuses_before_probe() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let fixture = Fixture::new();
    let grant = fixture.grant();
    for (name, value) in [
        ("PATH", "relative-bin:/usr/bin"),
        ("PATH", ":/usr/bin"),
        ("HOME", "relative-home"),
        ("CODEX_HOME", "relative-config"),
    ] {
        let mut prepared = fixture.prepare(&grant);
        let DispatchExecution::Subprocess(command) = &mut prepared.execution else {
            panic!("Codex CLI")
        };
        command.env(name, value);
        let error = fixture.finalize(&mut prepared, &grant, None).unwrap_err();
        assert!(
            error.contains(name) && error.contains("absolute"),
            "{error}"
        );
        assert!(!fixture.root.join("probe.json").exists());
        assert!(!fixture.root.join("worker.json").exists());
    }
}

#[test]
fn codex_refusal_reports_incomplete_credential_cleanup_without_exposing_metadata() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let fixture = Fixture::new();
    let private_key = "fixture-only-private-metadata-key";
    fixture
        .server
        .with_global_store(|store| {
            store
                .set_state(
                    tachi_credential_profile::CREDENTIAL_MATERIALIZATION_NAMESPACE,
                    private_key,
                    "{}",
                )
                .map_err(|error| error.to_string())
        })
        .unwrap();
    let error = fixture.failure("Codex configuration inspection refused");
    assert!(error.contains("temporary credential cleanup incomplete (1 errors)"));
    assert!(!fixture.config.exists());
    assert!(!error.contains(private_key));
    let status: Value =
        serde_json::from_slice(&std::fs::read(fixture.run.join("status.json")).unwrap()).unwrap();
    assert_eq!(status["state"], "TASK_STATE_FAILED");
    assert_eq!(status["error"], error);
    let trajectory = std::fs::read_to_string(fixture.run.join("trajectory.jsonl")).unwrap();
    assert!(!trajectory.contains(private_key));
    assert!(!fixture.root.join("probe.json").exists());
    assert!(!fixture.root.join("worker.json").exists());
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn codex_inventory_uses_materialized_home_and_refusal_cleans_only_its_run() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let fixture = Fixture::new();
    let final_home = fixture.root.join("credential-codex-home");
    std::fs::create_dir(&final_home).unwrap();
    std::fs::write(final_home.join("config.toml"), POLICY).unwrap();
    let persistent = fixture.root.join("persistent-key");
    let secret = "fixture-only-materialized-value";
    fixture
        .server
        .seed_vault("codex context fixture password")
        .unwrap();
    for (name, value) in [
        ("CODEX_TEST_HOME", final_home.to_str().unwrap()),
        ("CODEX_TEST_SECRET", secret),
    ] {
        fixture
            .server
            .seed_vault_secret(crate::vault_test_api::VaultSecretFixture {
                name: name.into(),
                value: value.into(),
                secret_type: "api_key".into(),
                description: "Codex context fixture".into(),
                allowed_agents: None,
            })
            .unwrap();
    }
    let profile = json!({
        "provider":"test", "entries":{"home":"CODEX_TEST_HOME", "secret":"CODEX_TEST_SECRET"},
        "allowed_consumers":{"agents":["codex"]}, "materializers":[
            {"type":"env", "source":"home", "target":"CODEX_HOME"},
            {"type":"config_overlay", "source":"secret", "target":"{credentials_dir}/probe.key", "chmod":"0600"}
        ]
    });
    // A pre-existing managed file outside this dispatch must survive its cleanup.
    let prior_profile: tachi_credential_profile::CredentialProfile = serde_json::from_value(json!({
        "entries":{"secret":"CODEX_TEST_SECRET"}, "allowed_consumers":{"agents":["codex"]},
        "materializers":[{"type":"config_overlay","source":"secret","target":persistent,"chmod":"0600"}]
    })).unwrap();
    fixture
        .server
        .with_global_store(|store| {
            apply_credential_materialization(
                "context",
                &prior_profile,
                "codex",
                store,
                &HashMap::from([("CODEX_TEST_SECRET".into(), secret.into())]),
                &CredentialApplyOptions {
                    allow_existing: false,
                    run_dir: Some(fixture.root.clone()),
                },
            )
        })
        .unwrap();
    let profile_dir = fixture.workspace.join(".tachi/credentials");
    std::fs::create_dir_all(&profile_dir).unwrap();
    std::fs::write(
        profile_dir.join("codex.json"),
        serde_json::to_vec(&json!({"credential_profiles":{"context":profile}})).unwrap(),
    )
    .unwrap();
    let mut original_grant = fixture.grant();
    original_grant.credential_profiles = vec!["context".into()];
    let carrier_grant = carrier_execution_grant(&original_grant, true);
    let authority = AnchoredDirectory::open_absolute(&fixture.workspace).unwrap();
    let mut prepared = fixture.prepare(&carrier_grant);
    apply_materialized_credentials(
        CredentialApplyInputs {
            server: &fixture.server,
            request: &fixture.request,
            grant: &original_grant,
            raw_credential_profiles: &original_grant.credential_profiles,
            assignment: &fixture.assignment,
            workspace_dir: &fixture.run,
            trajectory_path: &fixture.run.join("trajectory.jsonl"),
            dispatch_id: "codex-context",
            v2: true,
            plan_generated_at: None,
            plan_duration_ms: None,
            harness_transport: "cli",
            harness_server_url: &None,
            execution_backend_name: None,
            execution_backend_metadata: &None,
            acpx_enabled: false,
            native_acp_enabled: false,
            timeout_secs_for_status: 5,
        },
        &mut prepared.execution,
    )
    .unwrap();
    let ephemeral = fixture.run.join("credentials/probe.key");
    assert_eq!(std::fs::read_to_string(&ephemeral).unwrap(), secret);
    assert_eq!(std::fs::read_to_string(&persistent).unwrap(), secret);
    let other_run = fixture.root.join("other-run");
    std::fs::create_dir(&other_run).unwrap();
    let other_profile: tachi_credential_profile::CredentialProfile = serde_json::from_value(json!({
        "entries":{"secret":"CODEX_TEST_SECRET"}, "allowed_consumers":{"agents":["codex"]},
        "materializers":[{"type":"config_overlay","source":"secret","target":"{credentials_dir}/other.key","chmod":"0600"}]
    })).unwrap();
    fixture
        .server
        .with_global_store(|store| {
            apply_credential_materialization(
                "other",
                &other_profile,
                "codex",
                store,
                &HashMap::from([("CODEX_TEST_SECRET".into(), secret.into())]),
                &CredentialApplyOptions {
                    allow_existing: false,
                    run_dir: Some(other_run.clone()),
                },
            )
        })
        .unwrap();
    let error = fixture
        .finalize(&mut prepared, &carrier_grant, Some(&authority))
        .unwrap_err();
    assert!(
        error.starts_with("Codex already configures MCP server 'tachi'"),
        "{error}"
    );
    assert_eq!(
        fixture.observe("probe.json")["home"],
        final_home.to_string_lossy().as_ref()
    );
    let error = fixture.failure(&error);
    assert!(!error.contains("cleanup incomplete") && !error.contains("could not be confirmed"));
    assert!(!fixture.config.exists());
    assert!(!ephemeral.exists());
    assert_eq!(std::fs::read_to_string(&persistent).unwrap(), secret);
    assert_eq!(
        std::fs::read_to_string(other_run.join("credentials/other.key")).unwrap(),
        secret
    );
    assert_eq!(
        std::fs::read_to_string(final_home.join("config.toml")).unwrap(),
        POLICY
    );
    assert!(!fixture.root.join("worker.json").exists());
    let trajectory = std::fs::read_to_string(fixture.run.join("trajectory.jsonl")).unwrap();
    assert!(!trajectory.contains(secret) && !trajectory.contains(HEADER_SECRET));
    assert!(!error.contains(secret) && !error.contains(HEADER_SECRET));
}
