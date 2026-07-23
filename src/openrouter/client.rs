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

    async fn load_credential(&self) -> Result<SecretValue, OpenRouterFailure> {
        let store = self.credentials.clone();
        tokio::task::spawn_blocking(move || store.load(CredentialAccount::OpenRouterApiKey))
            .await
            .map_err(|_| OpenRouterFailure::new(OpenRouterFailureCategory::CredentialStore))?
            .map_err(|_| OpenRouterFailure::new(OpenRouterFailureCategory::CredentialStore))?
            .ok_or_else(|| OpenRouterFailure::new(OpenRouterFailureCategory::MissingCredential))
    }

    async fn get_with_retry(
        &self,
        path: &str,
        secret: &SecretValue,
        cancellation: CancellationToken,
        body_limit: usize,
    ) -> Result<Vec<u8>, OpenRouterFailure> {
        for attempt in 0..2 {
            let operation = async {
                let response = self
                    .client
                    .get(self.endpoint(path))
                    .headers(headers(secret, "application/json")?)
                    .send()
                    .await
                    .map_err(|error| {
                        if error.is_connect() || error.is_timeout() {
                            timeout_failure()
                        } else {
                            OpenRouterFailure::new(OpenRouterFailureCategory::Network)
                        }
                    })?;
                if response.status().is_success() {
                    return read_limited(response, body_limit, &cancellation)
                        .await
                        .map(GetAttempt::Success);
                }
                let status = response.status();
                let retry_delay =
                    retry_after(response.headers()).unwrap_or(self.timeouts.retry_delay);
                let _ = read_limited(response, MAX_ERROR_BODY_BYTES, &cancellation).await;
                Ok(GetAttempt::Status {
                    status,
                    retry_delay,
                })
            };
            let result = tokio::select! {
                _ = cancellation.cancelled() => return Err(cancelled()),
                result = timeout(self.timeouts.get_attempt, operation) => {
                    result.unwrap_or_else(|_| Err(timeout_failure()))
                },
            };
            match result {
                Ok(GetAttempt::Success(body)) => return Ok(body),
                Ok(GetAttempt::Status {
                    status,
                    retry_delay,
                }) if attempt == 0 && retryable_status(status) => {
                    cancellable_sleep(retry_delay, &cancellation).await?;
                }
                Ok(GetAttempt::Status { status, .. }) => return Err(status_failure(status)),
                Err(error)
                    if attempt == 0 && error.category() == OpenRouterFailureCategory::Timeout =>
                {
                    cancellable_sleep(self.timeouts.retry_delay, &cancellation).await?;
                }
                Err(error) => return Err(error),
            }
        }
        Err(OpenRouterFailure::new(OpenRouterFailureCategory::Network))
    }

    async fn send_chat(
        &self,
        body: &[u8],
        secret: &SecretValue,
        cancellation: CancellationToken,
    ) -> Result<Response, OpenRouterFailure> {
        tokio::select! {
            _ = cancellation.cancelled() => Err(cancelled()),
            result = timeout(
                self.timeouts.chat_headers,
                self.client
                    .post(self.endpoint(CHAT_PATH))
                    .headers(headers(secret, "text/event-stream")?)
                    .header(CONTENT_TYPE, "application/json")
                    .body(body.to_vec())
                    .send(),
            ) => match result {
                Ok(Ok(response)) => Ok(response),
                Ok(Err(error)) if error.is_timeout() => Err(timeout_failure()),
                Ok(Err(_)) => Err(OpenRouterFailure::new(OpenRouterFailureCategory::Network)),
                Err(_) => Err(timeout_failure()),
            },
        }
    }

    fn endpoint(&self, path: &str) -> Url {
        let mut url = self.base_url.clone();
        url.set_path(path);
        url.set_query(None);
        url
    }
}

fn headers(secret: &SecretValue, accept: &'static str) -> Result<HeaderMap, OpenRouterFailure> {
    let mut authorization = Zeroizing::new(Vec::with_capacity(
        "Bearer ".len() + secret.expose_bytes().len(),
    ));
    authorization.extend_from_slice(b"Bearer ");
    authorization.extend_from_slice(secret.expose_bytes());
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_bytes(&authorization)
            .map_err(|_| OpenRouterFailure::new(OpenRouterFailureCategory::CredentialStore))?,
    );
    headers.insert(
        USER_AGENT,
        HeaderValue::from_static(concat!("AgentHarness/", env!("CARGO_PKG_VERSION"))),
    );
    headers.insert("x-title", HeaderValue::from_static("AgentHarness"));
    headers.insert(ACCEPT, HeaderValue::from_static(accept));
    Ok(headers)
}

async fn read_limited(
    response: Response,
    limit: usize,
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, OpenRouterFailure> {
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    loop {
        let next = tokio::select! {
            _ = cancellation.cancelled() => return Err(cancelled()),
            value = stream.next() => value,
        };
        match next {
            Some(Ok(bytes)) => {
                if body.len().saturating_add(bytes.len()) > limit {
                    return Err(resource_limit());
                }
                body.extend_from_slice(&bytes);
            }
            Some(Err(_)) => return Err(OpenRouterFailure::new(OpenRouterFailureCategory::Network)),
            None => return Ok(body),
        }
    }
}

fn retryable_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 429 | 502 | 503 | 504)
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let seconds = headers
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    (seconds <= 2).then(|| Duration::from_secs(seconds))
}

async fn cancellable_sleep(
    delay: Duration,
    cancellation: &CancellationToken,
) -> Result<(), OpenRouterFailure> {
    tokio::select! {
        _ = cancellation.cancelled() => Err(cancelled()),
        _ = sleep(delay) => Ok(()),
    }
}

fn status_failure(status: StatusCode) -> OpenRouterFailure {
    let category = match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => OpenRouterFailureCategory::Unauthorized,
        StatusCode::TOO_MANY_REQUESTS => OpenRouterFailureCategory::RateLimited,
        _ => OpenRouterFailureCategory::Remote,
    };
    OpenRouterFailure::with_status(category, status.as_u16())
}

fn invalid_response() -> OpenRouterFailure {
    OpenRouterFailure::new(OpenRouterFailureCategory::InvalidResponse)
}

fn resource_limit() -> OpenRouterFailure {
    OpenRouterFailure::new(OpenRouterFailureCategory::ResourceLimit)
}

fn timeout_failure() -> OpenRouterFailure {
    OpenRouterFailure::new(OpenRouterFailureCategory::Timeout)
}

fn cancelled() -> OpenRouterFailure {
    OpenRouterFailure::new(OpenRouterFailureCategory::Cancelled)
}
