//! Operator-owned egress. Workers cannot supply local authority or a transport.
use super::config::{read_operator_file, GatewayConfig, Peer};
use super::profile::{self, RemoteJob, DELEGATION, PROVENANCE};
use super::transport;
use memcore::db::a2a::{
    self, A2aTransitionActor, RemoteSubmission, RemoteSubmissionAttempt, RemoteSubmissionBinding,
    RemoteSubmissionResult, REMOTE_CLOCK_SKEW_TOLERANCE_SECS,
};
use serde_json::{json, Value};
use std::error::Error;
use std::path::Path;
use tachi_bootstrap::cli::RemoteA2aAction;

fn binding(
    config: &GatewayConfig,
    job: &RemoteJob,
) -> Result<RemoteSubmissionBinding, Box<dyn Error>> {
    if job.sender != config.agent_identity_id || job.trust_domain != config.trust_domain {
        return Err("remote job does not match the operator's origin identity/domain".into());
    }
    let peer = peer(config, &job.recipient)?;
    let grant = peer
        .outbound
        .iter()
        .find(|grant| grant.repository == job.repository && grant.jobs.contains(&job.job))
        .ok_or("remote job has no current outbound repository/job grant")?;
    Ok(RemoteSubmissionBinding {
        issuer_id: config.issuer_id.clone(),
        canonical_project: grant.canonical_project.clone(),
        destination: peer.agent_identity_id.clone(),
        destination_host_id: peer.host_id.clone(),
        destination_key_sha256: peer.public_key_sha256.clone(),
        endpoint: peer
            .endpoint
            .clone()
            .ok_or("remote destination endpoint unavailable")?,
        repository: job.repository.clone(),
        job: job.job.as_str().into(),
        head_sha: job.head_sha.clone(),
        origin_claim_id: job.origin_claim.claim_id.clone(),
        origin_claim_version: job.origin_claim.transition_version as i64,
    })
}

fn peer<'a>(config: &'a GatewayConfig, id: &str) -> Result<&'a Peer, Box<dyn Error>> {
    config
        .peers
        .iter()
        .find(|peer| peer.agent_identity_id == id)
        .ok_or_else(|| "remote destination is no longer enrolled".into())
}

fn actor(
    server: &crate::MemoryServer,
    config: &GatewayConfig,
) -> Result<A2aTransitionActor, Box<dyn Error>> {
    crate::claims_ops::admit_agent_connection(
        server,
        Some(config.agent_identity_id.clone()),
        true,
    )?;
    let (Some(identity), connection_id, state) = server
        .work_claim_connection()
        .ok_or("origin admission unavailable")?
    else {
        return Err("origin admission unavailable".into());
    };
    if identity != config.agent_identity_id || state != "self_asserted" {
        return Err("origin admission does not match its operator".into());
    }
    Ok(A2aTransitionActor {
        agent_identity_id: identity,
        connection_id,
    })
}

fn read(
    server: &crate::MemoryServer,
    actor: &A2aTransitionActor,
    id: &str,
) -> Result<RemoteSubmission, Box<dyn Error>> {
    Ok(server.with_global_store(|store| {
        a2a::read_remote_submission(store.connection(), actor, id)
            .map_err(|error| error.to_string())
    })?)
}

fn request_bytes(submission: &RemoteSubmission, body: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    // The business bytes remain canonical and immutable. Transport correlation
    // is also stable, but is deliberately outside the business digest.
    let request = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"SendMessage\",\"params\":{{\"message\":{{\"messageId\":{id},\"role\":\"ROLE_USER\",\"parts\":[{{\"data\":{body}}}]}},\"configuration\":{{\"returnImmediately\":true,\"historyLength\":0}}}}}}",
        id = serde_json::to_string(&submission.submission_id)?,
    ).into_bytes();
    if request.len() > profile::MAX_REQUEST_BYTES {
        return Err("remote RPC exceeds 32 KiB".into());
    }
    Ok(request)
}

