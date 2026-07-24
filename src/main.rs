use std::future::Future;
use std::io::{self, stdout, Stdout};
use std::time::Duration;

use crossterm::event;
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::signal::unix::{signal, SignalKind};
use tokio::time::{self, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use vaire::app::{AppState, Intent};
use vaire::runtime::{RuntimeConfig, RuntimeHandle};
use vaire::terminal::{SystemTerminalOps, TerminalGuard, TerminalOps};
use vaire::tui::{render, UiState};

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("Vairë could not run: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config = RuntimeConfig::discover()?;
    let mut signals = Signals::install()?;
    let mut guard = TerminalGuard::enter(SystemTerminalOps::capture()?)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;
    let runtime = RuntimeHandle::spawn(config);
    let result = event_loop(&mut terminal, &runtime, &mut signals, &mut guard).await;
    runtime.shutdown().await;
    drop(terminal);
    let restore = guard.restore();
    result?;
    restore?;
    Ok(())
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    runtime: &RuntimeHandle,
    signals: &mut Signals,
    guard: &mut TerminalGuard<SystemTerminalOps>,
) -> io::Result<()> {
    let mut states = runtime.subscribe();
    let mut state: AppState = states.borrow_and_update().clone();
    let mut ui = UiState::default();
    ui.sync_activity_animation(&state);
    ui.sync_secret_editor(&state);
    let mut redraw = true;
    let mut tick = time::interval(Duration::from_millis(33));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut handled_claude_auth_operation = None;
    while !state.shutting_down {
        if let Some(request) = state
            .pending_claude_auth_request()
            .filter(|request| handled_claude_auth_operation != Some(request.operation_id))
            .cloned()
        {
            if redraw {
                terminal.draw(|frame| render(frame, &state, &ui))?;
            }
            let action = request.action;
            let auth = run_foreground_auth(
                guard,
                |cancellation| runtime.run_claude_auth(action, cancellation),
                signals.recv(),
            )
            .await?;
            let ForegroundAuthOutcome::Completed(result) = auth else {
                state.shutting_down = true;
                runtime.request_shutdown();
                return Ok(());
            };
            terminal.clear()?;
            handled_claude_auth_operation = Some(request.operation_id);
            runtime
                .finish_claude_auth(request, result)
                .await
                .map_err(io::Error::other)?;
            redraw = true;
            continue;
        }
        if redraw {
            terminal.draw(|frame| render(frame, &state, &ui))?;
            redraw = false;
        }
        tokio::select! {
            _ = tick.tick() => {
                if ui.advance_activity_animation(&state) {
                    redraw = true;
                }
                for _ in 0..32 {
                    if !event::poll(Duration::ZERO)? {
                        break;
                    }
                    if let Some(intent) = ui.handle_event_for_state(event::read()?, &state) {
                        if matches!(intent, Intent::Quit) {
                            state.shutting_down = true;
                            runtime.request_shutdown();
                        } else if let Err(message) = runtime.try_send(intent) {
                            ui.overlay = Some(message.to_owned());
                        }
                    }
                    if let Some((provider, secret)) = ui.take_submitted_secret() {
                        if let Err((secret, message)) = runtime.try_send_openrouter_credential(secret) {
                            ui.restore_provider_secret(provider, secret);
                            ui.overlay = Some(message.to_owned());
                        }
                    }
                    redraw = true;
                }
            }
            changed = states.changed() => {
                if changed.is_err() {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "background backend stopped unexpectedly",
                    ));
                } else {
                    state = states.borrow_and_update().clone();
                    ui.sync_activity_animation(&state);
                    ui.sync_secret_editor(&state);
                }
                redraw = true;
            }
            _ = signals.recv() => request_signal_shutdown(runtime, &mut state),
        }
    }
    terminal.draw(|frame| render(frame, &state, &ui))?;
    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
enum ForegroundAuthOutcome<T> {
    Completed(T),
    Interrupted,
}

async fn run_foreground_auth<T, Start, Auth, Signal, Output>(
    guard: &mut TerminalGuard<T>,
    start_auth: Start,
    signal: Signal,
) -> io::Result<ForegroundAuthOutcome<Output>>
where
    T: TerminalOps,
    Start: FnOnce(CancellationToken) -> Auth,
    Auth: Future<Output = Output>,
    Signal: Future<Output = ()>,
{
    guard.suspend()?;
    let cancellation = CancellationToken::new();
    let auth = start_auth(cancellation.clone());
    tokio::pin!(auth);
    tokio::pin!(signal);
    let outcome = tokio::select! {
        result = &mut auth => ForegroundAuthOutcome::Completed(result),
        _ = &mut signal => {
            cancellation.cancel();
            let _ = (&mut auth).await;
            ForegroundAuthOutcome::Interrupted
        }
    };
    guard.normalize_after_foreground_child()?;
    if matches!(&outcome, ForegroundAuthOutcome::Completed(_)) {
        guard.resume()?;
    }
    Ok(outcome)
}

fn request_signal_shutdown(runtime: &RuntimeHandle, state: &mut AppState) {
    state.shutting_down = true;
    runtime.request_shutdown();
}

struct Signals {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
    hangup: tokio::signal::unix::Signal,
    quit: tokio::signal::unix::Signal,
}

impl Signals {
    fn install() -> io::Result<Self> {
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
            hangup: signal(SignalKind::hangup())?,
            quit: signal(SignalKind::quit())?,
        })
    }

    async fn recv(&mut self) {
        tokio::select! {
            _ = self.interrupt.recv() => {}
            _ = self.terminate.recv() => {}
            _ = self.hangup.recv() => {}
            _ = self.quit.recv() => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    };

    use super::*;

    #[derive(Clone)]
    struct RecordingTerminalOps {
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl TerminalOps for RecordingTerminalOps {
        fn enter(&mut self) -> io::Result<()> {
            self.events.lock().unwrap().push("enter");
            Ok(())
        }

        fn restore(&mut self) -> io::Result<()> {
            self.events.lock().unwrap().push("restore");
            Ok(())
        }
    }

    #[tokio::test]
    async fn foreground_auth_suspends_normalizes_resumes_and_restores_in_order() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut guard = TerminalGuard::enter(RecordingTerminalOps {
            events: Arc::clone(&events),
        })
        .unwrap();

        let outcome =
            run_foreground_auth(&mut guard, |_| async { 7_u8 }, std::future::pending::<()>())
                .await
                .unwrap();
        assert_eq!(outcome, ForegroundAuthOutcome::Completed(7));
        drop(guard);

        assert_eq!(
            *events.lock().unwrap(),
            vec!["enter", "restore", "restore", "enter", "restore"]
        );
    }

    #[tokio::test]
    async fn foreground_auth_signal_cancels_and_settles_before_terminal_normalization() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let cancelled = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&cancelled);
        let mut guard = TerminalGuard::enter(RecordingTerminalOps {
            events: Arc::clone(&events),
        })
        .unwrap();

        let outcome = run_foreground_auth(
            &mut guard,
            move |cancellation| async move {
                cancellation.cancelled().await;
                observed.store(true, Ordering::SeqCst);
            },
            std::future::ready(()),
        )
        .await
        .unwrap();
        assert_eq!(outcome, ForegroundAuthOutcome::Interrupted);
        assert!(cancelled.load(Ordering::SeqCst));
        drop(guard);

        assert_eq!(*events.lock().unwrap(), vec!["enter", "restore", "restore"]);
    }
}
