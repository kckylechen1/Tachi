use super::*;

const QUERY: &str = "SyntheticIsolationRecall";

#[tokio::test]
async fn synthetic_isolation_reaches_search_ask_and_briefing_without_hiding_production_notes() {
    let (server, _home) = make_server_with_temp_home();
    server
        .with_global_store(|store| {
            for (id, path, marked, text) in [
                (
                    "isolation-reserved",
                    "/scratch/tachi-test/recall",
                    false,
                    "fabricated amber cargo manifests",
                ),
                (
                    "isolation-marked",
                    "/notes/recall-fixture",
                    true,
                    "invented violet weather stations",
                ),
                (
                    "isolation-scratch",
                    "/scratch/production/lesson",
                    false,
                    "actual green deployment recovery",
                ),
                (
                    "isolation-testing",
                    "/testing/production-note",
                    false,
                    "recorded blue service rollout",
                ),
            ] {
                let mut entry = make_entry(id);
                entry.path = path.to_string();
                entry.summary = format!("{QUERY} {text}");
                entry.text = entry.summary.clone();
                entry.metadata = json!({"synthetic_fixture": marked});
                store.upsert(&entry).map_err(|error| error.to_string())?;
            }
            Ok(())
        })
        .expect("seed synthetic and production recall rows");

    for action in ["search", "ask", "briefing"] {
        for include_test in [false, true] {
            let params = serde_json::from_value(json!({
                "action": action, "format": "json", "query": QUERY,
                "scope": "memory", "top_k": 10, "compact": false,
                "auto_conversation_scope": false, "multi_evidence_pack": false,
                "include_test": include_test
            }))
            .expect("public memory params");
            let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
                .await
                .expect("memory recall succeeds");
            for real in ["isolation-scratch", "isolation-testing"] {
                assert!(body.contains(real), "{action} must retain {real}: {body}");
            }
            for fixture in ["isolation-reserved", "isolation-marked"] {
                assert_eq!(
                    body.contains(fixture),
                    include_test,
                    "{action} policy for {fixture}: {body}"
                );
            }
        }
    }

    for (prefix, expected) in [
        ("/scratch/tachi-test", "isolation-reserved"),
        ("/notes", "isolation-marked"),
    ] {
        let params = serde_json::from_value(json!({
            "action": "search", "format": "json", "query": QUERY,
            "scope": "memory", "path_prefix": prefix,
            "auto_conversation_scope": false, "multi_evidence_pack": false
        }))
        .unwrap();
        let body =
            crate::facade_memory_ops::handle_tachi_memory_with_resources(&server, params, None)
                .await
                .unwrap()
                .0;
        assert_eq!(
            body.contains(expected),
            prefix == "/scratch/tachi-test",
            "{body}"
        );
    }

    let raw_params = serde_json::from_value(json!({
        "query": QUERY, "format": "json", "top_k": 10,
        "include_test": true, "mmr_threshold": null,
        "auto_conversation_scope": false, "multi_evidence_pack": false
    }))
    .unwrap();
    let raw = crate::memory_search_ops::handle_search_memory(&server, raw_params, false)
        .await
        .unwrap();
    assert!(
        serde_json::from_str::<Value>(&raw).unwrap().is_array(),
        "{raw}"
    );
    assert!(
        raw.contains("isolation-marked"),
        "raw search must forward opt-in: {raw}"
    );
}