pub(super) async fn run(
    server: &crate::MemoryServer,
    authority: &crate::MemoryServer,
    config: &GatewayConfig,
    action: &RemoteA2aAction,
) -> Result<(), Box<dyn Error>> {
    let actor = actor(server, config)?;
    let result = match action {
        RemoteA2aAction::Submit {
            config: path,
            request,
            prepare_only,
        } => {
            let bytes = read_operator_file(request, true)
                .map_err(|_| "remote request file unavailable or unsafe")?;
            let job =
                profile::parse_job(&bytes).map_err(|_| "invalid closed remote job profile")?;
            let body = job
                .canonical_body()
                .map_err(|_| "remote job cannot be canonicalized")?;
            let bound = binding(config, &job)?;
            require_gateway_key(authority, &bound)?;
            // Account for RPC framing before creating a durable row.
            if body.len() + 512 > profile::MAX_REQUEST_BYTES {
                return Err("remote job leaves insufficient bounded RPC space".into());
            }
            let prepared = server.with_global_store(|store| {
                a2a::prepare_remote_submission(store.connection(), &actor, &bound, &body)
                    .map_err(|error| error.to_string())
            })?;
            if *prepare_only {
                prepared
            } else {
                resume(
                    server,
                    authority,
                    config,
                    path,
                    &actor,
                    &prepared.submission_id,
                )
                .await?
            }
        }
        RemoteA2aAction::Resume {
            config: path,
            submission,
        } => resume(server, authority, config, path, &actor, submission).await?,
        RemoteA2aAction::Status { submission, .. } => read(server, &actor, submission)?,
        _ => unreachable!(),
    };
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn reload(path: &Path, original: &GatewayConfig) -> Result<GatewayConfig, Box<dyn Error>> {
    let config = GatewayConfig::load(path)
        .map_err(|_| "current remote operator configuration unavailable")?;
    if config.home != original.home
        || config.origin_database != original.origin_database
        || config.agent_identity_id != original.agent_identity_id
        || config.issuer_id != original.issuer_id
        || config.trust_domain != original.trust_domain
        || config.host_id != original.host_id
    {
        return Err(
            "origin configuration changed; submission remains on its original store and principal"
                .into(),
        );
    }
    Ok(config)
}

async fn resume(
    server: &crate::MemoryServer,
    authority: &crate::MemoryServer,
    original: &GatewayConfig,
    path: &Path,
    actor: &A2aTransitionActor,
    id: &str,
) -> Result<RemoteSubmission, Box<dyn Error>> {
    loop {
        let current = read(server, actor, id)?;
        if !current.retry_allowed {
            return Ok(current);
        }
        let body = current
            .body
            .as_deref()
            .ok_or("frozen remote submission body unavailable")?;
        let job = profile::parse_job(body.as_bytes())
            .map_err(|_| "frozen remote submission profile is invalid")?;
        let config = reload(path, original)?;
        let bound = binding(&config, &job)?;
        require_gateway_key(authority, &bound)?;
        let attempt = server.with_global_store(|store| {
            a2a::reserve_remote_submission(
                store.connection(),
                actor,
                &bound,
                &config.trust_domain,
                id,
            )
            .map_err(|error| error.to_string())
        })?;
        let Some(attempt) = attempt else {
            return read(server, actor, id);
        };
        let outcome = send(server, authority, original, path, actor, &config, &attempt).await;
        let finished = server.with_global_store(|store| {
            a2a::finish_remote_submission(store.connection(), actor, &attempt, &outcome)
                .map_err(|error| error.to_string())
        })?;
        if !finished.retry_allowed {
            return Ok(finished);
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

fn unknown(error_class: &'static str, retry_allowed: bool) -> RemoteSubmissionResult {
    RemoteSubmissionResult::Unknown {
        error_class,
        retry_allowed,
    }
}

/// Answer of the gateway authority ledger about the destination key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GatewayKeyCheck {
    Current,
    /// The ledger says the key is retired; asking again cannot change that.
    Revoked,
    /// The ledger could not be read. Nothing was sent, so a later resume may
    /// ask again instead of recording a permanent authority loss.
    Unavailable,
}

/// What the origin store established about the claim and key of an attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OriginAuthority {
    Current,
    /// The claim, key or attempt no longer authorizes egress.
    Changed,
    /// Storage could not answer.
    Unavailable,
}

fn revoked_key(
    store: &mut memcore::MemoryStore,
    binding: &RemoteSubmissionBinding,
) -> Result<bool, String> {
    memcore::verified_mtls_key_is_revoked(
        store.connection(),
        &binding.issuer_id,
        &format!("attestation:mtls-peer:{}", binding.destination_key_sha256),
    )
    .map_err(|error| error.to_string())
}

fn classify_key_check(answer: Result<bool, String>) -> GatewayKeyCheck {
    match answer {
        Ok(true) => GatewayKeyCheck::Revoked,
        Ok(false) => GatewayKeyCheck::Current,
        Err(_) => GatewayKeyCheck::Unavailable,
    }
}

fn gateway_key_check(
    authority: &crate::MemoryServer,
    binding: &RemoteSubmissionBinding,
) -> GatewayKeyCheck {
    classify_key_check(authority.with_global_store_read(|store| revoked_key(store, binding)))
}

fn require_gateway_key(
    authority: &crate::MemoryServer,
    binding: &RemoteSubmissionBinding,
) -> Result<(), Box<dyn Error>> {
    match gateway_key_check(authority, binding) {
        GatewayKeyCheck::Current => Ok(()),
        GatewayKeyCheck::Revoked => {
            Err("destination key is revoked by the gateway authority".into())
        }
        GatewayKeyCheck::Unavailable => Err("gateway key revocation state unavailable".into()),
    }
}

fn classify_attempt_check(answer: Result<bool, memcore::MemoryError>) -> OriginAuthority {
    match answer {
        Ok(true) => OriginAuthority::Current,
        Ok(false) => OriginAuthority::Changed,
        Err(memcore::MemoryError::InvalidArg(_)) => OriginAuthority::Changed,
        Err(_) => OriginAuthority::Unavailable,
    }
}

/// Decision of the local authority rechecks, which run after the TLS health
/// round trip and immediately before the task POST. Only an authority that
/// really changed ends the submission. A store that could not answer keeps
/// the attempt retryable, because no bytes were sent.
fn admit_egress(
    key: GatewayKeyCheck,
    origin: Result<OriginAuthority, String>,
) -> Result<(), RemoteSubmissionResult> {
    let (class, retry_allowed) = match (key, origin) {
        (GatewayKeyCheck::Current, Ok(OriginAuthority::Current)) => return Ok(()),
        (GatewayKeyCheck::Revoked, _) => ("destination_authority_changed", false),
        (GatewayKeyCheck::Unavailable, _) => ("destination_authority_unavailable", true),
        (GatewayKeyCheck::Current, Ok(OriginAuthority::Changed)) => {
            ("origin_authority_changed", false)
        }
        (GatewayKeyCheck::Current, Ok(OriginAuthority::Unavailable))
        | (GatewayKeyCheck::Current, Err(_)) => ("origin_authority_unavailable", true),
    };
    Err(unknown(class, retry_allowed))
}

async fn send(
    server: &crate::MemoryServer,
    authority: &crate::MemoryServer,
    original: &GatewayConfig,
    path: &Path,
    actor: &A2aTransitionActor,
    config: &GatewayConfig,
    attempt: &RemoteSubmissionAttempt,
) -> RemoteSubmissionResult {
    let Ok(peer) = peer(config, &attempt.submission.binding.destination) else {
        return unknown("operator_binding_changed", false);
    };
    let Ok(client) = transport::pinned_client(config, peer) else {
        return unknown("transport_configuration", false);
    };
    // A health request carries no task/context. TLS chain, hostname and enrolled
    // key are checked before even those HTTP bytes, including on a reconnect.
    if transport::probe_with_client(config, peer, &client)
        .await
        .is_err()
    {
        return unknown("destination_admission_unavailable", true);
    }
    let current_policy = reload(path, original).and_then(|config| {
        let job =
            profile::parse_job(attempt.body.as_bytes()).map_err(|_| "frozen profile invalid")?;
        binding(&config, &job)
    });
    if !current_policy.is_ok_and(|current| current == attempt.submission.binding) {
        return unknown("operator_binding_changed", false);
    }
    let key = classify_key_check(
        authority.with_global_store_read(|store| revoked_key(store, &attempt.submission.binding)),
    );
    let origin = server.with_global_store(|store| {
        Ok(classify_attempt_check(
            a2a::remote_submission_attempt_is_current(store.connection(), actor, attempt),
        ))
    });
    if let Err(result) = admit_egress(key, origin) {
        return result;
    }
    let Ok(bytes) = request_bytes(&attempt.submission, &attempt.body) else {
        return unknown("invalid_frozen_request", false);
    };
    let Ok(url) =
        url::Url::parse(&attempt.submission.binding.endpoint).and_then(|url| url.join("/rpc"))
    else {
        return unknown("transport_configuration", false);
    };
    let response = client
        .post(url)
        .header("content-type", "application/a2a+json")
        .header("a2a-version", "1.0")
        .header("a2a-extensions", format!("{DELEGATION},{PROVENANCE}"))
        .body(bytes)
        .send()
        .await;
    let Ok(mut response) = response else {
        return unknown("transport_unknown", true);
    };
    if !response.status().is_success() {
        return unknown("remote_http_refusal", response.status().is_server_error());
    }
    let header = |name| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
    };
    if header("a2a-version") != Some("1.0")
        || header("content-type").and_then(|value| value.split(';').next())
            != Some("application/a2a+json")
        || !header("a2a-extensions").is_some_and(|value| {
            let values: Vec<_> = value.split(',').map(str::trim).collect();
            values.contains(&DELEGATION) && values.contains(&PROVENANCE)
        })
        || response.headers().contains_key("content-encoding")
    {
        return unknown("invalid_response", false);
    }
    let mut bytes = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) if bytes.len() + chunk.len() <= 8192 => bytes.extend_from_slice(&chunk),
            Ok(None) => break,
            Ok(Some(_)) => return unknown("invalid_response", false),
            Err(_) => return unknown("transport_unknown", true),
        }
    }
    interpret_response(&bytes, &attempt.submission)
}

