//! `po-provider` — access to models, whatever their host.
//!
//! A single contract, [`Provider`], and one adapter per wire format. The conversions between
//! the canonical representation of `po-core` and a provider's format are pure functions,
//! tested without a network: this is where silent regressions hide (lost tool-call
//! identifiers, reasoning blocks not fed back, merged roles).

pub mod anthropic;
pub mod openai;
pub mod recovery;
pub mod sse;

use async_trait::async_trait;
use po_core::{Completion, CompletionResponse};

pub use sse::Delta;

pub use anthropic::AnthropicProvider;
pub use openai::OpenAiProvider;

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("{}", po_i18n::t!("provider.error.transport", detail = .0))]
    Transport(String),
    #[error("{}", po_i18n::t!("provider.error.unauthorized", provider = .provider))]
    Unauthorized { provider: String },
    #[error("{}", po_i18n::t!("provider.error.rate_limited", provider = .provider, hint = retry_hint(*.after_seconds)))]
    RateLimited { provider: String, after_seconds: Option<u64> },
    #[error("{}", po_i18n::t!("provider.error.unknown_model", model = .model, provider = .provider))]
    UnknownModel { provider: String, model: String },
    #[error("{}", po_i18n::t!("provider.error.context_overflow", tokens = .tokens, window = .window))]
    ContextOverflow { tokens: u32, window: u32 },
    #[error("{}", po_i18n::t!("provider.error.malformed", provider = .provider, detail = .detail))]
    Malformed { provider: String, detail: String },
    #[error("{}", po_i18n::t!("provider.error.http", provider = .provider, status = .status, body = .body))]
    Http { provider: String, status: u16, body: String },
}

/// Suffix of the rate-limit message telling when to try again, when the host said so.
fn retry_hint(after: Option<u64>) -> String {
    match after {
        Some(seconds) => po_i18n::t!("provider.error.retry_hint", seconds = seconds),
        None => String::new(),
    }
}

impl ProviderError {
    /// Does a new attempt have a chance of succeeding without changing anything?
    /// The router uses it to decide between retrying and switching to another model.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Transport(_) | Self::RateLimited { .. } => true,
            Self::Http { status, .. } => *status >= 500 || *status == 408,
            _ => false,
        }
    }

    /// Does the error condemn this model for the session? There is no point insisting on an
    /// invalid key or a nonexistent model: we must route elsewhere.
    pub fn is_fatal_for_model(&self) -> bool {
        matches!(
            self,
            Self::Unauthorized { .. } | Self::UnknownModel { .. } | Self::ContextOverflow { .. }
        )
    }
}

/// Translates an HTTP status into a typed error. Shared by all the adapters: providers differ
/// by the message body, not by the semantics of the codes.
pub fn classify_http(provider: &str, status: u16, body: &str, retry_after: Option<u64>) -> ProviderError {
    match status {
        401 | 403 => ProviderError::Unauthorized { provider: provider.to_owned() },
        429 => ProviderError::RateLimited {
            provider: provider.to_owned(),
            after_seconds: retry_after,
        },
        404 => ProviderError::UnknownModel {
            provider: provider.to_owned(),
            model: extract_model_name(body).unwrap_or_else(|| po_i18n::t!("provider.error.unknown_model_name").into()),
        },
        _ => ProviderError::Http {
            provider: provider.to_owned(),
            status,
            // An error body can contain the whole request; we bound it before surfacing it in
            // an interface.
            body: truncate(body, 500),
        },
    }
}

fn extract_model_name(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    value
        .pointer("/error/param")
        .or_else(|| value.pointer("/error/message"))
        .and_then(|v| v.as_str())
        .map(str::to_owned)
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// Contract shared by every model host, cloud or local.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Short name displayed in the interface and in errors.
    fn name(&self) -> &str;

    /// A full turn, without incremental streaming.
    async fn complete(&self, request: &Completion) -> Result<CompletionResponse, ProviderError>;

    /// A streamed turn: `on_delta` receives each fragment as soon as it arrives, and the
    /// complete response is returned at the end.
    ///
    /// The default implementation falls back on [`Provider::complete`] and delivers the
    /// response in one block: a provider that cannot stream remains usable, just less
    /// pleasant.
    async fn stream(
        &self,
        request: &Completion,
        on_delta: &mut (dyn FnMut(Delta) + Send),
    ) -> Result<CompletionResponse, ProviderError> {
        let response = self.complete(request).await?;
        let text = response.message.visible_text();
        if !text.is_empty() {
            on_delta(Delta::Text(text));
        }
        on_delta(Delta::Done);
        Ok(response)
    }

    /// Does this provider really stream? Lets the interface avoid promising a progressive
    /// display that will not come.
    fn supports_streaming(&self) -> bool {
        false
    }

    /// Models announced by the host. A local server exposes the real list; a cloud provider
    /// may return an empty list without it being an error.
    async fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_errors_are_retryable() {
        assert!(ProviderError::Transport("connection lost".into()).is_retryable());
        assert!(
            ProviderError::RateLimited { provider: "x".into(), after_seconds: Some(30) }
                .is_retryable()
        );
        assert!(
            ProviderError::Http { provider: "x".into(), status: 503, body: String::new() }
                .is_retryable()
        );
        assert!(
            !ProviderError::Http { provider: "x".into(), status: 400, body: String::new() }
                .is_retryable()
        );
    }

    #[test]
    fn definitive_errors_condemn_the_model() {
        assert!(ProviderError::Unauthorized { provider: "x".into() }.is_fatal_for_model());
        assert!(ProviderError::ContextOverflow { tokens: 9000, window: 8192 }.is_fatal_for_model());
        assert!(!ProviderError::Transport("noise".into()).is_fatal_for_model());
    }

    #[test]
    fn http_status_codes_are_translated() {
        assert!(matches!(
            classify_http("test", 401, "", None),
            ProviderError::Unauthorized { .. }
        ));
        assert!(matches!(
            classify_http("test", 429, "", Some(12)),
            ProviderError::RateLimited { after_seconds: Some(12), .. }
        ));
        assert!(matches!(
            classify_http("test", 500, "boom", None),
            ProviderError::Http { status: 500, .. }
        ));
    }

    #[test]
    fn the_model_name_is_extracted_from_the_error_body() {
        let body = r#"{"error":{"message":"model not found","param":"gpt-nonexistent"}}"#;
        match classify_http("openai", 404, body, None) {
            ProviderError::UnknownModel { model, .. } => assert_eq!(model, "gpt-nonexistent"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn a_large_error_body_is_bounded() {
        let body = "x".repeat(10_000);
        match classify_http("test", 500, &body, None) {
            ProviderError::Http { body, .. } => assert!(body.len() <= 505),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn truncation_respects_utf8_boundaries() {
        // Cutting in the middle of a multibyte character would cause a slice panic.
        let text = "é".repeat(400);
        let cut = truncate(&text, 501);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn the_retry_delay_is_shown_when_known() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let with = ProviderError::RateLimited { provider: "x".into(), after_seconds: Some(30) };
        assert!(with.to_string().contains("30 s"));
        let without = ProviderError::RateLimited { provider: "x".into(), after_seconds: None };
        assert!(!without.to_string().contains("réessayer"));
    }
}
