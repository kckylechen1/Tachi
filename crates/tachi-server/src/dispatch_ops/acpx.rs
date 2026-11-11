//! Retired transport boundary. Historical receipts remain readable, never executable.
use serde_json::Value;
use std::{path::Path, time::Duration};

pub(super) fn is_acpx_transport(transport: &str) -> bool {
    tachi_dispatch::transport_kind(transport) == tachi_dispatch::TransportKind::Acpx
}

pub(crate) async fn run_acpx_control_from_status(
    _run_dir: &Path,
    _status: &Value,
    _control: &str,
    _timeout: Duration,
) -> Result<Value, String> {
    Err("acpx controller is retired; historical receipt visibility does not grant control or authorize redispatch".into())
}
