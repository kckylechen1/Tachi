use super::{make_entry, make_mcp_capability, make_server, shell_params};
use crate::tool_params::{
    SandboxCheckParams, SandboxGetPolicyParams, SandboxListPoliciesParams, SandboxSetPolicyParams,
    SandboxSetRuleParams, SearchMemoryParams, TachiSearchParams,
};
use memcore::HubCapability;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};

mod facade;
mod policy;
mod rules;
mod runtime;
