use memcore::{MemoryEntry, TachiEventRecord};
use serde_json::{json, Map, Value};

use super::feedback::pattern_ref_json;

fn value_from_payload_or_metadata(
    payload: &Value,
    metadata: &Value,
    keys: &[&str],
) -> Option<Value> {
    keys.iter().find_map(|key| {
        payload
            .get(*key)
            .or_else(|| metadata.get(*key))
            .filter(|value| !value.is_null())
            .cloned()
    })
}

fn string_from_payload_or_metadata(
    payload: &Value,
    metadata: &Value,
    keys: &[&str],
) -> Option<String> {
    keys.iter()
        .find_map(|key| {
            payload
                .get(*key)
                .or_else(|| metadata.get(*key))
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn string_value<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn array_value_from_payload_or_metadata(payload: &Value, metadata: &Value, keys: &[&str]) -> Value {
    match value_from_payload_or_metadata(payload, metadata, keys) {
        Some(Value::Array(items)) => Value::Array(items),
        Some(Value::String(value)) if !value.trim().is_empty() => json!([value]),
        Some(value) => json!([value]),
        None => json!([]),
    }
}

fn merge_array_values(existing: Option<&Value>, incoming: Value) -> Value {
    let mut items = existing
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    match incoming {
        Value::Array(incoming_items) => {
            for item in incoming_items {
                if !items.iter().any(|existing| existing == &item) {
                    items.push(item);
                }
            }
        }
        Value::Null => {}
        other => {
            if !items.iter().any(|existing| existing == &other) {
                items.push(other);
            }
        }
    }
    Value::Array(items)
}

fn insert_if_some(map: &mut Map<String, Value>, key: &str, value: Option<String>) {
    if let Some(value) = value {
        map.insert(key.to_string(), json!(value));
    }
}

fn schema_marker(name: &str, issues: Vec<String>) -> Value {
    let status = if issues.is_empty() {
        "validated"
    } else {
        "needs_review"
    };
    json!({
        "name": name,
        "version": 1,
        "status": status,
        "issues": issues,
    })
}

fn has_non_empty_array(map: &Map<String, Value>, key: &str) -> bool {
    map.get(key)
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty())
}

fn validate_timeline(timeline: Option<&Value>) -> Vec<String> {
    let Some(map) = timeline.and_then(Value::as_object) else {
        return vec!["missing timeline metadata".to_string()];
    };
    let has_evidence = [
        "discoveries",
        "decisions",
        "open_threads",
        "evolution",
        "causal_edges",
        "external_validations",
    ]
    .iter()
    .any(|key| has_non_empty_array(map, key));
    let mut issues = Vec::new();
    if !has_evidence {
        issues.push("no timeline evidence arrays populated".to_string());
    }
    if map
        .get("latest_event_id")
        .and_then(Value::as_str)
        .is_none_or(|value| value.trim().is_empty())
    {
        issues.push("missing latest_event_id".to_string());
    }
    issues
}

fn validate_lexicon(lexicon: Option<&Value>) -> Vec<String> {
    let Some(map) = lexicon.and_then(Value::as_object) else {
        return vec!["missing lexicon metadata".to_string()];
    };
    let mut issues = Vec::new();
    if map
        .get("meaning")
        .and_then(Value::as_str)
        .is_none_or(|value| value.trim().is_empty())
    {
        issues.push("missing meaning".to_string());
    }
    if !has_non_empty_array(map, "shorthand_triggers") {
        issues.push("missing shorthand_triggers".to_string());
    }
    issues
}

pub(super) fn timeline_projection_metadata(
    existing: Option<&MemoryEntry>,
    event: &TachiEventRecord,
    payload: &Value,
) -> Value {
    let payload_metadata = payload
        .get("metadata")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let existing_timeline = existing
        .and_then(|entry| entry.metadata.get("timeline"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut timeline = existing_timeline.clone();

    let append_array = |timeline: &mut Map<String, Value>, key: &str, keys: &[&str]| {
        let incoming = array_value_from_payload_or_metadata(payload, &payload_metadata, keys);
        let merged = merge_array_values(existing_timeline.get(key), incoming);
        timeline.insert(key.to_string(), merged);
    };

    append_array(&mut timeline, "discoveries", &["discoveries", "findings"]);
    append_array(&mut timeline, "decisions", &["decisions", "resolved"]);
    append_array(
        &mut timeline,
        "open_threads",
        &["open_threads", "open_questions", "threads"],
    );
    append_array(
        &mut timeline,
        "evolution",
        &["evolution", "revisions", "transitions"],
    );
    append_array(
        &mut timeline,
        "causal_edges",
        &["causal_edges", "edges", "causes"],
    );
    append_array(
        &mut timeline,
        "external_validations",
        &["external_validations", "validations", "checks"],
    );

    insert_if_some(
        &mut timeline,
        "valid_from",
        string_from_payload_or_metadata(payload, &payload_metadata, &["valid_from"])
            .or_else(|| Some(event.created_at.clone())),
    );
    insert_if_some(
        &mut timeline,
        "valid_until",
        string_from_payload_or_metadata(payload, &payload_metadata, &["valid_until"]),
    );
    timeline.insert("latest_event_id".to_string(), json!(event.id));
    timeline.insert("latest_event_type".to_string(), json!(event.event_type));
    timeline.insert("session_id".to_string(), json!(event.session_id));
    timeline.insert("updated_at".to_string(), json!(event.created_at));

    Value::Object(timeline)
}

pub(super) fn bonding_projection_metadata(
    existing: Option<&MemoryEntry>,
    event: &TachiEventRecord,
    payload: &Value,
    hit: i64,
    hit_delta: i64,
) -> Value {
    let payload_metadata = payload
        .get("metadata")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let existing_lexicon = existing
        .and_then(|entry| entry.metadata.get("lexicon"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut lexicon = existing_lexicon.clone();

    insert_if_some(
        &mut lexicon,
        "origin_session",
        string_from_payload_or_metadata(payload, &payload_metadata, &["origin_session"])
            .or_else(|| Some(event.session_id.clone())),
    );
    insert_if_some(
        &mut lexicon,
        "origin_context",
        string_from_payload_or_metadata(
            payload,
            &payload_metadata,
            &["origin_context", "origin", "source_context"],
        ),
    );
    insert_if_some(
        &mut lexicon,
        "meaning",
        string_from_payload_or_metadata(payload, &payload_metadata, &["meaning", "definition"])
            .or_else(|| string_value(payload, &["summary", "title"]).map(str::to_string)),
    );

    for (field, keys) in [
        (
            "shorthand_triggers",
            &["shorthand_triggers", "triggers", "keys"][..],
        ),
        (
            "appropriate_contexts",
            &["appropriate_contexts", "use_when", "contexts"][..],
        ),
        (
            "inappropriate_contexts",
            &["inappropriate_contexts", "avoid_when", "countercontexts"][..],
        ),
    ] {
        let incoming = array_value_from_payload_or_metadata(payload, &payload_metadata, keys);
        let merged = merge_array_values(existing_lexicon.get(field), incoming);
        lexicon.insert(field.to_string(), merged);
    }

    lexicon.insert("callback_hits".to_string(), json!(hit));
    if let Some(last_successful_use) = string_from_payload_or_metadata(
        payload,
        &payload_metadata,
        &["last_successful_use", "last_success_at"],
    ) {
        lexicon.insert(
            "last_successful_use".to_string(),
            json!(last_successful_use),
        );
    } else if hit_delta > 0 {
        lexicon.insert("last_successful_use".to_string(), json!(event.created_at));
    }
    lexicon.insert("updated_at".to_string(), json!(event.created_at));

    Value::Object(lexicon)
}

pub(super) fn bonding_context_json(entry: &MemoryEntry) -> Value {
    let lexicon = entry
        .metadata
        .get("lexicon")
        .cloned()
        .unwrap_or_else(|| json!({}));
    json!({
        "id": entry.id,
        "path": entry.path,
        "summary": entry.summary,
        "content": entry.text,
        "schema": schema_marker("SharedLexicon", validate_lexicon(Some(&lexicon))),
        "pattern_ref": pattern_ref_json(entry),
        "projection_key": entry.metadata.get("projection_key").cloned().unwrap_or(Value::Null),
        "lexicon": lexicon,
        "counters": entry.metadata.get("counters").cloned().unwrap_or_else(|| json!({})),
        "source_event_id": entry.metadata.get("source_event_id").cloned().unwrap_or(Value::Null),
        "projected_event_ids": entry.metadata.get("projected_event_ids").cloned().unwrap_or_else(|| json!([])),
    })
}

pub(super) fn timeline_context_json(entry: &MemoryEntry) -> Value {
    let timeline = entry
        .metadata
        .get("timeline")
        .cloned()
        .unwrap_or_else(|| json!({}));
    json!({
        "id": entry.id,
        "path": entry.path,
        "summary": entry.summary,
        "content": entry.text,
        "schema": schema_marker("TimelineEntry", validate_timeline(Some(&timeline))),
        "projection_key": entry.metadata.get("projection_key").cloned().unwrap_or(Value::Null),
        "timeline": timeline,
        "source_event_id": entry.metadata.get("source_event_id").cloned().unwrap_or(Value::Null),
        "projected_event_ids": entry.metadata.get("projected_event_ids").cloned().unwrap_or_else(|| json!([])),
    })
}

fn contains_cjk(value: &str) -> bool {
    value.chars().any(|ch| {
        matches!(
            ch as u32,
            0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0x3040..=0x30FF | 0xAC00..=0xD7AF
        )
    })
}

fn contains_ascii_word(value: &str) -> bool {
    value.chars().any(|ch| ch.is_ascii_alphabetic())
}

fn number_from_payload_or_metadata(
    payload: &Value,
    metadata: &Value,
    keys: &[&str],
) -> Option<f64> {
    keys.iter().find_map(|key| {
        payload
            .get(*key)
            .or_else(|| metadata.get(*key))
            .and_then(Value::as_f64)
    })
}

fn array_strings_from_payload_or_metadata(
    payload: &Value,
    metadata: &Value,
    keys: &[&str],
) -> Vec<String> {
    let value = value_from_payload_or_metadata(payload, metadata, keys);
    match value {
        Some(Value::Array(items)) => items
            .into_iter()
            .filter_map(|item| item.as_str().map(str::trim).map(str::to_string))
            .filter(|item| !item.is_empty())
            .collect(),
        Some(Value::String(value)) if !value.trim().is_empty() => vec![value.trim().to_string()],
        _ => Vec::new(),
    }
}

fn known_affect_markers(text: &str) -> Vec<String> {
    let lower = text.to_ascii_lowercase();
    let mut markers = Vec::new();
    for (needle, marker) in [
        ("fomo", "fomo"),
        ("panic", "panic"),
        ("urgent", "urgency"),
        ("hurry", "urgency"),
        ("stuck", "stuck"),
        ("blocked", "blocked"),
        ("焦虑", "anxiety"),
        ("急", "urgency"),
        ("卡住", "stuck"),
        ("崩", "failure-pressure"),
    ] {
        if lower.contains(needle) || text.contains(needle) {
            let marker = marker.to_string();
            if !markers.contains(&marker) {
                markers.push(marker);
            }
        }
    }
    markers
}

pub(super) fn affect_projection_metadata(event: &TachiEventRecord, payload: &Value) -> Value {
    let payload_metadata = payload
        .get("metadata")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let text = string_from_payload_or_metadata(
        payload,
        &payload_metadata,
        &["text", "content", "summary", "state", "message"],
    )
    .unwrap_or_default();
    let input_chars =
        number_from_payload_or_metadata(payload, &payload_metadata, &["input_chars", "input_len"]);
    let output_chars = number_from_payload_or_metadata(
        payload,
        &payload_metadata,
        &["output_chars", "output_len"],
    );
    let io_ratio = match (input_chars, output_chars) {
        (Some(input), Some(output)) if input > 0.0 => Some(output / input),
        _ => None,
    };
    let mut markers =
        array_strings_from_payload_or_metadata(payload, &payload_metadata, &["markers", "signals"]);
    for marker in known_affect_markers(&text) {
        if !markers.contains(&marker) {
            markers.push(marker);
        }
    }
    let language_switch = payload
        .get("language_switch")
        .or_else(|| payload_metadata.get("language_switch"))
        .and_then(Value::as_bool)
        .unwrap_or_else(|| contains_cjk(&text) && contains_ascii_word(&text));

    json!({
        "schema": schema_marker("AffectSignal", Vec::new()),
        "source_event_id": event.id,
        "guardrails": {
            "authority": "tone_and_reminder_only",
            "live_effect": "tone_only",
            "score_effect": "none",
            "iron_effect": "none",
            "execution_effect": "none",
            "portfolio_effect": "none",
        },
        "signals": {
            "language_switch": language_switch,
            "known_markers": markers,
            "input_chars": input_chars,
            "output_chars": output_chars,
            "io_ratio": io_ratio,
        },
    })
}

fn timeline_open_threads(timeline: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for row in timeline {
        let Some(threads) = row
            .get("timeline")
            .and_then(|value| value.get("open_threads"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for thread in threads {
            if thread.is_null() {
                continue;
            }
            out.push(json!({
                "timeline_id": row.get("id").cloned().unwrap_or(Value::Null),
                "projection_key": row.get("projection_key").cloned().unwrap_or(Value::Null),
                "thread": thread,
            }));
        }
    }
    out
}

fn compact_event_ref(event: &TachiEventRecord) -> Value {
    json!({
        "id": event.id,
        "event_type": event.event_type,
        "source_repo": event.source_repo,
        "adapter": event.adapter,
        "project": event.project,
        "domain": event.domain,
        "session_id": event.session_id,
        "actor": event.actor,
        "authority": event.authority.as_str(),
        "projection_hints": event.projection_hints.iter().map(|kind| kind.as_str()).collect::<Vec<_>>(),
        "created_at": event.created_at,
    })
}

pub(super) fn a2a_context_bundle(
    pattern_refs: &[Value],
    bonding: &[Value],
    timeline: &[Value],
    events: &[TachiEventRecord],
) -> Value {
    let bonding_refs = bonding
        .iter()
        .map(|row| {
            json!({
                "id": row.get("id").cloned().unwrap_or(Value::Null),
                "projection_key": row.get("projection_key").cloned().unwrap_or(Value::Null),
                "summary": row.get("summary").cloned().unwrap_or(Value::Null),
                "pattern_ref": row.get("pattern_ref").cloned().unwrap_or(Value::Null),
            })
        })
        .collect::<Vec<_>>();
    json!({
        "status": "ready",
        "mode": "read_only_local_bundle",
        "transport": "not_configured",
        "subscription": {
            "available": true,
            "mode": "poll_context",
            "cursor_field": "event_refs[].created_at",
            "write_authority": false
        },
        "share_policy": {
            "allow": [
                "event_refs",
                "pattern_refs",
                "timeline.open_threads",
                "bonding.protocol_refs"
            ],
            "deny": [
                "unreviewed_conclusions",
                "execution_authority",
                "scoring_authority",
                "memory_mutation"
            ]
        },
        "cold_seat": {
            "ingest_conclusions": false,
            "must_revalidate": true,
            "may_read": ["event_refs", "pattern_refs", "open_threads", "protocol_refs"]
        },
        "pattern_refs": pattern_refs,
        "bonding_refs": bonding_refs,
        "open_threads": timeline_open_threads(timeline),
        "event_refs": events.iter().map(compact_event_ref).collect::<Vec<_>>(),
    })
}

pub(super) fn host_lifecycle_contract() -> Value {
    json!({
        "schema": schema_marker("HostContinuityLifecycle", Vec::new()),
        "status": "v1_contract",
        "hosts": ["codex", "claude", "gemini", "cursor", "openclaw", "opencode"],
        "source_of_truth": {
            "memory": "tachi_events + memory DB",
            "project_cycle": ".tachi/runs/<flow_id>/",
            "verification": ".tachi/runs/<flow_id>/verification.json",
            "host_state": "adapter-local only; not authoritative"
        },
        "event_envelope": {
            "required": ["event_id", "event_type", "host", "adapter", "project", "created_at"],
            "recommended": ["cwd", "flow_id", "session_id", "turn_id", "refs", "payload"],
            "event_type_prefix": "host."
        },
        "steps": [
            {
                "phase": "before_session",
                "tool": "tachi_status + tachi_memory.briefing or tachi_event.context",
                "writes": false,
                "purpose": "load runtime identity, project briefing, active flow hints, and profile guidance"
            },
            {
                "phase": "before_prompt",
                "tool": "tachi_event.context + tachi_task.cycle_status when flow_id/issue_ref/pr_ref exists",
                "writes": false,
                "purpose": "attach compact memory, linked docs/specs, unresolved criteria, and host instruction packet"
            },
            {
                "phase": "after_tool",
                "tool": "host-local tool summary + optional tachi_event emit",
                "writes": "conditional",
                "purpose": "record safe tool facts and run post-edit feedback providers without writing ordinary memory by default"
            },
            {
                "phase": "after_compact",
                "tool": "checkpoint / compact session record",
                "writes": "conditional",
                "purpose": "preserve active flow, dispatch refs, open criteria, and next-step directive after host compaction"
            },
            {
                "phase": "before_stop",
                "tool": "cycle_status + verification ledger read model",
                "writes": false,
                "purpose": "allow stop or return one bounded continuation directive when required criteria remain"
            },
            {
                "phase": "after_session",
                "tool": "capture_session / memory.saved / session.outcome / close_loop",
                "writes": "conditional",
                "purpose": "capture outcome, verification refs, issue/PR/docs refs, memory/wiki candidates, and subagent eval rows"
            }
        ],
        "guardrails": {
            "subagents": "suppress private user_model unless explicitly allowed",
            "a2a": "share evidence and open questions; do not ingest conclusions",
            "agent_md": "static startup alignment only; fresh recall stays in MCP",
            "control_plane": "Tachi owns memory, project-cycle state, evidence, dispatch, and profile projection; hosts execute"
        }
    })
}
