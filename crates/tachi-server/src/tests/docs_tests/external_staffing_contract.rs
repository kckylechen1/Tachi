use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const FIXTURE: &str = include_str!(
    "../../../../../docs/engineering/architecture/external-staffing-contract-v1.fixture.json"
);

#[derive(Debug)]
struct RustFunction {
    symbol: String,
    name: String,
    source: String,
    tool: bool,
    tool_contract: String,
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("tachi-server lives under <repo>/crates")
        .to_path_buf()
}

fn production_rust_files(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read production source directory") {
        let entry = entry.expect("read source entry");
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().and_then(|name| name.to_str()) != Some("tests") {
                production_rust_files(&path, files);
            }
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs")
            && !matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some("tests.rs" | "test_support.rs")
            )
        {
            files.push(path);
        }
    }
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .expect("source path is inside repository")
        .to_string_lossy()
        .replace('\\', "/")
}

fn code_before_line_comment(line: &str) -> &str {
    let mut quoted = false;
    let mut escaped = false;
    let bytes = line.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let character = bytes[index];
        if escaped {
            escaped = false;
        } else if character == b'\\' && quoted {
            escaped = true;
        } else if character == b'"' {
            quoted = !quoted;
        } else if !quoted
            && character == b'/'
            && bytes.get(index + 1).is_some_and(|next| *next == b'/')
        {
            return &line[..index];
        }
        index += 1;
    }
    line
}

fn function_name(line: &str) -> Option<String> {
    let line = code_before_line_comment(line);
    let marker = line.find("fn ")?;
    let before = &line[..marker];
    if before
        .chars()
        .last()
        .is_some_and(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return None;
    }
    let name = line[marker + 3..]
        .chars()
        .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
        .collect::<String>();
    (!name.is_empty()).then_some(name)
}

fn brace_delta(line: &str) -> i64 {
    let mut delta = 0_i64;
    let mut quoted = false;
    let mut escaped = false;
    for character in code_before_line_comment(line).chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && quoted {
            escaped = true;
        } else if character == '"' {
            quoted = !quoted;
        } else if !quoted && character == '{' {
            delta += 1;
        } else if !quoted && character == '}' {
            delta -= 1;
        }
    }
    delta
}

fn functions_in_source(relative: &str, source: &str) -> Vec<RustFunction> {
    let lines = source.lines().collect::<Vec<_>>();
    let mut functions = Vec::new();
    let mut pending_tool = false;
    let mut tool_attribute_start = None;
    let mut pending_test = false;
    let mut index = 0;

    while index < lines.len() {
        let trimmed = lines[index].trim_start();
        if trimmed.starts_with("#[cfg(test)]") {
            let mut module_line = index + 1;
            while module_line < lines.len()
                && (lines[module_line].trim().is_empty()
                    || lines[module_line].trim_start().starts_with("#["))
            {
                module_line += 1;
            }
            if module_line < lines.len() && lines[module_line].trim_start().starts_with("mod ") {
                index = module_line;
                let mut depth = 0_i64;
                let mut opened = false;
                while index < lines.len() {
                    let change = brace_delta(lines[index]);
                    opened |= code_before_line_comment(lines[index]).contains('{');
                    depth += change;
                    index += 1;
                    if opened && depth == 0 {
                        break;
                    }
                }
                pending_test = false;
                continue;
            }
        }
        if trimmed.starts_with("#[tool") {
            pending_tool = true;
            tool_attribute_start = Some(index);
        }
        if trimmed.starts_with("#[cfg(test)]") {
            pending_test = true;
        }
        let Some(name) = function_name(lines[index]) else {
            index += 1;
            continue;
        };
        let start = index;
        let mut depth = 0_i64;
        let mut opened = false;
        while index < lines.len() {
            let change = brace_delta(lines[index]);
            opened |= code_before_line_comment(lines[index]).contains('{');
            depth += change;
            index += 1;
            if opened && depth == 0 {
                break;
            }
        }
        if !pending_test {
            functions.push(RustFunction {
                symbol: format!("{relative}::{name}"),
                name,
                source: lines[start..index].join("\n"),
                tool: pending_tool,
                tool_contract: tool_attribute_start
                    .map(|attribute_start| lines[attribute_start..start].join("\n"))
                    .unwrap_or_default(),
            });
        }
        pending_tool = false;
        tool_attribute_start = None;
        pending_test = false;
    }
    functions
}

fn occurrence_count(source: &str, needle: &str) -> usize {
    source.match_indices(needle).count()
}

fn numbered_sites(function: &RustFunction, needle: &str, label: &str) -> Vec<String> {
    (1..=occurrence_count(&function.source, needle))
        .map(|ordinal| format!("{}::{label}#{ordinal}", function.symbol))
        .collect()
}

