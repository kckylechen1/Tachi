use super::*;

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn wiki_organize_omitted_dry_run_defaults_to_preview_at_handler_boundary() {
    let workspace = DocsWorktree::new();
    let server = make_server();
    let subtree = workspace.docs_path().join("engineering");
    fs::create_dir_all(&subtree).unwrap();
    let source = subtree.join("debug-fix.md");
    fs::write(&source, "# Debug fix\n").unwrap();

    let omitted: crate::tool_params::TachiWikiOrganizeParams =
        serde_json::from_value(json!({"dir_path": subtree}))
            .expect("organize params should deserialize without dry_run");
    assert!(omitted.dry_run, "omitted dry_run must default to true");

    let schema = rmcp::handler::server::tool::schema_for_type::<
        crate::tool_params::TachiWikiOrganizeParams,
    >();
    let schema_description = schema["properties"]["dry_run"]["description"]
        .as_str()
        .expect("dry_run schema description");
    assert!(
        schema_description.contains("Defaults to true (in preview mode)."),
        "public schema must document the safe default: {schema_description}"
    );
    assert_eq!(
        schema["properties"]["dry_run"]["default"],
        json!(true),
        "public schema default must match omitted-field runtime behavior"
    );

    let tool = server
        .tool_router
        .list_all()
        .into_iter()
        .find(|tool| tool.name == "tachi_wiki_organize")
        .expect("organize tool route");
    let description = tool.description.expect("organize tool description");
    assert!(
        description.contains("Omitted dry_run means preview; pass false to apply"),
        "tool description must document the safe default: {description}"
    );

    crate::docs_ops::handle_wiki_organize(&server, &omitted.dir_path, omitted.dry_run)
        .await
        .expect("omitted dry_run should execute the preview path");
    assert!(
        source.exists(),
        "omitted dry_run must not move the source through the production handler"
    );

    let explicit_apply: crate::tool_params::TachiWikiOrganizeParams =
        serde_json::from_value(json!({"dir_path": subtree, "dry_run": false}))
            .expect("explicit dry_run=false should deserialize");
    assert!(!explicit_apply.dry_run);
    crate::docs_ops::handle_wiki_organize(
        &server,
        &explicit_apply.dir_path,
        explicit_apply.dry_run,
    )
    .await
    .expect("explicit dry_run=false should execute the apply path");
    assert!(
        !source.exists(),
        "explicit false must apply the planned move"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_docs_organize_rejects_unsafe_roots_and_symlink_escape() {
    let workspace = DocsWorktree::new();
    let server = make_server();
    let temp_root = tempdir().expect("create unsafe temp root");
    let outside = tempdir().expect("create symlink target");
    let symlink_root = workspace.docs_path().join("linked-outside");
    let nested_symlink_root = workspace.docs_path().join("nested/linked-outside");
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), &symlink_root).expect("create escaping symlink");
    #[cfg(unix)]
    {
        fs::create_dir_all(nested_symlink_root.parent().unwrap()).unwrap();
        fs::create_dir_all(outside.path().join("child")).unwrap();
        std::os::unix::fs::symlink(outside.path(), &nested_symlink_root)
            .expect("create nested escaping symlink");
    }

    let mut unsafe_roots = vec![
        workspace.repo_path().to_path_buf(),
        workspace.repo_path().join(".git"),
        temp_root.path().to_path_buf(),
    ];
    if let Some(home) = dirs::home_dir() {
        unsafe_roots.push(home);
    }
    #[cfg(unix)]
    {
        unsafe_roots.push(symlink_root);
        unsafe_roots.push(nested_symlink_root.join("child"));
    }

    for root in unsafe_roots {
        let error = crate::docs_ops::handle_wiki_organize(&server, &root.to_string_lossy(), false)
            .await
            .expect_err("unsafe organize root must be refused");
        assert!(
            error.contains("protected invariant"),
            "refusal must name the protected invariant: {error}"
        );
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_docs_organize_accepts_a_real_docs_descendant() {
    let workspace = DocsWorktree::new();
    let server = make_server();
    let subtree = workspace.docs_path().join("engineering");
    fs::create_dir_all(&subtree).unwrap();
    let source = subtree.join("debug-fix.md");
    fs::write(&source, "# Debug fix\n").unwrap();

    crate::docs_ops::handle_wiki_organize(&server, &subtree.to_string_lossy(), false)
        .await
        .expect("real docs descendant should be eligible");

    assert!(!source.exists());
    assert!(subtree
        .join("engineering")
        .join("debugging")
        .join("debug-fix.md")
        .exists());
}

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_docs_organize_skips_symlinked_directories() {
    let server = make_server();
    let workspace = DocsWorktree::new();
    let outside = tempdir().expect("create outside dir");
    let docs_path = workspace.docs_path();
    let outside_doc = outside.path().join("escape.md");
    fs::write(&outside_doc, "# Outside\nThis file must not be organized.").unwrap();

    std::os::unix::fs::symlink(outside.path(), docs_path.join("linked")).unwrap();

    crate::docs_ops::handle_wiki_organize(&server, &docs_path.to_string_lossy(), false)
        .await
        .unwrap();

    assert!(outside_doc.exists());
    assert!(!docs_path
        .join("engineering")
        .join("architecture")
        .join("escape.md")
        .exists());
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_docs_organize_dry_run_makes_no_changes() {
    let server = make_server();
    let workspace = DocsWorktree::new();
    let docs_path = workspace.docs_path();

    // A scattered file that would normally be classified/moved.
    let src = docs_path.join("design-prd.md");
    fs::write(
        &src,
        r#"---
title: "Design PRD File"
summary: "A product requirement document"
organize: true
---
# Design Requirements
- [ ] P2: Do other things (unresolved)
"#,
    )
    .unwrap();
    let original = fs::read_to_string(&src).unwrap();
    let db_before = fs::read(server.global_db_path_buf()).expect("read DB before dry-run");

    let res_str =
        crate::docs_ops::handle_wiki_organize(&server, &docs_path.to_string_lossy(), true)
            .await
            .unwrap();
    let res: serde_json::Value = serde_json::from_str(&res_str).unwrap();

    // Reports success in dry-run mode and flags it.
    assert_eq!(res["status"], "success");
    assert_eq!(res["dry_run"], true);
    assert_eq!(res["moved_files"], 1);

    // The plan is described in the log.
    let log = res["log"].as_array().unwrap();
    assert!(
        log.iter()
            .any(|m| m.as_str().unwrap_or("").contains("[dry-run] Would move")),
        "dry-run log should describe the planned move; got {log:?}"
    );

    // Crucially: nothing on disk changed.
    assert!(src.exists(), "source file must NOT be moved in dry-run");
    assert_eq!(
        fs::read_to_string(&src).unwrap(),
        original,
        "source content must be untouched in dry-run"
    );
    assert_eq!(
        fs::read(server.global_db_path_buf()).expect("read DB after dry-run"),
        db_before,
        "dry-run must not mutate the task-sync database"
    );
    assert!(
        !docs_path.join("product").exists(),
        "standard directories must NOT be created in dry-run"
    );
    assert!(
        !docs_path.join("_index.md").exists(),
        "_index.md must NOT be written in dry-run"
    );
}

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_docs_organize_refuses_dequeued_directory_identity_swap() {
    let server = make_server();
    let workspace = DocsWorktree::new();
    let docs_path = workspace.docs_path().canonicalize().unwrap();
    let queued = docs_path.join("queued");
    let displaced = docs_path.join("queued-before-swap");
    fs::create_dir_all(&queued).unwrap();
    fs::write(queued.join("planned.md"), "# Planned\n").unwrap();

    let outside = tempdir().expect("create outside swap target");
    let sentinel = outside.path().join("outside-sentinel.md");
    fs::write(&sentinel, "outside must remain untouched").unwrap();
    let outside_path = outside.path().to_path_buf();
    let queued_for_hook = queued.clone();
    crate::docs_ops::set_organize_test_hook(
        crate::docs_ops::OrganizeTestPoint::DirectoryDequeued,
        queued.clone(),
        Box::new(move || {
            fs::rename(&queued_for_hook, &displaced).expect("displace dequeued directory");
            std::os::unix::fs::symlink(&outside_path, &queued_for_hook)
                .expect("swap dequeued directory to outside symlink");
        }),
    );

    let error =
        crate::docs_ops::handle_wiki_organize(&server, docs_path.to_string_lossy().as_ref(), false)
            .await
            .expect_err("dequeued directory identity swap must fail closed");
    assert!(error.contains("protected invariant"), "{error}");
    assert_eq!(
        fs::read_to_string(&sentinel).unwrap(),
        "outside must remain untouched"
    );
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 1);
}

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_docs_organize_refuses_destination_parent_swap_before_write() {
    let server = make_server();
    let workspace = DocsWorktree::new();
    let docs_path = workspace.docs_path().canonicalize().unwrap();
    fs::write(
        docs_path.join("product-note.md"),
        r#"---
title: "Product Note"
category: "docs/product"
organize: true
---
# Product note
"#,
    )
    .unwrap();

    let destination_parent = docs_path.join("product");
    let displaced = docs_path.join("product-before-swap");
    let outside = tempdir().expect("create outside destination target");
    let sentinel = outside.path().join("outside-sentinel.md");
    fs::write(&sentinel, "outside must remain untouched").unwrap();
    let outside_path = outside.path().to_path_buf();
    let parent_for_hook = destination_parent.clone();
    crate::docs_ops::set_organize_test_hook(
        crate::docs_ops::OrganizeTestPoint::DestinationParentReady,
        destination_parent,
        Box::new(move || {
            fs::rename(&parent_for_hook, &displaced).expect("displace destination parent");
            std::os::unix::fs::symlink(&outside_path, &parent_for_hook)
                .expect("swap destination parent to outside symlink");
        }),
    );

    let error =
        crate::docs_ops::handle_wiki_organize(&server, docs_path.to_string_lossy().as_ref(), false)
            .await
            .expect_err("destination parent identity swap must fail closed");
    assert!(error.contains("protected invariant"), "{error}");
    assert_eq!(
        fs::read_to_string(&sentinel).unwrap(),
        "outside must remain untouched"
    );
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 1);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn test_docs_organize_concurrent_apply_refuses_docs_lock() {
    let server = make_server();
    let workspace = DocsWorktree::new();
    let docs_path = workspace.docs_path().canonicalize().unwrap();
    let acquired = std::sync::Arc::new(std::sync::Barrier::new(2));
    let release = std::sync::Arc::new(std::sync::Barrier::new(2));
    let hook_acquired = acquired.clone();
    let hook_release = release.clone();
    crate::docs_ops::set_organize_test_hook(
        crate::docs_ops::OrganizeTestPoint::ApplyLockAcquired,
        docs_path.clone(),
        Box::new(move || {
            hook_acquired.wait();
            hook_release.wait();
        }),
    );

    let first_server = server.clone();
    let first_docs = docs_path.clone();
    let first = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(crate::docs_ops::handle_wiki_organize(
                &first_server,
                first_docs.to_string_lossy().as_ref(),
                false,
            ))
    });

    acquired.wait();
    let concurrent =
        crate::docs_ops::handle_wiki_organize(&server, docs_path.to_string_lossy().as_ref(), false)
            .await;
    release.wait();
    first
        .join()
        .expect("join lock owner organize")
        .expect("lock owner organize should complete");

    let error = concurrent.expect_err("concurrent organize apply must refuse the docs lock");
    assert!(error.contains("already held"), "{error}");
}
