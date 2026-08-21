use chrono::Utc;

use crate::MemoryServer;
use tachi_dispatch::{SignatureEvidenceRow, COUNTER_CLAUSE_TOP_N};
use tachi_params::{ExecutionGrant, ResolvedStaffAssignment, StaffAssignmentRequest};

use crate::dispatch_profile::ResolvedDispatchProfile;

pub(super) fn render_task_route_overlay(route: &crate::copilot_ops::TaskBriefRouting) -> String {
    let intent = route.intent;
    let mut lines = vec![
        "## Tachi task route".to_string(),
        format!("- intent: {intent}"),
    ];

    let labels = route
        .selected_sops
        .iter()
        .take(4)
        .filter_map(|sop| {
            let id = sop.get("id").and_then(|v| v.as_str())?;
            let reason = sop.get("reason").and_then(|v| v.as_str()).unwrap_or("");
            Some(if reason.is_empty() {
                format!("  - {id}")
            } else {
                format!("  - {id}: {reason}")
            })
        })
        .collect::<Vec<_>>();
    if !labels.is_empty() {
        lines.push("- selected_sops:".to_string());
        lines.extend(labels);
    }

    let steps = route
        .tool_plan
        .iter()
        .take(5)
        .filter_map(|step| {
            let tool = step.get("tool").and_then(|v| v.as_str())?;
            let action = step.get("action").and_then(|v| v.as_str()).unwrap_or("");
            let when = step.get("when").and_then(|v| v.as_str()).unwrap_or("");
            Some(format!("  - {tool}({action}): {when}"))
        })
        .collect::<Vec<_>>();
    if !steps.is_empty() {
        lines.push("- tool_plan:".to_string());
        lines.extend(steps);
    }

    lines.join("\n")
}

/// Resolve the normalized vendor family (e.g. "glm", "codex") for a dispatch
/// from `params.agent`/`params.model`, falling back to the resolved dispatch
/// profile's `backend`/model when the caller didn't set them explicitly.
/// `None` when no vendor is derivable or it resolves to `"unknown"` (which
/// never receives projection). Shared by [`resolve_vaccination_lane`] (#735)
/// and the lane-card seat-matching overlay (#1202/#993) so both projection
/// paths agree on exactly the same vendor for the same dispatch.
pub(super) fn resolve_dispatch_vendor(
    assignment: &ResolvedStaffAssignment,
    profile: &ResolvedDispatchProfile,
) -> Option<String> {
    let profile_def = profile
        .selected_profile
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .and_then(crate::dispatch_profile::resolve_dispatch_profile);
    let backend = (!assignment.selected_backend.trim().is_empty())
        .then_some(assignment.selected_backend.as_str())
        .unwrap_or(profile.agent.as_str());
    let model = assignment
        .selected_model
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .or_else(|| profile_def.and_then(tachi_dispatch::profile_resolved_model));
    let vendor = tachi_dispatch::normalize_vendor(backend, model.as_deref());
    (vendor != "unknown").then_some(vendor)
}

/// Resolve the `(role_class, vendor)` lane for a dispatch, or `None` when it is
/// not derivable or the vendor is `unknown` (which never receives projection).
fn resolve_vaccination_lane(
    request: &StaffAssignmentRequest,
    assignment: &ResolvedStaffAssignment,
    profile: &ResolvedDispatchProfile,
) -> Option<(String, String)> {
    let vendor = resolve_dispatch_vendor(assignment, profile)?;
    let role_source = profile.role.clone().or_else(|| {
        request
            .stage
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
    })?;
    let role = tachi_dispatch::dispatch_role_class(&role_source)?;
    Some((role.to_string(), vendor))
}

