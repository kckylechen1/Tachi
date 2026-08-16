use super::catalog_import::DeploymentAttribution;
use super::provider_health::SelectedProviderSecret;
use super::{LlmClient, ProviderAuthProbeClass, ProviderAuthProbeFamily, ProviderAuthProbeResult};
use memcore::vault::health::{EvidenceKind, TypedOutcome};
use memcore::vault::VaultKeyHealth;
use serde::Deserialize;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

const AUTH_PROBE_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const AUTH_PROBE_TIMEOUT: Duration = Duration::from_secs(8);
const AUTH_PROBE_MAX_BODY_BYTES: usize = 1024 * 1024;
const DEEPSEEK_HOST: &str = "api.deepseek.com";
const DEEPSEEK_MODELS_URL: &str = "https://api.deepseek.com/models";
const SILICONFLOW_HOST: &str = "api.siliconflow.cn";
const SILICONFLOW_MODELS_URL: &str = "https://api.siliconflow.cn/v1/models";
const ZAI_HOST: &str = "api.z.ai";
const ZAI_BIGMODEL_HOST: &str = "open.bigmodel.cn";

/// One provider family's documented, owner-verified authentication-probe
/// target (#1680 D6).
///
/// `host` and `endpoint` are **compile-time constants and must stay that
/// way**: they are the anti-SSRF boundary. A probe carries a bearer
/// credential, so the set of hosts it may ever contact changes only through
/// code review — never through configuration, a database row, or a
/// provider-supplied redirect. [`ProbeTarget::from_base_url`] matches a
/// configured `base_url`'s host against this table exactly, so a lookalike
/// (`api.deepseek.com.attacker.invalid`) is simply not in it.
///
/// `endpoint` is `Option` because a recognized family can still have no
/// documented *non-generating* GET endpoint — the Z.AI/BigModel hosts are
/// exactly that case, and callers must be able to tell "no safe probe exists"
/// ([`ProviderAuthProbeClass::UnsupportedNoDocumentedProbe`]) from "this
/// configuration is malformed".
///
/// This table is the single source for both users of these strings: this
/// module's own host recognition, and `tachi-server`'s `API_KEY_DEFS` probe
/// column, whose rows reference these constants rather than re-spelling them
/// (the crate graph only runs that way — `tachi-llm` must not depend on
/// `tachi-server`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderProbeDescriptor {
    /// Public-safe family label carried on the probe receipt.
    pub family: ProviderAuthProbeFamily,
    /// Canonical registry family id (`API_KEY_DEFS::provider_kind`), so a
    /// descriptor and a registry row can be checked against each other.
    pub provider_kind: &'static str,
    /// The exact host this family is probed at. Never a suffix or pattern.
    pub host: &'static str,
    /// The exact documented non-generating GET endpoint, or `None` when the
    /// family has none.
    pub endpoint: Option<&'static str>,
}

pub const DEEPSEEK_AUTH_PROBE: ProviderProbeDescriptor = ProviderProbeDescriptor {
    family: ProviderAuthProbeFamily::DeepSeek,
    provider_kind: "deepseek",
    host: DEEPSEEK_HOST,
    endpoint: Some(DEEPSEEK_MODELS_URL),
};

pub const SILICONFLOW_AUTH_PROBE: ProviderProbeDescriptor = ProviderProbeDescriptor {
    family: ProviderAuthProbeFamily::SiliconFlow,
    provider_kind: "siliconflow",
    host: SILICONFLOW_HOST,
    endpoint: Some(SILICONFLOW_MODELS_URL),
};

/// Z.AI's current primary domain. No documented non-generating GET endpoint,
/// so it is recognized (not "malformed configuration") but never probed.
pub const ZAI_AUTH_PROBE: ProviderProbeDescriptor = ProviderProbeDescriptor {
    family: ProviderAuthProbeFamily::ZaiBigModel,
    provider_kind: "zai",
    host: ZAI_HOST,
    endpoint: None,
};

/// The same family's older BigModel domain — a second recognized host, not a
/// second family.
pub const ZAI_BIGMODEL_AUTH_PROBE: ProviderProbeDescriptor = ProviderProbeDescriptor {
    family: ProviderAuthProbeFamily::ZaiBigModel,
    provider_kind: "zai",
    host: ZAI_BIGMODEL_HOST,
    endpoint: None,
};