fn keys(value: &Value, expected: &[&str]) -> bool {
    value.as_object().is_some_and(|object| {
        object.len() == expected.len() && expected.iter().all(|key| object.contains_key(*key))
    })
}

fn interpret_response(bytes: &[u8], submission: &RemoteSubmission) -> RemoteSubmissionResult {
    let Ok(response) = profile::parse_unique(bytes) else {
        return unknown("invalid_response", false);
    };
    if response["jsonrpc"] != "2.0" || response["id"] != submission.submission_id {
        return unknown("invalid_response", false);
    }
    if keys(&response, &["jsonrpc", "id", "error"]) {
        // Even a permanent RPC refusal is not a host execution receipt. An
        // earlier lost response could have left an existing task at that key.
        return unknown(
            "remote_rpc_refusal",
            response["error"]["code"].as_i64() == Some(-32603),
        );
    }
    if !keys(&response, &["jsonrpc", "id", "result"]) || !keys(&response["result"], &["task"]) {
        return unknown("invalid_response", false);
    }
    let task = &response["result"]["task"];
    if !valid_refusal(task, submission) {
        return unknown("invalid_response", false);
    }
    RemoteSubmissionResult::Refused {
        task_id: task["id"].as_str().unwrap().into(),
        receipt: task.clone(),
    }
}