/// Format the vaccination-clause lines. Isolated from IO so the storage-failure
/// guard (frozen decision 3) is unit-testable: an `Err` load yields an empty
/// section and a logged warning — never a blocked or failed dispatch.
fn vaccination_overlay_lines(
    rows: Result<Vec<SignatureEvidenceRow>, String>,
    trust: Result<Option<&'static str>, String>,
    now_epoch: i64,
    role: &str,
    vendor: &str,
) -> Vec<String> {
    let rows = match rows {
        Ok(rows) => rows,
        Err(err) => {
            eprintln!(
                "[dispatch] signature projection skipped for lane {role}/{vendor} (packet assembles without vaccination clauses): {err}"
            );
            return Vec::new();
        }
    };
    let clauses = tachi_dispatch::project_counter_clauses(&rows, now_epoch, COUNTER_CLAUSE_TOP_N);
    let trust = trust.unwrap_or_else(|err| {
        eprintln!("[dispatch] self_report_trust skipped for vendor {vendor}: {err}");
        None
    });
    if clauses.is_empty() && trust.is_none() {
        return Vec::new();
    }
    let mut lines = vec![
        "## Frozen-spec vaccination clauses".to_string(),
        format!(
            "- lane: {role}/{vendor} (auto-projected from adjudicated failures; ACT-R-decayed, top {COUNTER_CLAUSE_TOP_N})"
        ),
    ];
    if let Some(trust) = trust {
        lines.push(format!(
            "- self_report_trust: {trust} — independently re-verify this vendor's self-reported CI/gate output before trusting it."
        ));
    }
    for clause in clauses {
        let severity = serde_json::to_value(clause.severity)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        lines.push(format!(
            "- [{severity}] {}: {}",
            clause.signature, clause.counter_clause
        ));
    }
    lines
}

