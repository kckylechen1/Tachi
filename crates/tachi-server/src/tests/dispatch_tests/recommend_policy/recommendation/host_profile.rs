use super::*;
use tachi_params::ExecutionLevel;

fn assert_host_admission_fields(
    admission: &serde_json::Value,
    expected_requested: Option<&str>,
    expected_effective: &str,
    expected_max: Option<&str>,
    expected_profile: &str,
    expected_source: &str,
    expected_allowed: bool,
    expected_reason: &str,
) {
    assert_eq!(
        admission["requested_level"],
        match expected_requested {
            Some(level) => json!(level),
            None => json!(null),
        }
    );
    assert_eq!(admission["effective_level"], json!(expected_effective));
    assert_eq!(
        admission["max_execution_level"],
        match expected_max {
            Some(level) => json!(level),
            None => json!(null),
        }
    );
    assert_eq!(admission["host_profile"], json!(expected_profile));
    assert_eq!(admission["profile_source"], json!(expected_source));
    assert_eq!(admission["allowed"], json!(expected_allowed));
    assert_eq!(admission["reason_code"], json!(expected_reason));
    let reason = admission["reason_code"]
        .as_str()
        .expect("reason_code string");
    assert!(
        matches!(
            reason,
            "host_profile_allows" | "host_profile_mismatch" | "host_profile_invalid"
        ),
        "reason_code must be from the closed set, got {reason}"
    );
}

#[tokio::test]
async fn tachi_task_recommend_host_profile_allows_l1_on_development() {
    let _env = crate::host_profile::HostProfileTestOverride::set(Some("development"));
    let server = make_server();
    let mut params = task_params("recommend");
    params.task = Some("plan a small documentation update".to_string());
    params.execution_level = Some(ExecutionLevel::L1);
    params.limit = Some(10);

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("recommend should succeed under development/L1");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");
    let admission = rec
        .get("host_admission")
        .expect("host_admission receipt required");
    assert_host_admission_fields(
        admission,
        Some("L1"),
        "L1",
        Some("L1"),
        "development",
        "app_home_config_env",
        true,
        "host_profile_allows",
    );
    assert!(
        rec.get("recommended_profile").is_some(),
        "allowed recommend must keep executable route fields: {rec:#}"
    );
}

#[tokio::test]
async fn tachi_task_recommend_host_profile_mismatch_declines_l2_on_development() {
    let _env = crate::host_profile::HostProfileTestOverride::set(Some("development"));
    let server = make_server();
    let mut params = task_params("recommend");
    params.task = Some("inspect product data diagnostics".to_string());
    params.execution_level = Some(ExecutionLevel::L2);
    params.limit = Some(10);

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("structured decline is a successful JSON response");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");
    let admission = rec
        .get("host_admission")
        .expect("host_admission receipt required");
    assert_host_admission_fields(
        admission,
        Some("L2"),
        "L2",
        Some("L1"),
        "development",
        "app_home_config_env",
        false,
        "host_profile_mismatch",
    );
    assert!(
        rec.get("recommended_profile").is_none(),
        "mismatch must not emit executable route: {rec:#}"
    );
    assert!(
        rec.get("candidates").is_none(),
        "mismatch must not emit candidates: {rec:#}"
    );
}

#[tokio::test]
async fn tachi_task_recommend_host_profile_allows_l3_on_home_data() {
    let _env = crate::host_profile::HostProfileTestOverride::set(Some("home_data"));
    let server = make_server();
    let mut params = task_params("recommend");
    params.task = Some("run product-data side effect carefully".to_string());
    params.execution_level = Some(ExecutionLevel::L3);
    params.limit = Some(10);

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("home_data permits L3");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");
    let admission = rec
        .get("host_admission")
        .expect("host_admission receipt required");
    assert_host_admission_fields(
        admission,
        Some("L3"),
        "L3",
        Some("L3"),
        "home_data",
        "app_home_config_env",
        true,
        "host_profile_allows",
    );
    assert!(rec.get("recommended_profile").is_some());
}

#[tokio::test]
async fn tachi_task_recommend_host_profile_release_mismatch_on_l2() {
    let _env = crate::host_profile::HostProfileTestOverride::set(Some("release"));
    let server = make_server();
    let mut params = task_params("recommend");
    params.task = Some("product diagnostic on release host".to_string());
    params.execution_level = Some(ExecutionLevel::L2);
    params.limit = Some(10);

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("structured decline");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");
    assert_host_admission_fields(
        rec.get("host_admission").expect("receipt"),
        Some("L2"),
        "L2",
        Some("L1"),
        "release",
        "app_home_config_env",
        false,
        "host_profile_mismatch",
    );
}

