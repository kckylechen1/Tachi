// pack_ops.rs — Pack system server operations
//
// Handles pack registration, listing, removal, and agent projection.

use crate::server_state::MemoryServer;
use crate::tool_params::{
    PackGetParams, PackListParams, PackProjectParams, PackRegisterParams, PackRemoveParams,
    ProjectionListParams,
};
use crate::utils::sanitize_safe_path_name;
use chrono::Utc;
use memory_core::{AgentKind, AgentProjection, Pack, PackAssetRef, PackManifest, PackOverlay};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

mod assets;
mod handlers;
mod manifest;
mod projection;
mod skills;
mod types;

use self::assets::*;
pub(crate) use self::handlers::*;
use self::manifest::*;
use self::projection::*;
use self::skills::*;
use self::types::*;
