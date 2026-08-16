//! Thin provider adapters that intentionally reuse the OpenAI-compatible wire.
//!
//! xAI, OpenRouter, and a generic OpenAI-compatible endpoint all speak the
//! same request/response and SSE grammar this broker already pins in
//! [`super::openai_compat`]. Their production distinction is therefore *which
//! dialect the catalog chose*, not a second copy of the grammar. Copying the
//! mapping would buy drift and nothing else.

use super::canonical::CanonicalInvocationRequest;
use super::disposition::BeforeSendRefusal;
use super::openai_compat::OpenAiCompatWire;
use super::stream::{StreamDecoderUnavailable, WireStreamDecoder};
use super::wire::{
    AuthMaterialRef, ProviderErrorClassification, ProviderWire, ResponseHeaders, WireCapabilities,
    WireHttpRequest, WireOutcome,
};

/// Matches `WireDialect::Xai`.
pub const XAI_DIALECT: &str = "xai";
/// Matches `WireDialect::OpenRouter`.
pub const OPEN_ROUTER_DIALECT: &str = "open_router";
/// Matches `WireDialect::GenericCompat`.
pub const GENERIC_COMPAT_DIALECT: &str = "generic_compat";

macro_rules! openai_family_wire {
    ($name:ident, $dialect:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct $name {
            inner: OpenAiCompatWire,
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl $name {
            pub const fn new() -> Self {
                Self {
                    inner: OpenAiCompatWire::new(),
                }
            }

            pub const fn narrowed_to(declared: WireCapabilities) -> Self {
                Self {
                    inner: OpenAiCompatWire::narrowed_to(declared),
                }
            }

            pub const fn ceiling() -> WireCapabilities {
                OpenAiCompatWire::ceiling()
            }
        }

        impl ProviderWire for $name {
            fn dialect(&self) -> &'static str {
                $dialect
            }

            fn capabilities(&self) -> WireCapabilities {
                self.inner.capabilities()
            }

            fn build_request(
                &self,
                request: &CanonicalInvocationRequest,
                auth: AuthMaterialRef<'_>,
            ) -> Result<WireHttpRequest, BeforeSendRefusal> {
                self.inner.build_request(request, auth)
            }

            fn parse_response(
                &self,
                status: u16,
                headers: &ResponseHeaders,
                body: &[u8],
            ) -> WireOutcome {
                self.inner.parse_response(status, headers, body)
            }

            fn new_stream_decoder(
                &self,
            ) -> Result<Box<dyn WireStreamDecoder>, StreamDecoderUnavailable> {
                self.inner.new_stream_decoder()
            }

            fn classify_error(
                &self,
                status: u16,
                headers: &ResponseHeaders,
                body_excerpt: &str,
            ) -> ProviderErrorClassification {
                self.inner.classify_error(status, headers, body_excerpt)
            }
        }
    };
}

openai_family_wire!(XaiWire, XAI_DIALECT);
openai_family_wire!(OpenRouterWire, OPEN_ROUTER_DIALECT);
openai_family_wire!(GenericCompatWire, GENERIC_COMPAT_DIALECT);
