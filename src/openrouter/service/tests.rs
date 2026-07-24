use std::sync::Arc;
use std::time::Duration;

use tempfile::{tempdir, TempDir};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use url::Url;

use crate::credentials::{CredentialAccount, FakeCredentialStore, SecretValue};

use super::*;
use crate::openrouter::{FileOpenRouterStore, OpenRouterTimeouts};

fn service(
    credentials: Arc<FakeCredentialStore>,
    store: Arc<FileOpenRouterStore>,
) -> OpenRouterService {
    let client = OpenRouterClient::with_loopback_base_url(
        Url::parse("http://127.0.0.1:9").unwrap(),
        credentials.clone(),
        OpenRouterTimeouts {
            connect: Duration::from_millis(20),
            get_attempt: Duration::from_millis(50),
            chat_headers: Duration::from_millis(50),
            sse_idle: Duration::from_millis(50),
            chat_total: Duration::from_millis(100),
            retry_delay: Duration::ZERO,
        },
    )
    .unwrap();
    OpenRouterService::new(client, credentials, store)
}

async fn sse_service(
    body: &'static str,
    keep_open: bool,
) -> (
    OpenRouterService,
    Arc<FileOpenRouterStore>,
    tokio::task::JoinHandle<()>,
    TempDir,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = vec![0_u8; 16 * 1024];
        let _ = stream.read(&mut request).await.unwrap();
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        stream.write_all(body.as_bytes()).await.unwrap();
        if keep_open {
            std::future::pending::<()>().await;
        }
    });
    let directory = tempdir().unwrap();
    let store = Arc::new(FileOpenRouterStore::new(directory.path().join("openrouter")).unwrap());
    let credentials = Arc::new(FakeCredentialStore::with_openrouter_key(
        SecretValue::from_input("offline-test-key").unwrap(),
    ));
    let client = OpenRouterClient::with_loopback_base_url(
        base,
        credentials.clone(),
        OpenRouterTimeouts {
            connect: Duration::from_secs(1),
            get_attempt: Duration::from_secs(1),
            chat_headers: Duration::from_secs(1),
            sse_idle: Duration::from_secs(2),
            chat_total: Duration::from_secs(2),
            retry_delay: Duration::ZERO,
        },
    )
    .unwrap();
    (
        OpenRouterService::new(client, credentials, store.clone()),
        store,
        server,
        directory,
    )
}

mod auth_preflight;
mod drain_shutdown;
mod stream_outcomes;
