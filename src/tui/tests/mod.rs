use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::Backend, backend::TestBackend, layout::Position, Terminal};
use unicode_width::UnicodeWidthStr;

use super::{
    header_text, render, sanitize_terminal_text, truncate_for_display, wrap_for_display, UiState,
    ACTIVITY_FRAMES, ACTIVITY_TICKS_PER_FRAME, MAX_COMPOSER_BYTES,
};
use crate::app::{
    Action, AppState, AuthState, ConnectionState, DomainEvent, Intent, ThinkingEntry, ThinkingKind,
    ThreadChoice, ThreadDeleteConfirmation, ThreadPickerPhase, ThreadPickerState, ThreadState,
    TranscriptEntry, TranscriptRole, TurnState,
};
use crate::persistence::AccountScope;

fn draw(state: &AppState, ui: &UiState, width: u16, height: u16) -> Terminal<TestBackend> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| render(frame, state, ui)).unwrap();
    terminal
}

fn screen(state: &AppState, ui: &UiState, width: u16, height: u16) -> String {
    let terminal = draw(state, ui, width, height);
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn header(state: &AppState, width: u16) -> String {
    screen(state, &UiState::default(), width, 20)
        .lines()
        .next()
        .unwrap()
        .to_owned()
}

fn ready() -> AppState {
    AppState {
        connection: ConnectionState::Ready { generation: 1 },
        auth: AuthState::SignedIn {
            scope: AccountScope::from_chatgpt_email("user@example.com"),
        },
        thread: ThreadState::Ready {
            id: "thread".to_owned(),
        },
        selected_model: Some("model-a".to_owned()),
        selected_reasoning: Some("high".to_owned()),
        ..AppState::default()
    }
}

fn waiting() -> AppState {
    let mut state = ready();
    state.turn = TurnState::Starting;
    state.transcript.push(TranscriptEntry {
        role: TranscriptRole::User,
        text: "hello".to_owned(),
        item_id: None,
        turn_id: None,
    });
    state
}

mod conversation;
mod display;
mod input_activity;
mod layout_header;
mod thread_picker;
