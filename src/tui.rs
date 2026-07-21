use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};
use unicode_width::UnicodeWidthChar;

use crate::{
    app::{AppState, AuthState, ConnectionState, Intent, ThreadState, TranscriptRole, TurnState},
    command::{parse, HELP_TEXT},
};

const MIN_WIDTH: u16 = 36;
const MIN_HEIGHT: u16 = 9;
const MAX_COMPOSER_ROWS: usize = 5;
const MAX_MESSAGE_ROWS: usize = 4;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UiState {
    pub composer: String,
    pub overlay: Option<String>,
    /// Number of wrapped transcript rows held above the automatic bottom position.
    pub scroll_from_bottom: usize,
}

impl UiState {
    pub fn handle_event(&mut self, event: Event) -> Option<Intent> {
        match event {
            Event::Key(key) => self.handle_key(key),
            Event::Paste(text) => {
                self.composer.push_str(&sanitize_terminal_text(&text));
                None
            }
            _ => None,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Intent> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(Intent::Quit);
        }
        match key.code {
            KeyCode::Esc => {
                if self.overlay.take().is_some() {
                    None
                } else {
                    Some(Intent::Interrupt)
                }
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                self.composer.push('\n');
                None
            }
            KeyCode::Enter => self.submit(),
            KeyCode::Backspace => {
                self.composer.pop();
                None
            }
            KeyCode::PageUp | KeyCode::Up => {
                self.scroll_from_bottom = self.scroll_from_bottom.saturating_add(1);
                None
            }
            KeyCode::PageDown | KeyCode::Down => {
                self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(1);
                None
            }
            KeyCode::Home => {
                self.scroll_from_bottom = usize::MAX / 2;
                None
            }
            KeyCode::End => {
                self.scroll_from_bottom = 0;
                None
            }
            KeyCode::Char(character)
                if !key.modifiers.contains(KeyModifiers::CONTROL) && !character.is_control() =>
            {
                self.composer.push(character);
                None
            }
            _ => None,
        }
    }

    fn submit(&mut self) -> Option<Intent> {
        match parse(&self.composer) {
            Ok(Intent::Help) => {
                self.overlay = Some(HELP_TEXT.to_owned());
                self.composer.clear();
                None
            }
            Ok(intent) => {
                self.composer.clear();
                self.scroll_from_bottom = 0;
                Some(intent)
            }
            Err(error) => {
                self.overlay = Some(error.to_string());
                None
            }
        }
    }
}

/// Removes terminal control characters before any text is measured or rendered.
/// Newlines remain as layout separators; tabs become spaces.
pub fn sanitize_terminal_text(value: &str) -> String {
    let mut sanitized = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\n' => sanitized.push('\n'),
            '\t' => sanitized.push_str("    "),
            character if character.is_control() => {}
            character => sanitized.push(character),
        }
    }
    sanitized
}