/// Every host an auth probe may ever contact. Adding one is a code change.
pub const AUTH_PROBE_DESCRIPTORS: &[ProviderProbeDescriptor] = &[
    DEEPSEEK_AUTH_PROBE,
    SILICONFLOW_AUTH_PROBE,
    ZAI_AUTH_PROBE,
    ZAI_BIGMODEL_AUTH_PROBE,
];

/// Exact-host lookup — the anti-SSRF admission decision. No normalization, no
/// suffix matching, no case folding beyond what the URL parser already did.
pub fn auth_probe_descriptor_for_host(host: &str) -> Option<&'static ProviderProbeDescriptor> {
    AUTH_PROBE_DESCRIPTORS
        .iter()
        .find(|descriptor| descriptor.host == host)
}

/// The probe target for a canonical registry family id, preferring the host
/// that actually has a documented endpoint when a family has several (Z.AI has
/// two hosts and no endpoint on either, so it stays `None`-endpointed either
/// way).
pub fn auth_probe_descriptor_for_provider_kind(
    provider_kind: &str,
) -> Option<&'static ProviderProbeDescriptor> {
    AUTH_PROBE_DESCRIPTORS
        .iter()
        .find(|descriptor| {
            descriptor.provider_kind == provider_kind && descriptor.endpoint.is_some()
        })
        .or_else(|| {
            AUTH_PROBE_DESCRIPTORS
                .iter()
                .find(|descriptor| descriptor.provider_kind == provider_kind)
        })
}

#[derive(Clone, Copy)]
struct ProbeTarget {
    family: ProviderAuthProbeFamily,
    safe_host: &'static str,
    endpoint: Option<&'static str>,
    configuration_valid: bool,
}

impl ProbeTarget {
    /// A recognized descriptor, taken as-is. Every field is a compile-time
    /// constant, so there is no configuration left to validate.
    fn from_descriptor(descriptor: &ProviderProbeDescriptor) -> Self {
        Self {
            family: descriptor.family,
            safe_host: descriptor.host,
            endpoint: descriptor.endpoint,
            configuration_valid: true,
        }
    }

    fn from_base_url(base_url: &str) -> Self {
        let Ok(url) = reqwest::Url::parse(base_url) else {
            return Self::malformed();
        };
        let Some(host) = url.host_str() else {
            return Self::malformed();
        };

        let Some(descriptor) = auth_probe_descriptor_for_host(host) else {
            return Self::unsupported();
        };
        let recognized = Self::from_descriptor(descriptor);

        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port_or_known_default() != Some(443)
        {
            return Self {
                configuration_valid: false,
                ..recognized
            };
        }
        recognized
    }

    fn malformed() -> Self {
        Self {
            family: ProviderAuthProbeFamily::Unsupported,
            safe_host: "unrecognized",
            endpoint: None,
            configuration_valid: false,
        }
    }

    fn unsupported() -> Self {
        Self {
            family: ProviderAuthProbeFamily::Unsupported,
            safe_host: "unrecognized",
            endpoint: None,
            configuration_valid: true,
        }
    }
}

#[derive(Deserialize)]
struct ModelList {
    data: Vec<ModelListEntry>,
}

#[derive(Deserialize)]
struct ModelListEntry {
    id: String,
}

impl LlmClient {
    /// Perform one official, non-generating provider authentication request
    /// for the configured reasoning lane. The request never follows redirects,
    /// retries, rotates keys, invokes fallback, or mutates provider health.
    pub async fn probe_reasoning_auth_no_content(&self) -> ProviderAuthProbeResult {
        self.probe_reasoning_auth_inner(None, None).await
    }

    #[cfg(test)]
    pub(in crate::llm) async fn probe_reasoning_auth_with_endpoint_for_tests(
        &self,
        endpoint: &str,
    ) -> ProviderAuthProbeResult {
        self.probe_reasoning_auth_inner(Some(endpoint), None).await
    }

    #[cfg(test)]
    pub(in crate::llm) async fn probe_reasoning_auth_with_direct_endpoint_for_tests(
        &self,
        endpoint: &str,
        host: &str,
        address: SocketAddr,
    ) -> ProviderAuthProbeResult {
        self.probe_reasoning_auth_inner(Some(endpoint), Some((host, address)))
            .await
    }

