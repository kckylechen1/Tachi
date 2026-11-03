use super::*;
use crate::tool_params::{TachiMemoryParams, TachiSaveParams};
use serde_json::{json, Value};

fn forwarded<T: serde::Serialize>(params: &T) -> Option<JsonObject> {
    Some(
        serde_json::to_value(params)
            .unwrap()
            .as_object()
            .unwrap()
            .clone(),
    )
}

#[test]
fn serialized_optional_nulls_keep_default_workspace_binding() {
    for action in ["search", "ask", "save", "checkpoint", "extract_facts"] {
        let params: TachiMemoryParams = serde_json::from_value(json!({
            "action": action,
            "query": "workspace continuity",
            "text": "Remember the calling workspace's durable convention."
        }))
        .unwrap();
        let mut arguments = forwarded(&params);
        // Older typed forwarding includes optional selectors as JSON null.
        arguments
            .as_mut()
            .unwrap()
            .insert("storage_scope".into(), Value::Null);
        arguments
            .as_mut()
            .unwrap()
            .insert("scope".into(), Value::Null);
        arguments
            .as_mut()
            .unwrap()
            .insert("project".into(), Value::Null);
        enforce_session_project(
            "tachi_memory",
            &mut arguments,
            "current",
            "proxy",
            EnforcementRole::Preflight,
        )
        .unwrap();
        assert!(arguments.as_ref().unwrap()["project"].is_null());
        enforce_session_project(
            "tachi_memory",
            &mut arguments,
            "current",
            "HTTP",
            EnforcementRole::Authoritative,
        )
        .unwrap();
        assert_eq!(
            arguments.as_ref().unwrap()["project"],
            json!("current"),
            "{action}"
        );
        assert_eq!(
            arguments.as_ref().unwrap()[PROJECT_EXPLICIT_MARKER],
            json!(false)
        );
        let decoded: TachiMemoryParams = serde_json::from_value(json!(arguments.unwrap())).unwrap();
        assert_eq!(decoded.project.as_deref(), Some("current"));
        assert!(!decoded.project_explicit);
    }
}

#[test]
fn wire_save_and_serialized_memory_accept_nulls_and_equivalent_global_scopes() {
    let save_payload = json!({
        "text": "A shared convention explicitly belongs in global memory.",
        "kind": "facts", "storage_scope": "GLOBAL", "project": null
    });
    // tachi_save receives this wire object; its params are not serialized for
    // forwarding. Unified tachi_memory below exercises typed serialization.
    let save: TachiSaveParams = serde_json::from_value(save_payload.clone()).unwrap();
    assert_eq!(save.scope.as_deref(), Some("GLOBAL"));
    assert!(save.project.is_none());
    let mut cases = vec![(
        "tachi_save",
        Some(save_payload.as_object().unwrap().clone()),
    )];
    for action in ["save", "checkpoint", "extract_facts"] {
        for legacy in [Value::Null, json!("gLoBaL")] {
            let params: TachiMemoryParams = serde_json::from_value(json!({
                "action": action, "text": "A shared convention belongs in global.",
                "storage_scope": "GLOBAL", "scope": legacy, "project": null
            }))
            .unwrap();
            cases.push(("tachi_memory", forwarded(&params)));
        }
    }
    for (tool, mut arguments) in cases {
        enforce_session_project(
            tool,
            &mut arguments,
            "current",
            "HTTP",
            EnforcementRole::Authoritative,
        )
        .unwrap();
        reject_unbound_cross_project_write(tool, &arguments, None, "HTTP").unwrap();
        stamp_unbound_project_explicit_marker(tool, &mut arguments);
        assert!(arguments
            .as_ref()
            .unwrap()
            .get("project")
            .is_none_or(Value::is_null));
        assert_eq!(
            arguments.as_ref().unwrap()[PROJECT_EXPLICIT_MARKER],
            json!(false)
        );
    }
}

#[test]
fn null_project_never_becomes_explicit_read_provenance() {
    for tool in [
        "tachi_memory",
        "tachi_search",
        "tachi_wiki",
        "tachi_wiki_search",
    ] {
        let mut arguments = Some(
            json!({
                "project": null, "__tachi_project_explicit": true
            })
            .as_object()
            .unwrap()
            .clone(),
        );
        stamp_unbound_project_explicit_marker(tool, &mut arguments);
        assert_eq!(arguments.unwrap()[PROJECT_EXPLICIT_MARKER], json!(false));
    }
}

#[test]
fn nonnull_invalid_scope_and_project_values_still_refuse() {
    for payload in [
        json!({"action": "save", "storage_scope": false}),
        json!({"action": "save", "storage_scope": "global", "scope": false}),
        json!({"action": "save", "storage_scope": "global", "project": false}),
        json!({"action": "save", "storage_scope": "global", "scope": "project"}),
        json!({"action": "search", "storage_scope": "global"}),
    ] {
        let mut arguments = Some(payload.as_object().unwrap().clone());
        assert!(
            enforce_session_project(
                "tachi_memory",
                &mut arguments,
                "current",
                "HTTP",
                EnforcementRole::Authoritative,
            )
            .is_err(),
            "{payload}"
        );
    }
}
