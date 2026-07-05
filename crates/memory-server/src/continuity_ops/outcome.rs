use memory_core::{OutcomeEvidenceBasis, SessionOutcomeKind, TachiEventRecord};
use serde_json::{json, Value};

use crate::tool_params::TachiEventParams;
use crate::MemoryServer;
use memory_server_runtime::query_limit;

use super::storage::read_events;
use super::{event_query_from_params, target_from_event_params};

fn payload_string<'a>(payload: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| payload.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn review_target(review: &TachiEventRecord) -> Option<&str> {
    payload_string(
        &review.payload,
        &["target_event_id", "event_id", "label_event_id"],
    )
}

fn gold_outcome(review: &TachiEventRecord) -> SessionOutcomeKind {
    SessionOutcomeKind::from_str_opt(payload_string(
        &review.payload,
        &["gold_outcome", "outcome", "expected_outcome", "label"],
    ))
}

fn gold_basis(review: &TachiEventRecord) -> OutcomeEvidenceBasis {
    OutcomeEvidenceBasis::from_str_opt(payload_string(
        &review.payload,
        &[
            "gold_evidence_basis",
            "evidence_basis",
            "expected_evidence_basis",
            "basis",
        ],
    ))
}

pub(crate) fn evaluate_outcome_labels(
    server: &MemoryServer,
    params: &TachiEventParams,
) -> Result<Value, String> {
    let target = target_from_event_params(server, params);
    let limit = query_limit(params.limit);
    let mut outcome_query = event_query_from_params(params);
    outcome_query.event_type = Some("session.outcome".to_string());
    outcome_query.limit = limit;
    let outcomes = read_events(server, &target, &outcome_query)?;

    let mut review_query = event_query_from_params(params);
    review_query.event_type = Some("session.outcome.review".to_string());
    review_query.limit = limit;
    let reviews = read_events(server, &target, &review_query)?;

    let mut reviewed = 0usize;
    let mut outcome_matches = 0usize;
    let mut basis_matches = 0usize;
    let mut full_matches = 0usize;
    let mut missing_targets = 0usize;
    let mut rows = Vec::new();

    for review in reviews {
        let target_event_id = review_target(&review).map(str::to_string);
        let matched = target_event_id
            .as_deref()
            .and_then(|id| outcomes.iter().find(|event| event.id == id))
            .or_else(|| {
                outcomes.iter().find(|event| {
                    !review.session_id.is_empty() && event.session_id == review.session_id
                })
            });

        let Some(label_event) = matched else {
            missing_targets += 1;
            rows.push(json!({
                "review_event_id": review.id,
                "target_event_id": target_event_id,
                "session_id": review.session_id,
                "status": "missing_target",
            }));
            continue;
        };

        reviewed += 1;
        let expected_outcome = gold_outcome(&review);
        let expected_basis = gold_basis(&review);
        let actual_outcome = SessionOutcomeKind::from_str_opt(payload_string(
            &label_event.payload,
            &["outcome", "outcome_label", "label"],
        ));
        let actual_basis = OutcomeEvidenceBasis::from_str_opt(payload_string(
            &label_event.payload,
            &[
                "evidence_basis",
                "basis",
                "label_basis",
                "adversarial_basis",
            ],
        ));
        let outcome_match =
            expected_outcome != SessionOutcomeKind::Unknown && expected_outcome == actual_outcome;
        let basis_match =
            expected_basis != OutcomeEvidenceBasis::Unverified && expected_basis == actual_basis;
        if outcome_match {
            outcome_matches += 1;
        }
        if basis_match {
            basis_matches += 1;
        }
        if outcome_match && basis_match {
            full_matches += 1;
        }
        rows.push(json!({
            "review_event_id": review.id,
            "label_event_id": label_event.id,
            "session_id": label_event.session_id,
            "expected": {
                "outcome": expected_outcome.as_str(),
                "evidence_basis": expected_basis.as_str(),
            },
            "actual": {
                "outcome": actual_outcome.as_str(),
                "evidence_basis": actual_basis.as_str(),
            },
            "outcome_match": outcome_match,
            "basis_match": basis_match,
        }));
    }

    let ratio = |count: usize| {
        if reviewed > 0 {
            Some(count as f64 / reviewed as f64)
        } else {
            None
        }
    };

    Ok(json!({
        "status": "completed",
        "reviewed": reviewed,
        "missing_targets": missing_targets,
        "outcome_accuracy": ratio(outcome_matches),
        "evidence_basis_accuracy": ratio(basis_matches),
        "full_match_rate": ratio(full_matches),
        "rows": rows,
        "note": "Read-only label-quality harness. Reviews are session.outcome.review events with target_event_id or matching session_id; no labels or counters are mutated.",
    }))
}