    async fn probe_reasoning_auth_inner(
        &self,
        endpoint_override: Option<&str>,
        resolution_override: Option<(&str, SocketAddr)>,
    ) -> ProviderAuthProbeResult {
        let lane = &self.reasoning;
        // Resolved before the request as it always was: the lane's currently
        // usable credential, chosen without advancing the round-robin cursor.
        let selected = self.selected_secret_readonly(&lane.api_key_envs);
        self.probe_target_inner(
            ProbeTarget::from_base_url(&lane.base_url),
            lane.model.clone(),
            Some(lane.model.as_str()),
            selected,
            endpoint_override,
            resolution_override,
        )
        .await
    }

    /// Probe **one named credential** — a single `(logical_name, key_id)` pool
    /// member — instead of "whichever key the reasoning lane would have used"
    /// (#1680 D6).
    ///
    /// `descriptor` comes from the caller's registry lookup, which is what
    /// keeps the probeable set registry-driven while the hosts themselves stay
    /// compile-time constants (`tachi-server`'s
    /// `status_health::auth_probe_descriptor_for_env_name`). The member's
    /// value is read without the availability filter the lane path applies:
    /// re-examining a credential the health table currently calls unusable is
    /// the entire reason to probe one by name.
    ///
    /// Observation only — this never mutates health. Use
    /// [`LlmClient::probe_member_auth_and_record`] to record what it learned.
    pub async fn probe_member_auth_no_content(
        &self,
        descriptor: &ProviderProbeDescriptor,
        logical_name: &str,
        key_id: &str,
    ) -> ProviderAuthProbeResult {
        self.probe_member_auth_inner(descriptor, logical_name, key_id, None, None)
            .await
    }

