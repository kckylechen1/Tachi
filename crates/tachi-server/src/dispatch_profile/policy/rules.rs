use super::super::*;

pub(in crate::dispatch_profile) fn load_route_policy_rule_loadout(
    server: &MemoryServer,
    risk: &DispatchRisk,
) -> Result<RoutePolicyRuleLoadout, String> {
    let records = server.with_global_store_read(|store| {
        store
            .list_state(ROUTE_POLICY_RULE_NS)
            .map_err(|e| format!("list route policy rules: {e}"))
    })?;
    let records = records
        .into_iter()
        .map(|row| RoutePolicyRuleRecord {
            proposal_id: row.key,
            value_json: row.value_json,
        })
        .collect::<Vec<_>>();

    Ok(tachi_dispatch::build_route_policy_rule_loadout(
        &records, risk,
    ))
}