/// Project the vendor-keyed counter-clauses for this dispatch's `(role, vendor)`
/// lane into the packet's frozen-spec section. Returns `None` when the lane is
/// not derivable or there is nothing to inject. Projection failure is swallowed
/// (frozen decision 3): the packet always assembles.
pub(super) fn render_vendor_vaccination_overlay(
    server: &MemoryServer,
    request: &StaffAssignmentRequest,
    assignment: &ResolvedStaffAssignment,
    profile: &ResolvedDispatchProfile,
) -> Option<String> {
    let (role, vendor) = resolve_vaccination_lane(request, assignment, profile)?;
    let rows = crate::signature_evidence::rows_for_lane(server, &role, &vendor);
    let trust = crate::signature_evidence::self_report_trust_for_vendor(server, &vendor);
    let lines = vaccination_overlay_lines(rows, trust, Utc::now().timestamp(), &role, &vendor);
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

pub(super) fn render_dispatch_profile_overlay(
    server: &MemoryServer,
    request: &StaffAssignmentRequest,
    assignment: &ResolvedStaffAssignment,
    _grant: &ExecutionGrant,
    profile: &ResolvedDispatchProfile,
    allowed_mcp_servers: &[String],
) -> String {
    let mut lines = vec!["## Dispatch profile".to_string()];
    if let Some(profile_name) = profile
        .selected_profile
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        lines.push(format!("- profile: {profile_name}"));
        if let Some(profile_def) = crate::dispatch_profile::resolve_dispatch_profile(profile_name) {
            lines.push("- skill_loadout:".to_string());
            match crate::dispatch_profile::profile_skill_loadout_json_for_server(
                server,
                profile_def,
            ) {
                Ok(loadout) => {
                    for label in [
                        "common_skills",
                        "signature_skills",
                        "projected_signature_skills",
                        "passive_traits",
                        "projected_passive_traits",
                        "forbidden_skills",
                    ] {
                        let items = loadout
                            .get(label)
                            .and_then(|value| value.as_array())
                            .into_iter()
                            .flatten()
                            .filter_map(|value| value.as_str())
                            .collect::<Vec<_>>();
                        if !items.is_empty() {
                            lines.push(format!("  - {label}: {}", items.join(", ")));
                        }
                    }
                    if let Some(status) = loadout
                        .get("projection")
                        .and_then(|projection| projection.get("status"))
                        .and_then(|status| status.as_str())
                    {
                        lines.push(format!("  - projection_status: {status}"));
                    }
                }
                Err(err) => {
                    lines.push(format!("  - loadout_error: {err}"));
                }
            }
            match crate::dispatch_profile::profile_evidence_contract_json_for_server(
                server,
                profile_def,
            ) {
                Ok(contract) => {
                    lines.push("- evidence_contract:".to_string());
                    for label in ["required", "projected_required"] {
                        let items = contract
                            .get(label)
                            .and_then(|value| value.as_array())
                            .into_iter()
                            .flatten()
                            .filter_map(|value| value.as_str())
                            .collect::<Vec<_>>();
                        if !items.is_empty() {
                            lines.push(format!("  - {label}: {}", items.join(", ")));
                        }
                    }
                    if let Some(status) = contract
                        .get("projection")
                        .and_then(|projection| projection.get("status"))
                        .and_then(|status| status.as_str())
                    {
                        lines.push(format!("  - evidence_projection_status: {status}"));
                    }
                }
                Err(err) => {
                    lines.push(format!("  - evidence_contract_error: {err}"));
                }
            }
            match crate::dispatch_profile::profile_json_for_server(server, profile_def) {
                Ok(profile_json) => {
                    let card_fields = ["projected_weak_against", "demotion_targets"];
                    let mut emitted = false;
                    for label in card_fields {
                        let items = profile_json
                            .get(label)
                            .and_then(|value| value.as_array())
                            .into_iter()
                            .flatten()
                            .filter_map(|value| value.as_str())
                            .collect::<Vec<_>>();
                        if !items.is_empty() {
                            if !emitted {
                                lines.push("- profile_card_evolution:".to_string());
                                emitted = true;
                            }
                            lines.push(format!("  - {label}: {}", items.join(", ")));
                        }
                    }
                }
                Err(err) => {
                    lines.push(format!("  - profile_card_error: {err}"));
                }
            }
        }
    }
    if !assignment.selected_backend.trim().is_empty() {
        lines.push(format!("- backend: {}", assignment.selected_backend));
    }
    if let Some(stage) = request.stage.as_deref().filter(|s| !s.trim().is_empty()) {
        lines.push(format!("- stage: {stage}"));
    }
    if let Some(tool_profile) = profile
        .tool_profile
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        lines.push(format!("- tachi_tool_profile: {tool_profile}"));
    }
    if let Some(flow_id) = request.flow_id.as_deref().filter(|s| !s.trim().is_empty()) {
        lines.push(format!("- flow_id: {flow_id}"));
    }
    if let Some(issue_ref) = request
        .issue_ref
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        lines.push(format!("- issue_ref: {issue_ref}"));
    }
    if let Some(pr_ref) = request.pr_ref.as_deref().filter(|s| !s.trim().is_empty()) {
        lines.push(format!("- pr_ref: {pr_ref}"));
    }
    if let Ok(compact) = serde_json::to_string(&profile.mcp_access) {
        lines.push(format!("- tool_access: {compact}"));
    }
    if !allowed_mcp_servers.is_empty() {
        lines.push(format!(
            "- allowed_mcp_servers: {}",
            allowed_mcp_servers.join(", ")
        ));
    }
    lines.push("- completion_report: report files changed, tests run, blockers, and any unavailable MCP/GitHub context explicitly.".to_string());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g4_storage_error_yields_empty_section_not_a_failure() {
        // A simulated storage failure during projection must degrade to an empty
        // section (packet assembles) — never propagate an error (frozen decision 3).
        let lines = vaccination_overlay_lines(
            Err("simulated store failure".to_string()),
            Ok(None),
            0,
            "implementer",
            "glm",
        );
        assert!(
            lines.is_empty(),
            "storage error must produce no clauses: {lines:?}"
        );
    }

    #[test]
    fn trust_error_is_swallowed_but_clauses_still_project() {
        // If only the trust lookup errors, clauses still project and the packet
        // assembles without the trust line.
        let now = chrono::Utc::now().timestamp();
        let rows = vec![SignatureEvidenceRow {
            kind: tachi_dispatch::SignatureRowKind::Signature,
            signature: "fake_security_fix".to_string(),
            severity: Some(tachi_dispatch::Severity::High),
            evidence_ref: None,
            recorded_at_epoch: now,
        }];
        let lines = vaccination_overlay_lines(
            Ok(rows),
            Err("trust lookup failed".to_string()),
            now,
            "implementer",
            "glm",
        );
        assert!(lines.iter().any(|l| l.contains("fake_security_fix")));
        assert!(!lines.iter().any(|l| l.contains("self_report_trust")));
    }
}
