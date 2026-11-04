use super::*;
use crate::launcher::{build_codex_launch, DispatchLaunchParams};

fn http_config() -> Value {
    json!({"mcpServers":{"tachi":{
        "type":"http", "url":"http://127.0.0.1:6919/mcp",
        "headers":{"Authorization":"Bearer private-test-token", "X-Tachi-Project":"Repo"}
    }}})
}

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/codex_mcp_overrides.json")).unwrap()
}

// Consumer model of Codex config/{overrides,merge,mcp_types}.rs: CLI values
// become one layer, table/table merge is recursive, transport decoding rejects
// mixed fields even when a server is disabled. This is not a Codex CLI smoke.
fn merge(base: &mut Value, overlay: &Value) {
    if let (Some(base), Some(overlay)) = (base.as_object_mut(), overlay.as_object()) {
        for (key, value) in overlay {
            match base.get_mut(key) {
                Some(existing) => merge(existing, value),
                None => {
                    base.insert(key.clone(), value.clone());
                }
            }
        }
    } else {
        *base = overlay.clone();
    }
}

fn effective_config(mut base: Value, projection: &CodexMcpProjection) -> Value {
    let mut cli = json!({});
    for (path, value) in &projection.overrides {
        let mut nested = value.clone();
        for part in path.split('.').rev() {
            nested = json!({(part): nested});
        }
        merge(&mut cli, &nested);
    }
    merge(&mut base, &cli);
    base
}

#[derive(Debug, serde::Deserialize)]
struct ConsumerMcpServer {
    #[serde(default = "enabled")]
    enabled: bool,
    command: Option<String>,
    url: Option<String>,
    args: Option<Vec<String>>,
    env: Option<HashMap<String, String>>,
    env_vars: Option<Vec<String>>,
    http_headers: Option<HashMap<String, String>>,
    env_http_headers: Option<HashMap<String, String>>,
}

fn enabled() -> bool {
    true
}

fn decode_servers(config: &Value) -> Result<HashMap<String, ConsumerMcpServer>, String> {
    let servers: HashMap<String, ConsumerMcpServer> =
        serde_json::from_value(config["mcp_servers"].clone()).map_err(|error| error.to_string())?;
    for (name, server) in &servers {
        if server.command.is_some() {
            if server.url.is_some()
                || server.http_headers.is_some()
                || server.env_http_headers.is_some()
            {
                return Err(format!("stdio server '{name}' has HTTP fields"));
            }
        } else if server.url.is_some() {
            if server.args.is_some() || server.env.is_some() || server.env_vars.is_some() {
                return Err(format!("HTTP server '{name}' has stdio fields"));
            }
        } else {
            return Err(format!("MCP server '{name}' has no transport"));
        }
    }
    Ok(servers)
}

#[test]
fn existing_stdio_or_http_policy_refuses_injection_without_disabling_or_bypassing_it() {
    for transport in [
        json!({"command":"/old/tachi", "args":["serve"], "env":{"EXISTING":"private-value"}}),
        json!({"url":"http://old-endpoint/mcp"}),
    ] {
        for enabled in [true, false] {
            let policy = json!({
                "name":"tachi", "enabled":enabled, "transport":transport,
                "enabled_tools":["tachi_search"], "disabled_tools":["tachi_save"],
                "tools":{"tachi_memory":{"approval_mode":"prompt"}}
            });
            let inventory =
                parse_inventory(&serde_json::to_vec(&vec![policy.clone()]).unwrap()).unwrap();
            let input = http_config();
            let before = input.clone();
            let error = project(&input, &inventory, "test_run", |_| None)
                .err()
                .unwrap();
            assert!(error.contains("complete per-server tool policy"), "{error}");
            assert!(error.contains("injection was refused"), "{error}");
            assert!(!error.contains("private-value"));
            assert_eq!(input, before);
        }
    }
}

