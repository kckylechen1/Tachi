use super::*;
use crate::{AgentIdentity, MemoryStore};

const PROJECT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OTHER: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn seed(store: &MemoryStore) {
    for id in ["sender", "recipient", "foreign"] {
        crate::insert_agent_identity(
            store.connection(),
            &AgentIdentity {
                agent_identity_id: id.into(),
                display_name: None,
                seat: None,
                capability_json: None,
                created_at: "2026-11-11T00:00:00Z".into(),
            },
        )
        .unwrap();
        let project = if id == "foreign" { OTHER } else { PROJECT };
        record_a2a_project_admission(
            store.connection(),
            &format!("admission-{id}"),
            id,
            &format!("connection-{id}"),
            project,
        )
        .unwrap();
    }
    record_a2a_project_admission(
        store.connection(),
        "other-recipient",
        "recipient",
        "other-recipient",
        OTHER,
    )
    .unwrap();
}

fn request() -> NewA2aEnvelope {
    NewA2aEnvelope {
        envelope_id: "coordination-1".into(),
        issuer_agent_identity_id: "sender".into(),
        issuer_connection_id: "connection-sender".into(),
        recipient_agent_identity_id: "recipient".into(),
        subject_ref: "coordination:release-2.1".into(),
        body: "Candidate is ready for review".into(),
        idempotency_key: "release-1".into(),
        created_at: "2026-11-11T00:00:00Z".into(),
        expires_at: "2026-11-12T00:00:00Z".into(),
    }
}

fn actor(connection: &str) -> A2aTransitionActor {
    A2aTransitionActor {
        agent_identity_id: "recipient".into(),
        connection_id: connection.into(),
    }
}

#[test]
fn project_send_replays_after_consumption_and_scrub_without_cross_project_disclosure() {
    let mut store = MemoryStore::open_in_memory().unwrap();
    seed(&store);
    let req = request();
    assert!(matches!(
        insert_a2a_coordination(store.connection_mut(), &req, PROJECT).unwrap(),
        A2aInsertOutcome::Created { .. }
    ));
    assert!(matches!(
        insert_a2a_coordination(store.connection_mut(), &req, PROJECT).unwrap(),
        A2aInsertOutcome::Replay { .. }
    ));
    assert!(list_a2a_status(store.connection(), "recipient", 10)
        .unwrap()
        .is_empty());
    let rows = list_a2a_project_status(
        store.connection(),
        &actor("connection-recipient"),
        PROJECT,
        10,
    )
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].current_state, "received");
    assert!(!serde_json::to_string(&rows).unwrap().contains(&req.body));
    assert!(
        list_a2a_project_status(store.connection(), &actor("other-recipient"), OTHER, 10)
            .unwrap()
            .is_empty()
    );
    assert!(consume_a2a_for_project(
        store.connection_mut(),
        &actor("other-recipient"),
        OTHER,
        10,
        "2026-11-11T01:00:00Z"
    )
    .unwrap()
    .is_empty());
    assert!(consume_a2a_for_recipient(
        store.connection_mut(),
        &actor("connection-recipient"),
        10,
        "2026-11-11T01:00:00Z"
    )
    .unwrap()
    .is_empty());
    let delivered = consume_a2a_for_project(
        store.connection_mut(),
        &actor("connection-recipient"),
        PROJECT,
        10,
        "2026-11-11T01:00:00Z",
    )
    .unwrap();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].body.as_deref(), Some(req.body.as_str()));
    assert!(consume_a2a_for_project(
        store.connection_mut(),
        &actor("connection-recipient"),
        PROJECT,
        10,
        "2026-11-11T01:00:00Z"
    )
    .unwrap()
    .is_empty());
    store
        .connection()
        .execute(
            "UPDATE a2a_envelopes SET body=NULL WHERE envelope_id=?1",
            [&req.envelope_id],
        )
        .unwrap();
    let replay = insert_a2a_coordination(store.connection_mut(), &req, PROJECT).unwrap();
    assert!(
        matches!(replay, A2aInsertOutcome::Replay { envelope, .. } if envelope.current_state == "consumed" && envelope.body.is_none())
    );
    for changed in [
        NewA2aEnvelope {
            body: "Changed input".into(),
            ..req.clone()
        },
        NewA2aEnvelope {
            expires_at: "2026-11-13T00:00:00Z".into(),
            ..req.clone()
        },
    ] {
        assert!(matches!(
            insert_a2a_coordination(store.connection_mut(), &changed, PROJECT),
            Err(MemoryError::Duplicate(_))
        ));
    }
    let rows = list_a2a_project_status(
        store.connection(),
        &actor("connection-recipient"),
        PROJECT,
        10,
    )
    .unwrap();
    assert_eq!(
        rows[0]
            .receipts
            .iter()
            .map(|r| r.state.as_str())
            .collect::<Vec<_>>(),
        ["received", "accepted", "consumed"]
    );
    let memories: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))
        .unwrap();
    assert_eq!(memories, 0);
}

