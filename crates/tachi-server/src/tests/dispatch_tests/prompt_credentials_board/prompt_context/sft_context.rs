use super::*;

#[tokio::test]
async fn staff_prompt_excludes_synthetic_recall_and_cards_but_keeps_real_avoidance() {
    let (server, _home) = make_server_with_temp_home();
    server
        .with_global_store(|store| {
            for (id, path, marker, text) in [
                (
                    "synthetic-staff-scratch",
                    "/scratch/tachi-test/staff",
                    false,
                    "SYNTHETIC_SCRATCH_POISON",
                ),
                (
                    "synthetic-staff-notes",
                    "/notes/staff-fixture",
                    true,
                    "SYNTHETIC_NOTES_POISON",
                ),
                (
                    "synthetic-staff-sft",
                    "/sft/fixture/staff",
                    true,
                    "SYNTHETIC_SFT_POISON",
                ),
                (
                    "synthetic-staff-eval",
                    "/eval/fixture/staff",
                    true,
                    "SYNTHETIC_EVAL_POISON",
                ),
                (
                    "real-staff-eval",
                    "/eval/production/lease",
                    false,
                    "REAL_AVOIDANCE_LESSON preserve lease ownership before retrying",
                ),
            ] {
                let mut entry = make_entry(id);
                entry.path = path.to_string();
                entry.text =
                    format!("Fix dispatch routing regression failure partial watchdog: {text}");
                entry.summary = entry.text.clone();
                entry.metadata = json!({"synthetic_fixture": marker});
                store.upsert(&entry).map_err(|error| error.to_string())?;
            }
            for (id, synthetic, clause) in [
                ("synthetic-staff-card", true, "SYNTHETIC_CARD_POISON"),
                (
                    "real-staff-card",
                    false,
                    "REAL_SEAT_CLAUSE verify the reviewed diff",
                ),
            ] {
                let mut entry = make_entry(id);
                entry.path = "/cards/codex".to_string();
                entry.category = "wiki".to_string();
                entry.metadata = json!({
                    "synthetic_fixture": synthetic, "source": "dispatch-ledger",
                    "source_file": "codex.md", "content_hash": id, "authority": "advisory",
                    "counter_clauses_present": true, "counter_clauses": clause
                });
                store.upsert(&entry).map_err(|error| error.to_string())?;
            }
            Ok(())
        })
        .unwrap();

    let prompt = crate::dispatch_ops::assemble_prompt(
        &server,
        &dispatch_params(Some("codex"), "Fix dispatch routing regression"),
    )
    .await;
    assert!(!prompt.contains("SYNTHETIC_"), "{prompt}");
    assert!(prompt.contains("REAL_AVOIDANCE_LESSON"), "{prompt}");
    assert!(
        prompt.contains("## Prior pitfalls / avoidance notes"),
        "{prompt}"
    );
    assert!(prompt.contains("REAL_SEAT_CLAUSE"), "{prompt}");
    let exact = crate::dispatch_ops::resolve_exact_seat_card_readiness(&server, "codex")
        .expect("synthetic sibling must not make a real seat ambiguous");
    assert!(exact.counter_clauses.contains("REAL_SEAT_CLAUSE"));
}

#[tokio::test]
async fn dispatch_prompt_injects_sft_examples_as_style_only_context() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut live = make_entry("live-dispatch-memory");
            live.path = "/scratch/sigil/live-dispatch".to_string();
            live.text = "fix dispatch routing regression current implementation fact.".to_string();
            live.summary = "Current dispatch fact".to_string();
            store.upsert(&live).map_err(|e| e.to_string())?;

            let mut sft = make_entry("sft-dispatch-example");
            sft.path = "/sft/v4/strict/engineering/42".to_string();
            sft.text = "[结论] fix dispatch routing regression historical answer shape.\n[根因] Historical root cause.\n[方案] Historical proposal.\n[反方案] Historical anti-pattern.\n[验证] Historical verification.".to_string();
            sft.summary = "SFT dispatch example".to_string();
            sft.topic = "sft-memory".to_string();
            sft.importance = 1.0;
            store.upsert(&sft).map_err(|e| e.to_string())
        })
        .expect("seed dispatch SFT example");

    let prompt = crate::dispatch_ops::assemble_prompt(
        &server,
        &dispatch_params(Some("codex"), "Fix dispatch routing regression"),
    )
    .await;

    assert!(
        prompt.contains("## Relevant context from Tachi memory/wiki"),
        "{prompt}"
    );
    assert!(
        prompt.contains("/scratch/sigil/live-dispatch")
            && prompt.contains("current implementation fact"),
        "live memory should still be normal context: {prompt}"
    );
    assert!(
        prompt.contains("## SFT gold examples (style only, not live facts)"),
        "{prompt}"
    );
    assert!(
        prompt.contains("/sft/v4/strict/engineering/42"),
        "SFT example should be isolated under the SFT section: {prompt}"
    );
    assert!(
        prompt.contains("Do not treat historical SFT samples as current project truth"),
        "{prompt}"
    );
}
