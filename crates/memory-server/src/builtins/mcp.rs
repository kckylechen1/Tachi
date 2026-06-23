use super::helpers::{make_local_mcp_capability, make_mcp_capability};
use super::*;

pub(super) fn builtin_mcp_capabilities() -> Result<Vec<HubCapability>, String> {
    Ok(vec![
        make_mcp_capability(
            "mcp:web-reader",
            "web-reader",
            "BigModel reader MCP for turning URLs into markdown.",
            "https://open.bigmodel.cn/api/mcp/web_reader/mcp",
            true,
            Some("general"),
            Some("/wiki/general/web-reader"),
        )?,
        make_mcp_capability(
            "mcp:zread",
            "zread",
            "BigModel zread MCP for repo/document reading.",
            "https://open.bigmodel.cn/api/mcp/zread/mcp",
            true,
            Some("coding"),
            Some("/wiki/coding/zread"),
        )?,
        make_mcp_capability(
            "mcp:web-search",
            "web-search",
            "BigModel search MCP for web search results.",
            "https://open.bigmodel.cn/api/mcp/web_search_prime/mcp",
            true,
            Some("general"),
            Some("/wiki/general/web-search"),
        )?,
        make_local_mcp_capability(
            "mcp:vision",
            "vision",
            "BigModel vision MCP for screenshot and chart analysis (local stdio package).",
            "npx",
            &["-y", "@z_ai/mcp-server@latest"],
            json!({
                "Z_AI_API_KEY": "${vault:ZAI_API_KEY|BIGMODEL_API_KEY|REASONING_API_KEY}",
                "Z_AI_MODE": "ZAI"
            }),
            false,
        )?,
    ])
}