pub fn render(frame: &mut Frame<'_>, state: &AppState, ui: &UiState) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        let message = format!(
            "AgentHarness\nTerminal too small ({}x{}). Resize to at least {MIN_WIDTH}x{MIN_HEIGHT}.\nCtrl-C quits.",
            area.width, area.height
        );
        frame.render_widget(
            Paragraph::new(message)
                .block(Block::default().borders(Borders::ALL))
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }

    let composer_text = sanitize_terminal_text(&ui.composer);
    let content_width = area.width.saturating_sub(2).max(1);
    let composer_wrapped = wrap_for_display(&composer_text, content_width);
    let message = message_text(state);
    let message_wrapped = message
        .as_ref()
        .map(|message| wrap_for_display(&message.text, content_width));
    let (composer_rows, message_rows) = panel_rows(
        area.height,
        composer_wrapped.rows,
        message_wrapped.as_ref().map(|wrapped| wrapped.rows),
    );
    let regions = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(2),
            Constraint::Length(composer_rows),
            Constraint::Length(message_rows),
        ])
        .split(area);

    frame.render_widget(
        Paragraph::new(status_text(state)).style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        regions[0],
    );
    render_transcript(frame, regions[1], state, ui.scroll_from_bottom);
    render_composer(frame, regions[2], &composer_wrapped);
    render_message_or_help(
        frame,
        regions[3],
        message.as_ref(),
        message_wrapped.as_ref(),
    );

    if let Some(overlay) = &ui.overlay {
        render_overlay(frame, area, overlay);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WrappedText {
    text: String,
    rows: usize,
    tail_column: usize,
}

fn wrap_for_display(value: &str, width: u16) -> WrappedText {
    let width = usize::from(width.max(1));
    let mut text = String::with_capacity(value.len());
    let mut row = 0_usize;
    let mut column = 0_usize;
    for character in value.chars() {
        if character == '\n' {
            text.push('\n');
            row = row.saturating_add(1);
            column = 0;
            continue;
        }
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if column > 0 && column.saturating_add(character_width) > width {
            text.push('\n');
            row = row.saturating_add(1);
            column = 0;
        }
        text.push(character);
        column = column.saturating_add(character_width);
    }
    WrappedText {
        text,
        rows: row.saturating_add(1),
        tail_column: column,
    }
}

fn panel_rows(
    terminal_height: u16,
    requested_composer_rows: usize,
    requested_message_rows: Option<usize>,
) -> (u16, u16) {
    let composer_rows = requested_composer_rows.clamp(1, MAX_COMPOSER_ROWS);
    let Some(requested_message_rows) = requested_message_rows else {
        let extra_budget = usize::from(terminal_height.saturating_sub(7));
        let composer_extra = composer_rows.saturating_sub(1).min(extra_budget);
        return ((3 + composer_extra) as u16, 1);
    };

    // Base: status 1 + transcript 2 + composer 3 + message 3 = 9 rows.
    let mut extra_budget = usize::from(terminal_height.saturating_sub(9));
    let requested_message_rows = requested_message_rows.clamp(1, MAX_MESSAGE_ROWS);
    let message_extra = requested_message_rows.saturating_sub(1).min(extra_budget);
    extra_budget = extra_budget.saturating_sub(message_extra);
    let composer_extra = composer_rows.saturating_sub(1).min(extra_budget);
    ((3 + composer_extra) as u16, (3 + message_extra) as u16)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MessageText {
    title: &'static str,
    text: String,
    failure: bool,
}

fn message_text(state: &AppState) -> Option<MessageText> {
    if let TurnState::Failed { message, .. } = &state.turn {
        let failure = sanitize_terminal_text(message);
        let notice = state.notice.as_deref().map(sanitize_terminal_text);
        let text = notice
            .filter(|notice| notice != &failure)
            .map_or_else(|| failure.clone(), |notice| format!("{failure}\n{notice}"));
        return Some(MessageText {
            title: " Turn failed ",
            text,
            failure: true,
        });
    }
    state.notice.as_deref().map(|notice| MessageText {
        title: " Message ",
        text: sanitize_terminal_text(notice),
        failure: false,
    })
}

fn status_text(state: &AppState) -> String {
    let connection = match &state.connection {
        ConnectionState::Disconnected => "offline".to_owned(),
        ConnectionState::Connecting => "connecting".to_owned(),
        ConnectionState::Ready { .. } => "connected".to_owned(),
        ConnectionState::Failed(message) => {
            format!(
                "error: {}",
                sanitize_terminal_text(message).replace('\n', " ")
            )
        }
    };
    let auth = match &state.auth {
        AuthState::Unknown => "auth?".to_owned(),
        AuthState::SignedOut => "signed out".to_owned(),
        AuthState::SigningIn { .. } => "signing in".to_owned(),
        AuthState::SignedIn { .. } => "signed in".to_owned(),
        AuthState::Unsupported(message) => {
            format!("unsupported: {}", sanitize_terminal_text(message))
        }
    };
    let thread = match &state.thread {
        ThreadState::None => "no thread".to_owned(),
        ThreadState::Resuming { .. } => "resuming".to_owned(),
        ThreadState::Ready { .. } => "thread ready".to_owned(),
        ThreadState::ResumeFailed { .. } => "resume failed".to_owned(),
        ThreadState::AccountMismatch { .. } => "account mismatch".to_owned(),
    };
    let turn = match &state.turn {
        TurnState::Idle => "idle",
        TurnState::Starting => "starting",
        TurnState::Streaming { .. } => "streaming",
        TurnState::Completed { .. } => "completed",
        TurnState::Interrupted { .. } => "interrupted",
        TurnState::Failed { .. } => "failed",
    };
    let model = state
        .selected_model
        .as_deref()
        .map(sanitize_terminal_text)
        .unwrap_or_else(|| "model?".to_owned());
    let reasoning = state
        .selected_reasoning
        .as_deref()
        .map(sanitize_terminal_text)
        .unwrap_or_else(|| "reasoning?".to_owned());
    let shutdown = if state.shutting_down {
        " • shutting down"
    } else {
        ""
    };
    format!(" {connection} • {auth} • {thread} • {model}/{reasoning} • {turn}{shutdown}")
}

fn render_transcript(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    scroll_from_bottom: usize,
) {
    let mut lines = Vec::<Line<'static>>::new();
    if state.transcript.is_empty() {
        let prompt = match (&state.auth, &state.thread) {
            (AuthState::SignedOut, _) => {
                "Signed out. Use /login to connect your ChatGPT subscription."
            }
            (_, ThreadState::ResumeFailed { .. }) => {
                "The saved thread could not be resumed. Use /resume to retry; it was not replaced."
            }
            _ => "Ready for one conversation. Type a message and press Enter.",
        };
        lines.push(Line::from(Span::styled(
            prompt,
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for entry in &state.transcript {
            let (prefix, color) = match entry.role {
                TranscriptRole::User => ("You", Color::Green),
                TranscriptRole::Assistant => ("Agent", Color::Cyan),
            };
            lines.push(Line::from(Span::styled(
                format!("{prefix}:"),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )));
            let text = sanitize_terminal_text(&entry.text);
            if text.is_empty() {
                lines.push(Line::from(""));
            } else {
                lines.extend(text.lines().map(|line| Line::from(line.to_owned())));
            }
            lines.push(Line::from(""));
        }
    }

    let block = Block::default()
        .title(" Conversation ")
        .borders(Borders::ALL);
    let inner = block.inner(area);
    let wrap_width = inner.width.max(1) as usize;
    let line_count = lines
        .iter()
        .map(|line| line.width().max(1).div_ceil(wrap_width))
        .sum::<usize>();
    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    let max_top = line_count.saturating_sub(inner.height as usize);
    let from_bottom = scroll_from_bottom.min(max_top);
    let top = max_top.saturating_sub(from_bottom).min(u16::MAX as usize) as u16;
    frame.render_widget(paragraph.scroll((top, 0)), area);
}

fn render_composer(frame: &mut Frame<'_>, area: Rect, wrapped: &WrappedText) {
    let block = Block::default().title(" Message ").borders(Borders::ALL);
    let inner = block.inner(area);
    let scroll = wrapped.rows.saturating_sub(inner.height as usize);
    frame.render_widget(
        Paragraph::new(wrapped.text.clone())
            .block(block)
            .scroll((scroll.min(u16::MAX as usize) as u16, 0)),
        area,
    );
    let last_line_width = wrapped.tail_column.min(u16::MAX as usize) as u16;
    let visible_tail_row = wrapped
        .rows
        .saturating_sub(1)
        .saturating_sub(scroll)
        .min(u16::MAX as usize) as u16;
    let x = inner
        .x
        .saturating_add(last_line_width.min(inner.width.saturating_sub(1)));
    let y = inner
        .y
        .saturating_add(visible_tail_row.min(inner.height.saturating_sub(1)));
    frame.set_cursor_position((x, y));
}

fn render_message_or_help(
    frame: &mut Frame<'_>,
    area: Rect,
    message: Option<&MessageText>,
    wrapped: Option<&WrappedText>,
) {
    let (Some(message), Some(wrapped)) = (message, wrapped) else {
        frame.render_widget(
            Paragraph::new(
                "Enter send • Alt-Enter newline • PgUp/PgDn scroll • Esc interrupt/help • /help",
            )
            .style(Style::default().fg(Color::Yellow)),
            area,
        );
        return;
    };
    let color = if message.failure {
        Color::Red
    } else {
        Color::Yellow
    };
    frame.render_widget(
        Paragraph::new(wrapped.text.clone())
            .block(Block::default().title(message.title).borders(Borders::ALL))
            .style(Style::default().fg(color)),
        area,
    );
}

fn render_overlay(frame: &mut Frame<'_>, area: Rect, value: &str) {
    let width = area.width.saturating_sub(6).min(76);
    let height = area.height.saturating_sub(4).min(16);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    let popup = Rect::new(x, y, width, height);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(sanitize_terminal_text(value))
            .block(
                Block::default()
                    .title(" Help / message — Esc closes ")
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: false }),
        popup,
    );
}

#[cfg(test)]
mod tests {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::Backend, backend::TestBackend, layout::Position, Terminal};

    use super::{render, sanitize_terminal_text, UiState};
    use crate::app::{
        Action, AppState, AuthState, ConnectionState, DomainEvent, ThreadState, TranscriptEntry,
        TranscriptRole, TurnState,
    };

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

    fn ready() -> AppState {
        AppState {
            connection: ConnectionState::Ready { generation: 1 },
            auth: AuthState::SignedIn { scope: None },
            thread: ThreadState::Ready {
                id: "thread".to_owned(),
            },
            selected_model: Some("model-a".to_owned()),
            selected_reasoning: Some("high".to_owned()),
            ..AppState::default()
        }
    }

    #[test]
    fn renders_signed_out_and_resume_failed_actions() {
        let mut state = AppState {
            connection: ConnectionState::Ready { generation: 1 },
            auth: AuthState::SignedOut,
            ..AppState::default()
        };
        assert!(screen(&state, &UiState::default(), 90, 20).contains("/login"));
        state.auth = AuthState::SignedIn { scope: None };
        state.thread = ThreadState::ResumeFailed {
            id: "secret-id".to_owned(),
            message: "gone".to_owned(),
        };
        let rendered = screen(&state, &UiState::default(), 90, 20);
        assert!(rendered.contains("resume failed"));
        assert!(rendered.contains("/resume"));
        assert!(!rendered.contains("secret-id"));
    }

    #[test]
    fn login_completion_is_visible_without_follow_up_input() {
        let mut state = AppState {
            connection: ConnectionState::Ready { generation: 1 },
            auth: AuthState::SigningIn {
                login_id: "login-active".to_owned(),
            },
            notice: Some("Complete sign-in in the browser".to_owned()),
            ..AppState::default()
        };

        state.reduce(Action::Event(DomainEvent::AccountLoaded(None)));

        let rendered = screen(&state, &UiState::default(), 90, 20);
        assert!(rendered.contains("signed in"));
        assert!(rendered.contains("Signed in to ChatGPT"));
        assert!(!rendered.contains("Complete sign-in"));
    }

    #[test]
    fn renders_ready_streaming_completed_and_error_states() {
        let mut state = ready();
        assert!(screen(&state, &UiState::default(), 100, 20).contains("thread ready"));
        state.transcript.push(TranscriptEntry {
            role: TranscriptRole::Assistant,
            text: "partial reply".to_owned(),
            item_id: None,
            turn_id: None,
        });
        state.turn = TurnState::Streaming {
            thread_id: "thread".to_owned(),
            turn_id: "turn".to_owned(),
        };
        let streaming = screen(&state, &UiState::default(), 100, 20);
        assert!(streaming.contains("streaming"));
        assert!(streaming.contains("partial reply"));
        state.turn = TurnState::Completed {
            turn_id: "turn".to_owned(),
        };
        assert!(screen(&state, &UiState::default(), 100, 20).contains("completed"));
        state.connection = ConnectionState::Failed("upgrade Codex".to_owned());
        state.turn = TurnState::Failed {
            turn_id: None,
            message: "failed".to_owned(),
        };
        let failed = screen(&state, &UiState::default(), 100, 20);
        assert!(failed.contains("upgrade Codex"));
        assert!(failed.contains("failed"));
    }

    #[test]
    fn handles_small_terminals_and_malicious_control_text() {
        let small = screen(&ready(), &UiState::default(), 24, 6);
        assert!(small.contains("Terminal too small"));
        let mut state = ready();
        state.connection = ConnectionState::Failed("\u{1b}[2Jbad\u{009b}text".to_owned());
        state.transcript.push(TranscriptEntry {
            role: TranscriptRole::Assistant,
            text: "safe\u{1b}[31m\ttext\u{0007}".to_owned(),
            item_id: None,
            turn_id: None,
        });
        let rendered = screen(&state, &UiState::default(), 100, 20);
        assert!(rendered.contains("safe[31m    text"));
        assert!(!rendered.contains('\u{1b}'));
        assert!(!rendered.contains('\u{009b}'));
        assert_eq!(sanitize_terminal_text("a\r\u{1b}\tb"), "a    b");
    }

    #[test]
    fn composer_wraps_long_wide_input_and_keeps_tail_cursor_visible() {
        let normal_ui = UiState {
            composer: format!("{}{}", "a".repeat(50), "界".repeat(20)),
            ..UiState::default()
        };
        let mut normal = draw(&ready(), &normal_ui, 40, 16);
        assert_eq!(
            normal.backend_mut().get_cursor_position().unwrap(),
            Position::new(15, 13)
        );

        let small_ui = UiState {
            composer: "界".repeat(70),
            ..UiState::default()
        };
        let mut small = draw(&ready(), &small_ui, 36, 9);
        assert_eq!(
            small.backend_mut().get_cursor_position().unwrap(),
            Position::new(5, 6)
        );
        let rendered = (0..9)
            .map(|y| {
                (0..36)
                    .map(|x| small.backend().buffer()[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains('界'));
    }

    #[test]
    fn wraps_long_catalog_notices_and_shows_actual_turn_failure() {
        let mut state = ready();
        state.notice = Some(
            "Available models: model-alpha, model-beta, model-gamma, model-delta, \
             model-epsilon, model-zeta, model-eta, model-theta, model-iota, model-kappa"
                .to_owned(),
        );
        let catalog = screen(&state, &UiState::default(), 50, 18);
        assert!(catalog.contains("Available models"));
        assert!(catalog.contains("model-kappa"));

        state.notice = None;
        state.turn = TurnState::Failed {
            turn_id: Some("turn".to_owned()),
            message: "The selected model\u{1b}[31m rejected this request; choose /model and retry."
                .to_owned(),
        };
        let failure = screen(&state, &UiState::default(), 54, 18);
        assert!(failure.contains("Turn failed"));
        assert!(failure.contains("selected model[31m rejected"));
        assert!(failure.contains("/model and retry."));
        assert!(!failure.contains('\u{1b}'));
    }

    #[test]
    fn input_supports_multiline_help_interrupt_scroll_and_quit() {
        let mut ui = UiState::default();
        ui.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('h'),
            KeyModifiers::NONE,
        )));
        ui.handle_event(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)));
        assert_eq!(ui.composer, "h\n");
        ui.composer = "/help".to_owned();
        assert!(ui
            .handle_event(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE
            )))
            .is_none());
        assert!(ui.overlay.is_some());
        assert!(ui
            .handle_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
            .is_none());
        assert!(matches!(
            ui.handle_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))),
            Some(crate::app::Intent::Interrupt)
        ));
        ui.handle_event(Event::Key(KeyEvent::new(
            KeyCode::PageUp,
            KeyModifiers::NONE,
        )));
        assert_eq!(ui.scroll_from_bottom, 1);
        assert!(matches!(
            ui.handle_event(Event::Key(KeyEvent::new(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL
            ))),
            Some(crate::app::Intent::Quit)
        ));
    }
}
