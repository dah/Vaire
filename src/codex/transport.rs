use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use serde::Serialize;
use serde_json::{json, Value};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{self, Instant};
use tokio_util::codec::{FramedRead, LinesCodec};

use super::protocol::{classify_message, InboundEvent, InboundMessage, RequestId, RpcErrorObject};
use super::safety::{denial_response, FullAccessPolicy, IsolationPaths};
use crate::diagnostics::{
    redact_remote_message, DiagnosticEvent, DiagnosticSink, NoopDiagnosticSink,
};

pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub const EVENT_QUEUE_CAPACITY: usize = 64;
pub const MAX_PENDING_REQUESTS: usize = 128;
const RETIRED_REQUEST_CAPACITY: usize = 1_024;
// Installed 0.144.6 schema notifications containing tool output the current UI does not render.
// Item/turn terminal events remain queued so the conversation lifecycle still completes.
const UNRENDERED_TOOL_PROGRESS_NOTIFICATIONS: [&str; 7] = [
    "command/exec/outputDelta",
    "process/outputDelta",
    "turn/diff/updated",
    "item/commandExecution/outputDelta",
    "item/commandExecution/terminalInteraction",
    "item/fileChange/outputDelta",
    "item/fileChange/patchUpdated",
];
// Item lifecycle snapshots for these installed-schema variants are likewise not
// rendered. Drop only known tool variants: agent and reasoning completions carry
// user-visible state and must continue through the bounded queue.
const UNRENDERED_TOOL_ITEM_TYPES: [&str; 10] = [
    "commandExecution",
    "fileChange",
    "mcpToolCall",
    "dynamicToolCall",
    "collabAgentToolCall",
    "subAgentActivity",
    "webSearch",
    "imageView",
    "sleep",
    "imageGeneration",
];
const SHUTDOWN_GRACE: Duration = Duration::from_millis(500);
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub struct RequestTimeouts {
    pub initialize: Duration,
    pub auth: Duration,
    pub catalog: Duration,
    pub thread: Duration,
    pub turn: Duration,
    pub fallback: Duration,
}

impl Default for RequestTimeouts {
    fn default() -> Self {
        Self {
            initialize: Duration::from_secs(10),
            auth: Duration::from_secs(30),
            catalog: Duration::from_secs(10),
            thread: Duration::from_secs(15),
            turn: Duration::from_secs(10),
            fallback: Duration::from_secs(10),
        }
    }
}

