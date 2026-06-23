use super::{make_server, TempHomeGuard};
use crate::tool_params::{
    PackGetParams, PackListParams, PackProjectParams, PackRegisterParams, PackRemoveParams,
    ProjectionListParams,
};
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};

mod project_openclaw;
mod project_safety;
mod project_skills;
mod projection_list;
mod registry;
