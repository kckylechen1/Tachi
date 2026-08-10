use rmcp::model::Tool;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

const FIXTURE: &str = include_str!(
    "../../../../../docs/engineering/architecture/execution-surface-census-v1.fixture.json"
);
const COMPARISON_SURFACES: &[&str] = &[
    "tachi_agent_eval",
    "tachi_orchestrator",
    "tachi_staff",
    "tachi_task",
];

fn live_native_tools() -> Vec<Tool> {
    super::tool_profile_router_coverage::native_route_definitions()
}

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| (key.clone(), canonicalize(value)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect::<Map<_, _>>(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(canonicalize).collect()),
        _ => value.clone(),
    }
}

fn serialized_bytes(value: &Value) -> usize {
    serde_json::to_vec(&canonicalize(value))
        .expect("canonical census value serializes")
        .len()
}

fn projected_tools_from(tools: Vec<Tool>, profile: tachi_hub::ToolProfile) -> Vec<Tool> {
    let tools = crate::server_handler::prepare_native_tool_definitions(tools);
    let mut tools = crate::server_handler::project_tool_definitions(tools, Some(profile), None);
    tools.sort_by(|left, right| left.name.cmp(&right.name));
    tools
}

fn projected_tools(profile: tachi_hub::ToolProfile) -> Vec<Tool> {
    projected_tools_from(live_native_tools(), profile)
}

fn action_inventory(tool: &Tool) -> Option<Vec<String>> {
    let mut actions = tool.input_schema["properties"]["action"]["enum"]
        .as_array()?
        .iter()
        .map(|action| action.as_str().expect("action enum string").to_owned())
        .collect::<Vec<_>>();
    actions.sort();
    Some(actions)
}