fn staffing_ledger_root_variables(source: &str) -> Vec<&str> {
    ["std::env::var", "std::env::var_os"]
        .into_iter()
        .flat_map(|call| {
            source.match_indices(call).filter_map(move |(index, _)| {
                let argument = source[index + call.len()..].trim_start();
                let argument = argument.strip_prefix('(')?.trim_start();
                let variable = argument.strip_prefix('"')?.split('"').next()?;
                let is_root = variable.starts_with("TACHI_") && variable.ends_with("_ROOT");
                // Skills and config roots locate static inputs, not staffing ledgers.
                let is_known_non_ledger =
                    matches!(variable, "TACHI_SKILLS_ROOT" | "TACHI_CONFIG_ROOT");
                (is_root && !is_known_non_ledger).then_some(variable)
            })
        })
        .collect()
}

fn observe_functions(functions: &[RustFunction]) -> Value {
    let mut definitions = functions
        .iter()
        .filter(|function| function.name == "handle_tachi_dispatch")
        .map(|function| function.symbol.clone())
        .collect::<Vec<_>>();
    let mut production_adopters = functions
        .iter()
        .filter(|function| {
            function.name != "handle_tachi_dispatch"
                && occurrence_count(&function.source, "handle_tachi_dispatch(") > 0
        })
        .flat_map(|function| {
            numbered_sites(function, "handle_tachi_dispatch(", "dispatch-kernel-call")
        })
        .collect::<Vec<_>>();
    let mut request_builders = functions
        .iter()
        .filter(|function| occurrence_count(&function.source, "TachiDispatchParams {") > 0)
        .flat_map(|function| numbered_sites(function, "TachiDispatchParams {", "request-literal"))
        .collect::<Vec<_>>();
    let mut launch_facades = functions
        .iter()
        .filter(|function| {
            function.tool
                && (function.tool_contract.contains("action='dispatch'")
                    || function.tool_contract.contains("launch=true"))
        })
        .map(|function| function.symbol.clone())
        .collect::<Vec<_>>();

    let mut ledger_roots = Vec::new();
    let mut ledger_writers = Vec::new();
    let mut linked_result_copies = Vec::new();
    for function in functions {
        let is_legacy_owner =
            function.symbol.contains("/arena_ops/") || function.symbol.contains("/shell_ops/");
        let is_staffing_flow_projection = function
            .symbol
            .contains("/task_lifecycle/flow_artifacts/dispatch_markers.rs::")
            && function.source.contains("run_dir_for_flow_id(")
            && function.source.contains("dispatch_id");
        if is_legacy_owner
            && function.source.contains("PathBuf")
            && !staffing_ledger_root_variables(&function.source).is_empty()
        {
            ledger_roots.push(function.symbol.clone());
        }
        let writer_calls = [
            "write_run_status_file(",
            "write_json_file_owner_only(",
            "write_owner_only_file_atomic(",
            "write_json_atomic(",
            "append_flow_event(",
            "tokio::fs::write(",
            "std::fs::write(",
        ];
        let writes_artifact = writer_calls
            .iter()
            .any(|call| function.source.contains(call));
        let names_ledger_artifact = ["status", "result_path", "mission", "run_dir"]
            .iter()
            .any(|marker| function.source.contains(marker));
        if (is_legacy_owner || is_staffing_flow_projection)
            && writes_artifact
            && names_ledger_artifact
        {
            for call in writer_calls {
                ledger_writers.extend(numbered_sites(function, call, call.trim_end_matches('(')));
            }
        }
        if is_legacy_owner {
            ledger_writers.extend(numbered_sites(
                function,
                "append_run_event(",
                "append_run_event",
            ));
        }
        if function.source.contains("read_linked_dispatch_result(")
            && function.source.contains("result_path")
        {
            for call in writer_calls {
                linked_result_copies.extend(numbered_sites(function, call, "linked-result-copy"));
            }
        }
    }
    definitions.sort();
    production_adopters.sort();
    request_builders.sort();
    launch_facades.sort();
    ledger_roots.sort();
    ledger_writers.sort();
    linked_result_copies.sort();

    json!({
        "adoption_entrypoint_definitions": definitions,
        "production_adopters": production_adopters,
        "request_builders": request_builders,
        "launch_advertising_facades": launch_facades,
        "legacy_secondary_ledger_roots": ledger_roots,
        "legacy_staffing_projection_writer_sites": ledger_writers,
        "linked_result_copies": linked_result_copies,
    })
}

