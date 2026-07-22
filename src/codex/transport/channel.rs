use super::*;

pub(in crate::codex::transport) enum CommandMessage {
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

pub(in crate::codex::transport) struct ShutdownMessage {
    pub(in crate::codex::transport) reply: oneshot::Sender<Result<(), TransportError>>,
}

pub(in crate::codex::transport) enum ReaderMessage {
    Frame(Value),
    FramingError(String),
    Eof,
}

pub(in crate::codex::transport) struct PendingRequest {
    pub(in crate::codex::transport) deadline: Instant,
    pub(in crate::codex::transport) method: String,
    pub(in crate::codex::transport) reply: oneshot::Sender<Result<Value, TransportError>>,
}

pub(in crate::codex::transport) struct OutboundRequestIds {
    pub(in crate::codex::transport) next: Option<u64>,
}

impl Default for OutboundRequestIds {
    fn default() -> Self {
        Self { next: Some(1) }
    }
}

impl OutboundRequestIds {
    pub(in crate::codex::transport) fn allocate(&mut self) -> Result<u64, TransportError> {
        let id = self.next.ok_or_else(|| {
            TransportError::Protocol("app-server request id space was exhausted".to_owned())
        })?;
        self.next = id.checked_add(1);
        Ok(id)
    }
}

#[derive(Default)]
pub(in crate::codex::transport) struct RetiredRequestIds {
    pub(in crate::codex::transport) ids: HashSet<u64>,
    pub(in crate::codex::transport) order: VecDeque<u64>,
}

impl RetiredRequestIds {
    pub(in crate::codex::transport) fn insert(&mut self, id: u64) {
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

    pub(in crate::codex::transport) fn remove(&mut self, id: u64) -> bool {
        self.ids.remove(&id)
    }
}

pub(in crate::codex::transport) struct ConnectionRuntime {
    pub(in crate::codex::transport) child: Child,
    pub(in crate::codex::transport) stdin: ChildStdin,
    pub(in crate::codex::transport) commands: mpsc::Receiver<CommandMessage>,
    pub(in crate::codex::transport) shutdown: mpsc::Receiver<ShutdownMessage>,
    pub(in crate::codex::transport) reader: mpsc::Receiver<ReaderMessage>,
    pub(in crate::codex::transport) events: mpsc::Sender<TransportEvent>,
    pub(in crate::codex::transport) generation: u64,
    pub(in crate::codex::transport) diagnostics: Arc<dyn DiagnosticSink>,
}