#[test]
fn new_http_table_preserves_other_mcp_and_carrier_configuration() {
    let base = json!({
        "sandbox_mode":"read-only", "cli_auth_credentials_store":"keyring", "model":"user-model",
        "mcp_servers":{"other":{"command":"/other/server","args":["--keep"],
            "enabled_tools":["allowed"], "disabled_tools":["denied"]}}
    });
    let projection = project(
        &http_config(),
        &BTreeSet::from(["other".into()]),
        "test_run",
        |_| None,
    )
    .unwrap();
    let merged = effective_config(base.clone(), &projection);
    let decoded =
        decode_servers(&merged).expect("merged config must retain valid transport shapes");
    assert!(decoded["tachi_dispatch_test_run_0"].enabled);
    assert_eq!(
        decoded["tachi_dispatch_test_run_0"].url.as_deref(),
        Some("http://127.0.0.1:6919/mcp")
    );
    assert!(decoded["tachi_dispatch_test_run_0"].command.is_none());
    assert_eq!(merged["mcp_servers"]["other"], base["mcp_servers"]["other"]);
    for key in ["sandbox_mode", "cli_auth_credentials_store", "model"] {
        assert_eq!(merged[key], base[key]);
    }
    assert_eq!(
        json!(projection.rendered_overrides()),
        fixture()["fresh_http"]
    );

    let mut previous_table_overlay =
        json!({"mcp_servers":{"tachi":{"command":"tachi", "args":["serve"]}}});
    merge(
        &mut previous_table_overlay,
        &json!({"mcp_servers":{"tachi":{
            "url":"http://127.0.0.1:6919/mcp", "http_headers":{"X-Tachi-Project":"Repo"}
        }}}),
    );
    assert!(decode_servers(&previous_table_overlay)
        .unwrap_err()
        .contains("stdio"));
}

#[test]
fn no_existing_server_means_no_invalid_disabled_stub() {
    let projection = project(&http_config(), &BTreeSet::new(), "test_run", |_| None).unwrap();
    let merged = effective_config(json!({}), &projection);
    assert!(merged["mcp_servers"].get("tachi").is_none());
    assert_eq!(decode_servers(&merged).unwrap().len(), 1);
    assert!(decode_servers(&json!({"mcp_servers":{"tachi":{"enabled":false}}})).is_err());
    assert!(project(
        &http_config(),
        &BTreeSet::from(["tachi_dispatch_test_run_0".into()]),
        "test_run",
        |_| None
    )
    .is_err());
}

#[test]
fn http_secrets_use_only_child_environment_and_keep_sandbox_and_auth_context() {
    let projection = project(&http_config(), &BTreeSet::new(), "test_run", |_| None).unwrap();
    let input = DispatchLaunchParams {
        sandbox: Some("read-only".into()),
        ..Default::default()
    };
    let mut launch = build_codex_launch(&input, "task").unwrap();
    let mut worker = Command::new(&launch.program);
    worker.args(&launch.args);
    projection.apply(&mut worker);
    launch.args = worker
        .get_args()
        .map(|arg| arg.to_str().unwrap().to_string())
        .collect();
    launch.env_vars = worker
        .get_envs()
        .map(|(name, value)| {
            (
                name.to_str().unwrap().to_string(),
                value.unwrap().to_str().unwrap().to_string(),
            )
        })
        .collect();
    assert!(launch
        .args
        .windows(2)
        .any(|pair| pair == ["--sandbox", "read-only"]));
    assert!(!launch
        .args
        .iter()
        .any(|arg| arg.contains("bypass-approvals")));
    assert!(!launch.args.join(" ").contains("private-test-token"));
    assert!(!format!("{launch:?}").contains("private-test-token"));
    assert_eq!(
        launch.env_vars["TACHI_CODEX_MCP_TEST_RUN_0_0"],
        "Bearer private-test-token"
    );
    assert_eq!(launch.env_vars["TACHI_CODEX_MCP_TEST_RUN_0_1"], "Repo");
    assert!(!launch.env_vars.contains_key("CODEX_HOME"));
    assert!(!launch.env_vars.contains_key("OPENAI_API_KEY"));
    let spec = launch.into_launch_spec(
        "codex",
        "task",
        30,
        HashMap::from([("EXISTING_AUTH_ENV".into(), "keep".into())]),
    );
    assert_eq!(spec.env_vars["EXISTING_AUTH_ENV"], "keep");
    assert_eq!(
        spec.env_vars["TACHI_CODEX_MCP_TEST_RUN_0_0"],
        "Bearer private-test-token"
    );
}

