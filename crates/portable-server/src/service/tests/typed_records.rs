use super::*;
use rmcp::model::{CallToolRequestParams, ClientConfig, ProtocolVersion};
use rmcp::ServiceExt;
use serde_json::Value;

fn typed_server() -> PortableServer {
    let mut config = crate::config::Config::parse(Vec::<String>::new(), |_| None).unwrap();
    config.project_db_paths.push(":memory:".to_string());
    config.typed_records =
        Some(serde_json::from_value(crate::config::typed_fixture_value()).unwrap());
    crate::build_server(config).unwrap()
}

fn input(id: &str, kind: &str, revision: &str) -> Value {
    json!({
        "id":id,"kind":{"name":kind,"version":1},
        "keys":{"id":"preference-1","revision":revision},
        "payload":{"text":"Use concise explanations."},"links":[]
    })
}

async fn wire_call(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ClientConfig>,
    name: &'static str,
    args: Value,
    error: bool,
) -> Value {
    let result = client
        .call_tool(
            CallToolRequestParams::new(name).with_arguments(args.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_eq!(result.is_error == Some(true), error, "{name}: {result:?}");
    let text = &result
        .content
        .iter()
        .find_map(|content| content.as_text())
        .unwrap()
        .text;
    if error {
        json!({"error":text})
    } else {
        serde_json::from_str(text).unwrap()
    }
}

#[tokio::test]
async fn initialized_service_round_trips_typed_save_get_and_every_save_alias_with_host_authority() {
    let server = typed_server();
    let served = server.clone();
    let (client_io, server_io) = tokio::io::duplex(65536);
    let task = tokio::spawn(async move {
        served
            .serve(server_io)
            .await
            .unwrap()
            .waiting()
            .await
            .unwrap()
    });
    let client = ClientConfig::default()
        .with_protocol_version(ProtocolVersion::V_2025_11_25)
        .serve(client_io)
        .await
        .unwrap();
    let record = input("pref-1", "user.preference", "1");
    let saved = wire_call(
        &client,
        "save",
        json!({
            "typed_record":record,"writer":"request-forgery","role":"other-role"
        }),
        false,
    )
    .await;
    assert_eq!(saved["typed_record"]["writer"], "private-stdio-host");
    assert_eq!(saved["prior_idempotent"], false);
    for name in ["hapi_save", "tachi_save", "hapi_memory", "tachi_memory"] {
        let mut args = json!({"typed_record":record});
        if name.ends_with("_memory") {
            args["action"] = json!("save");
        }
        let replay = wire_call(&client, name, args, false).await;
        assert_eq!(replay["typed_record"], saved["typed_record"]);
        assert_eq!(replay["prior_idempotent"], true);
    }
    let got = wire_call(&client, "get", json!({"id":"pref-1","typed":true}), false).await;
    assert_eq!(got, saved["typed_record"]);
    assert!(server
        .stores
        .project()
        .unwrap()
        .store
        .lock()
        .unwrap()
        .get_typed_record("pref-1")
        .unwrap()
        .is_none());
    assert!(wire_call(&client, "get", json!({"id":"pref-1"}), false)
        .await
        .is_null());

    for extra in [
        json!({"scope":"project"}),
        json!({"scope":"global"}),
        json!({"text":""}),
        json!({"text":"ordinary note"}),
        json!({"id":"ordinary-id"}),
        json!({"summary":""}),
        json!({"keywords":[]}),
        json!({"metadata":{}}),
    ] {
        let mut mixed = json!({"typed_record":record});
        mixed
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        wire_call(&client, "save", mixed, true).await;
    }
    for extra in [
        json!({"query":"ignored query"}),
        json!({"mmr_threshold":0.85}),
        json!({"mmr_threshold":null}),
        json!({"graph_expand_hops":1}),
        json!({"include_archived":false}),
        json!({"context_symbols":[]}),
    ] {
        let mut args = json!({"action":"save","typed_record":record});
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        wire_call(&client, "hapi_memory", args, true).await;
    }
    wire_call(
        &client,
        "tachi_memory",
        json!({"action":"search","typed_record":record,"query":"query"}),
        true,
    )
    .await;
    wire_call(
        &client,
        "save",
        json!({"text":"note","predecessor":"pref-1"}),
        true,
    )
    .await;
    for empty in [json!({}), json!({"text":"  "})] {
        wire_call(&client, "save", empty, true).await;
    }
    let denied = input("denied-episode", "user.episode", "1");
    let refused = wire_call(
        &client,
        "save",
        json!({"typed_record":denied,"writer":"unrestricted"}),
        true,
    )
    .await;
    assert!(refused["error"]
        .as_str()
        .unwrap()
        .contains("not authorized"));
    assert!(wire_call(
        &client,
        "get",
        json!({"id":"denied-episode","typed":true}),
        false
    )
    .await
    .is_null());
    let mut forged = input("forged-record", "user.preference", "2");
    forged["writer"] = json!("unrestricted");
    wire_call(&client, "save", json!({"typed_record":forged}), true).await;

    let mut changed = record.clone();
    changed["payload"]["text"] = json!("cannot overwrite an immutable record");
    wire_call(&client, "save", json!({"typed_record":changed}), true).await;
    let correction = input("pref-2", "user.preference", "2");
    let corrected = wire_call(
        &client,
        "save",
        json!({"typed_record":correction,"predecessor":"pref-1"}),
        false,
    )
    .await;
    assert_eq!(corrected["typed_record"]["revision"], 2);
    assert!(!corrected["receipt"].is_null());
    let old = wire_call(&client, "get", json!({"id":"pref-1","typed":true}), false).await;
    assert_eq!(old["payload"], record["payload"]);
    assert_eq!(old["superseded_by"], "pref-2");
    let legacy = wire_call(
        &client,
        "save",
        json!({"id":"ordinary","text":"ordinary remains mutable"}),
        false,
    )
    .await;
    assert_eq!(legacy["operation"], "created");
    client.cancel().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn typed_records_default_off_and_clearing_writer_preserves_only_configured_reads() {
    let ordinary = boot(None, "default");
    assert!(ordinary
        .save(Parameters(
            serde_json::from_value(json!({
                "typed_record":input("pref-1", "user.preference", "1")
            }))
            .unwrap()
        ))
        .await
        .is_err());
    assert!(ordinary
        .get(Parameters(GetParams {
            id: "pref-1".to_string(),
            typed: true
        }))
        .await
        .is_err());
    ordinary
        .save(Parameters(save_params("ordinary note", "/notes")))
        .await
        .unwrap();

    let configured = typed_server();
    let written = configured
        .save(Parameters(
            serde_json::from_value(json!({
                "typed_record":input("pref-1", "user.preference", "1")
            }))
            .unwrap(),
        ))
        .await
        .unwrap();
    let reader = configured.without_typed_writer();
    let record = reader
        .get(Parameters(GetParams {
            id: "pref-1".to_string(),
            typed: true,
        }))
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&written).unwrap()["typed_record"],
        serde_json::from_str::<Value>(&record).unwrap()
    );
    assert!(reader
        .save(Parameters(
            serde_json::from_value(json!({
                "typed_record":input("pref-2", "user.preference", "2")
            }))
            .unwrap()
        ))
        .await
        .is_err());
}
