use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("tachi-server lives under crates/tachi-server")
        .to_path_buf()
}

fn host_adapter_lifecycle_doc() -> String {
    fs::read_to_string(
        repo_root().join("docs/engineering/architecture/host-adapter-lifecycle-v1.md"),
    )
    .expect("read host adapter lifecycle doc")
}

#[test]
fn generic_chat_agent_memory_contract_stays_layered_and_portable() {
    let doc = host_adapter_lifecycle_doc();

    assert!(doc.contains("## Generic Chat-Agent Memory Adapter Contract"));
    assert!(doc.contains("### Context Assembly Input"));
    assert!(doc.contains("### Context Assembly Output"));
    assert!(doc.contains("### Memory Tool Surface"));
    assert!(doc.contains("### Event Projection"));
    assert!(doc.contains("### Policy Boundary"));
    assert!(doc.contains("This section is a target adapter contract"));

    for operation in [
        "`retain` / `save`",
        "`recall` / `search`",
        "`reflect` / `synthesize`",
        "`status` / `readiness`",
        "`read_by_id`",
        "`edit`",
    ] {
        assert!(
            doc.contains(operation),
            "generic chat-agent tool surface should include {operation}"
        );
    }
    assert!(doc.contains("current save path; `retain` is adapter vocabulary"));
    assert!(
        doc.contains("proposed/target | Allowed only where the kernel has reviewed edit semantics")
    );
    assert!(doc.contains("`memory.saved` current; request event proposed"));
    for proposed_event in [
        "memory.recall_requested",
        "memory.recall_returned",
        "memory.reflect_requested",
        "memory.reflection_returned",
    ] {
        assert!(
            doc.contains(proposed_event),
            "contract should name proposed event {proposed_event}"
        );
    }

    let stable = doc
        .find("Stable mental model first")
        .expect("stable mental model layer");
    let recall = doc
        .find("Immediate recall second")
        .expect("immediate recall layer");
    let reflection = doc.find("Reflection third").expect("reflection layer");
    assert!(
        stable < recall && recall < reflection,
        "context assembly must keep stable summaries before immediate recall and reflection"
    );

    assert!(
        doc.contains("The adapter must operate without Tachi GitHub, dispatch, ship, release, or")
    );
    assert!(doc.contains(
        "The surface deliberately excludes GitHub, dispatch, ship, release notes, worker"
    ));
    assert!(doc.contains(
        "RomanBath may be a fixture or\nexample consumer, but no field, policy, or prompt shape is RomanBath-specific."
    ));
    assert!(doc
        .contains("Persona, character-card behavior, tone, roleplay style, safety narration, and"));
    assert!(doc.contains(
        "The portable kernel owns durable\nmemory schema, recall primitives, provenance, access history, and continuity"
    ));
}
