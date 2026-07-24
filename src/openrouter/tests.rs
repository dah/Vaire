use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::credentials::{FakeCredentialOperation, FakeCredentialStore, SecretValue};

use super::{
    ChatMessage, ChatRequest, ChatRole, ChatStreamEvent, OpenRouterClient,
    OpenRouterFailureCategory, OpenRouterStreamStage, OpenRouterTimeouts, TokenUsage,
};

const TEST_KEY: &str = "sk-or-v1-recognizable-offline-test-key";

#[derive(Clone)]
struct Script {
    status: u16,
    content_type: &'static str,
    headers: Vec<(&'static str, &'static str)>,
    parts: Vec<Vec<u8>>,
    delay: Duration,
}

impl Script {
    fn json(status: u16, body: &str) -> Self {
        Self {
            status,
            content_type: "application/json",
            headers: Vec::new(),
            parts: vec![body.as_bytes().to_vec()],
            delay: Duration::ZERO,
        }
    }

    fn sse(parts: &[&str]) -> Self {
        Self {
            status: 200,
            content_type: "text/event-stream",
            headers: Vec::new(),
            parts: parts.iter().map(|part| part.as_bytes().to_vec()).collect(),
            delay: Duration::from_millis(5),
        }
    }
}

async fn fake_server(
    scripts: Vec<Script>,
) -> (Url, Arc<Mutex<Vec<Vec<u8>>>>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let task = tokio::spawn(async move {
        for script in scripts {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            let header_end = loop {
                let count = socket.read(&mut buffer).await.unwrap();
                if count == 0 {
                    return;
                }
                request.extend_from_slice(&buffer[..count]);
                if let Some(position) = request.windows(4).position(|value| value == b"\r\n\r\n") {
                    break position + 4;
                }
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            while request.len() < header_end + content_length {
                let count = socket.read(&mut buffer).await.unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..count]);
            }
            captured.lock().unwrap().push(request);
            let reason = if script.status == 200 {
                "OK"
            } else if script.status == 503 {
                "Service Unavailable"
            } else if script.status == 401 {
                "Unauthorized"
            } else {
                "Error"
            };
            let total: usize = script.parts.iter().map(Vec::len).sum();
            let mut response = format!(
                "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
                script.status, reason, script.content_type, total
            );
            for (name, value) in script.headers {
                response.push_str(name);
                response.push_str(": ");
                response.push_str(value);
                response.push_str("\r\n");
            }
            response.push_str("\r\n");
            if socket.write_all(response.as_bytes()).await.is_err() {
                continue;
            }
            for part in script.parts {
                if socket.write_all(&part).await.is_err() {
                    break;
                }
                if !script.delay.is_zero() {
                    tokio::time::sleep(script.delay).await;
                }
            }
        }
    });
    (
        Url::parse(&format!("http://{address}")).unwrap(),
        requests,
        task,
    )
}

fn test_client(base: Url, store: Arc<FakeCredentialStore>) -> OpenRouterClient {
    OpenRouterClient::with_loopback_base_url(
        base,
        store,
        OpenRouterTimeouts {
            connect: Duration::from_secs(1),
            get_attempt: Duration::from_secs(1),
            chat_headers: Duration::from_secs(1),
            sse_idle: Duration::from_millis(200),
            chat_total: Duration::from_secs(2),
            retry_delay: Duration::from_millis(1),
        },
    )
    .unwrap()
}

fn credentials() -> Arc<FakeCredentialStore> {
    Arc::new(FakeCredentialStore::with_openrouter_key(
        SecretValue::from_input(TEST_KEY).unwrap(),
    ))
}

fn request_text(request: &[u8]) -> String {
    String::from_utf8_lossy(request).into_owned()
}

mod auth_catalog;
mod cancellation_limits;
mod chat_failures;
mod chat_success;
