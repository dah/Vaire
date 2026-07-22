use std::io::{self, stdout, Stdout};
use std::time::Duration;

use agentharness::app::{AppState, Intent};
use agentharness::runtime::{RuntimeConfig, RuntimeHandle};
use agentharness::terminal::{SystemTerminalOps, TerminalGuard};
use agentharness::tui::{render, UiState};
use crossterm::event;
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::signal::unix::{signal, SignalKind};
use tokio::time::{self, MissedTickBehavior};

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("AgentHarness could not run: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config = RuntimeConfig::discover()?;
    let mut signals = Signals::install()?;
    let mut guard = TerminalGuard::enter(SystemTerminalOps)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;
    let runtime = RuntimeHandle::spawn(config);
    let result = event_loop(&mut terminal, &runtime, &mut signals).await;
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
) -> io::Result<()> {
    let mut states = runtime.subscribe();
    let mut state: AppState = states.borrow_and_update().clone();
    let mut ui = UiState::default();
    let mut redraw = true;
    let mut tick = time::interval(Duration::from_millis(33));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    while !state.shutting_down {
        if redraw {
            terminal.draw(|frame| render(frame, &state, &ui))?;
            redraw = false;
        }
        tokio::select! {
            _ = tick.tick() => {
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
                }
                redraw = true;
            }
            _ = signals.interrupt.recv() => request_signal_shutdown(runtime, &mut state),
            _ = signals.terminate.recv() => request_signal_shutdown(runtime, &mut state),
            _ = signals.hangup.recv() => request_signal_shutdown(runtime, &mut state),
            _ = signals.quit.recv() => request_signal_shutdown(runtime, &mut state),
        }
    }
    terminal.draw(|frame| render(frame, &state, &ui))?;
    Ok(())
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
}
