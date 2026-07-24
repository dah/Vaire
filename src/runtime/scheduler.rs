use super::*;
use crate::backend::BackendRuntimeEvent;

pub(in crate::runtime) enum RuntimeWork {
    Shutdown,
    Command(Option<RuntimeCommand>),
    Event(BackendRuntimeEvent),
}

#[cfg(test)]
pub(in crate::runtime) enum EventCompletion {
    Shutdown,
    Finished(Result<bool, crate::backend::BackendError>),
}

pub(in crate::runtime) async fn next_open_work<Event>(
    shutdowns: &mut mpsc::Receiver<()>,
    intents: &mut mpsc::Receiver<RuntimeCommand>,
    event: Event,
    prefer_event: bool,
) -> RuntimeWork
where
    Event: Future<Output = BackendRuntimeEvent>,
{
    if prefer_event {
        tokio::select! {
            biased;
            _ = shutdowns.recv() => RuntimeWork::Shutdown,
            event = event => RuntimeWork::Event(event),
            intent = intents.recv() => RuntimeWork::Command(intent),
        }
    } else {
        tokio::select! {
            biased;
            _ = shutdowns.recv() => RuntimeWork::Shutdown,
            intent = intents.recv() => RuntimeWork::Command(intent),
            event = event => RuntimeWork::Event(event),
        }
    }
}

#[cfg(test)]
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

pub(in crate::runtime) async fn finish_work_and_latch_shutdown<T, Work>(
    shutdowns: &mut mpsc::Receiver<()>,
    work: Work,
) -> (T, bool)
where
    Work: Future<Output = T>,
{
    tokio::pin!(work);
    tokio::select! {
        biased;
        _ = shutdowns.recv() => (work.await, true),
        result = &mut work => (result, false),
    }
}

pub(in crate::runtime) async fn run_backend(
    config: RuntimeConfig,
    mut intents: mpsc::Receiver<RuntimeCommand>,
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
                intent = intents.recv() => RuntimeWork::Command(intent),
            }
        };

        match work {
            RuntimeWork::Shutdown => {
                shutdown_backend(&mut backend, &states).await;
                break;
            }
            RuntimeWork::Command(Some(RuntimeCommand::Intent(intent))) => {
                prefer_event = true;
                let quitting = matches!(intent, Intent::Quit);
                let effects = backend.accept_intent(intent);
                publish(&states, backend.state());
                let (execution, shutdown_latched) = if quitting {
                    (backend.execute_pending(effects).await, false)
                } else {
                    finish_work_and_latch_shutdown(&mut shutdowns, backend.execute_pending(effects))
                        .await
                };
                if let Err(error) = execution {
                    backend.record_error(error.to_string());
                }
                publish(&states, backend.state());
                if quitting {
                    break;
                }
                if shutdown_latched {
                    shutdown_backend(&mut backend, &states).await;
                    break;
                }
            }
            RuntimeWork::Command(Some(RuntimeCommand::OpenRouterCredential(value))) => {
                prefer_event = true;
                let _ = backend.accept_openrouter_credential(value);
                publish(&states, backend.state());
            }
            RuntimeWork::Command(Some(RuntimeCommand::ClaudeCredential(value))) => {
                prefer_event = true;
                let (_, shutdown_latched) = finish_work_and_latch_shutdown(
                    &mut shutdowns,
                    backend.accept_claude_credential(value),
                )
                .await;
                publish(&states, backend.state());
                if shutdown_latched {
                    shutdown_backend(&mut backend, &states).await;
                    break;
                }
            }
            RuntimeWork::Command(None) => {
                let effects = backend.accept_intent(Intent::Quit);
                let _ = backend.execute_pending(effects).await;
                publish(&states, backend.state());
                break;
            }
            RuntimeWork::Event(event) => {
                prefer_event = false;
                match backend.process_received_event(event).await {
                    Ok(open) => event_open = open,
                    Err(error) => {
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
    intents: &mut mpsc::Receiver<RuntimeCommand>,
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
        let command = tokio::select! {
            biased;
            _ = shutdowns.recv() => RuntimeCommand::Intent(Intent::Quit),
            command = intents.recv() => match command {
                Some(command) => command,
                None => RuntimeCommand::Intent(Intent::Quit),
            },
        };
        let RuntimeCommand::Intent(intent) = command else {
            continue;
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
