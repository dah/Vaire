use super::*;

impl OpenRouterClient {
    pub(super) async fn load_credential(&self) -> Result<SecretValue, OpenRouterFailure> {
        let store = self.credentials.clone();
        tokio::task::spawn_blocking(move || store.load(CredentialAccount::OpenRouterApiKey))
            .await
            .map_err(|_| OpenRouterFailure::new(OpenRouterFailureCategory::CredentialStore))?
            .map_err(|_| OpenRouterFailure::new(OpenRouterFailureCategory::CredentialStore))?
            .ok_or_else(|| OpenRouterFailure::new(OpenRouterFailureCategory::MissingCredential))
    }

    pub(super) async fn get_with_retry(
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

    pub(super) async fn send_chat(
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

    pub(super) fn endpoint(&self, path: &str) -> Url {
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
        HeaderValue::from_static(concat!("vaire/", env!("CARGO_PKG_VERSION"))),
    );
    headers.insert(
        "x-title",
        HeaderValue::from_bytes("Vairë".as_bytes())
            .map_err(|_| OpenRouterFailure::new(OpenRouterFailureCategory::InvalidRequest))?,
    );
    headers.insert(ACCEPT, HeaderValue::from_static(accept));
    Ok(headers)
}

pub(super) async fn read_limited(
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

pub(super) fn status_failure(status: StatusCode) -> OpenRouterFailure {
    let category = match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => OpenRouterFailureCategory::Unauthorized,
        StatusCode::TOO_MANY_REQUESTS => OpenRouterFailureCategory::RateLimited,
        _ => OpenRouterFailureCategory::Remote,
    };
    OpenRouterFailure::with_status(category, status.as_u16())
}

pub(super) fn invalid_response() -> OpenRouterFailure {
    OpenRouterFailure::new(OpenRouterFailureCategory::InvalidResponse)
}

pub(super) fn resource_limit() -> OpenRouterFailure {
    OpenRouterFailure::new(OpenRouterFailureCategory::ResourceLimit)
}

pub(super) fn timeout_failure() -> OpenRouterFailure {
    OpenRouterFailure::new(OpenRouterFailureCategory::Timeout)
}

pub(super) fn cancelled() -> OpenRouterFailure {
    OpenRouterFailure::new(OpenRouterFailureCategory::Cancelled)
}
