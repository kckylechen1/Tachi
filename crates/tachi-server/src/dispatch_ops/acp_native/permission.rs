use serde_json::{json, Value};

pub(super) fn native_permission_response(permission_label: &str, params: &Value) -> Value {
    if permission_label == "approve-reads" && permission_request_is_read_like(params) {
        if let Some(option_id) = select_permission_option(params, true) {
            return json!({
                "outcome": {
                    "outcome": "selected",
                    "optionId": option_id,
                }
            });
        }
    }
    if let Some(option_id) = select_permission_option(params, false) {
        return json!({
            "outcome": {
                "outcome": "selected",
                "optionId": option_id,
            }
        });
    }
    json!({
        "outcome": {
            "outcome": "cancelled",
        }
    })
}

fn permission_request_is_read_like(params: &Value) -> bool {
    let haystack = serde_json::to_string(params)
        .unwrap_or_default()
        .to_ascii_lowercase();
    ["read", "search", "grep", "list", "view", "find"]
        .iter()
        .any(|needle| haystack.contains(needle))
        && ![
            "write", "edit", "delete", "remove", "terminal", "shell", "exec", "create",
        ]
        .iter()
        .any(|needle| haystack.contains(needle))
}

fn select_permission_option(params: &Value, approve: bool) -> Option<String> {
    let options = params.get("options").and_then(Value::as_array)?;
    let mut fallback = None;
    for option in options {
        let id = option
            .get("optionId")
            .or_else(|| option.get("id"))
            .or_else(|| option.get("name"))
            .and_then(Value::as_str)?;
        let label = serde_json::to_string(option)
            .unwrap_or_default()
            .to_ascii_lowercase();
        if fallback.is_none() {
            fallback = Some(id.to_string());
        }
        let selected = if approve {
            ["approve", "allow", "accept", "yes", "read"]
                .iter()
                .any(|needle| label.contains(needle))
                && !["deny", "reject", "cancel", "no"]
                    .iter()
                    .any(|needle| label.contains(needle))
        } else {
            ["deny", "reject", "cancel", "no"]
                .iter()
                .any(|needle| label.contains(needle))
        };
        if selected {
            return Some(id.to_string());
        }
    }
    if approve {
        fallback
    } else {
        None
    }
}
