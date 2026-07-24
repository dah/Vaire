use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::header::{
    HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, RETRY_AFTER, USER_AGENT,
};
use reqwest::{Client, Response, StatusCode, Url};
use tokio::time::{sleep, timeout, Instant};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

use crate::credentials::{CredentialAccount, CredentialStore, SecretValue};

use super::protocol::{KeyResponse, ModelsResponse};
use super::sse::{ChatAccumulator, SseDecoder};
use super::types::{
    ChatRequest, ChatStreamEvent, OpenRouterFailure, OpenRouterFailureCategory, OpenRouterModel,
    OpenRouterStreamStage, MAX_CATALOG_BODY_BYTES, MAX_CATALOG_MODELS, MAX_CATALOG_TEXT_BYTES,
    MAX_ERROR_BODY_BYTES, MAX_OUTBOUND_CHAT_BYTES,
};

const PRODUCTION_BASE_URL: &str = "https://openrouter.ai";
const KEY_PATH: &str = "/api/v1/key";
const MODELS_PATH: &str = "/api/v1/models/user";
const CHAT_PATH: &str = "/api/v1/chat/completions";

enum GetAttempt {
    Success(Vec<u8>),
    Status {
        status: StatusCode,
        retry_delay: Duration,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct OpenRouterTimeouts {
    pub connect: Duration,
    pub get_attempt: Duration,
    pub chat_headers: Duration,
    pub sse_idle: Duration,
    pub chat_total: Duration,
    pub retry_delay: Duration,
}

impl Default for OpenRouterTimeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(5),
            get_attempt: Duration::from_secs(15),
            chat_headers: Duration::from_secs(30),
            sse_idle: Duration::from_secs(60),
            chat_total: Duration::from_secs(6 * 60 * 60),
            retry_delay: Duration::from_millis(250),
        }
    }
}

#[derive(Clone)]
pub struct OpenRouterClient {
    client: Client,
    base_url: Url,
    credentials: Arc<dyn CredentialStore>,
    timeouts: OpenRouterTimeouts,
}

impl std::fmt::Debug for OpenRouterClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenRouterClient")
            .field("base_url", &self.base_url.as_str())
            .field("timeouts", &self.timeouts)
            .field("credentials", &"[REDACTED]")
            .finish()
    }
}

impl OpenRouterClient {
    pub fn production(credentials: Arc<dyn CredentialStore>) -> Result<Self, OpenRouterFailure> {
        Self::build(
            Url::parse(PRODUCTION_BASE_URL).expect("fixed OpenRouter URL is valid"),
            credentials,
            OpenRouterTimeouts::default(),
        )
    }

    /// Test-only configuration seam. The URL is rejected unless it is HTTP and loopback.
    pub fn with_loopback_base_url(
        base_url: Url,
        credentials: Arc<dyn CredentialStore>,
        timeouts: OpenRouterTimeouts,
    ) -> Result<Self, OpenRouterFailure> {
        if base_url.scheme() != "http"
            || !base_url.host_str().is_some_and(|host| {
                host == "localhost"
                    || host
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
            })
            || base_url.username() != ""
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
        {
            return Err(OpenRouterFailure::new(
                OpenRouterFailureCategory::InvalidRequest,
            ));
        }
        Self::build(base_url, credentials, timeouts)
    }

