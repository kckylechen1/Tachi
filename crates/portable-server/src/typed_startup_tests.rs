use super::*;
use crate::service::{GetParams, SaveParams};
use rmcp::handler::server::wrapper::Parameters;
use serde_json::json;

struct TestDir(std::path::PathBuf);
impl TestDir {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("portable-typed-startup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for TestDir {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            eprintln!(
                "portable typed fixture cleanup failed {}: {error}",
                self.0.display()
            );
        }
    }
}

fn configured(path: &std::path::Path) -> Config {
    let mut config = Config::parse(Vec::<String>::new(), |_| None).unwrap();
    config.db_path = path.to_string_lossy().to_string();
    config.typed_records = Some(serde_json::from_value(config::typed_fixture_value()).unwrap());
    config
}

#[tokio::test]
async fn typed_fresh_persistent_store_is_admitted_and_reopens_with_exact_role() {
    let dir = TestDir::new();
    let path = dir.0.join("typed.db");
    assert!(!path.exists());
    let server = build_server(configured(&path)).unwrap();
    let params: SaveParams = serde_json::from_value(json!({"typed_record":{
        "id":"pref-1","kind":{"name":"user.preference","version":1},
        "keys":{"id":"preference-1","revision":"1"},
        "payload":{"text":"Use concise explanations."},"links":[]
    }}))
    .unwrap();
    server.save(Parameters(params)).await.unwrap();
    drop(server);
    let check = MemoryStore::open_with_context(
        path.to_str().unwrap(),
        &DbOpenContext::open_existing_deny()
            .with_exact_profile(StoreProfile::PortableKernel)
            .with_exact_role("assistant"),
    )
    .unwrap();
    assert_eq!(check.db_label(), "assistant");
    assert_eq!(check.store_profile(), StoreProfile::PortableKernel);
    drop(check);
    let reopened = build_server(configured(&path)).unwrap();
    let record = reopened
        .get(Parameters(GetParams {
            id: "pref-1".to_string(),
            typed: true,
        }))
        .await
        .unwrap();
    let record: serde_json::Value = serde_json::from_str(&record).unwrap();
    assert_eq!(record["writer"], "private-stdio-host");
}

#[test]
fn typed_startup_refuses_wrong_or_missing_role_before_any_restamping() {
    let dir = TestDir::new();
    for role in [Some("engineering"), None] {
        let path = dir.0.join(if role.is_some() {
            "engineering.db"
        } else {
            "unstamped-role.db"
        });
        let mut context =
            DbOpenContext::create_fresh().with_exact_profile(StoreProfile::PortableKernel);
        if let Some(role) = role {
            context = context.with_exact_role(role);
        }
        drop(MemoryStore::open_with_context(path.to_str().unwrap(), &context).unwrap());
        let before = std::fs::read(&path).unwrap();
        let mut config = configured(&path);
        config.allow_schema_migration = true;
        let result = build_server(config);
        assert!(
            result.is_err(),
            "migration authority must not grant another store role"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[test]
fn daemon_build_refuses_programmatic_typed_config_without_creating_a_store() {
    let dir = TestDir::new();
    let path = dir.0.join("not-created.db");
    let mut config = configured(&path);
    config.daemon = true;
    assert!(build_server(config).is_err());
    assert!(!path.exists());
}
