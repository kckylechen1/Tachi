use super::*;
use crate::db::dispatch_adjudications::{
    append_dispatch_adjudication, append_dispatch_adjudication_correction, correction_event_key,
    list_adjudications_for_outcome, DispatchAdjudicationSignature, NewDispatchAdjudication,
};
use crate::db::mirror_eval::{
    append_mirror_eval_adjudication, list_adjudications_for_run, register_mirror_eval_run,
    NewMirrorEvalAdjudication, NewMirrorEvalRun,
};

fn open_conn() -> Connection {
    crate::db::enable_simple_auto_extension().unwrap();
    crate::db::register_sqlite_vec();
    let conn = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&conn).unwrap();
    for outcome_id in ["outcome-1", "outcome-2"] {
        crate::db::dispatch_outcomes::upsert_outcome(
            &conn,
            &crate::db::dispatch_outcomes::NewDispatchOutcome {
                outcome_id: outcome_id.to_string(),
                dispatch_id: format!("dispatch-{outcome_id}"),
                execution_outcome: "completed".to_string(),
                vendor: "codex".to_string(),
                ..Default::default()
            },
        )
        .unwrap();
    }
    conn
}

fn receipt(
    channel: AdjudicationChannel,
    actor: &str,
    model: Option<&str>,
    captured_at: &str,
) -> AdjudicationReceipt {
    AdjudicationReceipt {
        subject: AdjudicationReceiptSubject {
            actor: actor.to_string(),
            model: model.map(str::to_string),
        },
        channel,
        binding: Some(serde_json::json!({"connection_id": format!("conn-{captured_at}")})),
        captured_at: captured_at.to_string(),
    }
}

fn event(key: &str, receipt: Option<AdjudicationReceipt>) -> NewDispatchAdjudication {
    NewDispatchAdjudication {
        adjudication_id: format!("adj-{key}"),
        outcome_id: "outcome-1".to_string(),
        event_key: key.to_string(),
        verdict: Some("accepted".to_string()),
        not_required_reason: None,
        actor: "leader".to_string(),
        evidence_ref: "review-1".to_string(),
        signatures: vec![DispatchAdjudicationSignature {
            signature_id: "fake_security_fix".to_string(),
            evidence_ref: None,
            resolved: true,
        }],
        adjudication_receipt: receipt,
    }
}

#[test]
fn dispatch_append_lands_receipt_and_graded_basis_per_channel() {
    let conn = open_conn();
    for (key, channel, actor, basis) in [
        (
            "verified",
            AdjudicationChannel::VerifiedAdmission,
            "leader",
            "acknowledged_overlay",
        ),
        (
            "self",
            AdjudicationChannel::SelfAssertedLocal,
            "leader",
            "planned_unconfirmed",
        ),
        (
            "unverified",
            AdjudicationChannel::UnverifiedCaller,
            "leader",
            "unknown",
        ),
        (
            "mismatch",
            AdjudicationChannel::VerifiedAdmission,
            "someone-else",
            "unknown",
        ),
    ] {
        let row = append_dispatch_adjudication(
            &conn,
            &event(key, Some(receipt(channel, actor, None, "t1"))),
        )
        .unwrap();
        assert_eq!(row.adjudication_attribution_basis, basis, "{key}");
        let stored =
            AdjudicationReceipt::parse_stored(row.adjudication_receipt.as_deref().unwrap())
                .unwrap();
        assert_eq!(stored.channel, channel, "{key}: receipt landed verbatim");
    }
    let row = append_dispatch_adjudication(&conn, &event("none", None)).unwrap();
    assert_eq!(row.adjudication_receipt, None);
    assert_eq!(row.adjudication_attribution_basis, "unknown");
}