    fn build(
        base_url: Url,
        credentials: Arc<dyn CredentialStore>,
        timeouts: OpenRouterTimeouts,
    ) -> Result<Self, OpenRouterFailure> {
        let client = Client::builder()
            .connect_timeout(timeouts.connect)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| OpenRouterFailure::new(OpenRouterFailureCategory::Network))?;
        Ok(Self {
            client,
            base_url,
            credentials,
            timeouts,
        })
    }

    pub async fn validate_stored_key(
        &self,
        cancellation: CancellationToken,
    ) -> Result<(), OpenRouterFailure> {
        let secret = self.load_credential().await?;
        self.validate_candidate(&secret, cancellation).await
    }

    pub async fn validate_candidate(
        &self,
        secret: &SecretValue,
        cancellation: CancellationToken,
    ) -> Result<(), OpenRouterFailure> {
        let body = self
            .get_with_retry(KEY_PATH, secret, cancellation, MAX_ERROR_BODY_BYTES)
            .await?;
        let response: KeyResponse =
            serde_json::from_slice(&body).map_err(|_| invalid_response())?;
        if !response.is_valid() {
            return Err(invalid_response());
        }
        Ok(())
    }

    pub async fn fetch_catalog(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<OpenRouterModel>, OpenRouterFailure> {
        let secret = self.load_credential().await?;
        let body = self
            .get_with_retry(MODELS_PATH, &secret, cancellation, MAX_CATALOG_BODY_BYTES)
            .await?;
        let response: ModelsResponse =
            serde_json::from_slice(&body).map_err(|_| invalid_response())?;
        let mut ids = HashSet::with_capacity(response.data.len().min(MAX_CATALOG_MODELS));
        let mut text_bytes = 0usize;
        let mut models = Vec::with_capacity(response.data.len().min(MAX_CATALOG_MODELS));
        for raw in response.data {
            let model = OpenRouterModel::from(raw);
            if !model.validate() {
                return Err(invalid_response());
            }
            if !ids.insert(model.id.clone()) {
                continue;
            }
            if models.len() == MAX_CATALOG_MODELS {
                return Err(resource_limit());
            }
            text_bytes = text_bytes
                .saturating_add(model.id.len())
                .saturating_add(model.name.as_ref().map_or(0, String::len));
            if text_bytes > MAX_CATALOG_TEXT_BYTES {
                return Err(resource_limit());
            }
            models.push(model);
        }
        Ok(models)
    }

    pub async fn chat<F>(
        &self,
        request: &ChatRequest,
        cancellation: CancellationToken,
        mut on_event: F,
    ) -> Result<(), OpenRouterFailure>
    where
        F: FnMut(ChatStreamEvent),
    {
        let encoded = serde_json::to_vec(&request.to_wire())
            .map_err(|_| OpenRouterFailure::new(OpenRouterFailureCategory::InvalidRequest))?;
        if encoded.len() > MAX_OUTBOUND_CHAT_BYTES {
            return Err(resource_limit());
        }
        let secret = self.load_credential().await?;
        let response = self
            .send_chat(&encoded, &secret, cancellation.clone())
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            let _ = timeout(
                self.timeouts.sse_idle.min(self.timeouts.chat_total),
                read_limited(response, MAX_ERROR_BODY_BYTES, &cancellation),
            )
            .await;
            return Err(status_failure(status));
        }
        let content_type_ok = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"));
        if !content_type_ok {
            return Err(invalid_response().at_stage(OpenRouterStreamStage::ContentType));
        }

        let deadline = Instant::now() + self.timeouts.chat_total;
        let mut stream = response.bytes_stream();
        let mut decoder = SseDecoder::new();
        let mut accumulator = ChatAccumulator::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(timeout_failure());
            }
            let wait = self.timeouts.sse_idle.min(remaining);
            let next = tokio::select! {
                _ = cancellation.cancelled() => return Err(cancelled()),
                value = timeout(wait, stream.next()) => value.map_err(|_| timeout_failure())?,
            };
            match next {
                Some(Ok(bytes)) => {
                    for data in decoder.push(&bytes)? {
                        let consumed = accumulator.consume(&data)?;
                        let _compatibility_stage = consumed.compatibility_stage;
                        for event in consumed.events {
                            on_event(event);
                        }
                    }
                    if accumulator.is_done() {
                        break;
                    }
                }
                Some(Err(_)) => {
                    return Err(OpenRouterFailure::new(OpenRouterFailureCategory::Network));
                }
                None => {
                    for data in decoder.finish()? {
                        let consumed = accumulator.consume(&data)?;
                        let _compatibility_stage = consumed.compatibility_stage;
                        for event in consumed.events {
                            on_event(event);
                        }
                    }
                    break;
                }
            }
        }
        let (assistant_text, usage) = accumulator.finish()?;
        on_event(ChatStreamEvent::Finished {
            assistant_text,
            usage,
        });
        Ok(())
    }
}

mod http;

use http::{
    cancelled, invalid_response, read_limited, resource_limit, status_failure, timeout_failure,
};
