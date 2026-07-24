use super::{LlmClient, ProviderAuthProbeClass, ProviderAuthProbeFamily, ProviderAuthProbeResult};
use serde::Deserialize;
use std::time::{Duration, Instant};

const AUTH_PROBE_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const AUTH_PROBE_TIMEOUT: Duration = Duration::from_secs(8);
const AUTH_PROBE_MAX_BODY_BYTES: usize = 1024 * 1024;
const DEEPSEEK_HOST: &str = "api.deepseek.com";
const DEEPSEEK_MODELS_URL: &str = "https://api.deepseek.com/models";
const SILICONFLOW_HOST: &str = "api.siliconflow.cn";
const SILICONFLOW_MODELS_URL: &str = "https://api.siliconflow.cn/v1/models";

#[derive(Clone, Copy)]
struct ProbeTarget {
    family: ProviderAuthProbeFamily,
    safe_host: &'static str,
    endpoint: Option<&'static str>,
    configuration_valid: bool,
}

impl ProbeTarget {
    fn from_base_url(base_url: &str) -> Self {
        let Ok(url) = reqwest::Url::parse(base_url) else {
            return Self::malformed();
        };
        let Some(host) = url.host_str() else {
            return Self::malformed();
        };

        let recognized = match host {
            DEEPSEEK_HOST => Self {
                family: ProviderAuthProbeFamily::DeepSeek,
                safe_host: DEEPSEEK_HOST,
                endpoint: Some(DEEPSEEK_MODELS_URL),
                configuration_valid: true,
            },
            SILICONFLOW_HOST => Self {
                family: ProviderAuthProbeFamily::SiliconFlow,
                safe_host: SILICONFLOW_HOST,
                endpoint: Some(SILICONFLOW_MODELS_URL),
                configuration_valid: true,
            },
            "open.bigmodel.cn" => Self {
                family: ProviderAuthProbeFamily::ZaiBigModel,
                safe_host: "open.bigmodel.cn",
                endpoint: None,
                configuration_valid: true,
            },
            "api.z.ai" => Self {
                family: ProviderAuthProbeFamily::ZaiBigModel,
                safe_host: "api.z.ai",
                endpoint: None,
                configuration_valid: true,
            },
            _ => return Self::unsupported(),
        };

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
        self.probe_reasoning_auth_inner(None).await
    }

    #[cfg(test)]
    pub(in crate::llm) async fn probe_reasoning_auth_with_endpoint_for_tests(
        &self,
        endpoint: &str,
    ) -> ProviderAuthProbeResult {
        self.probe_reasoning_auth_inner(Some(endpoint)).await
    }

    async fn probe_reasoning_auth_inner(
        &self,
        endpoint_override: Option<&str>,
    ) -> ProviderAuthProbeResult {
        let lane = &self.reasoning;
        let target = ProbeTarget::from_base_url(&lane.base_url);
        let started = Instant::now();
        let result = |auth_class, selected_model_present, model_count| ProviderAuthProbeResult {
            provider_family: target.family,
            provider_host: target.safe_host.to_string(),
            effective_model: lane.model.clone(),
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
        let Some(selected) = self.selected_secret_readonly(&lane.api_key_envs) else {
            return result(ProviderAuthProbeClass::CredentialUnavailable, None, None);
        };

        let Ok(client) = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(AUTH_PROBE_CONNECT_TIMEOUT)
            .timeout(AUTH_PROBE_TIMEOUT)
            .build()
        else {
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
        let selected_model_present = models.data.iter().any(|entry| entry.id == lane.model);
        result(
            ProviderAuthProbeClass::AuthOk,
            Some(selected_model_present),
            Some(models.data.len()),
        )
    }
}

fn elapsed_millis(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
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