    /// Probe one named credential and record the verdict as that member's
    /// health, stamped [`EvidenceKind::Probed`] (#1680 D6).
    ///
    /// The write goes through the one `vault_key_health` writer and names
    /// exactly the member that was probed — a 401 for `..._2` cannot reach
    /// `..._1` (disc-4). Inconclusive probes are recorded as
    /// [`TypedOutcome::Unknown`], which stamps the attempt and the evidence and
    /// changes no part of the health binding (disc-5), and probes that never
    /// left the process (no documented endpoint, no such credential) record
    /// nothing at all — hence the `Option`.
    ///
    /// This channel writes health and nothing else: `provider_accounts` and
    /// its sibling tables are written only by `vault reconcile apply`.
    pub async fn probe_member_auth_and_record(
        &self,
        descriptor: &ProviderProbeDescriptor,
        logical_name: &str,
        key_id: &str,
    ) -> (ProviderAuthProbeResult, Option<VaultKeyHealth>) {
        let result = self
            .probe_member_auth_inner(descriptor, logical_name, key_id, None, None)
            .await;
        let health = self.record_probe_result(logical_name, key_id, result.auth_class);
        (result, health)
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub async fn probe_member_auth_and_record_with_endpoint_for_tests(
        &self,
        descriptor: &ProviderProbeDescriptor,
        logical_name: &str,
        key_id: &str,
        endpoint: &str,
    ) -> (ProviderAuthProbeResult, Option<VaultKeyHealth>) {
        let result = self
            .probe_member_auth_inner(descriptor, logical_name, key_id, Some(endpoint), None)
            .await;
        let health = self.record_probe_result(logical_name, key_id, result.auth_class);
        (result, health)
    }

    /// Turn a probe class into the member's health row, or into nothing.
    fn record_probe_result(
        &self,
        logical_name: &str,
        key_id: &str,
        auth_class: ProviderAuthProbeClass,
    ) -> Option<VaultKeyHealth> {
        let outcome = probe_health_outcome(auth_class)?;
        let member = SelectedProviderSecret {
            logical_name: logical_name.to_string(),
            key_id: key_id.to_string(),
            // The recorder never reads the value; a health write is about the
            // member's identity, never its secret.
            value: String::new(),
        };
        // Unattributed: an auth probe is a deliberate, non-generating request
        // about a *credential*, and its outcomes are auth-class by
        // construction. Deployment health takes nothing from it (#1681 D4) —
        // a key being rejected says nothing about the deployment behind the
        // endpoint.
        Some(self.apply_key_outcome(
            &member,
            outcome,
            EvidenceKind::Probed,
            None,
            DeploymentAttribution::Unattributed,
        ))
    }

    async fn probe_member_auth_inner(
        &self,
        descriptor: &ProviderProbeDescriptor,
        logical_name: &str,
        key_id: &str,
        endpoint_override: Option<&str>,
        resolution_override: Option<(&str, SocketAddr)>,
    ) -> ProviderAuthProbeResult {
        let selected = self.member_secret_readonly(logical_name, key_id);
        self.probe_target_inner(
            ProbeTarget::from_descriptor(descriptor),
            // A member probe is about a credential, not about a lane's model.
            String::new(),
            None,
            selected,
            endpoint_override,
            resolution_override,
        )
        .await
    }

    /// One probe request against one already-admitted target with one already
    /// chosen credential.
    ///
    /// Split out of the reasoning-lane entry (#1680 D6) so the same wire
    /// behaviour — no redirects, no retries, no rotation, no fallback, no
    /// health mutation, bounded body, never a response body on the receipt —
    /// serves both "probe the reasoning lane" and "probe this one pool
    /// member". `expected_model` is the lane's configured model when the probe
    /// is about a lane, and `None` for a member probe, which is about a
    /// credential and has no model to look for: the receipt then reports
    /// `selected_model_present: None` rather than a meaningless `false`.
    async fn probe_target_inner(
        &self,
        target: ProbeTarget,
        effective_model: String,
        expected_model: Option<&str>,
        selected: Option<SelectedProviderSecret>,
        endpoint_override: Option<&str>,
        resolution_override: Option<(&str, SocketAddr)>,
    ) -> ProviderAuthProbeResult {
        let started = Instant::now();
        let result = |auth_class, selected_model_present, model_count| ProviderAuthProbeResult {
            provider_family: target.family,
            provider_host: target.safe_host.to_string(),
            effective_model: effective_model.clone(),
            auth_class,
            selected_model_present,
            model_count,
            latency_ms: elapsed_millis(started),
        };

        if !target.configuration_valid {
            return result(ProviderAuthProbeClass::MalformedConfiguration, None, None);
        }
        let Some(documented_endpoint) = target.endpoint else {
            return result(
                ProviderAuthProbeClass::UnsupportedNoDocumentedProbe,
                None,
                None,
            );
        };
        let Some(selected) = selected else {
            return result(ProviderAuthProbeClass::CredentialUnavailable, None, None);
        };

        let mut client_builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            // SECURITY: this request carries a bearer credential. Reqwest's
            // `system-proxy` feature otherwise permits HTTP_PROXY,
            // HTTPS_PROXY, ALL_PROXY, or OS proxy configuration to receive
            // the request and own DNS/TCP. The documented provider endpoint
            // must always be contacted directly.
            .no_proxy()
            .connect_timeout(AUTH_PROBE_CONNECT_TIMEOUT)
            .timeout(AUTH_PROBE_TIMEOUT);
        if let Some((host, address)) = resolution_override {
            client_builder = client_builder.resolve(host, address);
        }
        let Ok(client) = client_builder.build() else {
            return result(ProviderAuthProbeClass::Transient, None, None);
        };
        let endpoint = endpoint_override.unwrap_or(documented_endpoint);
        let response = client
            .get(endpoint)
            .bearer_auth(selected.value)
            .send()
            .await;
        let Ok(mut response) = response else {
            return result(ProviderAuthProbeClass::Transient, None, None);
        };
        let status = response.status();

        if status.is_redirection() {
            return result(ProviderAuthProbeClass::RedirectRefused, None, None);
        }
        match status.as_u16() {
            401 | 403 => return result(ProviderAuthProbeClass::AuthFailed, None, None),
            402 => return result(ProviderAuthProbeClass::ProviderExhausted, None, None),
            429 => return result(ProviderAuthProbeClass::RateLimited, None, None),
            500..=599 => return result(ProviderAuthProbeClass::Transient, None, None),
            200 => {}
            _ => return result(ProviderAuthProbeClass::UnexpectedStatus, None, None),
        }

        let mut body = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk))
                    if body.len().saturating_add(chunk.len()) <= AUTH_PROBE_MAX_BODY_BYTES =>
                {
                    body.extend_from_slice(&chunk);
                }
                Ok(Some(_)) | Err(_) => {
                    return result(ProviderAuthProbeClass::MalformedResponse, None, None)
                }
                Ok(None) => break,
            }
        }
        let Ok(models) = serde_json::from_slice::<ModelList>(&body) else {
            return result(ProviderAuthProbeClass::MalformedResponse, None, None);
        };
        let selected_model_present =
            expected_model.map(|expected| models.data.iter().any(|entry| entry.id == expected));
        result(
            ProviderAuthProbeClass::AuthOk,
            selected_model_present,
            Some(models.data.len()),
        )
    }
}

