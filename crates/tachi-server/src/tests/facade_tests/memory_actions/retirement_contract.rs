use super::*;

const RETIRED_MEMORY_ACTIONS_AND_OWNERS: [(&str, &str); 8] = [
    ("progress", "tachi_task(action='status')"),
    ("readiness", "tachi_status"),
    ("delete", "tachi delete plan|apply"),
    ("gc", "tachi gc plan|apply"),
    ("doctor_scan", "tachi doctor"),
    ("ingest", "admitted adapter/operator ingest API"),
    ("ingest_source", "admitted adapter/operator ingest API"),
    ("pattern_feedback", "internal pattern-evidence API"),
];

const SURVIVING_MEMORY_ACTIONS: [&str; 9] = [
    "search",
    "get",
    "save",
    "briefing",
    "checkpoint",
    "alerts",
    "ask",
    "extract_facts",
    "consolidate",
];

#[tokio::test]
async fn retired_memory_router_calls_reject_with_canonical_owner_guidance() {
    let server = make_server();

    for (action, owner) in RETIRED_MEMORY_ACTIONS_AND_OWNERS {
        let error = server
            .tachi_memory(Parameters(tachi_memory_params(action)))
            .await
            .expect_err("retired Memory action must be rejected before its old handler runs");
        assert_eq!(
            error,
            format!("retired tachi_memory action '{action}'; use {owner}"),
            "retired action={action} must point at its canonical surviving owner",
        );
    }
}

#[tokio::test]
async fn exact_nine_surviving_memory_actions_remain_routed() {
    let server = make_server();

    for action in SURVIVING_MEMORY_ACTIONS {
        let result = server
            .tachi_memory(Parameters(tachi_memory_params(action)))
            .await;
        if let Err(error) = result {
            assert!(
                !error.contains("retired tachi_memory action") && !error.contains("Invalid action"),
                "surviving action={action} fell out of the production router: {error}",
            );
        }
    }
}

fn production_rust_sources() -> Vec<(std::path::PathBuf, String)> {
    fn visit(
        root: &std::path::Path,
        path: &std::path::Path,
        rows: &mut Vec<(std::path::PathBuf, String)>,
    ) {
        for entry in std::fs::read_dir(path).expect("read tachi-server source directory") {
            let path = entry.expect("read source entry").path();
            if path.is_dir() {
                if path.file_name().and_then(|name| name.to_str()) != Some("tests") {
                    visit(root, &path, rows);
                }
                continue;
            }
            if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
                rows.push((
                    path.strip_prefix(root)
                        .expect("source is beneath root")
                        .to_path_buf(),
                    std::fs::read_to_string(&path).expect("read Rust source"),
                ));
            }
        }
    }

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut rows = Vec::new();
    visit(&root, &root, &mut rows);
    rows.sort_by(|left, right| left.0.cmp(&right.0));
    rows
}

#[test]
fn retired_memory_production_handlers_and_router_arms_are_physically_absent() {
    let sources = production_rust_sources();
    let retired_symbols = [
        "handle_memory_progress(",
        "handle_memory_readiness(",
        "handle_pattern_feedback(",
        "handle_delete_memory(",
        "handle_memory_gc(",
        "handle_tachi_doctor_scan(",
        "handle_ingest(",
        "handle_ingest_source(",
        "emit_pattern_feedback_event(",
    ];

    for symbol in retired_symbols {
        let callers = sources
            .iter()
            .filter(|(_, source)| source.contains(symbol))
            .map(|(path, _)| path.display().to_string())
            .collect::<Vec<_>>();
        assert!(
            callers.is_empty(),
            "retired production symbol {symbol} remains in {callers:?}",
        );
    }

    let facade = sources
        .iter()
        .find(|(path, _)| path == std::path::Path::new("facade_memory_ops/mod.rs"))
        .map(|(_, source)| source)
        .expect("Memory facade production source");
    for (action, _) in RETIRED_MEMORY_ACTIONS_AND_OWNERS {
        assert!(
            !facade.contains(&format!("\"{action}\" =>")),
            "retired action={action} still owns a production router arm",
        );
    }
}
