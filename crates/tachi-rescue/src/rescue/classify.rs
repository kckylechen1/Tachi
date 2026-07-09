use super::types::{RescueAssignment, SourceRow};

pub fn classify(row: &SourceRow) -> RescueAssignment {
    let p = row.path.to_ascii_lowercase();
    let t_lc = row.text.to_ascii_lowercase();

    // --- 1. Trading / hapi (highest priority — must be isolated). ---
    // hapi is a private trading agent; treat any hapi-flavoured path or text
    // as equity_trading domain regardless of where it ended up.
    if p.starts_with("/hapi/")
        || p == "/hapi"
        || p.starts_with("/project/quant/hapi-history")
        || p.starts_with("/project/股票交易")
        || p.starts_with("/project/交易策略")
        || p.starts_with("/project/quant/stocks")
        || p.starts_with("/project/quant/positions")
    {
        return RescueAssignment {
            source_id: row.id.clone(),
            source_path: row.path.clone(),
            target: "hapi".into(),
            reason: format!("path-prefix trading: {}", row.path),
            trading: true,
        };
    }

    // --- 2. Quant analyzer. ---
    if p.starts_with("/project/quant_analyzer_2026")
        || p.starts_with("/project/quant-analyzer")
        || p.starts_with("/quant_analyzer_2026")
        || p.starts_with("/project/quant/")
        || p == "/project/quant"
    {
        return RescueAssignment {
            source_id: row.id.clone(),
            source_path: row.path.clone(),
            target: "quant".into(),
            reason: format!("path-prefix quant: {}", row.path),
            trading: false,
        };
    }

    // --- 3. Hyperion. ---
    if p.starts_with("/project/hyperion") || p.starts_with("/antigravity/hyperion") {
        return RescueAssignment {
            source_id: row.id.clone(),
            source_path: row.path.clone(),
            target: "hyperion".into(),
            reason: format!("path-prefix hyperion: {}", row.path),
            trading: false,
        };
    }

    // --- 4. OpenClaw. ---
    if p.starts_with("/project/openclaw") || p.starts_with("/openclaw") {
        return RescueAssignment {
            source_id: row.id.clone(),
            source_path: row.path.clone(),
            target: "openclaw".into(),
            reason: format!("path-prefix openclaw: {}", row.path),
            trading: false,
        };
    }

    // --- 5. Sigil. ---
    if p.starts_with("/project/sigil") || p.starts_with("/sigil") {
        return RescueAssignment {
            source_id: row.id.clone(),
            source_path: row.path.clone(),
            target: "sigil".into(),
            reason: format!("path-prefix sigil: {}", row.path),
            trading: false,
        };
    }

    // --- 6. Tachi (own infrastructure notes). ---
    if p.starts_with("/tachi/") || p == "/tachi" {
        return RescueAssignment {
            source_id: row.id.clone(),
            source_path: row.path.clone(),
            target: "tachi".into(),
            reason: format!("path-prefix tachi: {}", row.path),
            trading: false,
        };
    }

    // --- 7. Keyword fallback for ambiguous /project/* and root entries. ---
    // We check the body text only after path rules so well-pathed rows always
    // win. These keyword sets are intentionally narrow to avoid misrouting.
    let body_first_512: String = t_lc.chars().take(512).collect();
    let combined = format!("{} {}", p, body_first_512);

    let kw_trading = [
        "hapi",
        "持仓",
        "买入",
        "卖出",
        "止损",
        "止盈",
        "策略",
        "回测",
        "trading agent",
        "stock symbol",
        "ticker",
    ];
    if kw_trading.iter().any(|k| combined.contains(k)) {
        return RescueAssignment {
            source_id: row.id.clone(),
            source_path: row.path.clone(),
            target: "hapi".into(),
            reason: "keyword: trading vocab in body".into(),
            trading: true,
        };
    }

    let kw_quant = [
        "quant_analyzer",
        "quant-analyzer",
        "v8 engine",
        "v8-engine",
        "score_setup",
    ];
    if kw_quant.iter().any(|k| combined.contains(k)) {
        return RescueAssignment {
            source_id: row.id.clone(),
            source_path: row.path.clone(),
            target: "quant".into(),
            reason: "keyword: quant analyzer".into(),
            trading: false,
        };
    }

    // --- 8. Antigravity-specific or residual (user prefs, kanban, notes). ---
    // Everything that doesn't match a project bucket lands here so the
    // antigravity DB remains the catch-all for the orchestrator's own
    // memory + any rows we couldn't confidently route.
    RescueAssignment {
        source_id: row.id.clone(),
        source_path: row.path.clone(),
        target: "antigravity".into(),
        reason: "fallback: non-project / residual".into(),
        trading: false,
    }
}