fn observe_sources<'a>(sources: impl IntoIterator<Item = (&'a str, &'a str)>) -> Value {
    let functions = sources
        .into_iter()
        .flat_map(|(relative, source)| functions_in_source(relative, source))
        .collect::<Vec<_>>();
    observe_functions(&functions)
}

fn observed_topology() -> Value {
    let root = repo_root();
    let mut paths = Vec::new();
    production_rust_files(&root.join("crates/tachi-server/src"), &mut paths);
    paths.sort();
    let sources = paths
        .iter()
        .map(|path| {
            (
                relative_path(&root, path),
                std::fs::read_to_string(path).expect("read production Rust source"),
            )
        })
        .collect::<Vec<_>>();
    observe_sources(
        sources
            .iter()
            .map(|(relative, source)| (relative.as_str(), source.as_str())),
    )
}

fn budget_violations(observed: &Value, budgets: &Value) -> Vec<String> {
    let mut violations = Vec::new();
    for (key, budget_key) in [
        ("adoption_entrypoint_definitions", "launch_kernels"),
        ("production_adopters", "production_adopters"),
        ("request_builders", "request_builders"),
        ("launch_advertising_facades", "launch_advertising_facades"),
        (
            "legacy_secondary_ledger_roots",
            "legacy_secondary_ledger_roots",
        ),
        (
            "legacy_staffing_projection_writer_sites",
            "legacy_staffing_projection_writer_sites",
        ),
        ("linked_result_copies", "linked_result_copies"),
    ] {
        let actual = observed[key].as_array().expect("observed inventory").len() as u64;
        let frozen = budgets[budget_key].as_u64().expect("contraction budget");
        if actual != frozen {
            violations.push(format!(
                "{key}: observed {actual} must equal ratcheted {budget_key} budget {frozen}; lower the budget in the same deletion change, never leave regrowth room"
            ));
        }
    }
    violations
}

#[test]
fn external_staffing_topology_matches_the_single_kernel_contract() {
    let fixture: Value = serde_json::from_str(FIXTURE).expect("staffing fixture parses");
    let observed = observed_topology();
    assert_eq!(
        observed, fixture["observed_topology"],
        "external staffing symbol/site inventory drifted; inspect the production topology instead of mechanically refreshing this fixture"
    );
    let violations = budget_violations(&observed, &fixture["contraction_budgets"]);
    assert!(violations.is_empty(), "{}", violations.join("\n"));
}

#[test]
fn external_staffing_budgets_discriminate_every_growth_axis() {
    let fixture: Value = serde_json::from_str(FIXTURE).expect("staffing fixture parses");
    assert!(budget_violations(
        &fixture["observed_topology"],
        &fixture["contraction_budgets"]
    )
    .is_empty());
    for (key, budget_key) in [
        ("adoption_entrypoint_definitions", "launch_kernels"),
        ("production_adopters", "production_adopters"),
        ("request_builders", "request_builders"),
        ("launch_advertising_facades", "launch_advertising_facades"),
        (
            "legacy_secondary_ledger_roots",
            "legacy_secondary_ledger_roots",
        ),
        (
            "legacy_staffing_projection_writer_sites",
            "legacy_staffing_projection_writer_sites",
        ),
        ("linked_result_copies", "linked_result_copies"),
    ] {
        let mut grown = fixture["observed_topology"].clone();
        grown[key]
            .as_array_mut()
            .expect("inventory array")
            .push(json!("synthetic/new_site"));
        assert!(
            budget_violations(&grown, &fixture["contraction_budgets"])
                .iter()
                .any(|violation| violation.contains(key)),
            "{key} growth must exceed its hard budget"
        );

        let mut shrunk = fixture["observed_topology"].clone();
        shrunk[key].as_array_mut().expect("inventory array").pop();
        assert!(
            budget_violations(&shrunk, &fixture["contraction_budgets"])
                .iter()
                .any(|violation| violation.contains(key)),
            "{key} deletion must ratchet its budget in the same change"
        );
        let mut ratcheted = fixture["contraction_budgets"].clone();
        ratcheted[budget_key] = json!(shrunk[key].as_array().expect("inventory").len());
        assert!(budget_violations(&shrunk, &ratcheted).is_empty());
        assert!(
            budget_violations(&fixture["observed_topology"], &ratcheted)
                .iter()
                .any(|violation| violation.contains(key)),
            "{key} cannot regrow after a ratchet"
        );
    }
}

