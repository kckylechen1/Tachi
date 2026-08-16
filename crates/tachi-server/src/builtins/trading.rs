use super::helpers::make_skill_capability;
use super::*;

pub(super) fn builtin_trading_skills() -> Result<Vec<HubCapability>, String> {
    Ok(vec![
        make_skill_capability(
            "skill:trading-pre-market-briefing",
            "trading/pre-market-briefing",
            "Pre-market operating checklist for trading agents.",
            json!({
                "prompt": "Run the pre-market briefing checklist. Input:\n{{input}}",
                "content": "# /skills/trading/pre-market-briefing\n\n1. Review yesterday's daily summaries and unfinished items\n2. Pull current position vs shadow position diff\n3. Re-state regime and defense line\n4. Name the trades that are allowed today",
                "policy": { "visibility": "discoverable" },
                "domain": "trading",
                "skill_path": "/skills/trading/pre-market-briefing",
                "retention_policy": "durable",
                "tags": ["trading", "briefing", "preset"]
            }),
        )?,
        make_skill_capability(
            "skill:trading-regime-playbook",
            "trading/regime-playbook",
            "Market regime playbook template.",
            json!({
                "prompt": "Select or update the regime playbook. Input:\n{{input}}",
                "content": "# /skills/trading/regime-playbook\n\nMaintain playbooks for acceleration / pullback / range / bear.\nFor each regime define:\n- Setup quality\n- Entry / stop logic\n- Position sizing guardrail\n- What invalidates the playbook",
                "policy": { "visibility": "discoverable" },
                "domain": "trading",
                "skill_path": "/skills/trading/regime-playbook",
                "retention_policy": "durable",
                "memory_path_templates": ["/trading/regime/{date}"],
                "tags": ["trading", "regime", "preset"]
            }),
        )?,
        make_skill_capability(
            "skill:trading-post-trade-review",
            "trading/post-trade-review",
            "Post-trade review template.",
            json!({
                "prompt": "Write a post-trade review using this template. Input:\n{{input}}",
                "content": "# /skills/trading/post-trade-review\n\nFor each trade capture:\n- Snapshot at entry\n- Expected path\n- Actual path\n- Variance explanation\n- Whether to extract a permanent lesson",
                "policy": { "visibility": "discoverable" },
                "domain": "trading",
                "skill_path": "/skills/trading/post-trade-review",
                "retention_policy": "durable",
                "tags": ["trading", "review", "preset"]
            }),
        )?,
        make_skill_capability(
            "skill:trading-lesson-extractor",
            "trading/lesson-extractor",
            "Turn failed or costly trades into permanent lessons.",
            json!({
                "prompt": "Extract a durable trading lesson from this trade. Input:\n{{input}}",
                "content": "# /skills/trading/lesson-extractor\n\nClassify the miss: early entry / stop placement / regime misread / thesis drift.\nThen convert it into an actionable rule revision.\n\n## Storage\nWrite to `/trading/lesson/{ticker}`.\nRetention: permanent.",
                "policy": { "visibility": "discoverable" },
                "domain": "trading",
                "skill_path": "/skills/trading/lesson-extractor",
                "retention_policy": "permanent",
                "memory_path_templates": ["/trading/lesson/{ticker}"],
                "tags": ["trading", "lesson", "preset"]
            }),
        )?,
        make_skill_capability(
            "skill:trading-position-review",
            "trading/position-review",
            "Periodic position review template.",
            json!({
                "prompt": "Run a position review. Input:\n{{input}}",
                "content": "# /skills/trading/position-review\n\nReview hold / reduce / exit decisions against:\n- Trend integrity\n- Relative strength\n- Stop distance\n- Thesis drift\n- Liquidity risk",
                "policy": { "visibility": "discoverable" },
                "domain": "trading",
                "skill_path": "/skills/trading/position-review",
                "retention_policy": "durable",
                "tags": ["trading", "position", "preset"]
            }),
        )?,
        make_skill_capability(
            "skill:trading-position-snapshot",
            "trading/position-snapshot",
            "Ephemeral position snapshot template and integration contract.",
            json!({
                "prompt": "Capture a position snapshot. Input:\n{{input}}",
                "content": "# /skills/trading/position-snapshot\n\nCapture current holdings, cost basis, stop, and shadow-book diff.\n\n## Retention\nEphemeral unless promoted into a thesis or lesson.\n\n## Integration point\nAfter `PortfolioManager.buy()` / `PortfolioManager.sell()`, save a concise position snapshot with:\n`tachi_memory(action=\"save\", text=\"Position snapshot: {holdings, cost_basis, stop, shadow_book_diff}\", domain=\"trading\", path=f\"/trading/journal/{ticker}\", retention_policy=\"ephemeral\")`",
                "policy": { "visibility": "discoverable" },
                "domain": "trading",
                "skill_path": "/skills/trading/position-snapshot",
                "retention_policy": "ephemeral",
                "memory_path_templates": ["/trading/position", "/trading/shadow/position"],
                "integration_points": [
                    {
                        "system": "PortfolioManager.buy()/sell()",
                        "call": "tachi_memory(action='save', text='Position snapshot: {holdings, cost_basis, stop, shadow_book_diff}', domain='trading', path='/trading/journal/{ticker}', retention_policy='ephemeral')"
                    }
                ],
                "tags": ["trading", "position", "preset"]
            }),
        )?,
    ])
}