#[tokio::test]
async fn tachi_task_recommend_host_profile_omitted_level_defaults_to_l1() {
    let _env = crate::host_profile::HostProfileTestOverride::set(Some("development"));
    let server = make_server();
    let mut params = task_params("recommend");
    params.task = Some("small plan without explicit level".to_string());
    params.execution_level = None;
    params.limit = Some(10);

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("omitted level allowed as L1");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");
    assert_host_admission_fields(
        rec.get("host_admission").expect("receipt"),
        None,
        "L1",
        Some("L1"),
        "development",
        "app_home_config_env",
        true,
        "host_profile_allows",
    );
}

#[tokio::test]
async fn tachi_task_recommend_host_profile_missing_defaults_development() {
    let _env = crate::host_profile::HostProfileTestOverride::set(None);
    let server = make_server();
    let mut params = task_params("recommend");
    params.task = Some("missing profile falls back conservatively".to_string());
    params.execution_level = Some(ExecutionLevel::L1);
    params.limit = Some(10);

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("missing profile defaults");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");
    assert_host_admission_fields(
        rec.get("host_admission").expect("receipt"),
        Some("L1"),
        "L1",
        Some("L1"),
        "development",
        "default_development",
        true,
        "host_profile_allows",
    );
}

#[tokio::test]
async fn tachi_task_recommend_host_profile_invalid_declines_without_route() {
    let _env = crate::host_profile::HostProfileTestOverride::set(Some("not-a-real-profile"));
    let server = make_server();
    let mut params = task_params("recommend");
    params.task = Some("invalid profile must not recommend".to_string());
    params.execution_level = Some(ExecutionLevel::L1);
    params.limit = Some(10);

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("invalid profile structured decline");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");
    assert_host_admission_fields(
        rec.get("host_admission").expect("receipt"),
        Some("L1"),
        "L1",
        None,
        "invalid",
        "app_home_config_env",
        false,
        "host_profile_invalid",
    );
    assert!(rec.get("recommended_profile").is_none());
    assert!(rec.get("recommended_transport").is_none());
}

#[tokio::test]
async fn tachi_task_route_simulate_host_profile_allows_and_emits_receipt() {
    let _env = crate::host_profile::HostProfileTestOverride::set(Some("development"));
    let server = make_server();
    let mut params = task_params("route_simulate");
    params.execution_level = Some(ExecutionLevel::L1);
    params.limit = Some(10);

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("route_simulate allowed");
    let sim: serde_json::Value = serde_json::from_str(&raw).expect("route_simulate JSON");
    assert_eq!(sim["action"], json!("route_simulate"));
    assert_host_admission_fields(
        sim.get("host_admission").expect("receipt"),
        Some("L1"),
        "L1",
        Some("L1"),
        "development",
        "app_home_config_env",
        true,
        "host_profile_allows",
    );
    assert!(
        sim.get("policies").is_some(),
        "allowed route_simulate keeps policy results: {sim:#}"
    );
}

#[tokio::test]
async fn tachi_task_route_simulate_host_profile_without_task_and_mismatch() {
    let _env = crate::host_profile::HostProfileTestOverride::set(Some("development"));
    let server = make_server();
    let mut params = task_params("route_simulate");
    params.task = None;
    params.execution_level = Some(ExecutionLevel::L2);
    params.limit = Some(10);

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("route_simulate without task still admits");
    let sim: serde_json::Value = serde_json::from_str(&raw).expect("route_simulate JSON");
    assert_eq!(sim["action"], json!("route_simulate"));
    assert_host_admission_fields(
        sim.get("host_admission").expect("receipt"),
        Some("L2"),
        "L2",
        Some("L1"),
        "development",
        "app_home_config_env",
        false,
        "host_profile_mismatch",
    );
    assert!(
        sim.get("policies").is_none(),
        "declined route_simulate must not emit policies: {sim:#}"
    );
}

#[tokio::test]
async fn tachi_task_recommend_host_profile_repo_local_elevation_stays_ineffective() {
    let _env = crate::host_profile::HostProfileTestOverride::set(Some("development"));
    let server = make_server();
    let mut params = task_params("recommend");
    params.task = Some("repo-local home_data must not raise ceiling after #1018".to_string());
    params.execution_level = Some(ExecutionLevel::L2);
    params.limit = Some(10);

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("structured decline");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");
    assert_host_admission_fields(
        rec.get("host_admission").expect("receipt"),
        Some("L2"),
        "L2",
        Some("L1"),
        "development",
        "app_home_config_env",
        false,
        "host_profile_mismatch",
    );
}