fn elapsed_millis(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}

/// What a probe class is allowed to say about a credential (#1680 D6).
///
/// Three answers, and the boundaries between them are the fail-safe posture:
///
/// - A verdict the provider actually gave about *this credential*
///   (`AuthOk`/`AuthFailed`/`ProviderExhausted`/`RateLimited`) is recorded as
///   that verdict.
/// - A request that went out and came back saying nothing about the
///   credential — a transport failure, a refused redirect, an unparseable
///   body, an unexpected status — is [`TypedOutcome::Unknown`]: it stamps the
///   attempt and the evidence and leaves the health binding alone. A probe
///   Tachi could not complete must never clear a real auth failure nor
///   manufacture one (disc-5).
/// - A probe that never made a request at all (malformed configuration, no
///   documented endpoint, no such credential) is `None`: nothing was
///   attempted, so nothing — not even an attempt timestamp — is recorded.
///
/// `RateLimited` carries no `retry_after_secs` because the probe deliberately
/// does not read provider response headers into its receipt; the writer's
/// default cooldown applies.
fn probe_health_outcome(auth_class: ProviderAuthProbeClass) -> Option<TypedOutcome> {
    match auth_class {
        ProviderAuthProbeClass::AuthOk => Some(TypedOutcome::Success),
        ProviderAuthProbeClass::AuthFailed => Some(TypedOutcome::AuthFailed),
        ProviderAuthProbeClass::ProviderExhausted => Some(TypedOutcome::Exhausted),
        ProviderAuthProbeClass::RateLimited => Some(TypedOutcome::RateLimited {
            retry_after_secs: None,
        }),
        ProviderAuthProbeClass::Transient
        | ProviderAuthProbeClass::RedirectRefused
        | ProviderAuthProbeClass::MalformedResponse
        | ProviderAuthProbeClass::UnexpectedStatus => Some(TypedOutcome::Unknown),
        ProviderAuthProbeClass::MalformedConfiguration
        | ProviderAuthProbeClass::UnsupportedNoDocumentedProbe
        | ProviderAuthProbeClass::CredentialUnavailable => None,
    }
}

#[cfg(test)]
mod target_tests {
    use super::*;

    #[test]
    fn official_hosts_map_only_to_their_exact_documented_get_endpoints() {
        let deepseek = ProbeTarget::from_base_url(
            "https://api.deepseek.com/chat/completions?ignored=config-only",
        );
        assert_eq!(deepseek.family, ProviderAuthProbeFamily::DeepSeek);
        assert_eq!(deepseek.endpoint, Some(DEEPSEEK_MODELS_URL));

        let siliconflow = ProbeTarget::from_base_url(
            "https://api.siliconflow.cn/v1/chat/completions#ignored-config-only",
        );
        assert_eq!(siliconflow.family, ProviderAuthProbeFamily::SiliconFlow);
        assert_eq!(siliconflow.endpoint, Some(SILICONFLOW_MODELS_URL));
    }

    #[test]
    fn lookalike_or_non_https_hosts_never_receive_a_probe() {
        let lookalike =
            ProbeTarget::from_base_url("https://api.deepseek.com.attacker.invalid/models");
        assert_eq!(lookalike.family, ProviderAuthProbeFamily::Unsupported);
        assert_eq!(lookalike.endpoint, None);

        let non_https = ProbeTarget::from_base_url("http://api.deepseek.com/chat/completions");
        assert!(!non_https.configuration_valid);
        assert_eq!(non_https.endpoint, Some(DEEPSEEK_MODELS_URL));
    }
}
