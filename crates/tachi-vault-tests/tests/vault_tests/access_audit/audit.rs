use super::*;

#[tokio::test]
async fn vault_session_operations_record_audit_entries() {
    let server = make_server();

    server
        .seed_vault("audit-password")
        .expect("seed unlocked vault");

    server
        .vault_lock()
        .await
        .expect("vault_lock should succeed");

    let unlock_err = server
        .vault_unlock(Parameters(VaultUnlockParams {
            password: "wrong-audit-password".to_string(),
            password_fifo_path: None,
            use_keychain: false,
        }))
        .await
        .expect_err("wrong password should fail");
    assert!(unlock_err.contains("Wrong password"));

    server
        .vault_unlock(Parameters(VaultUnlockParams {
            password: "audit-password".to_string(),
            password_fifo_path: None,
            use_keychain: false,
        }))
        .await
        .expect("vault_unlock should succeed");

    let rows = server
        .with_global_store_read(|store| {
            let mut stmt = store
                .connection()
                .prepare(
                    "SELECT operation, secret_name, success
                     FROM vault_audit
                     ORDER BY id ASC",
                )
                .map_err(|e| format!("prepare vault audit query failed: {e}"))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })
                .map_err(|e| format!("query vault audit rows failed: {e}"))?;
            let mut rows_out = Vec::new();
            for row in rows {
                rows_out.push(row.map_err(|e| format!("read vault audit row failed: {e}"))?);
            }
            Ok(rows_out)
        })
        .expect("vault audit query should succeed");

    assert!(
        rows.contains(&("vault_lock".to_string(), None, 1)),
        "expected vault_lock audit row"
    );
    assert!(
        rows.contains(&("vault_unlock".to_string(), None, 0)),
        "expected failed vault_unlock audit row"
    );
    assert!(
        rows.iter()
            .filter(|(op, secret_name, success)| op == "vault_unlock"
                && secret_name.is_none()
                && *success == 1)
            .count()
            >= 1,
        "expected successful vault_unlock audit row"
    );
}

#[tokio::test]
async fn vault_lock_reports_audit_persistence_failure() {
    let server = make_server();

    server
        .seed_vault("audit-warning-password")
        .expect("seed unlocked vault");

    crate::test_support::with_unrestricted_fixture_connection(
        &server.global_db_path_buf(),
        |connection| connection.execute("DROP TABLE vault_audit", []).map(|_| ()),
    )
    .expect("drop vault_audit");

    let response = server
        .vault_lock()
        .await
        .expect("vault_lock business result should still succeed");
    let value: serde_json::Value =
        serde_json::from_str(&response).expect("vault_lock response should be JSON");

    assert_eq!(value["locked"], json!(true));
    let warning = value["vault_audit_warning"]
        .as_str()
        .expect("audit warning should be visible");
    assert!(warning.contains("vault_lock"), "{warning}");
    assert!(
        warning.contains("audit record was not persisted"),
        "{warning}"
    );
    assert!(
        !warning.contains("audit-warning-password"),
        "warning must not leak vault password: {warning}"
    );
}