fn valid_refusal(task: &Value, submission: &RemoteSubmission) -> bool {
    let token = |value: &Value| {
        value
            .as_str()
            .is_some_and(|text| profile::opaque_token(text, 128))
    };
    if !keys(task, &["id", "contextId", "status", "metadata"])
        || !token(&task["id"])
        || task["contextId"] != format!("context:{}", task["id"].as_str().unwrap())
        || !keys(&task["status"], &["state", "timestamp"])
        || task["status"]["state"] != "TASK_STATE_REJECTED"
        || !keys(&task["metadata"], &[PROVENANCE])
    {
        return false;
    }
    let provenance = &task["metadata"][PROVENANCE];
    let binding = &submission.binding;
    if !keys(
        provenance,
        &[
            "schema_version",
            "task_id",
            "input_digest",
            "origin",
            "origin_admission_ref",
            "destination",
            "destination_host_id",
            "trust_domain",
            "repository",
            "head_sha",
            "job",
            "origin_claim",
            "local_claim",
            "local_run_id",
            "revision",
            "receipts",
        ],
    ) || provenance["schema_version"] != "tachi.remote_receipt.v1"
        || provenance["task_id"] != task["id"]
        || provenance["input_digest"] != submission.body_digest
        || provenance["origin"] != submission.origin
        || !token(&provenance["origin_admission_ref"])
        || provenance["destination"] != binding.destination
        || provenance["destination_host_id"] != binding.destination_host_id
        || provenance["trust_domain"] != submission.trust_domain
        || provenance["repository"] != binding.repository
        || provenance["head_sha"] != binding.head_sha
        || provenance["job"] != binding.job
        || provenance["origin_claim"]
            != json!({"claim_id":binding.origin_claim_id,"transition_version":binding.origin_claim_version})
        || !provenance["local_claim"].is_null()
        || !provenance["local_run_id"].is_null()
        || provenance["revision"] != 2
    {
        return false;
    }
    let Some(receipts) = provenance["receipts"]
        .as_array()
        .filter(|items| items.len() == 2)
    else {
        return false;
    };
    let mut previous = None;
    for (index, receipt) in receipts.iter().enumerate() {
        if !keys(
            receipt,
            &[
                "receipt_id",
                "revision",
                "fact",
                "occurred_at",
                "actor",
                "admission_ref",
                "detail",
            ],
        ) || !token(&receipt["receipt_id"])
            || !token(&receipt["admission_ref"])
            || receipt["revision"] != (index + 1)
            || receipt["actor"] != binding.destination
            || receipt["fact"]
                != if index == 0 {
                    "received"
                } else {
                    "authorization_refused"
                }
            || receipt["detail"]
                != if index == 0 {
                    Value::Null
                } else {
                    json!({"reason":"execution_unavailable"})
                }
        {
            return false;
        }
        let Some(time) = receipt["occurred_at"]
            .as_str()
            .and_then(|time| time.parse::<jiff::Timestamp>().ok())
        else {
            return false;
        };
        // The destination stamped this receipt with its own clock. Accept it
        // when that clock runs ahead of this host, within the same bound the
        // destination allows for this origin's issued_at.
        if time
            > jiff::Timestamp::now()
                + jiff::SignedDuration::from_secs(REMOTE_CLOCK_SKEW_TOLERANCE_SECS)
            || previous.is_some_and(|previous| time < previous)
        {
            return false;
        }
        previous = Some(time);
    }
    receipts[0]["receipt_id"] != receipts[1]["receipt_id"]
        && receipts[0]["admission_ref"] == receipts[1]["admission_ref"]
        && task["status"]["timestamp"] == receipts[1]["occurred_at"]
}

#[cfg(test)]
mod tests;
