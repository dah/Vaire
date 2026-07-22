use super::*;

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
