use super::*;

pub(in crate::codex::transport) async fn run_connection(runtime: ConnectionRuntime) {
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