#[test]
fn stdio_environment_is_forwarded_only_if_it_does_not_change_the_carrier() {
    let config = json!({"mcpServers":{"hub":{
        "command":"/bin/adapter", "args":["serve", "quote\"back\\slash\nline", "before\u{7f}after"], "env":{"MODE":"readonly"}
    }}});
    let projection = project(&config, &BTreeSet::new(), "test_run", |key| {
        (key == "MODE").then(|| "readonly".into())
    })
    .unwrap();
    assert!(projection.env_vars.is_empty());
    let merged = effective_config(json!({}), &projection);
    let decoded = decode_servers(&merged).unwrap();
    assert_eq!(
        decoded["tachi_dispatch_test_run_0"]
            .env_vars
            .as_ref()
            .unwrap(),
        &["MODE"]
    );
    assert_eq!(
        json!(projection.rendered_overrides()),
        fixture()["stdio_env"]
    );
    for environment in [None, Some("different".to_string())] {
        let error = project(&config, &BTreeSet::new(), "test_run", |_| {
            environment.clone()
        })
        .err()
        .unwrap();
        assert!(error.contains("cannot safely project"));
        assert!(!error.contains("readonly"));
    }
}

#[test]
fn toml_table_keys_escape_del_as_well_as_string_values() {
    let value = json!({"key\u{7f}": "value"});
    assert_eq!(
        json!([format!("payload={}", mcp_toml_value(&value))]),
        fixture()["del_key"]
    );
}

