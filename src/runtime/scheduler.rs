use super::*;

pub(in crate::runtime) enum RuntimeWork {
    Shutdown,
    Intent(Option<Intent>),
    Event(Option<Result<SessionEvent, SessionError>>),
}

pub(in crate::runtime) enum EventCompletion {
    Shutdown,
    Finished(Result<bool, crate::backend::BackendError>),
}

pub(in crate::runtime) async fn next_open_work<Event>(
    shutdowns: &mut mpsc::Receiver<()>,
    intents: &mut mpsc::Receiver<Intent>,
    event: Event,
    prefer_event: bool,
) -> RuntimeWork
where
    Event: Future<Output = Option<Result<SessionEvent, SessionError>>>,
{
    if prefer_event {
        tokio::select! {
            biased;
            _ = shutdowns.recv() => RuntimeWork::Shutdown,
            event = event => RuntimeWork::Event(event),
            intent = intents.recv() => RuntimeWork::Intent(intent),
        }
    } else {
        tokio::select! {
            biased;
            _ = shutdowns.recv() => RuntimeWork::Shutdown,
            intent = intents.recv() => RuntimeWork::Intent(intent),
            event = event => RuntimeWork::Event(event),
        }
    }
}

pub(in crate::runtime) async fn finish_event_or_shutdown<Process>(
    shutdowns: &mut mpsc::Receiver<()>,
    process: Process,
) -> EventCompletion
where
    Process: Future<Output = Result<bool, crate::backend::BackendError>>,
{
    tokio::select! {
        biased;
        _ = shutdowns.recv() => EventCompletion::Shutdown,
        result = process => EventCompletion::Finished(result),
    }
}

pub(in crate::runtime) async fn run_backend(
    config: RuntimeConfig,
    mut intents: mpsc::Receiver<Intent>,
    mut shutdowns: mpsc::Receiver<()>,
    states: watch::Sender<AppState>,
) {
    let mut backend = match build_backend(config).await {
        Ok(backend) => backend,
        Err(error) => {
            run_failed_backend(&mut intents, &mut shutdowns, &states, error.to_string()).await;
            return;
        }
    };

    enum Operation<T> {
        Shutdown,
        Finished(T),
    }

    let startup = tokio::select! {
        biased;
        _ = shutdowns.recv() => Operation::Shutdown,
        result = backend.startup() => Operation::Finished(result),
    };
    match startup {
        Operation::Shutdown => {
            shutdown_backend(&mut backend, &states).await;
            return;
        }
        Operation::Finished(Err(error)) => backend.record_error(error.to_string()),
        Operation::Finished(Ok(())) => {}
    }
    publish(&states, backend.state());
    let mut event_open = true;
    let mut prefer_event = true;

    loop {
        let work = if event_open {
            next_open_work(
                &mut shutdowns,
                &mut intents,
                backend.receive_event(),
                prefer_event,
            )
            .await
        } else {
            tokio::select! {
                biased;
                _ = shutdowns.recv() => RuntimeWork::Shutdown,
                intent = intents.recv() => RuntimeWork::Intent(intent),
            }
        };

        match work {
            RuntimeWork::Shutdown => {
                shutdown_backend(&mut backend, &states).await;
                break;
            }
            RuntimeWork::Intent(Some(intent)) => {
                prefer_event = true;
                let quitting = matches!(intent, Intent::Quit);
                let effects = backend.accept_intent(intent);
                publish(&states, backend.state());
                let execution = if quitting {
                    Operation::Finished(backend.execute_pending(effects).await)
                } else {
                    tokio::select! {
                        biased;
                        _ = shutdowns.recv() => Operation::Shutdown,
                        result = backend.execute_pending(effects) => Operation::Finished(result),
                    }
                };
                match execution {
                    Operation::Shutdown => {
                        shutdown_backend(&mut backend, &states).await;
                        break;
                    }
                    Operation::Finished(Err(error)) => backend.record_error(error.to_string()),
                    Operation::Finished(Ok(())) => {}
                }
                publish(&states, backend.state());
                if quitting {
                    break;
                }
            }
            RuntimeWork::Intent(None) => {
                let effects = backend.accept_intent(Intent::Quit);
                let _ = backend.execute_pending(effects).await;
                publish(&states, backend.state());
                break;
            }
            RuntimeWork::Event(event) => {
                prefer_event = false;
                match finish_event_or_shutdown(
                    &mut shutdowns,
                    backend.process_received_event(event),
                )
                .await
                {
                    EventCompletion::Shutdown => {
                        shutdown_backend(&mut backend, &states).await;
                        break;
                    }
                    EventCompletion::Finished(Ok(open)) => event_open = open,
                    EventCompletion::Finished(Err(error)) => {
                        backend.record_error(error.to_string());
                        event_open = false;
                    }
                }
                publish(&states, backend.state());
            }
        }
    }
}

pub(in crate::runtime) async fn run_failed_backend(
    intents: &mut mpsc::Receiver<Intent>,
    shutdowns: &mut mpsc::Receiver<()>,
    states: &watch::Sender<AppState>,
    message: String,
) {
    let mut state = AppState {
        connection: ConnectionState::Failed(message.clone()),
        auth: AuthState::SignedOut,
        notice: Some(message),
        ..AppState::default()
    };
    publish(states, &state);
    loop {
        let intent = tokio::select! {
            biased;
            _ = shutdowns.recv() => Intent::Quit,
            intent = intents.recv() => match intent {
                Some(intent) => intent,
                None => Intent::Quit,
            },
        };
        let quitting = matches!(intent, Intent::Quit);
        let _ = state.reduce(Action::Intent(intent));
        publish(states, &state);
        if quitting {
            break;
        }
    }
}

pub(in crate::runtime) async fn shutdown_backend(
    backend: &mut BackendCoordinator<FilePreferences, MacOsBrowser>,
    states: &watch::Sender<AppState>,
) {
    let effects = backend.accept_intent(Intent::Quit);
    publish(states, backend.state());
    if let Err(error) = backend.execute_pending(effects).await {
        backend.record_error(error.to_string());
    }
    publish(states, backend.state());
}