#[test]
fn transport_retry_ignores_captured_at_and_binding_and_keeps_first_receipt() {
    let conn = open_conn();
    let first = append_dispatch_adjudication(
        &conn,
        &event(
            "retry",
            Some(receipt(
                AdjudicationChannel::VerifiedAdmission,
                "leader",
                Some("m"),
                "t1",
            )),
        ),
    )
    .unwrap();
    let mut retry = event(
        "retry",
        Some(receipt(
            AdjudicationChannel::VerifiedAdmission,
            "leader",
            Some("m"),
            "t2",
        )),
    );
    retry.adjudication_id = "adj-retry-transport".to_string();
    let replay = append_dispatch_adjudication(&conn, &retry).unwrap();
    assert_eq!(replay, first, "retry returns the original row");
    assert!(first
        .adjudication_receipt
        .as_deref()
        .unwrap()
        .contains("\"t1\""));
    assert_eq!(
        list_adjudications_for_outcome(&conn, "outcome-1")
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn same_key_provenance_change_conflicts_and_never_upgrades_in_place() {
    type Mutation = fn(&mut NewDispatchAdjudication);
    let cases: [(&str, Mutation); 4] = [
        ("channel upgrade", |e| {
            e.adjudication_receipt.as_mut().unwrap().channel =
                AdjudicationChannel::VerifiedAdmission
        }),
        ("receipt dropped", |e| e.adjudication_receipt = None),
        ("model changed", |e| {
            e.adjudication_receipt.as_mut().unwrap().subject.model = Some("other".to_string())
        }),
        ("subject actor changed", |e| {
            e.adjudication_receipt.as_mut().unwrap().subject.actor = "other".to_string()
        }),
    ];
    for (case, mutate) in cases {
        let conn = open_conn();
        let original = event(
            "upgrade",
            Some(receipt(
                AdjudicationChannel::UnverifiedCaller,
                "leader",
                Some("m"),
                "t1",
            )),
        );
        let first = append_dispatch_adjudication(&conn, &original).unwrap();
        let mut replay = original.clone();
        mutate(&mut replay);
        let error = append_dispatch_adjudication(&conn, &replay).expect_err(case);
        assert!(
            error.to_string().contains("payload mismatch"),
            "{case}: {error}"
        );
        assert_eq!(
            list_adjudications_for_outcome(&conn, "outcome-1").unwrap(),
            vec![first.clone()],
            "{case}"
        );
    }

    // A receipt-less legacy event cannot be upgraded by a later receipt either.
    let conn = open_conn();
    let first = append_dispatch_adjudication(&conn, &event("legacy", None)).unwrap();
    let upgraded = event(
        "legacy",
        Some(receipt(
            AdjudicationChannel::VerifiedAdmission,
            "leader",
            None,
            "t9",
        )),
    );
    assert!(append_dispatch_adjudication(&conn, &upgraded).is_err());
    assert_eq!(
        list_adjudications_for_outcome(&conn, "outcome-1").unwrap(),
        vec![first]
    );
}

#[test]
fn explicit_correction_chain_appends_under_server_generated_keys() {
    let conn = open_conn();
    let original = append_dispatch_adjudication(
        &conn,
        &event(
            "complete:outcome-1",
            Some(receipt(
                AdjudicationChannel::UnverifiedCaller,
                "leader",
                None,
                "t1",
            )),
        ),
    )
    .unwrap();

    let mut correction = event(
        "",
        Some(receipt(
            AdjudicationChannel::VerifiedAdmission,
            "leader",
            None,
            "t2",
        )),
    );
    correction.adjudication_id = "adj-correction-1".to_string();
    let first =
        append_dispatch_adjudication_correction(&conn, &original.event_key, &correction).unwrap();
    assert_eq!(first.event_key, "correction:outcome-1:complete:outcome-1");
    assert_eq!(
        first.event_key,
        correction_event_key("outcome-1", &original.event_key)
    );
    assert_eq!(first.adjudication_attribution_basis, "acknowledged_overlay");
    assert_eq!(first.insertion_seq, 2);

    // Replay of the same correction (fresh capture) returns the original row.
    let mut retry = correction.clone();
    retry.adjudication_id = "adj-correction-1-retry".to_string();
    retry.adjudication_receipt.as_mut().unwrap().captured_at = "t3".to_string();
    assert_eq!(
        append_dispatch_adjudication_correction(&conn, &original.event_key, &retry).unwrap(),
        first
    );

    // Different content under the same correction key conflicts.
    let mut conflicting = correction.clone();
    conflicting.verdict = Some("rejected".to_string());
    let error = append_dispatch_adjudication_correction(&conn, &original.event_key, &conflicting)
        .expect_err("same correction key, different content");
    assert!(error.to_string().contains("payload mismatch"), "{error}");

    // Chained correction cites the immediately-prior correction.
    let mut chained = event(
        "",
        Some(receipt(
            AdjudicationChannel::VerifiedAdmission,
            "leader",
            None,
            "t4",
        )),
    );
    chained.adjudication_id = "adj-correction-2".to_string();
    chained.verdict = Some("rejected".to_string());
    let second =
        append_dispatch_adjudication_correction(&conn, &first.event_key, &chained).unwrap();
    assert_eq!(
        second.event_key,
        "correction:outcome-1:correction:outcome-1:complete:outcome-1"
    );

    for (case, prior, outcome, key) in [
        ("unknown prior", "never-recorded", "outcome-1", ""),
        (
            "prior on another outcome",
            original.event_key.as_str(),
            "outcome-2",
            "",
        ),
        (
            "caller-authored key",
            original.event_key.as_str(),
            "outcome-1",
            "my-own-key",
        ),
        ("empty prior", " ", "outcome-1", ""),
    ] {
        let mut bad = event(key, None);
        bad.adjudication_id = format!("adj-bad-{case}");
        bad.outcome_id = outcome.to_string();
        assert!(
            append_dispatch_adjudication_correction(&conn, prior, &bad).is_err(),
            "{case}"
        );
    }
    // The plain entry never mints (or auto-converts into) a correction.
    let error = append_dispatch_adjudication(&conn, &event("correction:outcome-1:forged", None))
        .expect_err("reserved prefix");
    assert!(error.to_string().contains("reserved"), "{error}");
    let same_key_conflict = event("complete:outcome-1", None);
    assert!(append_dispatch_adjudication(&conn, &same_key_conflict).is_err());

    let history = list_adjudications_for_outcome(&conn, "outcome-1").unwrap();
    assert_eq!(
        history
            .iter()
            .map(|row| row.event_key.as_str())
            .collect::<Vec<_>>(),
        vec![
            original.event_key.as_str(),
            first.event_key.as_str(),
            second.event_key.as_str()
        ]
    );
}

fn mirror_event(
    key: &str,
    run: &str,
    receipt: Option<AdjudicationReceipt>,
) -> NewMirrorEvalAdjudication {
    NewMirrorEvalAdjudication {
        adjudication_id: format!("adj-{key}"),
        eval_run_id: run.to_string(),
        event_key: key.to_string(),
        actor: "leader".to_string(),
        verifier_model: Some("openai/gpt-5".to_string()),
        usefulness: "useful".to_string(),
        evidence_usable: true,
        evidence_ref: "run-1".to_string(),
        adjudication_receipt: receipt,
        ..Default::default()
    }
}

#[test]
fn mirror_append_binds_actor_and_model_and_keeps_replay_contract() {
    let conn = open_conn();
    let run = register_mirror_eval_run(
        &conn,
        &NewMirrorEvalRun {
            frozen_contract_ref: "kckylechen1/tachi#2012".to_string(),
            execution_origin: "host_native_subagent".to_string(),
            lifecycle_owner: "host".to_string(),
            native_child_id: Some("native-provenance".to_string()),
            ..Default::default()
        },
    )
    .unwrap()
    .eval_run_id;

    let bound = receipt(
        AdjudicationChannel::VerifiedAdmission,
        "leader",
        Some("openai/gpt-5"),
        "t1",
    );
    let first =
        append_mirror_eval_adjudication(&conn, &mirror_event("m1", &run, Some(bound.clone())))
            .unwrap();
    assert_eq!(first.adjudication_attribution_basis, "acknowledged_overlay");

    let mut retry = mirror_event(
        "m1",
        &run,
        Some(AdjudicationReceipt {
            captured_at: "t2".into(),
            binding: None,
            ..bound.clone()
        }),
    );
    retry.adjudication_id = "adj-m1-retry".to_string();
    assert_eq!(
        append_mirror_eval_adjudication(&conn, &retry).unwrap(),
        first
    );

    let upgraded = mirror_event("m1", &run, None);
    let error = append_mirror_eval_adjudication(&conn, &upgraded).expect_err("receipt change");
    assert!(error.to_string().contains("payload mismatch"), "{error}");

    let other_model = receipt(
        AdjudicationChannel::VerifiedAdmission,
        "leader",
        Some("anthropic/claude"),
        "t1",
    );
    let degraded =
        append_mirror_eval_adjudication(&conn, &mirror_event("m2", &run, Some(other_model)))
            .unwrap();
    assert_eq!(degraded.adjudication_attribution_basis, "unknown");
    let none = append_mirror_eval_adjudication(&conn, &mirror_event("m3", &run, None)).unwrap();
    assert_eq!(none.adjudication_attribution_basis, "unknown");
    assert_eq!(list_adjudications_for_run(&conn, &run).unwrap().len(), 3);
}

#[test]
fn malformed_typed_receipt_fails_deterministically_on_both_first_write_and_retry() {
    let conn = open_conn();
    let run = register_mirror_eval_run(
        &conn,
        &NewMirrorEvalRun {
            frozen_contract_ref: "kckylechen1/tachi#2012".to_string(),
            execution_origin: "host_native_subagent".to_string(),
            lifecycle_owner: "host".to_string(),
            native_child_id: Some("native-malformed".to_string()),
            ..Default::default()
        },
    )
    .unwrap()
    .eval_run_id;

    for (case, bad) in [
        (
            "empty captured_at",
            receipt(AdjudicationChannel::VerifiedAdmission, "leader", None, ""),
        ),
        (
            "blank actor",
            receipt(AdjudicationChannel::VerifiedAdmission, " ", None, "t1"),
        ),
        (
            "blank model",
            receipt(
                AdjudicationChannel::VerifiedAdmission,
                "leader",
                Some(" "),
                "t1",
            ),
        ),
    ] {
        let dispatch_ev = event(&format!("bad-d-{case}"), Some(bad.clone()));
        let first_err = append_dispatch_adjudication(&conn, &dispatch_ev)
            .expect_err(case)
            .to_string();
        let retry_err = append_dispatch_adjudication(&conn, &dispatch_ev)
            .expect_err(case)
            .to_string();
        assert_eq!(first_err, retry_err, "dispatch {case}: deterministic error");

        let mirror_ev = mirror_event(&format!("bad-m-{case}"), &run, Some(bad));
        let first_err = append_mirror_eval_adjudication(&conn, &mirror_ev)
            .expect_err(case)
            .to_string();
        let retry_err = append_mirror_eval_adjudication(&conn, &mirror_ev)
            .expect_err(case)
            .to_string();
        assert_eq!(first_err, retry_err, "mirror {case}: deterministic error");
    }

    assert!(list_adjudications_for_outcome(&conn, "outcome-1")
        .unwrap()
        .is_empty());
    assert!(list_adjudications_for_run(&conn, &run).unwrap().is_empty());
}