fn schema_metric(tool: &Tool) -> Value {
    let mut property_names = tool.input_schema["properties"]
        .as_object()
        .map(|properties| properties.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    property_names.sort();
    json!({
        "property_count": property_names.len(),
        "property_names": property_names,
        "input_schema_bytes": serialized_bytes(&Value::Object((*tool.input_schema).clone())),
    })
}

fn profile_observation(tools: &[Tool]) -> Value {
    let names = tools
        .iter()
        .map(|tool| tool.name.as_ref().to_owned())
        .collect::<Vec<_>>();
    let total_input_schema_bytes = tools
        .iter()
        .map(|tool| serialized_bytes(&Value::Object((*tool.input_schema).clone())))
        .sum::<usize>();
    let total_tool_definition_bytes = tools
        .iter()
        .map(|tool| {
            serialized_bytes(&serde_json::to_value(tool).expect("tool definition serializes"))
        })
        .sum::<usize>();
    json!({
        "visible_tool_count": names.len(),
        "visible_tools": names,
        "total_input_schema_bytes": total_input_schema_bytes,
        "total_tool_definition_bytes": total_tool_definition_bytes,
    })
}

fn observed_census() -> Value {
    let mut registered = live_native_tools();
    registered.sort_by(|left, right| left.name.cmp(&right.name));
    let registered_names = registered
        .iter()
        .map(|tool| tool.name.as_ref().to_owned())
        .collect::<Vec<_>>();

    let profiles = [
        ("standard", tachi_hub::ToolProfile::standard()),
        ("delegate", tachi_hub::ToolProfile::delegate()),
        ("coordinate", tachi_hub::ToolProfile::coordinate()),
        ("operate", tachi_hub::ToolProfile::operate()),
        ("admin", tachi_hub::ToolProfile::admin()),
    ]
    .into_iter()
    .map(|(name, profile)| {
        let tools = projected_tools(profile);
        (name.to_owned(), profile_observation(&tools))
    })
    .collect::<Map<_, _>>();

    let admin = projected_tools(tachi_hub::ToolProfile::admin());
    let mut action_inventories = admin
        .iter()
        .filter_map(|tool| {
            action_inventory(tool).map(|actions| {
                (
                    tool.name.as_ref().to_owned(),
                    json!({"action_count": actions.len(), "actions": actions}),
                )
            })
        })
        .collect::<Map<_, _>>();
    assert!(
        admin.iter().any(|tool| tool.name.as_ref() == "tachi_gh"),
        "canonical GH inventory requires the live admin tachi_gh definition"
    );
    let mut gh_actions = tachi_params::TACHI_GH_ACTIONS.to_vec();
    gh_actions.sort();
    action_inventories.insert(
        "tachi_gh".to_owned(),
        json!({"action_count": gh_actions.len(), "actions": gh_actions}),
    );

    let standard = projected_tools(tachi_hub::ToolProfile::standard());
    let standard_surfaces = standard
        .iter()
        .filter(|tool| tool.name.starts_with("tachi_"))
        .map(|tool| (tool.name.as_ref().to_owned(), schema_metric(tool)))
        .collect::<Map<_, _>>();
    let comparison_surfaces = [
        ("coordinate", tachi_hub::ToolProfile::coordinate()),
        ("admin", tachi_hub::ToolProfile::admin()),
    ]
    .into_iter()
    .map(|(profile_name, profile)| {
        let metrics = projected_tools(profile)
            .iter()
            .filter(|tool| COMPARISON_SURFACES.contains(&tool.name.as_ref()))
            .map(|tool| (tool.name.as_ref().to_owned(), schema_metric(tool)))
            .collect::<Map<_, _>>();
        (profile_name.to_owned(), Value::Object(metrics))
    })
    .collect::<Map<_, _>>();

    json!({
        "registered_native_tools": {
            "tool_count": registered_names.len(),
            "tools": registered_names,
        },
        "profiles": profiles,
        "canonical_action_inventories": action_inventories,
        "schema_surfaces": {
            "standard": standard_surfaces,
            "comparisons": comparison_surfaces,
        },
    })
}

fn budget_violations(observed: &Value, budgets: &Value) -> Vec<String> {
    let mut violations = Vec::new();
    for (profile, budget) in budgets["profiles"].as_object().expect("profile budgets") {
        let actual = &observed["profiles"][profile];
        for (actual_key, budget_key) in [
            ("visible_tool_count", "max_visible_tools"),
            ("total_input_schema_bytes", "max_total_input_schema_bytes"),
            (
                "total_tool_definition_bytes",
                "max_total_tool_definition_bytes",
            ),
        ] {
            let actual_value = actual[actual_key].as_u64().expect("observed metric");
            let maximum = budget[budget_key].as_u64().expect("budget metric");
            if actual_value > maximum {
                violations.push(format!(
                    "profile.{profile}.{actual_key}: observed {actual_value} exceeds {budget_key} {maximum}"
                ));
            }
        }
    }
    for (surface, budget) in budgets["surfaces"].as_object().expect("surface budgets") {
        let (profile, tool) = surface.split_once('.').expect("profile.tool budget name");
        let actual = if profile == "standard" {
            &observed["schema_surfaces"]["standard"][tool]
        } else {
            &observed["schema_surfaces"]["comparisons"][profile][tool]
        };
        for (actual_key, budget_key) in [
            ("property_count", "max_top_level_properties"),
            ("input_schema_bytes", "max_input_schema_bytes"),
        ] {
            let actual_value = actual[actual_key]
                .as_u64()
                .expect("observed surface metric");
            let maximum = budget[budget_key].as_u64().expect("surface budget metric");
            if actual_value > maximum {
                violations.push(format!(
                    "surface.{surface}.{actual_key}: observed {actual_value} exceeds {budget_key} {maximum}"
                ));
            }
        }
    }
    violations
}

#[test]
fn live_execution_surface_matches_fixture_and_provisional_budgets() {
    let fixture: Value = serde_json::from_str(FIXTURE).expect("execution census fixture parses");
    let observed = observed_census();
    if std::env::var_os("TACHI_PRINT_EXECUTION_SURFACE_CENSUS").is_some() {
        eprintln!(
            "{}",
            serde_json::to_string_pretty(&observed).expect("pretty census")
        );
    }
    assert_eq!(
        observed, fixture["observed"],
        "live execution surface drifted"
    );
    let violations = budget_violations(&observed, &fixture["provisional_budgets"]);
    assert!(violations.is_empty(), "{}", violations.join("\n"));

    let standard_task = &observed["schema_surfaces"]["standard"]["tachi_task"];
    let admin_task = &observed["schema_surfaces"]["comparisons"]["admin"]["tachi_task"];
    assert_ne!(
        standard_task, admin_task,
        "standard/admin task schemas differ"
    );

    let standard_tools = projected_tools(tachi_hub::ToolProfile::standard());
    let task = standard_tools
        .iter()
        .find(|tool| tool.name.as_ref() == "tachi_task")
        .expect("standard tachi_task");
    let actions = action_inventory(task).expect("standard task action inventory");
    assert!(!actions.iter().any(|action| action == "dispatch"));
    assert_eq!(
        observed["canonical_action_inventories"]["tachi_gh"]["action_count"],
        json!(18)
    );
    let properties = task.input_schema["properties"]
        .as_object()
        .expect("standard task properties");
    assert!(!properties.contains_key("dispatch_reason"));
    assert!(!observed["profiles"]["standard"]["visible_tools"]
        .as_array()
        .expect("standard tools")
        .iter()
        .any(|tool| tool == "tachi_agent_eval"));
}

#[test]
fn tachi_task_input_schema_caps_have_zero_headroom_per_profile() {
    let fixture: Value = serde_json::from_str(FIXTURE).expect("execution census fixture parses");
    let observed = observed_census();
    let surfaces = fixture["provisional_budgets"]["surfaces"]
        .as_object()
        .expect("surface budgets");
    let mut checked = 0;
    for (surface, budget) in surfaces {
        let Some((profile, tool)) = surface.split_once('.') else {
            continue;
        };
        if tool != "tachi_task" {
            continue;
        }
        let actual = if profile == "standard" {
            &observed["schema_surfaces"]["standard"][tool]["input_schema_bytes"]
        } else {
            &observed["schema_surfaces"]["comparisons"][profile][tool]["input_schema_bytes"]
        };
        assert_eq!(
            actual, &budget["max_input_schema_bytes"],
            "#1712 tachi_task schema cap for {profile} must have zero headroom"
        );
        checked += 1;
    }
    assert_eq!(
        checked, 2,
        "#1712 requires standard and admin tachi_task schema caps"
    );
}

#[test]
fn census_is_order_stable_and_budget_gate_discriminates_growth() {
    let native = live_native_tools();
    let mut reversed_native = native.clone();
    reversed_native.reverse();
    let mut original = projected_tools_from(native, tachi_hub::ToolProfile::standard());
    let reversed = projected_tools_from(reversed_native, tachi_hub::ToolProfile::standard());
    assert_eq!(
        profile_observation(&original),
        profile_observation(&reversed)
    );

    let fixture: Value = serde_json::from_str(FIXTURE).expect("execution census fixture parses");
    let mut grown = observed_census();
    let task = original
        .iter_mut()
        .find(|tool| tool.name.as_ref() == "tachi_task")
        .expect("standard tachi_task");
    let before = schema_metric(task);
    let mut schema = (*task.input_schema).clone();
    schema["properties"]
        .as_object_mut()
        .expect("task properties")
        .insert("census_test_growth".to_owned(), json!({"type": "string"}));
    task.input_schema = std::sync::Arc::new(schema);
    let after = schema_metric(task);
    assert_eq!(
        after["property_count"],
        json!(before["property_count"].as_u64().unwrap() + 1)
    );
    assert!(after["input_schema_bytes"].as_u64() > before["input_schema_bytes"].as_u64());
    grown["schema_surfaces"]["standard"]["tachi_task"] = after;
    let violations = budget_violations(&grown, &fixture["provisional_budgets"]);
    assert!(
        violations
            .iter()
            .any(|message| message.contains("surface.standard.tachi_task.property_count")),
        "standard task property growth must trip its named budget"
    );
    assert!(
        violations
            .iter()
            .any(|message| message.contains("surface.standard.tachi_task.input_schema_bytes")),
        "standard task schema-byte growth must trip its named budget"
    );
}