#[test]
fn foreign_and_absent_recipients_are_indistinguishable_and_expiry_wins_at_deadline() {
    let mut store = MemoryStore::open_in_memory().unwrap();
    seed(&store);
    let mut req = request();
    let mut denials = Vec::new();
    for id in ["foreign", "absent"] {
        req.recipient_agent_identity_id = id.into();
        denials.push(
            insert_a2a_coordination(store.connection_mut(), &req, PROJECT)
                .unwrap_err()
                .to_string(),
        );
    }
    assert_eq!(denials[0], denials[1]);
    req.recipient_agent_identity_id = "recipient".into();
    insert_a2a_coordination(store.connection_mut(), &req, PROJECT).unwrap();
    assert!(consume_a2a_for_project(
        store.connection_mut(),
        &actor("connection-recipient"),
        PROJECT,
        10,
        &req.expires_at
    )
    .unwrap()
    .is_empty());
    let status = list_a2a_project_status(
        store.connection(),
        &actor("connection-recipient"),
        PROJECT,
        10,
    )
    .unwrap();
    assert_eq!(status[0].current_state, "expired");
    assert_eq!(
        status[0]
            .receipts
            .iter()
            .map(|r| r.state.as_str())
            .collect::<Vec<_>>(),
        ["received", "expired"]
    );
    assert!(
        matches!(insert_a2a_coordination(store.connection_mut(), &req, PROJECT).unwrap(), A2aInsertOutcome::Replay { envelope, .. } if envelope.current_state == "expired")
    );
}

#[test]
fn competing_project_consumers_return_one_body_and_one_receipt_chain() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mailbox.db");
    {
        let mut store = MemoryStore::open(path.to_str().unwrap()).unwrap();
        seed(&store);
        insert_a2a_coordination(store.connection_mut(), &request(), PROJECT).unwrap();
    }
    let stores: Vec<_> = (0..4)
        .map(|_| MemoryStore::open(path.to_str().unwrap()).unwrap())
        .collect();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(stores.len()));
    let threads: Vec<_> = stores
        .into_iter()
        .map(|mut store| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                consume_a2a_for_project(
                    store.connection_mut(),
                    &actor("connection-recipient"),
                    PROJECT,
                    10,
                    "2026-11-11T01:00:00Z",
                )
                .unwrap()
                .len()
            })
        })
        .collect();
    assert_eq!(
        threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .sum::<usize>(),
        1
    );
    let store = MemoryStore::open(path.to_str().unwrap()).unwrap();
    let rows = list_a2a_project_status(
        store.connection(),
        &actor("connection-recipient"),
        PROJECT,
        10,
    )
    .unwrap();
    assert_eq!(rows[0].state_version, 3);
    assert_eq!(rows[0].receipts.len(), 3);
}