#[test]
fn external_staffing_source_observer_discriminates_same_function_growth_and_tests() {
    let observed = observe_sources([
        (
            "crates/tachi-server/src/shell_ops/actions/synthetic.rs",
            r#"
                async fn adopter() {
                    let _ = TachiDispatchParams { task: first };
                    let _ = TachiDispatchParams { task: second };
                    handle_tachi_dispatch(server, first).await;
                    handle_tachi_dispatch(server, second).await;
                    write_run_status_file(run_dir, first);
                    write_run_status_file(run_dir, second);
                    append_run_event(run_dir, first);
                    append_run_event(run_dir, second);
                }
                pub(crate) fn unrelated_path_factory() -> PathBuf {
                    std::env::var("TACHI_SHADOW_ROOT");
                    std::env::var_os("TACHI_SHADOW_ROOT");
                    PathBuf::new()
                }
                pub(crate) fn skills_configuration_location() -> PathBuf {
                    std::env::var("TACHI_SKILLS_ROOT");
                    std::env::var_os("TACHI_CONFIG_ROOT");
                    PathBuf::new()
                }
                #[cfg(test)]
                mod tests {
                    fn ignored() { handle_tachi_dispatch(server, test); }
                }
            "#,
        ),
        (
            "crates/tachi-server/src/arena_ops/actions/synthetic.rs",
            r#"
                fn copied_result() {
                    let result = read_linked_dispatch_result(id, hint);
                    write_owner_only_file_atomic(&result_path, result);
                    tokio::fs::write(&result_path, result);
                }
            "#,
        ),
        (
            "crates/tachi-server/src/tools/synthetic.rs",
            r#"
                fn handle_tachi_dispatch() {}
                fn handle_tachi_dispatch() {}
                #[tool(description = "legacy action='dispatch' contract")]
                fn tachi_new_launcher() { route(); }
            "#,
        ),
        (
            "crates/tachi-server/src/task_lifecycle/flow_artifacts/dispatch_markers.rs",
            r#"
                fn marker(flow_id: &str, dispatch_id: &str) {
                    let run_dir = run_dir_for_flow_id(flow_id);
                    write_json_atomic(&run_dir.join("status.json"), status);
                    append_flow_event(&run_dir, event);
                }
            "#,
        ),
    ]);

    assert_eq!(observed["production_adopters"].as_array().unwrap().len(), 2);
    assert_eq!(observed["request_builders"].as_array().unwrap().len(), 2);
    assert_eq!(
        observed["legacy_staffing_projection_writer_sites"]
            .as_array()
            .unwrap()
            .len(),
        8
    );
    assert_eq!(
        observed["legacy_secondary_ledger_roots"]
            .as_array()
            .unwrap(),
        &[json!(
            "crates/tachi-server/src/shell_ops/actions/synthetic.rs::unrelated_path_factory"
        )]
    );
    assert_eq!(
        observed["linked_result_copies"].as_array().unwrap().len(),
        2
    );
    assert_eq!(
        observed["launch_advertising_facades"].as_array().unwrap(),
        &[json!(
            "crates/tachi-server/src/tools/synthetic.rs::tachi_new_launcher"
        )]
    );

    let forward = observe_sources([
        (
            "crates/tachi-server/src/tools/a.rs",
            "fn first() { handle_tachi_dispatch(server, params); }",
        ),
        (
            "crates/tachi-server/src/tools/z.rs",
            "fn second() { handle_tachi_dispatch(server, params); }",
        ),
    ]);
    let reversed = observe_sources([
        (
            "crates/tachi-server/src/tools/z.rs",
            "fn second() { handle_tachi_dispatch(server, params); }",
        ),
        (
            "crates/tachi-server/src/tools/a.rs",
            "fn first() { handle_tachi_dispatch(server, params); }",
        ),
    ]);
    assert_eq!(forward, reversed, "source order must not change topology");

    let fixture: Value = serde_json::from_str(FIXTURE).expect("staffing fixture parses");
    let mut second_kernel = fixture["observed_topology"].clone();
    second_kernel["adoption_entrypoint_definitions"] =
        observed["adoption_entrypoint_definitions"].clone();
    assert!(
        budget_violations(&second_kernel, &fixture["contraction_budgets"])
            .iter()
            .any(|violation| violation.contains("adoption_entrypoint_definitions"))
    );
}

#[test]
fn external_staffing_source_observer_ignores_comment_functions_and_string_slashes() {
    let observed = observe_sources([(
        "crates/tachi-server/src/tools/parser_synthetic.rs",
        r#"
            // fn fake() { handle_tachi_dispatch(server, params); }
            fn real() {
                let url = "https://example.invalid/{not_a_brace}";
                handle_tachi_dispatch(server, params);
            }
            fn next() { let _ = TachiDispatchParams { task }; }
        "#,
    )]);

    assert_eq!(observed["production_adopters"].as_array().unwrap().len(), 1);
    assert_eq!(observed["request_builders"].as_array().unwrap().len(), 1);
}
