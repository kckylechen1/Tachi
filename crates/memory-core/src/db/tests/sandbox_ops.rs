use super::*;

#[test]
fn sandbox_policy_crud_roundtrip() {
    let conn = make_conn();

    set_sandbox_policy(
        &conn,
        "mcp:alpha",
        "process",
        r#"["PATH"]"#,
        r#"["/safe/read"]"#,
        r#"["/safe/write"]"#,
        r#"["/work"]"#,
        10_000,
        30_000,
        2,
        true,
    )
    .unwrap();

    set_sandbox_policy(
        &conn,
        "mcp:beta",
        "container",
        r#"["HOME"]"#,
        r#"["/ro"]"#,
        r#"["/rw"]"#,
        r#"["/sandbox"]"#,
        20_000,
        40_000,
        1,
        false,
    )
    .unwrap();

    let alpha = get_sandbox_policy(&conn, "mcp:alpha").unwrap().unwrap();
    assert_eq!(alpha["capability_id"], "mcp:alpha");
    assert_eq!(alpha["runtime_type"], "process");
    assert_eq!(alpha["enabled"], true);
    assert_eq!(alpha["env_allowlist"], json!(["PATH"]));

    let enabled = list_sandbox_policies(&conn, true, 10).unwrap();
    assert_eq!(enabled.len(), 1);
    assert_eq!(enabled[0]["capability_id"], "mcp:alpha");

    let limited = list_sandbox_policies(&conn, false, 1).unwrap();
    assert_eq!(limited.len(), 1);
}