#[test]
fn inventory_parsing_and_descriptor_refusal_do_not_echo_credentials() {
    let names = parse_inventory(br#"[{"name":"tachi","transport":{"command":"tachi","env":{"TOKEN":"private-token"}}},{"name":"other"}]"#).unwrap();
    assert_eq!(names, BTreeSet::from(["other".into(), "tachi".into()]));
    for raw in [
        br#"{"error":"private-token"}"#.as_slice(),
        b"private-token",
        br#"[{"transport":"private-token"}]"#,
    ] {
        assert!(!parse_inventory(raw).unwrap_err().contains("private-token"));
    }
    for servers in [
        json!({"unsafe.name":{"type":"http","url":"http://localhost/mcp"}}),
        json!({"tachi":{"type":"http","url":"http://localhost/mcp","command":"tachi"}}),
        json!({"tachi":{"type":"http","url":"http://localhost/mcp","headers":{"bad":3}}}),
    ] {
        assert!(project(
            &json!({"mcpServers":servers}),
            &BTreeSet::new(),
            "test_run",
            |_| None
        )
        .is_err());
    }
}

#[test]
fn worker_environment_preserves_override_removal_and_inheritance() {
    let inherited = std::env::var("PATH").expect("test harness PATH");
    let mut worker = Command::new("codex");
    assert_eq!(
        worker_environment(&worker, "PATH").as_deref(),
        Some(inherited.as_str())
    );

    worker.env("PATH", "admitted-path");
    assert_eq!(
        worker_environment(&worker, "PATH"),
        Some("admitted-path".into())
    );
    worker.env_remove("PATH");
    assert!(
        worker_environment(&worker, "PATH").is_none(),
        "an explicit removal must not recover the inherited value"
    );
}

mod admitted_context_tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::PathBuf;

    struct ConfigFixture(PathBuf);

    impl ConfigFixture {
        fn new(config: &Value) -> Self {
            let root =
                std::env::temp_dir().join(format!("tachi-codex-mcp-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&root).expect("own unique fixture directory");
            let fixture = Self(root);
            std::fs::write(fixture.path(), serde_json::to_vec(config).unwrap()).unwrap();
            fixture
        }

        fn path(&self) -> PathBuf {
            self.0.join("mcp.json")
        }
    }

    impl Drop for ConfigFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    struct WorkerSnapshot {
        program: OsString,
        args: Vec<OsString>,
        cwd: Option<PathBuf>,
        env: Vec<(OsString, Option<OsString>)>,
    }

    fn snapshot(worker: &Command) -> WorkerSnapshot {
        WorkerSnapshot {
            program: worker.get_program().to_owned(),
            args: worker.get_args().map(OsString::from).collect(),
            cwd: worker.get_current_dir().map(Path::to_path_buf),
            env: worker
                .get_envs()
                .map(|(key, value)| (key.to_owned(), value.map(OsString::from)))
                .collect(),
        }
    }

    fn worker() -> Command {
        let launch = build_codex_launch(
            &DispatchLaunchParams {
                sandbox: Some("read-only".into()),
                cwd: Some(".".into()),
                ..Default::default()
            },
            "bounded task",
        )
        .unwrap();
        let mut worker = Command::new(launch.program);
        worker
            .args(launch.args)
            .env("TACHI_MCP_TEST_KEEP", "original-value")
            .env_remove("TACHI_MCP_TEST_REMOVE");
        worker
    }

    #[cfg(unix)]
    fn inventory(payload: &str, exit_code: u8, marker: &Path) -> Command {
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                "printf 'probe' > \"$1\"; printf '%s' \"$2\"; exit \"$3\"",
                "tachi-inventory-fixture",
            ])
            .arg(marker)
            .arg(payload)
            .arg(exit_code.to_string());
        command
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_refusal_does_not_start_inventory_or_mutate_worker() {
        let fixture = ConfigFixture::new(&json!({"mcpServers":{"tachi":{
            "type":"http", "url":"http://localhost/mcp", "command":"ambiguous"
        }}}));
        let marker = fixture.0.join("probe");
        let mut inventory = inventory("[]", 0, &marker);
        let mut worker = worker();
        let before = snapshot(&worker);
        let error = apply_codex_mcp_config(&mut worker, &mut inventory, &fixture.path())
            .expect_err("invalid descriptor must be refused before inspection");

        assert!(error.contains("unambiguous HTTP descriptor"), "{error}");
        assert!(!marker.exists(), "invalid config must not run a probe");
        assert_eq!(snapshot(&worker), before);
    }

    #[cfg(unix)]
    #[test]
    fn later_collision_does_not_partially_apply_an_earlier_projection() {
        let mut config = http_config();
        config["mcpServers"]["aaa_first"] = json!({
            "type":"http", "url":"http://localhost/first",
            "headers":{"Authorization":"private-first-token"}
        });
        let fixture = ConfigFixture::new(&config);
        let original_config = std::fs::read(fixture.path()).unwrap();
        let marker = fixture.0.join("probe");
        let mut inventory = inventory(r#"[{"name":"tachi"}]"#, 0, &marker);
        let mut worker = worker();
        let before = snapshot(&worker);
        let error = apply_codex_mcp_config(&mut worker, &mut inventory, &fixture.path())
            .expect_err("second server's collision must refuse the complete projection");

        assert!(error.contains("complete per-server tool policy"), "{error}");
        assert!(!error.contains("private-first-token"));
        assert_eq!(std::fs::read(marker).unwrap(), b"probe");
        assert_eq!(snapshot(&worker), before);
        assert_eq!(std::fs::read(fixture.path()).unwrap(), original_config);
    }

    #[cfg(unix)]
    #[test]
    fn inventory_failure_leaves_worker_and_configuration_unchanged() {
        let fixture = ConfigFixture::new(&http_config());
        let original_config = std::fs::read(fixture.path()).unwrap();
        for (index, (payload, exit_code, expected)) in [
            ("private-invalid-output", 0, "not JSON"),
            ("[]", 7, "inspection failed"),
        ]
        .into_iter()
        .enumerate()
        {
            let marker = fixture.0.join(format!("probe-{index}"));
            let mut inventory = inventory(payload, exit_code, &marker);
            let mut worker = worker();
            let before = snapshot(&worker);
            let error = apply_codex_mcp_config(&mut worker, &mut inventory, &fixture.path())
                .expect_err("failed inventory must not publish overrides");

            assert!(error.contains(expected), "{error}");
            assert!(!error.contains("private-invalid-output"));
            assert_eq!(std::fs::read(marker).unwrap(), b"probe");
            assert_eq!(snapshot(&worker), before);
            assert_eq!(std::fs::read(fixture.path()).unwrap(), original_config);
        }
    }

    #[cfg(unix)]
    #[test]
    fn admitted_environment_and_http_headers_reach_the_actual_worker() {
        let mut config = http_config();
        config["mcpServers"]["hub"] = json!({
            "command":"/bin/adapter", "env":{"PATH":"admitted-path"}
        });
        let fixture = ConfigFixture::new(&config);
        let original_config = std::fs::read(fixture.path()).unwrap();
        let marker = fixture.0.join("probe");
        let mut inventory = inventory("[]", 0, &marker);
        inventory.env("PATH", "admitted-path");
        let mut worker = worker();
        worker.env("PATH", "admitted-path");
        let before = snapshot(&worker);

        apply_codex_mcp_config(&mut worker, &mut inventory, &fixture.path()).unwrap();

        let after = snapshot(&worker);
        assert_eq!(after.program, before.program);
        assert_eq!(after.cwd, before.cwd);
        assert_eq!(&after.args[..before.args.len()], before.args.as_slice());
        let args = after
            .args
            .iter()
            .map(|arg| arg.to_str().unwrap())
            .collect::<Vec<_>>();
        assert!(args
            .iter()
            .any(|arg| arg.contains("\"env_vars\" = [\"PATH\"]")));
        assert!(!args.iter().any(|arg| arg.contains("private-test-token")));
        assert!(!args.iter().any(|arg| arg.contains("admitted-path")));
        assert!(worker.get_envs().any(|(key, value)| {
            key.to_string_lossy().starts_with("TACHI_CODEX_MCP_")
                && value == Some(std::ffi::OsStr::new("Bearer private-test-token"))
        }));
        for entry in &before.env {
            assert!(after.env.contains(entry), "existing env change was lost");
        }
        assert_eq!(std::fs::read(marker).unwrap(), b"probe");
        assert_eq!(std::fs::read(fixture.path()).unwrap(), original_config);
    }

    #[cfg(unix)]
    #[test]
    fn explicit_removal_cannot_authorize_a_stdio_value_from_the_daemon() {
        let inherited = std::env::var("PATH").expect("test harness PATH");
        let fixture = ConfigFixture::new(&json!({"mcpServers":{"hub":{
            "command":"/bin/adapter", "env":{"PATH":inherited}
        }}}));
        let marker = fixture.0.join("probe");
        let mut inventory = inventory("[]", 0, &marker);
        inventory.env_remove("PATH");
        let mut worker = worker();
        worker.env_remove("PATH");
        let before = snapshot(&worker);
        let error = apply_codex_mcp_config(&mut worker, &mut inventory, &fixture.path())
            .expect_err("removed env must not be supplied through ambient fallback");

        assert!(error.contains("cannot safely project"), "{error}");
        assert!(!marker.exists(), "env refusal must precede inventory");
        assert_eq!(snapshot(&worker), before);
    }

    #[cfg(not(unix))]
    #[test]
    fn unsupported_platform_refuses_before_mutating_worker() {
        let fixture = ConfigFixture::new(&http_config());
        let mut inventory = Command::new("codex");
        inventory.args(["mcp", "list", "--json"]);
        let mut worker = worker();
        let before = snapshot(&worker);
        let error = apply_codex_mcp_config(&mut worker, &mut inventory, &fixture.path())
            .expect_err("platform lacks the required probe ownership boundary");

        assert!(error.contains("Unix owned process group"), "{error}");
        assert_eq!(snapshot(&worker), before);
    }
}

#[cfg(unix)]
mod inventory_process_tests {
    use super::*;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::CommandExt;

    fn assert_wrapper_cleanup(mode: &str, expected_error: &str) {
        let (mut witness, inherited) = UnixStream::pair().unwrap();
        witness.set_nonblocking(true).unwrap();
        let inherited_fd = inherited.as_raw_fd();
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                r#""$2" --exact "$3" --nocapture --test-threads=1 &
printf '[]'
case "$1" in running) wait ;; success) exit 0 ;; failure) exit 7 ;; esac"#,
                "tachi-inventory-wrapper",
                mode,
            ])
            .arg(std::env::current_exe().unwrap())
            .arg("launcher::codex_mcp::tests::inventory_process_tests::inventory_pipe_holder")
            .env("TACHI_CODEX_INVENTORY_TEST_FD", inherited_fd.to_string());
        // SAFETY: only change this fixture-owned descriptor in the child
        // between fork and exec; no allocation or locks occur in the callback.
        unsafe {
            command.pre_exec(move || {
                let flags = libc::fcntl(inherited_fd, libc::F_GETFD);
                if flags < 0
                    || libc::fcntl(inherited_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let result = inspect_inventory_command(
            &mut command,
            Duration::from_secs(1),
            Duration::from_millis(250),
        );
        drop(inherited);

        // EOF proves that no wrapper descendant still holds the pipe. A dead
        // orphan may remain a zombie until init reaps it, so kill(pid, 0) would
        // not be a reliable assertion. The fixture also expires after 5s if a
        // regression breaks cleanup; the test never blocks on its read end.
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut bytes = Vec::new();
        let mut buffer = [0; 32];
        loop {
            match witness.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => bytes.extend_from_slice(&buffer[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "wrapper child retained its pipe");
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("read wrapper witness: {error}"),
            }
        }
        assert_eq!(bytes, b"ready", "fixture descendant must have run");
        let error = result.unwrap_err();
        assert!(error.contains(expected_error), "{error}");
    }

    #[test]
    fn inventory_pipe_holder() {
        let Ok(value) = std::env::var("TACHI_CODEX_INVENTORY_TEST_FD") else {
            return;
        };
        let fd = value.parse().unwrap();
        // SAFETY: the parent fixture passes this owned socket descriptor
        // through exec. This process has no other Rust owner for it.
        let mut socket = unsafe { UnixStream::from_raw_fd(fd) };
        std::io::Write::write_all(&mut socket, b"ready").unwrap();
        std::thread::sleep(Duration::from_secs(5));
    }

    #[test]
    fn running_wrapper_timeout_terminates_descendant_and_closes_its_pipe() {
        assert_wrapper_cleanup("running", "timed out");
    }

    #[test]
    fn successful_wrapper_with_open_pipe_terminates_descendant() {
        assert_wrapper_cleanup("success", "did not close stdout");
    }

    #[test]
    fn failed_wrapper_with_open_pipe_terminates_descendant() {
        assert_wrapper_cleanup("failure", "did not close stdout");
    }

    #[test]
    fn ordinary_inventory_completion_preserves_exit_status() {
        let mut success = Command::new("/bin/sh");
        success.args(["-c", r#"printf '[{"name":"existing"}]'"#]);
        assert_eq!(
            inspect_inventory_command(
                &mut success,
                Duration::from_secs(1),
                Duration::from_millis(100)
            )
            .unwrap(),
            BTreeSet::from(["existing".into()])
        );
        let mut failure = Command::new("/bin/sh");
        failure.args(["-c", "printf '[]'; exit 7"]);
        let error = inspect_inventory_command(
            &mut failure,
            Duration::from_secs(1),
            Duration::from_millis(100),
        )
        .unwrap_err();
        assert!(error.contains("inspection failed"), "{error}");
    }

    #[test]
    fn group_absence_requires_reap_and_does_not_accept_live_or_zombie_members() {
        let child = Command::new("/bin/sh")
            .args(["-c", "read -r release; exit 7"])
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut process = InventoryProcess {
            child,
            owns_root: true,
        };
        let pgid = process.child.id() as libc::pid_t;
        assert!(process.observe_exit().is_ok_and(|exited| !exited));
        assert!(
            !inventory_group_is_absent(pgid),
            "a live process group must not clear a permission failure"
        );

        drop(process.child.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(2);
        while !process.observe_exit().unwrap() {
            assert!(Instant::now() < deadline, "fixture leader did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !inventory_group_is_absent(pgid),
            "an unreaped zombie (including Darwin EPERM) is not definite absence"
        );

        // The child has already exited. Reap without further group signals;
        // successful reap removes the last member and preserves its exit code.
        process.owns_root = false;
        assert_eq!(process.child.wait().unwrap().code(), Some(7));
        assert!(inventory_group_is_absent(pgid));
    }

    #[test]
    fn lost_child_ownership_disarms_all_later_group_signals() {
        let child = Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .process_group(0)
            .spawn()
            .unwrap();
        let mut process = InventoryProcess {
            child,
            owns_root: true,
        };
        process.child.wait().unwrap();
        assert_eq!(
            process.observe_exit().unwrap_err().raw_os_error(),
            Some(libc::ECHILD)
        );
        assert!(!process.owns_root);
        assert!(process
            .terminate_and_reap()
            .unwrap_err()
            .to_string()
            .contains("ownership was lost"));
    }
}
