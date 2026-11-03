use super::*;
use crate::launcher::{build_codex_launch_with_projection, DispatchLaunchParams};

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
    let launch = build_codex_launch_with_projection(&input, "task", Some(projection)).unwrap();
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
        "command":"/bin/adapter", "args":["serve", "quote\"back\\slash\nline"], "env":{"MODE":"readonly"}
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
