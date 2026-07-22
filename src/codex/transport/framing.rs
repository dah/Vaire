use super::*;

pub(in crate::codex::transport) struct BoundedJsonWriter {
    bytes: Vec<u8>,
    exceeded: bool,
}

impl BoundedJsonWriter {
    pub(in crate::codex::transport) fn new() -> Self {
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

pub(in crate::codex::transport) fn bounded_json_value<T: Serialize>(
    value: T,
) -> Result<Value, TransportError> {
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

pub(in crate::codex::transport) async fn write_frame(
    stdin: &mut ChildStdin,
    frame: &Value,
) -> Result<(), TransportError> {
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

pub(in crate::codex::transport) fn encode_frame(frame: &Value) -> Result<Vec<u8>, TransportError> {
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

pub(in crate::codex::transport) async fn write_frame_before(
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

pub(in crate::codex::transport) fn request_deadline(
    timeout: Duration,
) -> Result<Instant, TransportError> {
    Instant::now().checked_add(timeout).ok_or_else(|| {
        TransportError::Protocol("request timeout exceeds the supported timeout range".to_owned())
    })
}

pub(in crate::codex::transport) async fn reap_child(
    child: &mut Child,
) -> Result<(), TransportError> {
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
