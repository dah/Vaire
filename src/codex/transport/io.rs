use super::*;

pub(in crate::codex::transport) async fn read_stdout(
    stdout: tokio::process::ChildStdout,
    sender: mpsc::Sender<ReaderMessage>,
) {
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

pub(in crate::codex::transport) async fn drain_stderr(
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
