use rmcp::model::{InitializeRequestParams, ProtocolVersion};
use rmcp::service::{RequestContext, RoleServer};

const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
const META_CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";

/// The only MCP lifecycle modes Tachi implements.
///
/// Legacy peers derive authority from their initialized RMCP session. Modern
/// peers carry the SDK's typed client context on every request; that context
/// is request-scoped and must never be promoted into durable session state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum McpPeerMode {
    Legacy,
    Modern20260728,
}

impl McpPeerMode {
    pub(crate) fn from_context(
        context: &RequestContext<RoleServer>,
    ) -> Result<Self, rmcp::ErrorData> {
        let meta = &context.meta;
        let declared_version = meta.protocol_version();
        let has_inline_client_context = [
            META_PROTOCOL_VERSION,
            META_CLIENT_CAPABILITIES,
            META_CLIENT_INFO,
        ]
        .iter()
        .any(|key| meta.0.contains_key(*key));

        if declared_version.as_ref() == Some(&ProtocolVersion::V_2026_07_28) {
            let missing = meta.missing_required_keys(&ProtocolVersion::V_2026_07_28);
            if !missing.is_empty() {
                return Err(rmcp::ErrorData::invalid_params(
                    format!(
                        "request _meta is missing or has malformed required fields: {}",
                        missing.join(", ")
                    ),
                    None,
                ));
            }
            if meta.0.contains_key(META_CLIENT_INFO) && meta.client_info().is_none() {
                return Err(rmcp::ErrorData::invalid_params(
                    format!(
                        "request _meta field {META_CLIENT_INFO} is present but malformed"
                    ),
                    None,
                ));
            }
            // Read the typed SDK values rather than treating key presence as
            // capability proof. Decode optional clientInfo directly from this
            // request: `context.client_info()` may fall back to initialized
            // peer info when request metadata is not required.
            let _client_capabilities = context
                .client_capabilities()
                .expect("validated modern client capabilities");
            return Ok(Self::Modern20260728);
        }

        if let Some(version) = declared_version {
            if version >= ProtocolVersion::V_2026_07_28 {
                return Err(rmcp::ErrorData::unsupported_protocol_version(
                    version,
                    supported_protocol_versions(),
                ));
            }
            return Err(rmcp::ErrorData::invalid_request(
                "legacy MCP versions require initialize session semantics",
                None,
            ));
        }

        if has_inline_client_context
            || context
                .protocol_version()
                .is_some_and(|version| version >= ProtocolVersion::V_2026_07_28)
        {
            return Err(rmcp::ErrorData::invalid_params(
                format!(
                    "request _meta is missing or has malformed required fields: {}",
                    meta.missing_required_keys(&ProtocolVersion::V_2026_07_28)
                        .join(", ")
                ),
                None,
            ));
        }

        Ok(Self::Legacy)
    }

    pub(crate) fn require_modern(self) -> Result<Self, rmcp::ErrorData> {
        match self {
            Self::Modern20260728 => Ok(self),
            Self::Legacy => Err(rmcp::ErrorData::invalid_request(
                "server/discover requires MCP 2026-07-28 per-request metadata",
                None,
            )),
        }
    }
}

pub(crate) fn supported_protocol_versions() -> &'static [ProtocolVersion] {
    ProtocolVersion::known_up_to(&ProtocolVersion::V_2026_07_28)
}

pub(crate) fn reject_modern_initialize(
    request: &InitializeRequestParams,
) -> Result<(), rmcp::ErrorData> {
    if request.protocol_version >= ProtocolVersion::V_2026_07_28 {
        return Err(rmcp::ErrorData::unsupported_protocol_version(
            request.protocol_version.clone(),
            ProtocolVersion::known_up_to(&ProtocolVersion::V_2025_11_25),
        ));
    }
    Ok(())
}
