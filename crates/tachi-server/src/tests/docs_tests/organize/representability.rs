use super::*;

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn docs_representability_rejects_fallback_and_in_place_before_write() {
    let server = make_server();
    let workspace = DocsWorktree::new();
    let docs = workspace.docs_path();
    for in_place in [false, true] {
        let parent = if in_place {
            docs.join("product")
        } else {
            docs.clone()
        };
        fs::create_dir_all(&parent).unwrap();
        let source = parent.join("codec-note.md");
        let bytes = if in_place {
            "---\ntitle: 'Say \"hello\"'\ncategory: product\n---\nBody sentinel.\n"
        } else {
            "# Say \"hello\"\nBody sentinel.\n"
        };
        fs::write(&source, bytes).unwrap();
        let destination = docs.join("engineering/architecture/codec-note.md");
        for dry_run in [false, true] {
            let result =
                crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), dry_run)
                    .await;
            eprintln!("DOCS_CODEC_OBSERVED normal in_place={in_place} dry_run={dry_run} refused={} source_unchanged={} destination_exists={}", result.is_err(), fs::read(&source).ok().as_deref() == Some(bytes.as_bytes()), destination.exists());
            let error = result.unwrap_err();
            assert!(error.contains("frontmatter serialization must preserve all fields"));
            assert!(!error.contains("hello"));
            assert_eq!(fs::read(&source).unwrap(), bytes.as_bytes());
            assert!(!destination.exists());
            assert!(!docs.join("archive/codec-note.md").exists());
        }
        fs::remove_file(source).unwrap();
    }
    let source = docs.join("valid-note.md");
    let bytes = "---\ntitle: \"Literal\\n\"\nsummary: '!tag-like'\ncategory: product\n---\nBody sentinel.\n";
    fs::write(&source, bytes).unwrap();
    crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), true)
        .await
        .unwrap();
    assert_eq!(fs::read(&source).unwrap(), bytes.as_bytes());
    assert!(!docs.join("product/valid-note.md").exists());
    crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .unwrap();
    let destination = docs.join("product/valid-note.md");
    let expected = "---\ntitle: \"Literal\\n\"\nsummary: \"!tag-like\"\ncategory: \"product\"\n---\nBody sentinel.";
    assert!(!source.exists());
    assert_eq!(fs::read_to_string(&destination).unwrap(), expected);
    crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .unwrap();
    assert_eq!(fs::read_to_string(destination).unwrap(), expected);
    eprintln!("DOCS_CODEC_OBSERVED valid moved=true repeated_bytes_equal=true");
}

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn docs_representability_task_sync_warns_continues_and_counts_only_valid() {
    let server = make_server();
    let workspace = DocsWorktree::new();
    let docs = workspace.docs_path();
    let mut card = crate::tests::make_entry("codec-card");
    card.category = "kanban".into();
    card.metadata = json!({"status": "resolved"});
    server
        .with_global_store(|store| store.upsert(&card).map_err(|e| e.to_string()))
        .unwrap();
    let rejected = docs.join("a-rejected.md");
    let valid = docs.join("z-valid.md");
    let bad_bytes = "---\ntitle: 'Say \"hello\"'\norganize: false\n---\n- [ ] Finish <!-- tachi:codec-card -->\n";
    let valid_bytes = "---\ntitle: \"Literal\\n\"\norganize: false\n---\n- [ ] Finish <!-- tachi:codec-card -->\n";
    fs::write(&rejected, bad_bytes).unwrap();
    fs::write(&valid, valid_bytes).unwrap();
    for dry_run in [true, false] {
        let output =
            crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), dry_run)
                .await
                .unwrap();
        let result: serde_json::Value = serde_json::from_str(&output).unwrap();
        let logs: Vec<_> = result["log"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        eprintln!(
            "DOCS_CODEC_OBSERVED task_sync dry_run={dry_run} synced={} rejected_unchanged={}",
            result["synced_tasks"],
            fs::read(&rejected).unwrap() == bad_bytes.as_bytes()
        );
        assert_eq!(result["status"], "success");
        assert_eq!(result["synced_tasks"], 1);
        assert_eq!(result["moved_files"], 0);
        assert!(logs.iter().any(|line| line
            .starts_with("WARN: task sync write failed for 'a-rejected.md':")
            && line.contains("frontmatter serialization must preserve all fields")));
        assert!(!logs
            .iter()
            .any(|line| line.contains("Would sync") && line.contains("a-rejected.md")));
        assert!(!output.contains("hello"));
        assert_eq!(fs::read(&rejected).unwrap(), bad_bytes.as_bytes());
        assert!(!docs.join("archive/a-rejected.md").exists());
        assert!(!docs.join("engineering/architecture/a-rejected.md").exists());
        let expected = if dry_run {
            valid_bytes.to_string()
        } else {
            valid_bytes
                .replace("[ ]", "[x]")
                .trim_end_matches('\n')
                .to_string()
        };
        assert_eq!(fs::read_to_string(&valid).unwrap(), expected);
    }
}