impl RequestTimeouts {
    pub fn for_method(&self, method: &str) -> Duration {
        match method {
            "initialize" => self.initialize,
            "account/read" | "account/login/start" | "account/login/cancel" | "account/logout" => {
                self.auth
            }
            "model/list" => self.catalog,
            "thread/start" | "thread/resume" | "thread/read" | "thread/list" | "thread/delete" => {
                self.thread
            }
            "turn/start" | "turn/interrupt" => self.turn,
            _ => self.fallback,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProcessSpec {
    pub executable: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub env: Vec<(OsString, OsString)>,
}

impl ProcessSpec {
    pub fn codex(
        executable: impl Into<PathBuf>,
        paths: &IsolationPaths,
        policy: &FullAccessPolicy,
    ) -> Self {
        let mut env = vec![
            (
                OsString::from("CODEX_HOME"),
                paths.codex_home.as_os_str().to_owned(),
            ),
            (OsString::from("NO_COLOR"), OsString::from("1")),
        ];
        if let Some(path) = std::env::var_os("PATH") {
            env.push((OsString::from("PATH"), path));
        }
        Self {
            executable: executable.into(),
            args: policy.app_server_args(),
            cwd: paths.conversation.clone(),
            env,
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum TransportError {
    #[error("could not start app-server: {0}")]
    Spawn(String),
    #[error("app-server I/O failed: {0}")]
    Io(String),
    #[error("app-server protocol error: {0}")]
    Protocol(String),
    #[error("app-server returned error {code}: {message}")]
    Remote { code: i64, message: String },
    #[error("app-server request timed out")]
    Timeout,
    #[error("app-server connection closed")]
    Closed,
    #[error("app-server connection is unusable after safety violation: {0}")]
    SafetyViolation(String),
}

enum CommandMessage {
    Request {
        method: String,
        params: Value,
        deadline: Instant,
        reply: oneshot::Sender<Result<Value, TransportError>>,
    },
    Notification {
        method: String,
        params: Value,
        deadline: Instant,
        reply: oneshot::Sender<Result<(), TransportError>>,
    },
}

struct ShutdownMessage {
    reply: oneshot::Sender<Result<(), TransportError>>,
}

enum ReaderMessage {
    Frame(Value),
    FramingError(String),
    Eof,
}

struct PendingRequest {
    deadline: Instant,
    method: String,
    reply: oneshot::Sender<Result<Value, TransportError>>,
}

struct OutboundRequestIds {
    next: Option<u64>,
}

impl Default for OutboundRequestIds {
    fn default() -> Self {
        Self { next: Some(1) }
    }
}

impl OutboundRequestIds {
    fn allocate(&mut self) -> Result<u64, TransportError> {
        let id = self.next.ok_or_else(|| {
            TransportError::Protocol("app-server request id space was exhausted".to_owned())
        })?;
        self.next = id.checked_add(1);
        Ok(id)
    }
}

#[derive(Default)]
struct RetiredRequestIds {
    ids: HashSet<u64>,
    order: VecDeque<u64>,
}

impl RetiredRequestIds {
    fn insert(&mut self, id: u64) {
        if !self.ids.insert(id) {
            return;
        }
        self.order.push_back(id);
        if self.order.len() > RETIRED_REQUEST_CAPACITY {
            if let Some(expired) = self.order.pop_front() {
                self.ids.remove(&expired);
            }
        }
    }

    fn remove(&mut self, id: u64) -> bool {
        self.ids.remove(&id)
    }
}

struct ConnectionRuntime {
    child: Child,
    stdin: ChildStdin,
    commands: mpsc::Receiver<CommandMessage>,
    shutdown: mpsc::Receiver<ShutdownMessage>,
    reader: mpsc::Receiver<ReaderMessage>,
    events: mpsc::Sender<TransportEvent>,
    generation: u64,
    diagnostics: Arc<dyn DiagnosticSink>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TransportEvent {
    pub generation: u64,
    pub event: InboundEvent,
}

pub struct AppServerTransport {
    commands: mpsc::Sender<CommandMessage>,
    shutdown: mpsc::Sender<ShutdownMessage>,
    events: mpsc::Receiver<TransportEvent>,
    task: Option<JoinHandle<()>>,
    child_pid: u32,
    generation: u64,
    timeouts: RequestTimeouts,
}

impl AppServerTransport {
    pub async fn spawn(spec: ProcessSpec) -> Result<Self, TransportError> {
        Self::spawn_with_diagnostics(spec, Arc::new(NoopDiagnosticSink)).await
    }

    pub async fn spawn_with_diagnostics(
        spec: ProcessSpec,
        diagnostics: Arc<dyn DiagnosticSink>,
    ) -> Result<Self, TransportError> {
        let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
        let mut command = Command::new(&spec.executable);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("CODEX_") {
                command.env_remove(key);
            }
        }
        command
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .envs(spec.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = command
            .spawn()
            .map_err(|error| TransportError::Spawn(error.to_string()))?;
        let child_pid = child
            .id()
            .ok_or_else(|| TransportError::Spawn("child has no process id".to_owned()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| TransportError::Spawn("child stdin was not piped".to_owned()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| TransportError::Spawn("child stdout was not piped".to_owned()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| TransportError::Spawn("child stderr was not piped".to_owned()))?;

        let (reader_tx, reader_rx) = mpsc::channel(128);
        tokio::spawn(read_stdout(stdout, reader_tx));
        tokio::spawn(drain_stderr(stderr, generation, Arc::clone(&diagnostics)));

        let (command_tx, command_rx) = mpsc::channel(32);
        let (shutdown_tx, shutdown_rx) = mpsc::channel(1);
        let (event_tx, event_rx) = mpsc::channel(EVENT_QUEUE_CAPACITY);
        let task = tokio::spawn(run_connection(ConnectionRuntime {
            child,
            stdin,
            commands: command_rx,
            shutdown: shutdown_rx,
            reader: reader_rx,
            events: event_tx,
            generation,
            diagnostics,
        }));

        Ok(Self {
            commands: command_tx,
            shutdown: shutdown_tx,
            events: event_rx,
            task: Some(task),
            child_pid,
            generation,
            timeouts: RequestTimeouts::default(),
        })
    }

    pub fn child_pid(&self) -> u32 {
        self.child_pid
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn set_timeouts(&mut self, timeouts: RequestTimeouts) {
        self.timeouts = timeouts;
    }

    pub async fn request_default<T: Serialize>(
        &self,
        method: impl Into<String>,
        params: T,
    ) -> Result<Value, TransportError> {
        let method = method.into();
        let timeout = self.timeouts.for_method(&method);
        self.request(method, params, timeout).await
    }

    pub async fn request<T: Serialize>(
        &self,
        method: impl Into<String>,
        params: T,
        timeout: Duration,
    ) -> Result<Value, TransportError> {
        let params = bounded_json_value(params)?;
        let (reply_tx, reply_rx) = oneshot::channel();
        let deadline = request_deadline(timeout)?;
        match time::timeout_at(deadline, async {
            self.commands
                .send(CommandMessage::Request {
                    method: method.into(),
                    params,
                    deadline,
                    reply: reply_tx,
                })
                .await
                .map_err(|_| TransportError::Closed)?;
            reply_rx.await.map_err(|_| TransportError::Closed)?
        })
        .await
        {
            Ok(result) => result,
            Err(_) => Err(TransportError::Timeout),
        }
    }

    pub async fn notify<T: Serialize>(
        &self,
        method: impl Into<String>,
        params: T,
    ) -> Result<(), TransportError> {
        let params = bounded_json_value(params)?;
        let (reply_tx, reply_rx) = oneshot::channel();
        let timeout = self.timeouts.fallback;
        let deadline = request_deadline(timeout)?;
        match time::timeout_at(deadline, async {
            self.commands
                .send(CommandMessage::Notification {
                    method: method.into(),
                    params,
                    deadline,
                    reply: reply_tx,
                })
                .await
                .map_err(|_| TransportError::Closed)?;
            reply_rx.await.map_err(|_| TransportError::Closed)?
        })
        .await
        {
            Ok(result) => result,
            Err(_) => Err(TransportError::Timeout),
        }
    }

    pub async fn next_event(&mut self) -> Option<TransportEvent> {
        self.events.recv().await
    }

    pub async fn shutdown(&mut self) -> Result<(), TransportError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let mut result = if self
            .shutdown
            .send(ShutdownMessage { reply: reply_tx })
            .await
            .is_err()
        {
            Ok(())
        } else {
            match time::timeout(
                WRITE_TIMEOUT + SHUTDOWN_GRACE + Duration::from_secs(1),
                reply_rx,
            )
            .await
            {
                Ok(reply) => reply.unwrap_or(Ok(())),
                Err(_) => Err(TransportError::Io(
                    "app-server shutdown timed out".to_owned(),
                )),
            }
        };

        if let Some(mut task) = self.task.take() {
            match time::timeout(SHUTDOWN_GRACE + Duration::from_secs(1), &mut task).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    result = Err(TransportError::Io(error.to_string()));
                }
                Err(_) => {
                    task.abort();
                    let _ = task.await;
                    result = Err(TransportError::Io(
                        "app-server worker shutdown timed out".to_owned(),
                    ));
                }
            }
        }
        result
    }
}

async fn read_stdout(stdout: tokio::process::ChildStdout, sender: mpsc::Sender<ReaderMessage>) {
    let codec = LinesCodec::new_with_max_length(MAX_FRAME_BYTES);
    let mut lines = FramedRead::new(stdout, codec);

    while let Some(line) = lines.next().await {
        match line {
            Ok(line) => match serde_json::from_str::<Value>(&line) {
                Ok(frame) => {
                    if sender.send(ReaderMessage::Frame(frame)).await.is_err() {
                        return;
                    }
                }
                Err(_) => {
                    let _ = sender
                        .send(ReaderMessage::FramingError(
                            "stdout contained malformed JSON".to_owned(),
                        ))
                        .await;
                    return;
                }
            },
            Err(_) => {
                let _ = sender
                    .send(ReaderMessage::FramingError(
                        "stdout frame was invalid UTF-8 or exceeded the size limit".to_owned(),
                    ))
                    .await;
                return;
            }
        }
    }

    let _ = sender.send(ReaderMessage::Eof).await;
}

async fn drain_stderr(
    mut stderr: tokio::process::ChildStderr,
    generation: u64,
    diagnostics: Arc<dyn DiagnosticSink>,
) {
    let mut buffer = [0_u8; 8192];
    loop {
        match stderr.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(count) => diagnostics.record(DiagnosticEvent {
                category: "stderr_bytes",
                generation,
                method: None,
                request_id: None,
                byte_count: Some(count),
            }),
        }
    }
}

async fn run_connection(runtime: ConnectionRuntime) {
    let ConnectionRuntime {
        mut child,
        mut stdin,
        mut commands,
        mut shutdown,
        mut reader,
        events,
        generation,
        diagnostics,
    } = runtime;
    let mut request_ids = OutboundRequestIds::default();
    let mut pending = HashMap::<u64, PendingRequest>::new();
    let mut retired = RetiredRequestIds::default();
    let mut unusable_reason: Option<String> = None;
    let mut shutdown_reply = None;
    let mut timeout_tick = time::interval(Duration::from_millis(25));

    'connection: loop {
        retire_cancelled_requests(&mut pending, &mut retired, generation, diagnostics.as_ref());
        tokio::select! {
            message = shutdown.recv() => {
                if let Some(message) = message {
                    shutdown_reply = Some(message.reply);
                }
                break 'connection;
            }
            command = commands.recv() => {
                match command {
                    Some(CommandMessage::Request { method, params, deadline, reply }) => {
                        if let Some(reason) = &unusable_reason {
                            let _ = reply.send(Err(TransportError::SafetyViolation(reason.clone())));
                            continue;
                        }
                        if Instant::now() >= deadline {
                            let _ = reply.send(Err(TransportError::Timeout));
                            continue;
                        }
                        retire_cancelled_requests(
                            &mut pending,
                            &mut retired,
                            generation,
                            diagnostics.as_ref(),
                        );
                        if pending.len() >= MAX_PENDING_REQUESTS {
                            diagnostics.record(DiagnosticEvent {
                                category: "request_overload",
                                generation,
                                method: Some(method),
                                request_id: None,
                                byte_count: None,
                            });
                            let _ = reply.send(Err(TransportError::Protocol(
                                "too many concurrent app-server requests".to_owned(),
                            )));
                            continue;
                        }
                        let id = match request_ids.allocate() {
                            Ok(id) => id,
                            Err(error) => {
                                let _ = reply.send(Err(error));
                                continue;
                            }
                        };
                        diagnostics.record(DiagnosticEvent {
                            category: "request_sent",
                            generation,
                            method: Some(method.clone()),
                            request_id: Some(id),
                            byte_count: None,
                        });
                        let frame = json!({"id": id, "method": method, "params": params});
                        if let Err(error) = write_frame_before(&mut stdin, &frame, deadline).await {
                            let _ = reply.send(Err(error.clone()));
                            if matches!(error, TransportError::Protocol(_)) {
                                continue;
                            }
                            fail_pending(&mut pending, error);
                            break 'connection;
                        }
                        pending.insert(id, PendingRequest {
                            deadline,
                            method,
                            reply,
                        });
                    }
                    Some(CommandMessage::Notification { method, params, deadline, reply }) => {
                        if let Some(reason) = &unusable_reason {
                            let _ = reply.send(Err(TransportError::SafetyViolation(reason.clone())));
                            continue;
                        }
                        let frame = json!({"method": method, "params": params});
                        match write_frame_before(&mut stdin, &frame, deadline).await {
                            Ok(()) => {
                                let _ = reply.send(Ok(()));
                            }
                            Err(error) => {
                                let _ = reply.send(Err(error.clone()));
                                if matches!(error, TransportError::Protocol(_)) {
                                    continue;
                                }
                                fail_pending(&mut pending, error);
                                break 'connection;
                            }
                        }
                    }
                    None => break 'connection,
                }
            }
            incoming = reader.recv() => {
                match incoming {
                    Some(ReaderMessage::Frame(frame)) => {
                        match classify_message(frame) {
                            Ok(InboundMessage::Response { id, result }) => {
                                if unusable_reason.is_some() {
                                    continue;
                                }
                                let RequestId::Number(id) = id else {
                                    let error = TransportError::Protocol(
                                        "response used an uncorrelated string id".to_owned()
                                    );
                                    fail_pending(&mut pending, error.clone());
                                    send_event(&events, generation, InboundEvent::ConnectionClosed { category: "protocol".to_owned() }, diagnostics.as_ref());
                                    break 'connection;
                                };
                                let Some(request) = pending.remove(&id) else {
                                    if retired.remove(id) {
                                        diagnostics.record(DiagnosticEvent {
                                            category: "stale_response",
                                            generation,
                                            method: None,
                                            request_id: Some(id),
                                            byte_count: None,
                                        });
                                        continue;
                                    }
                                    let error = TransportError::Protocol(
                                        "response used an unknown request id".to_owned()
                                    );
                                    fail_pending(&mut pending, error.clone());
                                    send_event(&events, generation, InboundEvent::ConnectionClosed { category: "protocol".to_owned() }, diagnostics.as_ref());
                                    break 'connection;
                                };
                                diagnostics.record(DiagnosticEvent {
                                    category: "response_received",
                                    generation,
                                    method: Some(request.method.clone()),
                                    request_id: Some(id),
                                    byte_count: None,
                                });
                                let result = result.map_err(remote_error);
                                let _ = request.reply.send(result);
                            }
                            Ok(InboundMessage::Notification { method, params }) => {
                                if is_unrendered_tool_notification(&method, &params) {
                                    continue 'connection;
                                }
                                if unusable_reason.is_none() && !send_event(
                                    &events,
                                    generation,
                                    InboundEvent::Notification { method, params },
                                    diagnostics.as_ref(),
                                ) {
                                    fail_pending(
                                        &mut pending,
                                        TransportError::Protocol(
                                            "app-server event backlog exceeded the safety limit"
                                                .to_owned(),
                                        ),
                                    );
                                    break 'connection;
                                }
                            }
                            Ok(InboundMessage::ServerRequest { id, method, .. }) => {
                                let denial = denial_response(&id, &method);
                                if let Err(error) = write_frame_before(
                                    &mut stdin,
                                    &denial,
                                    Instant::now() + WRITE_TIMEOUT,
                                ).await {
                                    fail_pending(&mut pending, error);
                                    break 'connection;
                                }
                                let reason = method.clone();
                                unusable_reason = Some(reason.clone());
                                let delivered = send_event(
                                    &events,
                                    generation,
                                    InboundEvent::SafetyViolation { id, method },
                                    diagnostics.as_ref(),
                                );
                                fail_pending(
                                    &mut pending,
                                    TransportError::SafetyViolation(reason),
                                );
                                if !delivered {
                                    break 'connection;
                                }
                            }
                            Err(message) => {
                                fail_pending(
                                    &mut pending,
                                    TransportError::Protocol(message),
                                );
                                send_event(&events, generation, InboundEvent::ConnectionClosed { category: "protocol".to_owned() }, diagnostics.as_ref());
                                break 'connection;
                            }
                        }
                    }
                    Some(ReaderMessage::FramingError(message)) => {
                        fail_pending(&mut pending, TransportError::Protocol(message));
                        send_event(&events, generation, InboundEvent::ConnectionClosed { category: "framing".to_owned() }, diagnostics.as_ref());
                        break 'connection;
                    }
                    Some(ReaderMessage::Eof) | None => {
                        fail_pending(&mut pending, TransportError::Closed);
                        send_event(&events, generation, InboundEvent::ConnectionClosed { category: "eof".to_owned() }, diagnostics.as_ref());
                        break 'connection;
                    }
                }
            }
            _ = timeout_tick.tick() => {
                let now = Instant::now();
                let expired = pending
                    .iter()
                    .filter_map(|(id, request)| (request.deadline <= now).then_some(*id))
                    .collect::<Vec<_>>();
                for id in expired {
                    if let Some(request) = pending.remove(&id) {
                        diagnostics.record(DiagnosticEvent {
                            category: "request_timeout",
                            generation,
                            method: Some(request.method.clone()),
                            request_id: Some(id),
                            byte_count: None,
                        });
                        retired.insert(id);
                        let _ = request.reply.send(Err(TransportError::Timeout));
                    }
                }
            }
        }
    }

    drop(stdin);
    let shutdown_result = reap_child(&mut child).await;
    if let Some(reply) = shutdown_reply {
        let _ = reply.send(shutdown_result);
    }
}

fn is_unrendered_tool_notification(method: &str, params: &Value) -> bool {
    if UNRENDERED_TOOL_PROGRESS_NOTIFICATIONS.contains(&method) {
        return true;
    }
    if !matches!(method, "item/started" | "item/completed") {
        return false;
    }

    params
        .get("item")
        .and_then(Value::as_object)
        .and_then(|item| item.get("type").or_else(|| item.get("kind")))
        .and_then(Value::as_str)
        .is_some_and(|kind| UNRENDERED_TOOL_ITEM_TYPES.contains(&kind))
}

fn remote_error(error: RpcErrorObject) -> TransportError {
    TransportError::Remote {
        code: error.code,
        message: redact_remote_message(&error.message),
    }
}

fn send_event(
    events: &mpsc::Sender<TransportEvent>,
    generation: u64,
    event: InboundEvent,
    diagnostics: &dyn DiagnosticSink,
) -> bool {
    match events.try_send(TransportEvent { generation, event }) {
        Ok(()) => true,
        Err(mpsc::error::TrySendError::Full(_)) => {
            diagnostics.record(DiagnosticEvent::connection("event_backlog", generation));
            false
        }
        Err(mpsc::error::TrySendError::Closed(_)) => false,
    }
}

fn fail_pending(pending: &mut HashMap<u64, PendingRequest>, error: TransportError) {
    for (_, request) in pending.drain() {
        let _ = request.reply.send(Err(error.clone()));
    }
}

fn retire_cancelled_requests(
    pending: &mut HashMap<u64, PendingRequest>,
    retired: &mut RetiredRequestIds,
    generation: u64,
    diagnostics: &dyn DiagnosticSink,
) {
    let now = Instant::now();
    let cancelled = pending
        .iter()
        .filter_map(|(id, request)| request.reply.is_closed().then_some(*id))
        .collect::<Vec<_>>();
    for id in cancelled {
        if let Some(request) = pending.remove(&id) {
            diagnostics.record(DiagnosticEvent {
                category: if request.deadline <= now {
                    "request_timeout"
                } else {
                    "request_cancelled"
                },
                generation,
                method: Some(request.method),
                request_id: Some(id),
                byte_count: None,
            });
            retired.insert(id);
        }
    }
}

struct BoundedJsonWriter {
    bytes: Vec<u8>,
    exceeded: bool,
}

impl BoundedJsonWriter {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            exceeded: false,
        }
    }
}

impl std::io::Write for BoundedJsonWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if self.bytes.len().saturating_add(buffer.len()) > MAX_FRAME_BYTES {
            self.exceeded = true;
            return Err(std::io::Error::other(
                "serialized JSON exceeded the size limit",
            ));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn bounded_json_value<T: Serialize>(value: T) -> Result<Value, TransportError> {
    let mut writer = BoundedJsonWriter::new();
    let result = serde_json::to_writer(&mut writer, &value);
    if writer.exceeded {
        return Err(TransportError::Protocol(
            "outbound JSON-RPC params exceeded the size limit".to_owned(),
        ));
    }
    result.map_err(|error| TransportError::Protocol(error.to_string()))?;
    serde_json::from_slice(&writer.bytes)
        .map_err(|error| TransportError::Protocol(error.to_string()))
}

async fn write_frame(stdin: &mut ChildStdin, frame: &Value) -> Result<(), TransportError> {
    let bytes = encode_frame(frame)?;
    stdin
        .write_all(&bytes)
        .await
        .map_err(|error| TransportError::Io(error.to_string()))?;
    stdin
        .flush()
        .await
        .map_err(|error| TransportError::Io(error.to_string()))
}

fn encode_frame(frame: &Value) -> Result<Vec<u8>, TransportError> {
    let mut writer = BoundedJsonWriter::new();
    let result = serde_json::to_writer(&mut writer, frame);
    if writer.exceeded {
        return Err(TransportError::Protocol(
            "outbound JSON-RPC frame exceeded the size limit".to_owned(),
        ));
    }
    result.map_err(|error| TransportError::Protocol(error.to_string()))?;
    writer.bytes.push(b'\n');
    Ok(writer.bytes)
}

async fn write_frame_before(
    stdin: &mut ChildStdin,
    frame: &Value,
    deadline: Instant,
) -> Result<(), TransportError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(TransportError::Timeout);
    }
    match time::timeout(remaining.min(WRITE_TIMEOUT), write_frame(stdin, frame)).await {
        Ok(result) => result,
        Err(_) => Err(TransportError::Timeout),
    }
}

fn request_deadline(timeout: Duration) -> Result<Instant, TransportError> {
    Instant::now().checked_add(timeout).ok_or_else(|| {
        TransportError::Protocol("request timeout exceeds the supported timeout range".to_owned())
    })
}

async fn reap_child(child: &mut Child) -> Result<(), TransportError> {
    match time::timeout(SHUTDOWN_GRACE, child.wait()).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => Err(TransportError::Io(error.to_string())),
        Err(_) => {
            child
                .start_kill()
                .map_err(|error| TransportError::Io(error.to_string()))?;
            match time::timeout(SHUTDOWN_GRACE, child.wait()).await {
                Ok(result) => result
                    .map(|_| ())
                    .map_err(|error| TransportError::Io(error.to_string())),
                Err(_) => Err(TransportError::Io(
                    "app-server did not reap after termination".to_owned(),
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::{
        bounded_json_value, encode_frame, OutboundRequestIds, RequestTimeouts, RetiredRequestIds,
        TransportError, MAX_FRAME_BYTES, RETIRED_REQUEST_CAPACITY,
    };

    #[test]
    fn retired_request_correlation_is_bounded_and_keeps_the_newest_ids() {
        let mut retired = RetiredRequestIds::default();
        for id in 1..=(RETIRED_REQUEST_CAPACITY as u64 + 1) {
            retired.insert(id);
        }

        assert_eq!(retired.ids.len(), RETIRED_REQUEST_CAPACITY);
        assert!(!retired.remove(1));
        assert!(retired.remove(RETIRED_REQUEST_CAPACITY as u64 + 1));
    }

    #[test]
    fn login_cancellation_uses_the_authentication_timeout() {
        let timeouts = RequestTimeouts {
            auth: Duration::from_secs(37),
            fallback: Duration::from_secs(3),
            ..RequestTimeouts::default()
        };
        assert_eq!(
            timeouts.for_method("account/login/cancel"),
            Duration::from_secs(37)
        );
    }

    #[test]
    fn outbound_request_ids_never_saturate_into_duplicates() {
        let mut ids = OutboundRequestIds {
            next: Some(u64::MAX),
        };
        assert_eq!(ids.allocate().unwrap(), u64::MAX);
        assert!(ids.allocate().is_err());
    }

    #[test]
    fn outbound_serialization_is_bounded_and_frame_limit_excludes_the_delimiter() {
        let template = json!({"method":"exact","params":{"value":""}});
        let overhead = serde_json::to_vec(&template).unwrap().len();
        let exact = json!({
            "method":"exact",
            "params":{"value":"x".repeat(MAX_FRAME_BYTES - overhead)}
        });
        let encoded = encode_frame(&exact).unwrap();
        assert_eq!(encoded.len(), MAX_FRAME_BYTES + 1);
        assert_eq!(encoded.last(), Some(&b'\n'));

        let oversized = json!({
            "method":"exact",
            "params":{"value":"x".repeat(MAX_FRAME_BYTES - overhead + 1)}
        });
        assert!(matches!(
            encode_frame(&oversized),
            Err(TransportError::Protocol(message)) if message.contains("size limit")
        ));
        let oversized_method = json!({
            "method":"x".repeat(MAX_FRAME_BYTES + 1),
            "params":{}
        });
        assert!(matches!(
            encode_frame(&oversized_method),
            Err(TransportError::Protocol(message)) if message.contains("size limit")
        ));
        assert!(matches!(
            bounded_json_value(json!({"value":"x".repeat(MAX_FRAME_BYTES)})),
            Err(TransportError::Protocol(message)) if message.contains("size limit")
        ));
    }
}
